//! An invitation in the inbox, and the event it points at.
//!
//! Two halves meet here and they are stored a day apart: `messages.invite_uid`
//! is written when the mail syncs, `events.ical_uid` when the calendar does.
//! What these tests pin is what happens at each of the joints — including the
//! one that decides whether the feature can be trusted, which is the
//! invitation whose event has not arrived.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::Db;
use mach_lib::invite;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDb {
    path: PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mach-invite-test-{}-{}-{}.sqlite3",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).expect("open temp db");
        TempDb { path, db }
    }
}

impl std::ops::Deref for TempDb {
    type Target = Db;
    fn deref(&self) -> &Db {
        &self.db
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }
}

const UID: &str = "6r2h1c9k6ss30b9k6ss30b9k6s@google.com";
const NOW: i64 = 1_700_000_000_000;

fn account(db: &Db, email: &str) -> i64 {
    let conn = db.writer();
    q::upsert_account(
        &conn,
        &NewAccount {
            email: email.to_string(),
            display_name: Some(email.to_string()),
            token_ref: format!("keychain:{email}"),
            colour_index: 0,
        },
    )
    .expect("upsert account")
}

/// A conversation holding one message, with `invite_uid` set the way a sync
/// pass would have set it.
fn invitation_thread(db: &Db, account_id: i64, uid: Option<&str>) -> (i64, i64) {
    let conn = db.writer();
    let thread_id = q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: format!("t-{account_id}-{}", uid.unwrap_or("none")),
            participants: vec![Participant::new("alex@example.com")],
            subject: "Invitation: Quarterly review".into(),
            snippet: "Quarterly review".into(),
            last_message_at: NOW,
            is_unread: true,
            message_count: 1,
            has_attachments: true,
            label_ids: vec!["INBOX".into()],
        },
    )
    .expect("upsert thread");

    let message_id = q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: format!("m-{thread_id}"),
            from: Participant::new("alex@example.com"),
            subject: "Invitation: Quarterly review".into(),
            body_html: Some("<p>Google's own buttons live here</p>".into()),
            body_text: Some("Yes / No / Maybe".into()),
            snippet: "Quarterly review".into(),
            internal_date: NOW,
            is_unread: true,
            ..Default::default()
        },
    )
    .expect("upsert message");

    if let Some(uid) = uid {
        q::set_message_invitation(
            &conn,
            message_id,
            Some(&invite::Invitation {
                uid: uid.to_string(),
                method: invite::METHOD_REQUEST.into(),
            }),
        )
        .expect("set invitation");
    }

    (thread_id, message_id)
}

#[allow(clippy::too_many_arguments)]
fn event(
    db: &Db,
    account_id: i64,
    google_event_id: &str,
    uid: Option<&str>,
    start_ts: i64,
    response: Option<RsvpStatus>,
    series: Option<&str>,
    status: &str,
) -> i64 {
    let conn = db.writer();
    q::upsert_event(
        &conn,
        &NewEvent {
            account_id,
            calendar_id: "primary".into(),
            google_event_id: google_event_id.to_string(),
            title: "Quarterly review".into(),
            location: Some("Room 4".into()),
            start_ts,
            end_ts: start_ts + 3_600_000,
            rsvp_status: response,
            recurring_event_id: series.map(str::to_string),
            ical_uid: uid.map(str::to_string),
            status: status.to_string(),
            ..Default::default()
        },
    )
    .expect("upsert event")
}

// ---------------------------------------------------------------------------
// the join
// ---------------------------------------------------------------------------

#[test]
fn a_uid_finds_the_event_it_names() {
    let t = TempDb::new("match");
    let account_id = account(&t, "bruno@example.com");
    let event_id = event(
        &t,
        account_id,
        "evt-1",
        Some(UID),
        NOW + 86_400_000,
        Some(RsvpStatus::NeedsAction),
        None,
        "confirmed",
    );
    let (thread_id, _) = invitation_thread(&t, account_id, Some(UID));

    let conn = t.reader();
    let found = q::invitation_event(&conn, account_id, UID, NOW)
        .unwrap()
        .expect("the event the uid names");
    assert_eq!(found.event_id, Some(event_id));
    assert_eq!(found.title.as_deref(), Some("Quarterly review"));
    assert_eq!(found.location.as_deref(), Some("Room 4"));
    assert_eq!(found.response, Some(RsvpStatus::NeedsAction));
    assert!(!found.recurring);

    // And the same answer arrives on the message the reading pane renders.
    let detail = q::thread_with_messages(&conn, thread_id)
        .unwrap()
        .expect("the thread");
    let invitation = detail.messages[0]
        .invitation
        .as_ref()
        .expect("an invitation on the message");
    assert_eq!(invitation.uid, UID);
    assert_eq!(invitation.method, invite::METHOD_REQUEST);
    assert_eq!(invitation.event_id, Some(event_id));
}

#[test]
fn an_invitation_whose_event_is_not_here_yet_resolves_to_no_event() {
    let t = TempDb::new("absent");
    let account_id = account(&t, "bruno@example.com");
    let (thread_id, _) = invitation_thread(&t, account_id, Some(UID));

    let conn = t.reader();
    assert_eq!(q::invitation_event(&conn, account_id, UID, NOW).unwrap(), None);

    let detail = q::thread_with_messages(&conn, thread_id)
        .unwrap()
        .expect("the thread");
    let invitation = detail.messages[0]
        .invitation
        .as_ref()
        .expect("the message is still known to be an invitation");
    assert_eq!(
        invitation.event_id, None,
        "no event means no id to RSVP against, and the interface must say so"
    );
    assert_eq!(invitation.response, None);
    assert_eq!(invitation.title, None);
}

#[test]
fn an_event_on_another_account_is_not_a_match() {
    let t = TempDb::new("cross-account");
    let mine = account(&t, "bruno@example.com");
    let theirs = account(&t, "other@example.com");
    event(
        &t,
        theirs,
        "evt-1",
        Some(UID),
        NOW + 86_400_000,
        Some(RsvpStatus::Accepted),
        None,
        "confirmed",
    );

    let conn = t.reader();
    assert_eq!(
        q::invitation_event(&conn, mine, UID, NOW).unwrap(),
        None,
        "answering as the wrong address is worse than not answering"
    );
    assert!(q::invitation_event(&conn, theirs, UID, NOW).unwrap().is_some());
}

#[test]
fn a_cancelled_row_is_not_offered() {
    let t = TempDb::new("cancelled");
    let account_id = account(&t, "bruno@example.com");
    event(
        &t,
        account_id,
        "evt-1",
        Some(UID),
        NOW + 86_400_000,
        None,
        None,
        "cancelled",
    );

    let conn = t.reader();
    assert_eq!(q::invitation_event(&conn, account_id, UID, NOW).unwrap(), None);
}

#[test]
fn the_next_occurrence_of_a_series_is_the_one_answered() {
    let t = TempDb::new("series");
    let account_id = account(&t, "bruno@example.com");
    let day = 86_400_000;
    // Three occurrences of one meeting: two behind us, one ahead.
    event(&t, account_id, "evt_1", Some(UID), NOW - 7 * day, None, Some("evt"), "confirmed");
    event(&t, account_id, "evt_2", Some(UID), NOW - day, None, Some("evt"), "confirmed");
    let next = event(
        &t,
        account_id,
        "evt_3",
        Some(UID),
        NOW + 2 * day,
        Some(RsvpStatus::Tentative),
        Some("evt"),
        "confirmed",
    );

    let conn = t.reader();
    let found = q::invitation_event(&conn, account_id, UID, NOW)
        .unwrap()
        .expect("an occurrence");
    assert_eq!(found.event_id, Some(next));
    assert_eq!(found.response, Some(RsvpStatus::Tentative));
    assert!(found.recurring, "so the interface can say which occurrence this answers");
}

#[test]
fn ordinary_mail_carries_no_invitation() {
    let t = TempDb::new("ordinary");
    let account_id = account(&t, "bruno@example.com");
    let (thread_id, _) = invitation_thread(&t, account_id, None);

    let conn = t.reader();
    let detail = q::thread_with_messages(&conn, thread_id)
        .unwrap()
        .expect("the thread");
    assert_eq!(detail.messages[0].invitation, None);
}

#[test]
fn re_syncing_a_message_that_stopped_being_an_invitation_clears_it() {
    let t = TempDb::new("clear");
    let account_id = account(&t, "bruno@example.com");
    let (thread_id, message_id) = invitation_thread(&t, account_id, Some(UID));

    {
        let conn = t.writer();
        q::set_message_invitation(&conn, message_id, None).unwrap();
    }

    let conn = t.reader();
    let detail = q::thread_with_messages(&conn, thread_id).unwrap().unwrap();
    assert_eq!(detail.messages[0].invitation, None);
}

// ---------------------------------------------------------------------------
// the message half, as sync writes it
// ---------------------------------------------------------------------------

/// The whole path from a Gmail payload to the stored uid.
///
/// `prepare_message` is the seam the sync loop calls, so this is the test that
/// would fail if the calendar part stopped being reachable — if `walk` started
/// dropping parts with no filename, say, or if Gmail's inline `data` moved.
#[test]
fn a_google_invitation_payload_yields_a_uid() {
    use mach_lib::google::types as g;
    use mach_lib::sync::convert;

    let ics = format!(
        "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{UID}\r\n\
         SUMMARY:Quarterly review\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let payload: g::Message = serde_json::from_value(serde_json::json!({
        "id": "m1",
        "threadId": "t1",
        "labelIds": ["INBOX"],
        "internalDate": "1700000000000",
        "snippet": "Quarterly review",
        "payload": {
            "mimeType": "multipart/mixed",
            "headers": [
                {"name": "Subject", "value": "Invitation: Quarterly review"},
                {"name": "From", "value": "Alex <alex@example.com>"},
            ],
            "parts": [
                {
                    "mimeType": "multipart/alternative",
                    "parts": [
                        {
                            "mimeType": "text/plain",
                            "body": {"data": base64url("Quarterly review"), "size": 16},
                        },
                        {
                            "mimeType": "text/html",
                            "body": {"data": base64url("<p>Quarterly review</p>"), "size": 23},
                        },
                        {
                            // No filename: not a file anybody sees, and the only
                            // copy of the uid that arrives without a request.
                            "mimeType": "text/calendar",
                            "body": {"data": base64url(&ics), "size": ics.len()},
                        },
                    ],
                },
                {
                    "mimeType": "application/ics",
                    "filename": "invite.ics",
                    "body": {"attachmentId": "ANGjdJ", "size": ics.len()},
                },
            ],
        },
    }))
    .expect("a Gmail payload");

    let prepared = convert::prepare_message(1, &payload);
    let invitation = prepared.invitation.expect("the calendar part was read");
    assert_eq!(invitation.uid, UID);
    assert_eq!(invitation.method, invite::METHOD_REQUEST);

    // Recognising the invitation must not swallow any part: the attachment
    // rows are exactly what they were before this feature existed — the
    // nameless calendar part and the `invite.ics` beside it.
    assert_eq!(prepared.attachments.len(), 2);
    assert!(prepared
        .attachments
        .iter()
        .any(|a| a.filename == "invite.ics"));
}

#[test]
fn a_reply_payload_yields_nothing() {
    use mach_lib::google::types as g;
    use mach_lib::sync::convert;

    let ics = format!(
        "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:{UID}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let payload: g::Message = serde_json::from_value(serde_json::json!({
        "id": "m2",
        "threadId": "t1",
        "internalDate": "1700000000000",
        "payload": {
            "mimeType": "multipart/alternative",
            "headers": [{"name": "Subject", "value": "Accepted: Quarterly review"}],
            "parts": [
                {"mimeType": "text/plain", "body": {"data": base64url("Sam accepted"), "size": 12}},
                {"mimeType": "text/calendar", "body": {"data": base64url(&ics), "size": ics.len()}},
            ],
        },
    }))
    .expect("a Gmail payload");

    assert_eq!(convert::prepare_message(1, &payload).invitation, None);
}

/// Gmail's base64url, without the crate that would otherwise be a dependency
/// of the test only.
fn base64url(text: &str) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = text.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            }
        }
    }
    out
}
