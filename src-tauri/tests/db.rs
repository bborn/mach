//! Tests for the local SQLite store (U1).
//!
//! These are written against the public API of `mach_lib::db`. They use
//! temp-file databases (so WAL is real) except where an in-memory database is
//! explicitly the thing under test. No external crates: temp paths come from
//! `std::env::temp_dir()` with a process/counter-unique name and are removed on
//! drop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::schema;
use mach_lib::db::Db;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp-file database that deletes itself (and its -wal/-shm siblings).
struct TempDb {
    path: PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mach-db-test-{}-{}-{}.sqlite3",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).expect("open temp db");
        TempDb { path, db }
    }

    fn reopen(&self) -> Db {
        Db::open(&self.path).expect("reopen temp db")
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

fn account(db: &Db, email: &str, colour: i64) -> i64 {
    let conn = db.writer();
    q::upsert_account(
        &conn,
        &NewAccount {
            email: email.to_string(),
            display_name: Some(email.to_string()),
            token_ref: format!("keychain:{email}"),
            colour_index: colour,
        },
    )
    .expect("upsert account")
}

fn thread(db: &Db, account_id: i64, gmail_id: &str, subject: &str, at: i64) -> i64 {
    let conn = db.writer();
    q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: gmail_id.to_string(),
            participants: vec![Participant {
                name: Some("Tawny".into()),
                email: "tawny@example.com".into(),
            }],
            subject: subject.to_string(),
            snippet: format!("snippet of {subject}"),
            last_message_at: at,
            is_unread: true,
            message_count: 1,
            has_attachments: false,
            label_ids: vec!["INBOX".to_string()],
        },
    )
    .expect("upsert thread")
}

fn message(db: &Db, thread_id: i64, account_id: i64, gmail_id: &str, subject: &str, body: &str, at: i64) -> i64 {
    let conn = db.writer();
    q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: gmail_id.to_string(),
            rfc822_message_id: Some(format!("<{gmail_id}@example.com>")),
            reply_to: Vec::new(),
            in_reply_to: None,
            references: None,
            from: Participant {
                name: Some("Tawny".into()),
                email: "tawny@example.com".into(),
            },
            to: vec![Participant {
                name: None,
                email: "alex@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: subject.to_string(),
            body_html: Some(format!("<p>{body}</p>")),
            body_text: Some(body.to_string()),
            snippet: body.chars().take(40).collect(),
            internal_date: at,
            is_unread: true,
            is_draft: false,
        },
    )
    .expect("upsert message")
}

// ---------------------------------------------------------------------------
// migrations
// ---------------------------------------------------------------------------

#[test]
fn migrations_apply_cleanly_on_a_fresh_db() {
    let t = TempDb::new("fresh");
    let conn = t.reader();

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, schema::LATEST_VERSION as i64);

    for table in [
        "accounts",
        "threads",
        "thread_labels",
        "messages",
        "labels",
        "attachments",
        "events",
        "calendars",
        "messages_fts",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "expected table/vtable `{table}` to exist");
    }
}

#[test]
fn migrations_are_idempotent() {
    let t = TempDb::new("idem");

    let account_id = account(&t, "a@example.com", 0);
    let thread_id = thread(&t, account_id, "t1", "Hello", 1_000);

    // Re-running the migration runner directly must be a no-op.
    {
        let mut conn = t.writer();
        schema::migrate(&mut conn).expect("second migrate");
        schema::migrate(&mut conn).expect("third migrate");
    }
    // Re-opening the database re-runs the runner too.
    let again = t.reopen();

    let conn = again.reader();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, schema::LATEST_VERSION as i64);

    // Data survived; nothing was dropped and recreated.
    let found = q::thread_with_messages(&conn, thread_id).unwrap();
    assert!(found.is_some(), "thread must survive re-migration");
}

#[test]
fn an_older_database_gains_the_new_event_columns_without_losing_its_events() {
    // The upgrade path, not the fresh-install path: `ALTER TABLE` runs against a
    // populated `events`, and every new column has to read back as "we were
    // never told" rather than as a value nobody meant.
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let target = schema::MIGRATIONS
        .iter()
        .map(|m| m.version)
        .filter(|v| *v < schema::LATEST_VERSION)
        .max()
        .expect("a version before the newest");

    for migration in schema::MIGRATIONS.iter().filter(|m| m.version <= target) {
        let tx = conn.transaction().unwrap();
        tx.execute_batch(migration.sql).unwrap();
        tx.pragma_update(None, "user_version", migration.version)
            .unwrap();
        tx.commit().unwrap();
    }

    conn.execute(
        "INSERT INTO accounts (id, email, token_ref) VALUES (1, 'a@example.com', '')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (account_id, calendar_id, google_event_id, title, start_ts, end_ts)
         VALUES (1, 'primary', 'evt-1', 'Standup', 10, 20)",
        [],
    )
    .unwrap();

    assert_eq!(schema::migrate(&mut conn).unwrap(), schema::LATEST_VERSION);

    let events = q::events_in_range(&conn, 0, 100, None).unwrap();
    assert_eq!(events.len(), 1, "the row survived the ALTER");
    assert_eq!(events[0].title, "Standup");
    assert!(events[0].recurrence.is_empty());
    assert!(events[0].reminders.is_none());
    assert!(events[0].ical_uid.is_none());
    // Not `Some(false)`. "We do not know who organized this" must not read as
    // "you are not the organizer", or the first launch after the upgrade would
    // make every pre-existing event uneditable until the next sync.
    assert_eq!(events[0].organizer_self, None);
    assert_eq!(events[0].guests_can_modify, None);
    // Migration 7's columns, read on a row that predates them. A conference
    // nobody mentioned is `None` rather than an empty conference block, and an
    // unknown visibility is `None` rather than "default" — the store does not
    // get to invent Google's answers.
    assert!(events[0].conference.is_none());
    assert!(events[0].creator.is_none());
    assert!(events[0].attachments.is_empty());
    assert!(events[0].visibility.is_none());
    assert!(events[0].transparency.is_none());
    // No guest list was stored, so the addresses are projected into one. Both
    // are empty here; the projection is asserted against real attendees in
    // `a_guest_list_without_answers_is_projected_from_the_addresses`.
    assert!(events[0].guests.is_empty());
}

#[test]
fn a_guest_list_without_answers_is_projected_from_the_addresses() {
    // Every row written before migration 7 has addresses and no answers, and so
    // does every row a local guest-list edit has just touched. A reader must not
    // have to know which of the two columns holds the truth, so `guests` is
    // filled from `attendees` when it is `NULL` — with no `response`, which is
    // the honest way to say that nobody has told us.
    let db = Db::open_in_memory().unwrap();
    let account_id = q::upsert_account(
        &db.writer(),
        &NewAccount {
            email: "alex@example.com".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = db.writer();
    q::upsert_event(
        &conn,
        &NewEvent {
            account_id,
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: 10,
            end_ts: 20,
            attendees: vec![
                Participant {
                    name: Some("Tawny".into()),
                    email: "tawny@example.com".into(),
                },
                Participant::new("sean@offerlab.com"),
            ],
            status: "confirmed".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let events = q::events_in_range(&conn, 0, 100, None).unwrap();
    let guests = &events[0].guests;
    assert_eq!(guests.len(), 2);
    assert_eq!(guests[0].email, "tawny@example.com");
    assert_eq!(guests[0].name.as_deref(), Some("Tawny"));
    assert_eq!(guests[0].response, None, "no answer is not needsAction");
    assert!(!guests[1].organizer);
}

// ---------------------------------------------------------------------------
// calendar metadata (migration 6)
// ---------------------------------------------------------------------------

fn calendar(account_id: i64, calendar_id: &str) -> NewCalendar {
    NewCalendar {
        account_id,
        calendar_id: calendar_id.to_string(),
        selected: true,
        synced_at: 1_000,
        ..Default::default()
    }
}

#[test]
fn a_calendar_row_round_trips_every_field_google_sends() {
    let t = TempDb::new("calendar-round-trip");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    q::upsert_calendar(
        &conn,
        &NewCalendar {
            summary: Some("Ben — school".into()),
            summary_override: Some("Dad/Ben Schedule".into()),
            description: Some("Pickups".into()),
            time_zone: Some("America/Chicago".into()),
            color_id: Some("7".into()),
            background_color: Some("#9fe1e7".into()),
            foreground_color: Some("#000000".into()),
            access_role: Some("reader".into()),
            is_primary: false,
            ..calendar(a, "ben@group.calendar.google.com")
        },
    )
    .expect("upsert calendar");

    let rows = q::list_calendars(&conn, Some(a)).expect("list");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.summary.as_deref(), Some("Ben — school"));
    assert_eq!(row.background_color.as_deref(), Some("#9fe1e7"));
    assert_eq!(row.time_zone.as_deref(), Some("America/Chicago"));
    // The override is the name the user recognises, so it is the one `title`
    // answers with.
    assert_eq!(row.title(), Some("Dad/Ben Schedule"));
    assert!(!row.writable(), "a reader may not be offered an editor");
    assert!(row.selected);
    assert!(!row.deleted);
}

#[test]
fn an_unknown_access_role_stays_permissive() {
    // Silence, and anything Google invents after this was written, must mean
    // "not told" rather than "denied" — the same rule `organizer_self` follows.
    let t = TempDb::new("calendar-access");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    for (role, writable) in [
        (None, true),
        (Some("owner"), true),
        (Some("writer"), true),
        (Some("reader"), false),
        (Some("freeBusyReader"), false),
        (Some("somethingNew"), true),
    ] {
        q::upsert_calendar(
            &conn,
            &NewCalendar {
                access_role: role.map(str::to_string),
                ..calendar(a, "c@example.com")
            },
        )
        .unwrap();
        let rows = q::list_calendars(&conn, Some(a)).unwrap();
        assert_eq!(rows[0].writable(), writable, "role {role:?}");
    }
}

#[test]
fn resyncing_replaces_metadata_rather_than_accumulating_rows() {
    let t = TempDb::new("calendar-upsert");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    q::upsert_calendar(
        &conn,
        &NewCalendar {
            summary: Some("Old name".into()),
            description: Some("Once had one".into()),
            ..calendar(a, "team@group.calendar.google.com")
        },
    )
    .unwrap();
    q::upsert_calendar(
        &conn,
        &NewCalendar {
            summary: Some("New name".into()),
            synced_at: 2_000,
            ..calendar(a, "team@group.calendar.google.com")
        },
    )
    .unwrap();

    let rows = q::list_calendars(&conn, Some(a)).unwrap();
    assert_eq!(rows.len(), 1, "one calendar, one row");
    assert_eq!(rows[0].title(), Some("New name"));
    // Unlike an event upsert, a NULL here is a real answer: the description was
    // cleared in Google and must not be resurrected from the old row.
    assert_eq!(rows[0].description, None);
    assert_eq!(q::calendars_synced_at(&conn, a).unwrap(), Some(2_000));
}

#[test]
fn an_unsubscribed_calendar_is_tombstoned_rather_than_deleted() {
    let t = TempDb::new("calendar-tombstone");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    q::upsert_calendar(&conn, &calendar(a, "kept@example.com")).unwrap();
    q::upsert_calendar(&conn, &calendar(a, "gone@group.calendar.google.com")).unwrap();

    let marked =
        q::tombstone_missing_calendars(&conn, a, &["kept@example.com".to_string()]).unwrap();
    assert_eq!(marked, 1);

    let rows = q::list_calendars(&conn, Some(a)).unwrap();
    assert_eq!(rows.len(), 2, "the row survives so its events keep a name");
    let gone = rows
        .iter()
        .find(|c| c.calendar_id.starts_with("gone"))
        .unwrap();
    assert!(gone.deleted);

    // Running it again marks nothing new, so a steady state does not churn.
    let again =
        q::tombstone_missing_calendars(&conn, a, &["kept@example.com".to_string()]).unwrap();
    assert_eq!(again, 0);
}

#[test]
fn calendars_belong_to_their_account_and_leave_with_it() {
    let t = TempDb::new("calendar-cascade");
    let a = account(&t, "a@example.com", 0);
    let b = account(&t, "b@example.com", 1);
    let conn = t.writer();

    q::upsert_calendar(&conn, &calendar(a, "shared@group.calendar.google.com")).unwrap();
    q::upsert_calendar(&conn, &calendar(b, "shared@group.calendar.google.com")).unwrap();
    // The same Google calendar subscribed from two accounts is two rows, because
    // the name and the colour are per-subscription.
    assert_eq!(q::list_calendars(&conn, None).unwrap().len(), 2);

    conn.execute("DELETE FROM accounts WHERE id = ?1", [b]).unwrap();
    let rows = q::list_calendars(&conn, None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account_id, a);
}

#[test]
fn pragmas_are_configured_for_a_desktop_app() {
    let t = TempDb::new("pragma");
    let conn = t.reader();

    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal.to_lowercase(), "wal");

    let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(fk, 1, "foreign keys must be on for every connection");

    let sync: i64 = conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
    assert_eq!(sync, 1, "synchronous = NORMAL (1) is the WAL sweet spot");
}

// ---------------------------------------------------------------------------
// FTS5 triggers
// ---------------------------------------------------------------------------

#[test]
fn inserting_a_message_makes_it_findable() {
    let t = TempDb::new("fts-insert");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Invoice", 1_000);
    message(&t, th, a, "m1", "Invoice", "the quarterly velocipede statement", 1_000);

    let conn = t.reader();
    let hits = q::search_threads(&conn, "velocipede", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].thread_id, th);

    // Subject is indexed too.
    let hits = q::search_threads(&conn, "invoice", 10).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn updating_a_body_changes_what_matches() {
    let t = TempDb::new("fts-update");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    message(&t, th, a, "m1", "Subject", "original velocipede text", 1_000);

    // Same gmail message id => upsert path => UPDATE => trigger must reindex.
    message(&t, th, a, "m1", "Subject", "replaced with dirigible text", 1_000);

    let conn = t.reader();
    assert!(
        q::search_threads(&conn, "velocipede", 10).unwrap().is_empty(),
        "old term must be gone from the index"
    );
    assert_eq!(
        q::search_threads(&conn, "dirigible", 10).unwrap().len(),
        1,
        "new term must be in the index"
    );
}

#[test]
fn deleting_a_message_removes_it_from_the_index() {
    let t = TempDb::new("fts-delete");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    let m = message(&t, th, a, "m1", "Subject", "solitary velocipede", 1_000);

    {
        let conn = t.writer();
        q::delete_message(&conn, m).unwrap();
    }

    let conn = t.reader();
    assert!(q::search_threads(&conn, "velocipede", 10).unwrap().is_empty());
}

#[test]
fn cascading_a_thread_delete_also_clears_the_index() {
    // Deleting a thread cascades to its messages; the FTS delete trigger has to
    // fire for those cascade-deleted rows or the index silently drifts.
    let t = TempDb::new("fts-cascade");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    message(&t, th, a, "m1", "Subject", "cascading velocipede", 1_000);

    {
        let conn = t.writer();
        q::delete_thread(&conn, th).unwrap();
    }

    let conn = t.reader();
    assert!(
        q::search_threads(&conn, "velocipede", 10).unwrap().is_empty(),
        "FTS index must not outlive cascade-deleted messages"
    );
    let orphans: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(orphans, 0);

    // Ask the index itself, not the joined query: a join against `messages`
    // would hide a stale FTS entry rather than reveal it, and a drifting index
    // grows forever and starts scoring against text nobody can see.
    let stale: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'velocipede'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0, "messages_fts still holds terms for deleted rows");
}

#[test]
fn deleting_a_message_leaves_no_stale_index_entry() {
    let t = TempDb::new("fts-nostale");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    let m = message(&t, th, a, "m1", "Subject", "solitary velocipede", 1_000);
    {
        let conn = t.writer();
        q::delete_message(&conn, m).unwrap();
    }
    let conn = t.reader();
    let stale: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'velocipede'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0);
}

#[test]
fn search_results_are_ranked_subject_hits_first() {
    let t = TempDb::new("fts-rank");
    let a = account(&t, "a@example.com", 0);

    let body_hit = thread(&t, a, "t1", "Weekly notes", 1_000);
    message(&t, body_hit, a, "m1", "Weekly notes", "we discussed the velocipede at length", 1_000);

    let subject_hit = thread(&t, a, "t2", "Velocipede", 2_000);
    message(&t, subject_hit, a, "m2", "Velocipede", "see attached", 2_000);

    let conn = t.reader();
    let hits = q::search_threads(&conn, "velocipede", 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].thread_id, subject_hit,
        "a subject match outranks a body match"
    );
    assert!(hits[0].score <= hits[1].score, "scores must be ascending");
    assert_eq!(hits[0].account_id, a);
}

#[test]
fn fts_query_is_forgiving_of_user_input() {
    let t = TempDb::new("fts-input");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    message(&t, th, a, "m1", "Subject", "quarterly velocipede statement", 1_000);

    let conn = t.reader();
    // Prefix matching, punctuation and FTS operators must not blow up.
    assert_eq!(q::search_threads(&conn, "veloci", 10).unwrap().len(), 1);
    assert_eq!(q::search_threads(&conn, "\"unbalanced", 10).unwrap().len(), 0);
    assert_eq!(q::search_threads(&conn, "AND OR NOT", 10).unwrap().len(), 0);
    assert_eq!(q::search_threads(&conn, "   ", 10).unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// unified cross-account list
// ---------------------------------------------------------------------------

#[test]
fn unified_list_interleaves_accounts_by_timestamp() {
    let t = TempDb::new("unified");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    let a3 = account(&t, "three@example.com", 2);

    thread(&t, a1, "t1", "oldest", 1_000);
    thread(&t, a2, "t2", "second", 2_000);
    thread(&t, a1, "t3", "third", 3_000);
    thread(&t, a3, "t4", "newest", 4_000);

    let conn = t.reader();
    let rows = q::list_threads(&conn, &ThreadQuery::default()).unwrap();

    let subjects: Vec<&str> = rows.iter().map(|r| r.subject.as_str()).collect();
    assert_eq!(subjects, vec!["newest", "third", "second", "oldest"]);

    // The per-account colour bar data rides along on the row.
    assert_eq!(rows[0].account_email, "three@example.com");
    assert_eq!(rows[0].account_colour_index, 2);
    assert_eq!(rows[3].account_email, "one@example.com");

    // Label refs come back with the row.
    assert_eq!(rows[0].label_ids, vec!["INBOX".to_string()]);
}

#[test]
fn unified_list_paginates_with_a_cursor() {
    let t = TempDb::new("paginate");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);

    for i in 0..10 {
        let acct = if i % 2 == 0 { a1 } else { a2 };
        thread(&t, acct, &format!("t{i}"), &format!("s{i}"), 1_000 + i as i64);
    }

    let conn = t.reader();

    let page1 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page1.len(), 4);
    assert_eq!(
        page1.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["s9", "s8", "s7", "s6"]
    );

    let page2 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            after: Some(page1.last().unwrap().cursor()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        page2.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["s5", "s4", "s3", "s2"]
    );

    let page3 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            after: Some(page2.last().unwrap().cursor()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        page3.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["s1", "s0"]
    );

    let page4 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            after: Some(page3.last().unwrap().cursor()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(page4.is_empty(), "walking off the end returns empty, not an error");
}

#[test]
fn cursor_breaks_ties_on_identical_timestamps() {
    let t = TempDb::new("ties");
    let a = account(&t, "one@example.com", 0);
    for i in 0..5 {
        thread(&t, a, &format!("t{i}"), &format!("s{i}"), 7_000);
    }

    let conn = t.reader();
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = q::list_threads(
            &conn,
            &ThreadQuery {
                limit: 2,
                after: cursor,
                ..Default::default()
            },
        )
        .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = Some(page.last().unwrap().cursor());
        seen.extend(page.into_iter().map(|r| r.subject));
    }
    assert_eq!(seen.len(), 5, "no row skipped or repeated across pages");
    seen.sort();
    assert_eq!(seen, vec!["s0", "s1", "s2", "s3", "s4"]);
}

#[test]
fn list_can_be_filtered_to_one_account_and_one_label() {
    let t = TempDb::new("filter");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    thread(&t, a1, "t1", "from-one", 1_000);
    thread(&t, a2, "t2", "from-two", 2_000);

    // A thread that is not in the inbox.
    {
        let conn = t.writer();
        q::upsert_thread(
            &conn,
            &NewThread {
                account_id: a1,
                gmail_thread_id: "t3".into(),
                participants: vec![],
                subject: "archived".into(),
                snippet: String::new(),
                last_message_at: 3_000,
                is_unread: false,
                message_count: 1,
                has_attachments: false,
                label_ids: vec!["Label_7".into()],
            },
        )
        .unwrap();
    }

    let conn = t.reader();

    let only_a1 = q::list_threads(
        &conn,
        &ThreadQuery {
            account_id: Some(a1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        only_a1.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["archived", "from-one"]
    );

    let inbox = q::list_threads(
        &conn,
        &ThreadQuery {
            label_id: Some("INBOX".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        inbox.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["from-two", "from-one"]
    );

    let unread_in_a1 = q::list_threads(
        &conn,
        &ThreadQuery {
            account_id: Some(a1),
            unread_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        unread_in_a1.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["from-one"]
    );
}

// ---------------------------------------------------------------------------
// thread with messages
// ---------------------------------------------------------------------------

#[test]
fn thread_with_messages_returns_the_conversation_oldest_first() {
    let t = TempDb::new("thread-msgs");
    let a = account(&t, "one@example.com", 0);
    let th = thread(&t, a, "t1", "Re: lunch", 3_000);
    message(&t, th, a, "m3", "Re: lunch", "third", 3_000);
    message(&t, th, a, "m1", "lunch", "first", 1_000);
    message(&t, th, a, "m2", "Re: lunch", "second", 2_000);

    {
        let conn = t.writer();
        let m = q::thread_with_messages(&conn, th).unwrap().unwrap().messages[0].id;
        q::upsert_attachment(
            &conn,
            &NewAttachment {
                message_id: m,
                gmail_attachment_id: Some("att1".into()),
                filename: "menu.pdf".into(),
                mime_type: "application/pdf".into(),
                size_bytes: 4096,
                local_path: None,
            },
        )
        .unwrap();
    }

    let conn = t.reader();
    let found = q::thread_with_messages(&conn, th).unwrap().unwrap();
    assert_eq!(found.thread.subject, "Re: lunch");
    assert_eq!(
        found
            .messages
            .iter()
            .map(|m| m.body_text.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["first", "second", "third"]
    );
    assert_eq!(found.messages[0].attachments.len(), 1);
    assert_eq!(found.messages[0].attachments[0].filename, "menu.pdf");
    assert_eq!(found.messages[0].from.email, "tawny@example.com");
    assert_eq!(found.messages[0].to.len(), 1);

    assert!(q::thread_with_messages(&conn, 9_999).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// foreign keys
// ---------------------------------------------------------------------------

#[test]
fn foreign_keys_are_enforced() {
    let t = TempDb::new("fk-enforce");
    let a = account(&t, "one@example.com", 0);
    let conn = t.writer();
    let err = q::upsert_message(
        &conn,
        &NewMessage {
            thread_id: 4_242,
            account_id: a,
            gmail_message_id: "m1".into(),
            rfc822_message_id: None,
            reply_to: Vec::new(),
            in_reply_to: None,
            references: None,
            from: Participant::default(),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            subject: String::new(),
            body_html: None,
            body_text: None,
            snippet: String::new(),
            internal_date: 0,
            is_unread: false,
            is_draft: false,
        },
    );
    assert!(err.is_err(), "orphan message must be rejected");
}

#[test]
fn deleting_a_thread_cascades_to_messages_and_attachments() {
    let t = TempDb::new("fk-thread");
    let a = account(&t, "one@example.com", 0);
    let th = thread(&t, a, "t1", "s", 1_000);
    let m = message(&t, th, a, "m1", "s", "body", 1_000);
    {
        let conn = t.writer();
        q::upsert_attachment(
            &conn,
            &NewAttachment {
                message_id: m,
                gmail_attachment_id: None,
                filename: "a.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 3,
                local_path: None,
            },
        )
        .unwrap();
        q::delete_thread(&conn, th).unwrap();
    }

    let conn = t.reader();
    for (table, expected) in [("threads", 0), ("messages", 0), ("attachments", 0), ("thread_labels", 0)] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, expected, "{table} should be empty after cascade");
    }
}

#[test]
fn deleting_an_account_cascades_to_everything_it_owns() {
    let t = TempDb::new("fk-account");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    let th1 = thread(&t, a1, "t1", "s1", 1_000);
    message(&t, th1, a1, "m1", "s1", "keepme", 1_000);
    let th2 = thread(&t, a2, "t2", "s2", 2_000);
    message(&t, th2, a2, "m2", "s2", "keepme", 2_000);

    {
        let conn = t.writer();
        q::upsert_label(
            &conn,
            &NewLabel {
                account_id: a1,
                gmail_label_id: "INBOX".into(),
                name: "Inbox".into(),
                label_type: LabelType::System,
            },
        )
        .unwrap();
        q::upsert_event(
            &conn,
            &NewEvent {
                account_id: a1,
                calendar_id: "primary".into(),
                google_event_id: "e1".into(),
                title: "standup".into(),
                description: None,
                location: None,
                start_ts: 10,
                end_ts: 20,
                is_all_day: false,
                attendees: vec![],
                rsvp_status: Some(RsvpStatus::Accepted),
                recurring_event_id: None,
                status: "confirmed".into(),
                html_link: None,
                updated_at: 0,
                ..Default::default()
            },
        )
        .unwrap();
        q::delete_account(&conn, a1).unwrap();
    }

    let conn = t.reader();
    let rows = q::list_threads(&conn, &ThreadQuery::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account_email, "two@example.com");

    for table in ["labels", "events"] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table} rows for the deleted account should be gone");
    }

    // The surviving account's message is still searchable; the deleted one is not.
    let hits = q::search_threads(&conn, "keepme", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].thread_id, th2);
}

// ---------------------------------------------------------------------------
// empty database edge cases
// ---------------------------------------------------------------------------

#[test]
fn an_empty_database_returns_empty_results_not_errors() {
    let t = TempDb::new("empty");
    let conn = t.reader();

    assert!(q::list_threads(&conn, &ThreadQuery::default()).unwrap().is_empty());
    assert!(q::list_threads(
        &conn,
        &ThreadQuery {
            account_id: Some(1),
            label_id: Some("INBOX".into()),
            unread_only: true,
            limit: 25,
            after: Some(ThreadCursor {
                last_message_at: 5,
                id: 5
            }),
        }
    )
    .unwrap()
    .is_empty());
    assert!(q::search_threads(&conn, "anything", 10).unwrap().is_empty());
    assert!(q::thread_with_messages(&conn, 1).unwrap().is_none());
    assert!(q::list_accounts(&conn).unwrap().is_empty());
    assert!(q::list_labels(&conn, None).unwrap().is_empty());
    assert!(q::events_in_range(&conn, 0, i64::MAX, None).unwrap().is_empty());
    assert!(q::unread_counts(&conn).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// accounts / labels / events
// ---------------------------------------------------------------------------

#[test]
fn account_upsert_is_stable_and_carries_the_history_watermark() {
    let t = TempDb::new("accounts");
    let id = account(&t, "one@example.com", 3);
    let same = account(&t, "one@example.com", 3);
    assert_eq!(id, same, "upsert by email must not create a second row");

    {
        let conn = t.writer();
        q::set_history_id(&conn, id, Some("987654321")).unwrap();
        q::set_calendar_sync_token(&conn, id, Some("tok-abc")).unwrap();
    }

    let conn = t.reader();
    let accounts = q::list_accounts(&conn).unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].history_id.as_deref(), Some("987654321"));
    assert_eq!(accounts[0].calendar_sync_token.as_deref(), Some("tok-abc"));
    assert_eq!(accounts[0].colour_index, 3);
    assert_eq!(accounts[0].token_ref, "keychain:one@example.com");
}

#[test]
fn events_in_range_spans_accounts_and_clips_to_the_window() {
    let t = TempDb::new("events");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    {
        let conn = t.writer();
        for (acct, id, title, start, end) in [
            (a1, "e1", "before", 0i64, 50i64),
            (a1, "e2", "inside", 200, 300),
            (a2, "e3", "overlaps-start", 90, 150),
            (a2, "e4", "after", 900, 950),
        ] {
            q::upsert_event(
                &conn,
                &NewEvent {
                    account_id: acct,
                    calendar_id: "primary".into(),
                    google_event_id: id.into(),
                    title: title.into(),
                    description: None,
                    location: None,
                    start_ts: start,
                    end_ts: end,
                    is_all_day: false,
                    attendees: vec![Participant {
                        name: None,
                        email: "x@example.com".into(),
                    }],
                    rsvp_status: None,
                    recurring_event_id: None,
                    status: "confirmed".into(),
                    html_link: None,
                    updated_at: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }

    let conn = t.reader();
    let window = q::events_in_range(&conn, 100, 400, None).unwrap();
    assert_eq!(
        window.iter().map(|e| e.title.clone()).collect::<Vec<_>>(),
        vec!["overlaps-start", "inside"]
    );
    assert_eq!(window[0].attendees.len(), 1);

    let only_a1 = q::events_in_range(&conn, 0, i64::MAX, Some(a1)).unwrap();
    assert_eq!(only_a1.len(), 2);
}

#[test]
fn unread_counts_are_reported_per_account() {
    let t = TempDb::new("unread");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    thread(&t, a1, "t1", "s", 1);
    thread(&t, a1, "t2", "s", 2);
    thread(&t, a2, "t3", "s", 3);
    {
        let conn = t.writer();
        q::set_thread_unread(&conn, 1, false).unwrap();
    }

    let conn = t.reader();
    let counts = q::unread_counts(&conn).unwrap();
    let get = |id: i64| counts.iter().find(|c| c.account_id == id).map(|c| c.unread).unwrap_or(0);
    assert_eq!(get(a1), 1);
    assert_eq!(get(a2), 1);
}

// ---------------------------------------------------------------------------
// connection handling
// ---------------------------------------------------------------------------

#[test]
fn in_memory_databases_work_for_tests() {
    let db = Db::open_in_memory().expect("open in-memory");
    let a = {
        let conn = db.writer();
        q::upsert_account(
            &conn,
            &NewAccount {
                email: "mem@example.com".into(),
                display_name: None,
                token_ref: "k".into(),
                colour_index: 0,
            },
        )
        .unwrap()
    };
    let conn = db.reader();
    assert_eq!(q::list_accounts(&conn).unwrap().len(), 1);
    assert_eq!(q::list_accounts(&conn).unwrap()[0].id, a);
}

#[test]
fn a_reader_sees_committed_writes_while_the_writer_keeps_working() {
    // The point of WAL: the UI's reads never queue behind the sync loop's
    // writes. Two threads, one Db handle, no deadlock, no "database is locked".
    let t = TempDb::new("concurrent");
    let a = account(&t, "one@example.com", 0);

    let writer_db = t.db.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..200 {
            let conn = writer_db.writer();
            q::upsert_thread(
                &conn,
                &NewThread {
                    account_id: a,
                    gmail_thread_id: format!("t{i}"),
                    participants: vec![],
                    subject: format!("s{i}"),
                    snippet: String::new(),
                    last_message_at: i as i64,
                    is_unread: false,
                    message_count: 1,
                    has_attachments: false,
                    label_ids: vec!["INBOX".into()],
                },
            )
            .expect("concurrent write");
        }
    });

    let mut max_seen = 0;
    for _ in 0..200 {
        let conn = t.reader();
        let rows = q::list_threads(&conn, &ThreadQuery::default()).unwrap();
        max_seen = max_seen.max(rows.len());
    }
    writer.join().unwrap();

    let conn = t.reader();
    let rows = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 500,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 200);
    let _ = max_seen;
}

#[test]
fn readers_cannot_write() {
    let t = TempDb::new("readonly");
    let conn = t.reader();
    let err = conn.execute(
        "INSERT INTO accounts (email, token_ref, colour_index, created_at) VALUES ('x', 'y', 0, 0)",
        [],
    );
    assert!(err.is_err(), "pool readers must be query_only");
}
