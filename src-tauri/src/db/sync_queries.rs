//! Extra read/write helpers owned by the sync engine unit.
//!
//! Three things live here that `db::queries` does not need to know about:
//!
//! 1. **Backfill progress.** A 12-month backfill is tens of thousands of
//!    `messages.get` calls. Quitting the app halfway through must not throw that
//!    away, so the enumeration cursor and the not-yet-fetched work queue are
//!    rows in SQLite, not fields in a struct.
//! 2. **Per-message label sets.** The core schema tracks labels per *thread*
//!    (that is what the rail filters on), but `users.history.list` reports
//!    `labelsAdded`/`labelsRemoved` per *message*. Without the per-message set,
//!    removing `UNREAD` from one message of a five-message thread would either
//!    clear the whole thread or be dropped. The shadow table makes the thread's
//!    label set a derived union, which is also what makes replaying the same
//!    history batch twice a no-op.
//! 3. **Per-calendar sync tokens.** `accounts.calendar_sync_token` holds one
//!    token, but an account has several calendars. The primary calendar's token
//!    is mirrored into that column (so the rest of the app sees what it
//!    expects); the others live here.
//!
//! These tables are created on demand rather than in `db::schema`, which the
//! sync unit does not own. `ensure_schema` is idempotent and costs one
//! `CREATE TABLE IF NOT EXISTS` sweep per process.

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::models::{NewThread, Participant};
use crate::db::{queries, Result};

/// How many participants a thread row carries. The list shows three or four;
/// ten is enough for "+7 others" without bloating every row.
const MAX_THREAD_PARTICIPANTS: usize = 10;

pub const SYNC_SCHEMA: &str = r#"
-- Backfill checkpoint, one row per account. Deleted when the backfill finishes.
CREATE TABLE IF NOT EXISTS sync_backfill (
    account_id       INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    -- The account's historyId as it was BEFORE enumeration started. This is the
    -- watermark promoted into accounts.history_id once the backfill completes,
    -- so that changes which happened *during* the backfill are still replayable.
    start_history_id TEXT,
    -- messages.list pageToken to resume enumeration from.
    page_token       TEXT,
    enumeration_done INTEGER NOT NULL DEFAULT 0,
    window_start_ms  INTEGER NOT NULL DEFAULT 0,
    queued_total     INTEGER NOT NULL DEFAULT 0,
    fetched_total    INTEGER NOT NULL DEFAULT 0,
    started_at       INTEGER NOT NULL DEFAULT 0
);

-- Message ids enumerated but not yet fetched. A row disappears in the same
-- transaction that writes its message, so a crash can only ever re-do work,
-- never skip it.
CREATE TABLE IF NOT EXISTS sync_backfill_queue (
    account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    gmail_message_id TEXT    NOT NULL,
    gmail_thread_id  TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, gmail_message_id)
) WITHOUT ROWID;

-- Per-message Gmail label set, as a JSON array. The thread's label set is the
-- union of these.
CREATE TABLE IF NOT EXISTS sync_message_labels (
    account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    gmail_message_id TEXT    NOT NULL,
    label_ids        TEXT    NOT NULL DEFAULT '[]',
    PRIMARY KEY (account_id, gmail_message_id)
) WITHOUT ROWID;

-- Per-calendar incremental syncToken.
CREATE TABLE IF NOT EXISTS sync_calendar_state (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    calendar_id TEXT    NOT NULL,
    sync_token  TEXT,
    PRIMARY KEY (account_id, calendar_id)
) WITHOUT ROWID;
"#;

/// Create the sync engine's own tables if they are not there yet.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SYNC_SCHEMA)?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// backfill checkpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillCheckpoint {
    pub account_id: i64,
    /// The watermark captured *before* enumeration. `None` only if a row was
    /// written by an older version; treated as "start over".
    pub start_history_id: Option<String>,
    pub page_token: Option<String>,
    pub enumeration_done: bool,
    pub window_start_ms: i64,
    pub queued_total: i64,
    pub fetched_total: i64,
    pub started_at: i64,
}

pub fn backfill_checkpoint(conn: &Connection, account_id: i64) -> Result<Option<BackfillCheckpoint>> {
    Ok(conn
        .query_row(
            "SELECT account_id, start_history_id, page_token, enumeration_done, window_start_ms,
                    queued_total, fetched_total, started_at
             FROM sync_backfill WHERE account_id = ?1",
            [account_id],
            |row| {
                Ok(BackfillCheckpoint {
                    account_id: row.get(0)?,
                    start_history_id: row.get(1)?,
                    page_token: row.get(2)?,
                    enumeration_done: row.get::<_, i64>(3)? != 0,
                    window_start_ms: row.get(4)?,
                    queued_total: row.get(5)?,
                    fetched_total: row.get(6)?,
                    started_at: row.get(7)?,
                })
            },
        )
        .optional()?)
}

/// Start (or restart) a backfill. Wipes any half-finished queue so a full
/// resync after a `HistoryExpired` cannot inherit a stale cursor.
///
/// `start_history_id` must be read from `users.getProfile` **before** the first
/// `messages.list` call — that ordering is the whole point of this row.
pub fn begin_backfill(
    conn: &Connection,
    account_id: i64,
    start_history_id: &str,
    window_start_ms: i64,
) -> Result<BackfillCheckpoint> {
    conn.execute(
        "DELETE FROM sync_backfill_queue WHERE account_id = ?1",
        [account_id],
    )?;
    conn.execute(
        "INSERT INTO sync_backfill
             (account_id, start_history_id, page_token, enumeration_done, window_start_ms,
              queued_total, fetched_total, started_at)
         VALUES (?1, ?2, NULL, 0, ?3, 0, 0, ?4)
         ON CONFLICT(account_id) DO UPDATE SET
             start_history_id = excluded.start_history_id,
             page_token       = NULL,
             enumeration_done = 0,
             window_start_ms  = excluded.window_start_ms,
             queued_total     = 0,
             fetched_total    = 0,
             started_at       = excluded.started_at",
        params![account_id, start_history_id, window_start_ms, now_ms()],
    )?;
    Ok(BackfillCheckpoint {
        account_id,
        start_history_id: Some(start_history_id.to_string()),
        page_token: None,
        enumeration_done: false,
        window_start_ms,
        queued_total: 0,
        fetched_total: 0,
        started_at: now_ms(),
    })
}

pub fn set_backfill_cursor(
    conn: &Connection,
    account_id: i64,
    page_token: Option<&str>,
    enumeration_done: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE sync_backfill SET page_token = ?2, enumeration_done = ?3 WHERE account_id = ?1",
        params![account_id, page_token, enumeration_done as i64],
    )?;
    Ok(())
}

/// Queue message ids for fetching. Already-queued ids are ignored, so
/// re-enumerating a page after a crash cannot duplicate work. Returns how many
/// rows were genuinely new.
pub fn enqueue_backfill(
    conn: &Connection,
    account_id: i64,
    refs: &[(String, String)],
) -> Result<usize> {
    if refs.is_empty() {
        return Ok(0);
    }
    let mut added = 0usize;
    {
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO sync_backfill_queue (account_id, gmail_message_id, gmail_thread_id)
             VALUES (?1, ?2, ?3)",
        )?;
        for (message_id, thread_id) in refs {
            added += stmt.execute(params![account_id, message_id, thread_id])?;
        }
    }
    conn.execute(
        "UPDATE sync_backfill SET queued_total = queued_total + ?2 WHERE account_id = ?1",
        params![account_id, added as i64],
    )?;
    Ok(added)
}

/// The next slice of work, newest-looking first. Gmail message ids are
/// approximately monotonic, so descending order means the user's recent mail
/// lands in the store before the tail of the year does.
pub fn next_backfill_batch(
    conn: &Connection,
    account_id: i64,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    next_backfill_batch_after(conn, account_id, None, limit)
}

/// The same slice, resumed from a cursor.
///
/// The backfill now keeps many `messages.get` calls in flight at once, so it
/// hands ids out faster than it deletes their rows: a plain `LIMIT` would keep
/// returning the ids that are already on the wire. Paging by the last id handed
/// out — the queue's own primary key, walked in the same descending order —
/// gives every read a fresh slice without an in-memory exclusion set, and costs
/// an index seek rather than a scan.
///
/// A row that is leased but never written stays in the queue and is picked up
/// by the next backfill pass; the cursor is per-pass state, not durable state.
pub fn next_backfill_batch_after(
    conn: &Connection,
    account_id: i64,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT gmail_message_id, gmail_thread_id FROM sync_backfill_queue
         WHERE account_id = ?1 AND (?2 IS NULL OR gmail_message_id < ?2)
         ORDER BY gmail_message_id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![account_id, after, limit as i64], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Mark ids as done. Called in the same transaction as the message writes.
pub fn dequeue_backfill(conn: &Connection, account_id: i64, message_ids: &[String]) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "DELETE FROM sync_backfill_queue WHERE account_id = ?1 AND gmail_message_id = ?2",
    )?;
    let mut removed = 0usize;
    for id in message_ids {
        removed += stmt.execute(params![account_id, id])?;
    }
    drop(stmt);
    conn.execute(
        "UPDATE sync_backfill SET fetched_total = fetched_total + ?2 WHERE account_id = ?1",
        params![account_id, removed as i64],
    )?;
    Ok(())
}

pub fn backfill_remaining(conn: &Connection, account_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT count(*) FROM sync_backfill_queue WHERE account_id = ?1",
        [account_id],
        |row| row.get(0),
    )?)
}

/// Backfill finished: promote the pre-backfill watermark and drop the
/// checkpoint. One transaction, so there is no window in which the store claims
/// to be caught up without a watermark to prove it.
pub fn finish_backfill(conn: &Connection, account_id: i64, start_history_id: &str) -> Result<()> {
    queries::set_history_id(conn, account_id, Some(start_history_id))?;
    clear_backfill(conn, account_id)
}

pub fn clear_backfill(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM sync_backfill_queue WHERE account_id = ?1",
        [account_id],
    )?;
    conn.execute("DELETE FROM sync_backfill WHERE account_id = ?1", [account_id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// per-message labels
// ---------------------------------------------------------------------------

fn labels_to_json(labels: &[String]) -> String {
    serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string())
}

fn labels_from_json(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Normalise a label set: deduplicated and sorted, so two paths that produce
/// the same set produce the same bytes.
fn normalise(mut labels: Vec<String>) -> Vec<String> {
    labels.retain(|l| !l.is_empty());
    labels.sort();
    labels.dedup();
    labels
}

pub fn set_message_labels(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
    labels: &[String],
) -> Result<()> {
    let labels = normalise(labels.to_vec());
    conn.execute(
        "INSERT INTO sync_message_labels (account_id, gmail_message_id, label_ids)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id, gmail_message_id) DO UPDATE SET label_ids = excluded.label_ids",
        params![account_id, gmail_message_id, labels_to_json(&labels)],
    )?;
    Ok(())
}

pub fn message_labels(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
) -> Result<Option<Vec<String>>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT label_ids FROM sync_message_labels
             WHERE account_id = ?1 AND gmail_message_id = ?2",
            params![account_id, gmail_message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.map(|r| labels_from_json(&r)))
}

/// Add labels to a message's set. Set semantics, so replaying the same history
/// record is a no-op. Returns the resulting set, or `None` if the message has
/// no row (i.e. we have never seen it).
pub fn add_message_labels(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
    add: &[String],
) -> Result<Option<Vec<String>>> {
    let Some(mut current) = message_labels(conn, account_id, gmail_message_id)? else {
        return Ok(None);
    };
    current.extend(add.iter().cloned());
    let current = normalise(current);
    set_message_labels(conn, account_id, gmail_message_id, &current)?;
    Ok(Some(current))
}

/// Remove labels from a message's set. Removing a label that is not there is a
/// no-op, which is what makes a replayed batch idempotent.
pub fn remove_message_labels(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
    remove: &[String],
) -> Result<Option<Vec<String>>> {
    let Some(mut current) = message_labels(conn, account_id, gmail_message_id)? else {
        return Ok(None);
    };
    current.retain(|l| !remove.iter().any(|r| r == l));
    let current = normalise(current);
    set_message_labels(conn, account_id, gmail_message_id, &current)?;
    Ok(Some(current))
}

pub fn delete_message_labels(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM sync_message_labels WHERE account_id = ?1 AND gmail_message_id = ?2",
        params![account_id, gmail_message_id],
    )?;
    Ok(())
}

/// The union of every label on every message of a thread — what the rail
/// filters on.
pub fn thread_label_union(conn: &Connection, thread_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT l.label_ids FROM messages m
         JOIN sync_message_labels l
           ON l.account_id = m.account_id AND l.gmail_message_id = m.gmail_message_id
         WHERE m.thread_id = ?1",
    )?;
    let rows = stmt.query_map([thread_id], |row| row.get::<_, String>(0))?;
    let mut out: Vec<String> = Vec::new();
    for raw in rows {
        out.extend(labels_from_json(&raw?));
    }
    Ok(normalise(out))
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

/// Look up a thread's row id by its Gmail id.
pub fn thread_row_id(
    conn: &Connection,
    account_id: i64,
    gmail_thread_id: &str,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM threads WHERE account_id = ?1 AND gmail_thread_id = ?2",
            params![account_id, gmail_thread_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Get or create the thread row a message can hang off.
///
/// Deliberately `INSERT OR IGNORE` rather than `queries::upsert_thread` with a
/// default `NewThread`: the upsert would clobber an existing thread's subject,
/// snippet and timestamps with zeroes before `recompute_thread` put them back.
pub fn ensure_thread(conn: &Connection, account_id: i64, gmail_thread_id: &str) -> Result<i64> {
    if let Some(id) = thread_row_id(conn, account_id, gmail_thread_id)? {
        return Ok(id);
    }
    conn.execute(
        "INSERT OR IGNORE INTO threads (account_id, gmail_thread_id) VALUES (?1, ?2)",
        params![account_id, gmail_thread_id],
    )?;
    thread_row_id(conn, account_id, gmail_thread_id)?.ok_or_else(|| {
        crate::db::DbError::Other(format!("could not create thread {gmail_thread_id}"))
    })
}

/// Rebuild a thread's denormalised summary from the messages currently stored
/// under it, and rewrite its label set from the per-message union.
///
/// Every field on `threads` is derived, never accumulated, so this is safe to
/// run any number of times — which is what makes a replayed history batch
/// converge instead of drifting. `has_attachments` is set here; it is the only
/// place that sets it.
///
/// Returns `false` when the thread had no messages left and was deleted.
pub fn recompute_thread(conn: &Connection, thread_id: i64) -> Result<bool> {
    let Some((account_id, gmail_thread_id)) = conn
        .query_row(
            "SELECT account_id, gmail_thread_id FROM threads WHERE id = ?1",
            [thread_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };

    struct Row {
        from_name: Option<String>,
        from_email: String,
        subject: String,
        snippet: String,
        internal_date: i64,
        is_unread: bool,
    }

    let rows: Vec<Row> = {
        let mut stmt = conn.prepare(
            "SELECT from_name, from_email, subject, snippet, internal_date, is_unread
             FROM messages WHERE thread_id = ?1 ORDER BY internal_date, id",
        )?;
        let mapped = stmt.query_map([thread_id], |row| {
            Ok(Row {
                from_name: row.get(0)?,
                from_email: row.get(1)?,
                subject: row.get(2)?,
                snippet: row.get(3)?,
                internal_date: row.get(4)?,
                is_unread: row.get(5)?,
            })
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };

    if rows.is_empty() {
        queries::delete_thread(conn, thread_id)?;
        return Ok(false);
    }

    // The subject of a conversation is the subject it started with; the snippet
    // is whatever was said last.
    let subject = rows
        .iter()
        .map(|r| r.subject.as_str())
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let last = rows.last().expect("non-empty");
    let snippet = last.snippet.clone();
    let last_message_at = rows.iter().map(|r| r.internal_date).max().unwrap_or(0);
    let is_unread = rows.iter().any(|r| r.is_unread);

    let mut participants: Vec<Participant> = Vec::new();
    for row in &rows {
        if row.from_email.is_empty() {
            continue;
        }
        if participants
            .iter()
            .any(|p| p.email.eq_ignore_ascii_case(&row.from_email))
        {
            continue;
        }
        participants.push(Participant {
            name: row.from_name.clone().filter(|n| !n.is_empty()),
            email: row.from_email.clone(),
        });
        if participants.len() >= MAX_THREAD_PARTICIPANTS {
            break;
        }
    }

    let has_attachments: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachments a
                       JOIN messages m ON m.id = a.message_id
                       WHERE m.thread_id = ?1)",
        [thread_id],
        |row| row.get(0),
    )?;

    let label_ids = thread_label_union(conn, thread_id)?;

    queries::upsert_thread(
        conn,
        &NewThread {
            account_id,
            gmail_thread_id,
            participants,
            subject,
            snippet,
            last_message_at,
            is_unread,
            message_count: rows.len() as i64,
            has_attachments,
            label_ids,
        },
    )?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

/// Delete a message by its Gmail id, returning the thread row it belonged to so
/// the caller can recompute (or drop) that thread.
pub fn delete_message_by_gmail_id(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
) -> Result<Option<i64>> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, thread_id FROM messages WHERE account_id = ?1 AND gmail_message_id = ?2",
            params![account_id, gmail_message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((message_id, thread_id)) = row else {
        // Already gone. Still clear any orphaned label row so a replay is clean.
        delete_message_labels(conn, account_id, gmail_message_id)?;
        return Ok(None);
    };
    queries::delete_message(conn, message_id)?;
    delete_message_labels(conn, account_id, gmail_message_id)?;
    Ok(Some(thread_id))
}

/// Drop a message's attachment rows before re-inserting them.
///
/// `attachments` is unique on `(message_id, gmail_attachment_id)`, and SQLite
/// treats two NULLs as distinct — so an inline part with no attachment id would
/// accumulate a new row on every resync without this.
pub fn clear_message_attachments(conn: &Connection, message_id: i64) -> Result<()> {
    conn.execute("DELETE FROM attachments WHERE message_id = ?1", [message_id])?;
    Ok(())
}

/// Set `messages.is_unread` from a label set. Gmail expresses read state as the
/// presence of the `UNREAD` label; the column is the denormalised form.
pub fn set_message_unread_from_labels(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
    labels: &[String],
) -> Result<()> {
    let unread = labels.iter().any(|l| l == "UNREAD");
    conn.execute(
        "UPDATE messages SET is_unread = ?3 WHERE account_id = ?1 AND gmail_message_id = ?2",
        params![account_id, gmail_message_id, unread],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// calendar
// ---------------------------------------------------------------------------

pub fn calendar_sync_token(
    conn: &Connection,
    account_id: i64,
    calendar_id: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT sync_token FROM sync_calendar_state WHERE account_id = ?1 AND calendar_id = ?2",
            params![account_id, calendar_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Store (or clear) a calendar's incremental token. The primary calendar's
/// token is mirrored into `accounts.calendar_sync_token` so the rest of the app
/// keeps seeing what it expects there.
pub fn set_calendar_sync_token(
    conn: &Connection,
    account_id: i64,
    calendar_id: &str,
    token: Option<&str>,
    is_primary: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_calendar_state (account_id, calendar_id, sync_token)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id, calendar_id) DO UPDATE SET sync_token = excluded.sync_token",
        params![account_id, calendar_id, token],
    )?;
    if is_primary {
        queries::set_calendar_sync_token(conn, account_id, token)?;
    }
    Ok(())
}

pub fn delete_event_by_google_id(
    conn: &Connection,
    account_id: i64,
    calendar_id: &str,
    google_event_id: &str,
) -> Result<bool> {
    let changed = conn.execute(
        "DELETE FROM events
         WHERE account_id = ?1 AND calendar_id = ?2 AND google_event_id = ?3",
        params![account_id, calendar_id, google_event_id],
    )?;
    Ok(changed > 0)
}
