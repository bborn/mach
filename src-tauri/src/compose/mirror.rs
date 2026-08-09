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
use super::markdown;
use super::mime::Mailbox;
use super::{ensure_compose_schema, ComposeError, Result};

/// The namespace a not-yet-pushed draft's ids live in. Shares its shape with
/// the outbox's `mach-outbox:` so that "is this row ours and provisional" is
/// one question with one answer.
pub const MIRROR_PREFIX: &str = "mach-draft:";

pub fn mirror_message_id(draft_id: &str) -> String {
    format!("{MIRROR_PREFIX}{draft_id}")
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
    db.write(ensure_compose_schema)?;

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
    let body_text = markdown::to_text(&draft.body);
    let body_html = markdown::to_html(&draft.body);
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
                    has_attachments: false,
                    label_ids: vec!["DRAFT".to_string()],
                },
            )?,
        };

        let existed: Option<i64> = conn
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND gmail_message_id = ?2",
                params![draft.account_id, gmail_message_id],
                |row| row.get(0),
            )
            .optional()?;

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
                snippet: snippet.clone(),
                internal_date: now_ms,
                is_unread: false,
                is_draft: true,
            },
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
    db.write(ensure_compose_schema)?;
    // Both ids, because a draft that has been pushed is filed under Gmail's,
    // and one that has not is filed under the placeholder — and after a failed
    // push there can be one of each.
    let placeholder = mirror_message_id(&draft.id);
    let gmail_message_id = current_id(draft);
    db.write(|conn| {
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT m.thread_id, t.gmail_thread_id
                   FROM messages m JOIN threads t ON t.id = m.thread_id
                  WHERE m.gmail_message_id = ?1 OR m.gmail_message_id = ?2",
                params![&gmail_message_id, &placeholder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((thread_id, gmail_thread_id)) = row else {
            return Ok(());
        };

        conn.execute(
            "DELETE FROM messages WHERE gmail_message_id = ?1 OR gmail_message_id = ?2",
            params![&gmail_message_id, &placeholder],
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
        // The thread first: a synthetic one has to become the real conversation
        // before its message can point at a Gmail id, or a sync pass could land
        // the real message in a thread that is still provisional.
        if !gmail_thread_id.is_empty() {
            let thread_id: Option<i64> = conn
                .query_row(
                    "SELECT thread_id FROM messages
                      WHERE gmail_message_id = ?1 OR gmail_message_id = ?2",
                    params![&placeholder, &previous],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(thread_id) = thread_id {
                // Only a provisional thread is renamed. A reply's thread is a
                // real conversation and already carries Gmail's own id.
                conn.execute(
                    "UPDATE OR IGNORE threads SET gmail_thread_id = ?2
                      WHERE id = ?1 AND gmail_thread_id LIKE 'mach-draft:%'",
                    params![thread_id, gmail_thread_id],
                )?;
            }
        }
        conn.execute(
            "UPDATE OR IGNORE messages SET gmail_message_id = ?3
              WHERE gmail_message_id = ?1 OR gmail_message_id = ?2",
            params![placeholder, previous, gmail_message_id],
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
