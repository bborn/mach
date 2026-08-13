//! Reading the facts the rule needs out of SQLite.
//!
//! Two entry points, and the split between them is the whole performance story.
//! [`candidate`] answers about one message and runs when he presses the key, so
//! it may be as expensive as it needs to be. [`offers_for_thread`] runs every
//! time a conversation is opened, so it starts with the one predicate that
//! eliminates almost every message — `list_unsubscribe IS NOT NULL` — and only
//! pays for the sender lookups on the rows that survive it.
//!
//! Most conversations hold no message with the header at all, and a newsletter
//! is nearly always a thread of one. So the two shapes in practice are "one
//! cheap statement, no rows" and "one cheap statement, one row, two lookups".

use rusqlite::{Connection, OptionalExtension};

use crate::db::Result;
use crate::unsub::rule::{self, Candidate, Verdict};

/// The row behind a candidate, before the sender lookups are paid for.
struct Row {
    message_id: i64,
    account_id: i64,
    gmail_message_id: String,
    from_email: String,
    list_unsubscribe: Option<String>,
    list_unsubscribe_post: Option<String>,
    list_id: Option<String>,
    precedence: Option<String>,
}

const SELECT_COLUMNS: &str = "\
    id, account_id, gmail_message_id, from_email, list_unsubscribe, \
    list_unsubscribe_post, list_id, precedence";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        message_id: row.get(0)?,
        account_id: row.get(1)?,
        gmail_message_id: row.get(2)?,
        from_email: row.get(3)?,
        list_unsubscribe: row.get(4)?,
        list_unsubscribe_post: row.get(5)?,
        list_id: row.get(6)?,
        precedence: row.get(7)?,
    })
}

fn hydrate(conn: &Connection, row: Row) -> Result<Candidate> {
    let messages_from_sender = messages_from_sender(conn, row.account_id, &row.from_email)?;

    // The one expensive read in this file, and it is skipped whenever it cannot
    // change the answer.
    //
    // [`rule::is_established`] is `has_written_to_sender ||
    // messages_from_sender >= ESTABLISHED_MESSAGE_COUNT`, so a sender who has
    // already cleared the count is established whatever this returns. That
    // covers every real newsletter — they are the senders with a lot of
    // messages — and leaves the query for the small senders and the one-off
    // blasts, where it is the difference between "he corresponds with them" and
    // "report this".
    //
    // The saving is not theoretical. Against the owner's 67,000-message store
    // the lookup is ~230 ms for a sender he has never written to, and a
    // conversation must not wait a quarter of a second to draw a button. See
    // [`has_written_to_sender`] for why it costs that much even indexed.
    //
    // This is a deliberate coupling to the *shape* of the rule rather than to
    // its constants, and `false` is always the safe answer to skip with: it can
    // only make a verdict stricter, never more permissive.
    let has_written_to_sender = if messages_from_sender >= rule::ESTABLISHED_MESSAGE_COUNT {
        false
    } else {
        has_written_to_sender(conn, row.account_id, &row.from_email)?
    };

    Ok(Candidate {
        labels: message_labels(conn, row.account_id, &row.gmail_message_id)?,
        messages_from_sender,
        has_written_to_sender,
        from_email: row.from_email,
        list_unsubscribe: row.list_unsubscribe,
        list_unsubscribe_post: row.list_unsubscribe_post,
        list_id: row.list_id,
        precedence: row.precedence,
    })
}

/// Everything the rule needs about one message, or `None` when there is no such
/// message.
pub fn candidate(conn: &Connection, message_id: i64) -> Result<Option<Candidate>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM messages WHERE id = ?1");
    let row = conn.query_row(&sql, [message_id], map_row).optional()?;
    match row {
        Some(row) => Ok(Some(hydrate(conn, row)?)),
        None => Ok(None),
    }
}

/// The verdict for every message in a thread that has anything to say about
/// unsubscribing, oldest first. A message with no `List-Unsubscribe` is absent
/// rather than present with a decline — nothing downstream distinguishes them
/// and the whole point of the filter is not to build a candidate for it.
pub fn offers_for_thread(conn: &Connection, thread_id: i64) -> Result<Vec<(i64, Verdict)>> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM messages
          WHERE thread_id = ?1
            AND list_unsubscribe IS NOT NULL
            AND TRIM(list_unsubscribe) <> ''
          ORDER BY internal_date, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<Row> = stmt
        .query_map([thread_id], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let message_id = row.message_id;
        // Fail closed, and only here. The labels live in `sync_message_labels`,
        // which the sync engine creates at boot rather than a migration, so a
        // store that has never synced does not have it. Without labels the rule
        // cannot see a `SPAM` label, and a message in Spam that reads as
        // unlabelled is exactly the one that must not be offered — so a message
        // whose facts cannot be assembled gets no offer rather than a guess.
        //
        // `candidate`, which the action goes through, propagates instead: by
        // then somebody has pressed a key and is owed an answer.
        let Ok(candidate) = hydrate(conn, row) else {
            continue;
        };
        out.push((message_id, rule::verdict(&candidate)));
    }
    Ok(out)
}

/// A message's own Gmail labels.
///
/// The message's, not the thread's union: a thread can hold one archived
/// message and one Gmail filed as spam, and the question is always about the
/// message that carries the header.
///
/// `sync_message_labels` is keyed by `(account_id, gmail_message_id)` and holds
/// a JSON array — see [`crate::db::sync_queries::set_message_labels`]. An
/// absent row means the sync loop has not written labels for this message,
/// which the rule reads as "no labels", so a message with no row is never
/// mistaken for one in Spam.
fn message_labels(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
) -> Result<Vec<String>> {
    Ok(
        crate::db::sync_queries::message_labels(conn, account_id, gmail_message_id)?
            .unwrap_or_default(),
    )
}

/// How many messages this account holds from that exact address, counted no
/// further than the rule needs.
///
/// The rule only asks whether the count reaches
/// [`rule::ESTABLISHED_MESSAGE_COUNT`], so the subquery stops there — a sender
/// with eight thousand messages must not cost eight thousand rows.
/// `idx_messages_sender` (migration 14) makes it a short index scan.
///
/// Matched on the stored string exactly rather than case-folded, because
/// `LOWER(from_email)` would put the index out of reach for a query that runs
/// on every thread open. A sender who varies the case of their own address
/// across messages therefore counts as two senders; Gmail normalises `From`
/// per sender in practice, and the cost of being wrong is a false negative.
fn messages_from_sender(conn: &Connection, account_id: i64, from_email: &str) -> Result<i64> {
    if from_email.trim().is_empty() {
        return Ok(0);
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
             SELECT 1 FROM messages
              WHERE account_id = ?1 AND from_email = ?2
              LIMIT ?3
         )",
        rusqlite::params![account_id, from_email, rule::ESTABLISHED_MESSAGE_COUNT],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Whether anything he sent has that address on `To`, `Cc` or `Bcc`.
///
/// "Sent" is `from_email` being one of the account addresses, which is the same
/// definition [`crate::db::queries::address_book`] uses and for the same reason:
/// the `SENT` label lives in `sync_message_labels` and is not indexed for this
/// question, while `from_email` is.
///
/// `json_each` over the recipient columns rather than a `LIKE`, again matching
/// `address_book` — recipients are display data in a JSON column and always
/// have been. `json_valid` guards it because `json_each` *errors* on malformed
/// text and one bad row must not cost the answer.
fn has_written_to_sender(conn: &Connection, account_id: i64, from_email: &str) -> Result<bool> {
    let email = from_email.trim().to_ascii_lowercase();
    if email.is_empty() {
        return Ok(false);
    }
    const SQL: &str = "\
        WITH mine(email) AS (SELECT lower(email) FROM accounts) \
        SELECT 1 FROM messages m, json_each(m.to_json) v \
         WHERE m.account_id = ?1 AND m.is_draft = 0 AND json_valid(m.to_json) \
           AND lower(m.from_email) IN mine \
           AND trim(lower(json_extract(v.value, '$.email'))) = ?2 \
        UNION ALL \
        SELECT 1 FROM messages m, json_each(m.cc_json) v \
         WHERE m.account_id = ?1 AND m.is_draft = 0 AND json_valid(m.cc_json) \
           AND lower(m.from_email) IN mine \
           AND trim(lower(json_extract(v.value, '$.email'))) = ?2 \
        UNION ALL \
        SELECT 1 FROM messages m, json_each(m.bcc_json) v \
         WHERE m.account_id = ?1 AND m.is_draft = 0 AND json_valid(m.bcc_json) \
           AND lower(m.from_email) IN mine \
           AND trim(lower(json_extract(v.value, '$.email'))) = ?2 \
        LIMIT 1";
    let found: Option<i64> = conn
        .query_row(SQL, rusqlite::params![account_id, email], |row| row.get(0))
        .optional()?;
    Ok(found.is_some())
}
