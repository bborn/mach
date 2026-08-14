//! The draft, as something the mailbox can see.
//!
//! # The bug this exists to end
//!
//! The agent drafted a reply, said so, and the Drafts mailbox read "Nothing in
//! DRAFT." Both statements were true. A composer draft was a row in
//! `compose_drafts`; the Drafts mailbox lists threads carrying Gmail's `DRAFT`
//! label, out of `thread_labels`. Two stores, and the one the user opens had
//! never heard of the other.
//!
//! # The fix, and why it is the outbox's fix
//!
//! Saving a draft writes a **mirror**: an ordinary `messages` row with
//! `is_draft = 1`, inside the conversation it answers, and a `DRAFT` row in
//! `thread_labels`. Nothing about the Drafts mailbox changes — it still lists
//! threads carrying a label — and the draft is in it immediately, with no
//! network in the path. That is the same optimistic write
//! [`outbox`](super::outbox) does for a sent message, for the same reason: the
//! UI never waits on Google.
//!
//! The mirror's `gmail_message_id` is a placeholder, `mach-draft:<draft id>`,
//! exactly as the outbox's is `mach-outbox:<id>`. When the push in
//! [`remote`](super::remote) comes back, [`adopt`] swaps in the real ids — so
//! the sync pass that later sees this draft coming down from Gmail upserts
//! *onto this row* rather than inserting a second copy beside it. **That is the
//! whole duplicate story**: one draft, one row, whichever end learns about it
//! first.
//!
//! A draft with no conversation behind it (`c`, or the agent writing to
//! somebody new) gets a synthetic thread keyed the same way, which is adopted
//! the same way.

use rusqlite::{params, OptionalExtension};

use crate::db::models::{NewMessage, NewThread, Participant};
use crate::db::{command_queries, queries, Db};

use super::draft::Draft;

use super::mime::Mailbox;
use super::{ensure_compose_schema, ComposeError, Result};

/// The namespace a not-yet-pushed draft's ids live in. Shares its shape with
/// the outbox's `mach-outbox:` so that "is this row ours and provisional" is
/// one question with one answer —
/// [`is_local_message_id`](crate::db::models::is_local_message_id), which the
/// command layer asks before naming any id to Gmail.
pub const MIRROR_PREFIX: &str = crate::db::models::DRAFT_ID_PREFIX;

pub fn mirror_message_id(draft_id: &str) -> String {
    format!("{MIRROR_PREFIX}{draft_id}")
}

/// Point every row this draft owns at the id it is filed under now, and take out
/// anything already sitting there that is not it.
///
/// # The identity a mirror has, and the one it does not
///
/// `gmail_message_id` is not an identity. **`drafts.update` mints a new message
/// id on every save**, whoever calls it, so the id a mirror answers to moves
/// under it several times while one draft is being written. Two writers moved
/// it — this module writing the row under the new id, [`adopt`] renaming the old
/// one — and `adopt` renamed with `UPDATE OR IGNORE`, which meant that when both
/// ran the collision was *swallowed*: the old row stayed, the new row stayed,
/// and the conversation held two `DRAFT` rows with byte-identical text. Every
/// removal path addresses a draft by at most two ids, so the loser was
/// unreachable — it survived the send (a `DRAFT` row beside the reply the owner
/// had just watched leave) and it survived ⌘⇧⌫ ("There is no draft here to throw
/// away"), until a sync pass happened to sweep it seconds or hours later.
///
/// `mach_draft_id` is the identity: the `compose_drafts` row the mirror stands
/// for, which does not change while the draft exists. This runs before every
/// write so the invariant is restored rather than assumed, and
/// `idx_messages_mach_draft` — the unique index
/// [`ensure_one_mirror_per_draft`](super::ensure_one_mirror_per_draft) creates —
/// turns a second row from a thing that happens quietly into a write that fails
/// loudly.
///
/// The row taken out of the way is Gmail's own copy of *this same draft*, landed
/// by a sync pass under the id this draft is about to occupy. It is not a second
/// message; it is this one, imported from the other end, and the mirror is
/// standing in for it.
fn claim_row(
    conn: &rusqlite::Connection,
    account_id: i64,
    draft_id: &str,
    gmail_message_id: &str,
    also: &[&str],
) -> rusqlite::Result<()> {
    // A row under this id that is not a draft is the message this draft became:
    // `drafts.send` hands back the id the draft was filed under. Nothing is
    // renamed onto it — the mirror is over, and the rows that were it go.
    let sent: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM messages
              WHERE account_id = ?1 AND gmail_message_id = ?2 AND is_draft = 0",
            params![account_id, gmail_message_id],
            |row| row.get(0),
        )
        .optional()?;

    // Every id this draft has answered to. The callers know at most two beyond
    // the current one — the placeholder it was written under and the message id
    // Gmail replaced — and both name rows that are this draft.
    let older = also.first().map(|id| id.to_string());
    let oldest = also.get(1).map(|id| id.to_string());
    const MINE: &str = "account_id = ?1 AND is_draft = 1 \
                        AND (mach_draft_id = ?2 OR gmail_message_id = ?3 \
                             OR gmail_message_id = ?4 OR gmail_message_id = ?5)";

    // The row that survives, and the reason for the order: the stamped row is
    // the one the reading pane and the composer have been addressing by its
    // local id, so continuity is worth more than which Gmail id it happens to
    // hold. The rest of the ordering only decides between rows that are all new
    // to the draft.
    let keep: Option<i64> = conn
        .query_row(
            &format!(
                "SELECT id FROM messages WHERE {MINE}
                  ORDER BY (mach_draft_id = ?2) DESC, (gmail_message_id = ?3) DESC, id DESC
                  LIMIT 1"
            ),
            params![account_id, draft_id, gmail_message_id, &older, &oldest],
            |row| row.get(0),
        )
        .optional()?;

    conn.execute(
        &format!("DELETE FROM messages WHERE {MINE} AND (?6 IS NULL OR id <> ?6)"),
        params![
            account_id,
            draft_id,
            gmail_message_id,
            &older,
            &oldest,
            keep.filter(|_| sent.is_none()),
        ],
    )?;
    if sent.is_none() {
        if let Some(keep) = keep {
            conn.execute(
                "UPDATE messages SET gmail_message_id = ?2, mach_draft_id = ?3 WHERE id = ?1",
                params![keep, gmail_message_id, draft_id],
            )?;
        }
    }
    Ok(())
}

/// The id the mirror row is filed under *right now*.
///
/// The placeholder until Gmail has been told, and Gmail's own message id after
/// that — because [`adopt`] renames the row. Writing the mirror by the
/// placeholder alone was a duplicate factory: the first save after a push found
/// no row under `mach-draft:…` and inserted a second one beside the adopted
/// one, so a thread ended up with two copies of one unsent reply.
fn current_id(draft: &Draft) -> String {
    draft
        .remote
        .message_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| mirror_message_id(&draft.id))
}

/// Write (or rewrite) the mirror. Returns the local thread it landed in.
///
/// Idempotent: autosave calls this every few hundred milliseconds, and it must
/// leave one row rather than a transcript of the typing.
pub fn mirror(db: &Db, draft: &Draft, now_ms: i64) -> Result<i64> {
    ensure_compose_schema(db)?;

    let account = db
        .read(|conn| command_queries::account_by_id(conn, draft.account_id))?
        .ok_or(ComposeError::UnknownAccount(draft.account_id))?;
    let from = Participant {
        name: account
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty()),
        email: account.email.clone(),
    };

    let gmail_message_id = current_id(draft);
    let (body_text, body_html) = super::draft::body_parts(draft);
    let snippet = snippet(&body_text);
    let subject = draft.subject.clone();
    let to = participants(&draft.to);
    let cc = participants(&draft.cc);
    let bcc = participants(&draft.bcc);

    let thread_id = draft.thread_id;
    let synthetic_thread_id = mirror_message_id(&draft.id);

    let landed: i64 = db.write(|conn| {
        // Which conversation. A reply has one; anything else gets a thread of
        // its own, so it can be opened, read and resumed like any other row.
        let thread_id = match thread_id {
            Some(id) if queries::thread_summary(conn, id)?.is_some() => id,
            _ => queries::upsert_thread(
                conn,
                &NewThread {
                    account_id: draft.account_id,
                    gmail_thread_id: synthetic_thread_id.clone(),
                    participants: to.clone(),
                    subject: subject.clone(),
                    snippet: snippet.clone(),
                    last_message_at: now_ms,
                    is_unread: false,
                    message_count: 1,
                    has_attachments: !draft.attachments.is_empty(),
                    label_ids: vec!["DRAFT".to_string()],
                },
            )?,
        };

        // "Is this draft already in the conversation" is a question about the
        // draft, not about whichever message id it currently answers to — see
        // `claim_row`, which runs next and is what makes those the same row.
        let existed: Option<i64> = conn
            .query_row(
                "SELECT id FROM messages
                  WHERE account_id = ?1 AND (mach_draft_id = ?3 OR gmail_message_id = ?2)",
                params![draft.account_id, gmail_message_id, draft.id],
                |row| row.get(0),
            )
            .optional()?;
        claim_row(conn, draft.account_id, &draft.id, &gmail_message_id, &[])?;

        queries::upsert_message(
            conn,
            &NewMessage {
                thread_id,
                account_id: draft.account_id,
                gmail_message_id: gmail_message_id.clone(),
                // A draft has no Message-ID yet: the one that goes on the wire
                // is minted at send, and claiming one here would put an id in
                // the store that no sent message will ever carry.
                rfc822_message_id: None,
                in_reply_to: None,
                reply_to: Vec::new(),
                references: None,
                from: from.clone(),
                to: to.clone(),
                cc: cc.clone(),
                bcc: bcc.clone(),
                subject: subject.clone(),
                body_html: Some(body_html.clone()),
                body_text: Some(body_text.clone()),
                // Mach's own plain-text alternative is derived from the HTML
                // and is not flowed. It never claims to be.
                body_text_flowed: false,
                body_text_delsp: false,
                snippet: snippet.clone(),
                internal_date: now_ms,
                is_unread: false,
                is_draft: true,
                // A draft Mach wrote is not a newsletter.
                list_unsubscribe: None,
                list_unsubscribe_post: None,
                list_id: None,
                precedence: None,
            },
        )?;

        // Whose row this is. Written here and nowhere else, and never rewritten:
        // it is what "the mirror of this draft" means.
        conn.execute(
            "UPDATE messages SET mach_draft_id = ?3
              WHERE account_id = ?1 AND gmail_message_id = ?2",
            params![draft.account_id, &gmail_message_id, &draft.id],
        )?;

        // The label is the whole point: this is what the Drafts mailbox reads.
        conn.execute(
            "INSERT OR IGNORE INTO thread_labels (thread_id, gmail_label_id) VALUES (?1, 'DRAFT')",
            [thread_id],
        )?;

        // Only the first write is a new message in the conversation. Counting
        // every autosave would have a thread claiming forty messages by lunch.
        if existed.is_none() {
            conn.execute(
                "UPDATE threads SET message_count = message_count + 1 WHERE id = ?1",
                [thread_id],
            )?;
        }
        Ok(thread_id)
    })?;

    Ok(landed)
}

/// Take the mirror back out: the draft was sent, or thrown away.
///
/// The `DRAFT` label goes with it only when nothing else in the conversation is
/// a draft, because a thread can hold two of them, and a synthetic thread is
/// deleted once it is empty — otherwise discarding a message you never wrote
/// would leave a subject line in the Drafts list for ever.
pub fn unmirror(db: &Db, draft: &Draft) -> Result<()> {
    unmirror_ids(db, &draft.id, Some(&current_id(draft)))
}

/// The same removal, addressed by ids rather than by a draft object.
///
/// [`unmirror`] is the ordinary door; this one exists because the send path
/// reaches here from the outbox, where the draft row is already gone and only
/// its ids survive — on the outbox row, put there at queue time. One
/// implementation rather than two, because "take the draft out of the
/// conversation" is exactly the operation that has been got wrong four times in
/// this file's history.
///
/// **Only a row that is still a draft is ever deleted.** That single condition
/// is what makes this safe to call after a send: `drafts.send` can hand back the
/// same message id the draft had, and the row under that id is by then the sent
/// message. Deleting it would remove the reply the owner just watched leave.
///
/// Addressed by the draft first and by the ids second. The ids are a draft's
/// *current* names and a draft has had several; `mach_draft_id` is the one that
/// held still, so a mirror stranded under a message id nobody remembers — the
/// shape every bug in this file's history has ended in — goes with the rest of
/// them rather than outliving the draft it stood for.
pub fn unmirror_ids(db: &Db, draft_id: &str, gmail_message_id: Option<&str>) -> Result<()> {
    ensure_compose_schema(db)?;
    // Both ids, because a draft that has been pushed is filed under Gmail's,
    // and one that has not is filed under the placeholder — and after a failed
    // push there can be one of each.
    let placeholder = mirror_message_id(draft_id);
    let gmail_message_id = gmail_message_id
        .filter(|id| !id.is_empty())
        .unwrap_or(&placeholder)
        .to_string();
    // The sweep reaches here with no draft to name; an empty string must not
    // match the rows of every draft that has none.
    let owner = (!draft_id.is_empty()).then(|| draft_id.to_string());
    db.write(|conn| {
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT m.thread_id, t.gmail_thread_id
                   FROM messages m JOIN threads t ON t.id = m.thread_id
                  WHERE m.is_draft = 1
                    AND (m.gmail_message_id = ?1 OR m.gmail_message_id = ?2
                         OR (?3 IS NOT NULL AND m.mach_draft_id = ?3))",
                params![&gmail_message_id, &placeholder, &owner],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((thread_id, gmail_thread_id)) = row else {
            return Ok(());
        };

        conn.execute(
            "DELETE FROM messages
              WHERE is_draft = 1
                AND (gmail_message_id = ?1 OR gmail_message_id = ?2
                     OR (?3 IS NOT NULL AND mach_draft_id = ?3))",
            params![&gmail_message_id, &placeholder, &owner],
        )?;
        conn.execute(
            "UPDATE threads SET message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?1)
              WHERE id = ?1",
            [thread_id],
        )?;

        let drafts_left: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND is_draft = 1",
            [thread_id],
            |row| row.get(0),
        )?;
        if drafts_left == 0 {
            conn.execute(
                "DELETE FROM thread_labels WHERE thread_id = ?1 AND gmail_label_id = 'DRAFT'",
                [thread_id],
            )?;
        }

        let left: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ?1",
            [thread_id],
            |row| row.get(0),
        )?;
        if left == 0 && gmail_thread_id.starts_with(MIRROR_PREFIX) {
            queries::delete_thread(conn, thread_id)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Take out mirror rows that no longer stand for anything.
///
/// # What this is cleaning up
///
/// A mirror is supposed to have exactly one thing behind it: a row in
/// `compose_drafts`, and through it a draft on Gmail. Four separate bugs in this
/// path have left rows that have neither — the owner's mailbox currently holds
/// two mirrors of one reply on the same conversation, filed under two message
/// ids, with no draft row for either. `drafts.update` mints a new message id on
/// every save, so a mirror that missed one adoption is stranded under an id
/// nothing will ever address again, and it renders in the conversation as a
/// `Draft` that cannot be opened, edited or discarded.
///
/// [`forget_drafts_missing_from`](super::draft::forget_drafts_missing_from)
/// cannot reach these: it walks `compose_drafts`, and their whole problem is
/// that they are not in it. So this walks the other way — from the messages —
/// and it is called from the same sweep with the same listing.
///
/// # The one direction to be careful in
///
/// **A draft sent from another client also leaves `drafts.list`**, and the
/// message it became must survive. Three things keep it:
///
///  * the row must still be a draft *locally*. `sync::mail` now clears
///    `is_draft` when Gmail reports the `DRAFT` label removed, so a draft that
///    was sent on the phone stops matching in the same pass that learns about
///    the send — before this runs, because messages are synced first.
///  * the message must not be the message of any draft Gmail just listed.
///  * the row must predate the listing, so a draft written here a moment ago —
///    which Gmail has not been asked about yet — is not swept as litter.
///
/// Everything removed is a local row. Nothing here touches Gmail.
pub fn forget_orphan_mirrors(
    db: &Db,
    account_id: i64,
    live_message_ids: &[String],
    listed_at: i64,
) -> Result<Vec<String>> {
    ensure_compose_schema(db)?;
    let candidates: Vec<(String, Option<String>)> = db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT gmail_message_id, mach_draft_id FROM messages
              WHERE account_id = ?1 AND is_draft = 1 AND internal_date < ?2",
        )?;
        let rows = stmt.query_map(params![account_id, listed_at], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?;

    let mut removed = Vec::new();
    for (gmail_message_id, owner) in candidates {
        if live_message_ids.contains(&gmail_message_id) {
            continue;
        }
        // Which draft row would own this mirror, if one did: the stamp it was
        // written with, the id Gmail filed it under for a pushed draft, and the
        // placeholder for one that never got that far.
        let placeholder_owner = owner.or_else(|| {
            gmail_message_id
                .strip_prefix(MIRROR_PREFIX)
                .map(str::to_string)
        });
        let claimed: bool = db.read(|conn| {
            let by_message: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM compose_drafts WHERE gmail_message_id = ?1",
                    [&gmail_message_id],
                    |row| row.get(0),
                )
                .optional()?;
            if by_message.is_some() {
                return Ok(true);
            }
            let Some(owner) = placeholder_owner.as_deref() else {
                return Ok(false);
            };
            Ok(conn
                .query_row("SELECT 1 FROM compose_drafts WHERE id = ?1", [owner], |row| {
                    row.get::<_, i64>(0)
                })
                .optional()?
                .is_some())
        })?;
        if claimed {
            continue;
        }
        unmirror_ids(
            db,
            placeholder_owner.as_deref().unwrap_or_default(),
            Some(&gmail_message_id),
        )?;
        removed.push(gmail_message_id);
    }
    Ok(removed)
}

/// Swap the ids the mirror is standing in for with Gmail's own, once a push has
/// landed.
///
/// This is what makes the same draft one row on both sides. Without it the next
/// sync pass finds a `DRAFT` message it has never seen and inserts it, and the
/// owner has two copies of one unsent reply.
///
/// `previous` matters as much as the placeholder: **`drafts.update` mints a new
/// message id every time**. Gmail replaces the message behind the draft rather
/// than editing it, so a mirror that only ever answered to `mach-draft:<id>`
/// would be adopted once and then go stale on the second keystroke-save — and
/// the stale row is a duplicate waiting for the next sync.
///
/// # The rename that used to be allowed to fail
///
/// This was `UPDATE OR IGNORE`, and the `IGNORE` was the whole duplicate: a sync
/// pass that had already imported the draft under its new message id — or an
/// autosave that wrote the mirror under it while the push was in flight — put a
/// row where this one was going, the rename was silently refused, and the draft
/// was two rows from then on. So the row in the way is taken out first. It is
/// the same draft, filed under the same id, arriving from the other end; there
/// is nothing in it this row does not have.
pub fn adopt(
    db: &Db,
    draft_id: &str,
    previous: Option<&str>,
    gmail_message_id: &str,
    gmail_thread_id: &str,
) -> Result<()> {
    if gmail_message_id.is_empty() {
        return Ok(());
    }
    let placeholder = mirror_message_id(draft_id);
    let previous = previous.unwrap_or(&placeholder).to_string();
    db.write(|conn| {
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT thread_id, account_id FROM messages
                  WHERE mach_draft_id = ?1
                     OR gmail_message_id = ?2 OR gmail_message_id = ?3",
                params![draft_id, &placeholder, &previous],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((thread_id, account_id)) = row else {
            return Ok(());
        };

        // The thread first: a synthetic one has to become the real conversation
        // before its message can point at a Gmail id, or a sync pass could land
        // the real message in a thread that is still provisional.
        //
        // Only a provisional thread is renamed. A reply's thread is a real
        // conversation and already carries Gmail's own id.
        if !gmail_thread_id.is_empty() {
            conn.execute(
                "UPDATE OR IGNORE threads SET gmail_thread_id = ?2
                  WHERE id = ?1 AND gmail_thread_id LIKE 'mach-draft:%'",
                params![thread_id, gmail_thread_id],
            )?;
        }

        claim_row(
            conn,
            account_id,
            draft_id,
            gmail_message_id,
            &[&previous, &placeholder],
        )?;
        Ok(())
    })?;
    Ok(())
}

fn participants(list: &[Mailbox]) -> Vec<Participant> {
    list.iter().map(|m| m.to_participant()).collect()
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(200).collect()
}
