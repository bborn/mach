//! The read paths the UI renders from, plus the narrow write surface the sync
//! loop uses to fill the store.
//!
//! Every function takes `&Connection` rather than `&Db` so that callers choose
//! their own connection (a pooled reader for UI reads, the writer for sync) and
//! so that several calls can be composed inside one transaction. Nothing here
//! opens connections or manages locks; that is `db::mod`'s job.
//!
//! Writes live here — not in the sync unit — because keeping every statement
//! that knows the column layout in one file is what lets the schema change
//! without a grep across the codebase.

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::db::models::*;
use crate::db::Result;

/// Page size used when `ThreadQuery::limit` is left at zero. Roughly three
/// screens of a Linear-density list.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Upper bound on a single page, so a bad `limit` from the UI cannot ask for
/// the entire mailbox.
pub const MAX_PAGE_SIZE: u32 = 1000;

/// Gmail's own id for the Drafts mailbox. Named because it is the one label
/// whose membership this module answers from two places — see `list_threads`.
pub const DRAFT_LABEL: &str = "DRAFT";

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn people_to_json(people: &[Participant]) -> String {
    serde_json::to_string(people).unwrap_or_else(|_| "[]".to_string())
}

/// JSON columns are display data written by us. A malformed one is a bug, not a
/// user-facing failure, so it degrades to "no participants" rather than turning
/// an inbox render into an error dialog.
fn people_from_json(raw: &str) -> Vec<Participant> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn csv_to_vec(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// accounts
// ---------------------------------------------------------------------------

fn map_account(row: &Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        email: row.get(1)?,
        display_name: row.get(2)?,
        token_ref: row.get(3)?,
        history_id: row.get(4)?,
        calendar_sync_token: row.get(5)?,
        colour_index: row.get(6)?,
        created_at: row.get(7)?,
    })
}

const ACCOUNT_COLUMNS: &str = "id, email, display_name, token_ref, history_id, \
                               calendar_sync_token, colour_index, created_at";

pub fn list_accounts(conn: &Connection) -> Result<Vec<Account>> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY colour_index, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_account)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn account_by_email(conn: &Connection, email: &str) -> Result<Option<Account>> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE email = ?1");
    Ok(conn.query_row(&sql, [email], map_account).optional()?)
}

/// Insert or update an account, keyed on its email address. Returns the row id.
pub fn upsert_account(conn: &Connection, new: &NewAccount) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO accounts (email, display_name, token_ref, colour_index, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(email) DO UPDATE SET
             display_name = excluded.display_name,
             token_ref    = excluded.token_ref,
             colour_index = excluded.colour_index
         RETURNING id",
        params![
            new.email,
            new.display_name,
            new.token_ref,
            new.colour_index,
            now_ms()
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Move the Gmail history watermark. Called after every successful incremental
/// sync; this single column is what makes resync cheap.
pub fn set_history_id(conn: &Connection, account_id: i64, history_id: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET history_id = ?2 WHERE id = ?1",
        params![account_id, history_id],
    )?;
    Ok(())
}

pub fn set_calendar_sync_token(conn: &Connection, account_id: i64, token: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET calendar_sync_token = ?2 WHERE id = ?1",
        params![account_id, token],
    )?;
    Ok(())
}

/// Removes the account and, by cascade, every thread, message, attachment,
/// label and event that belonged to it.
pub fn delete_account(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute("DELETE FROM accounts WHERE id = ?1", [account_id])?;
    Ok(())
}

/// Unread thread count per account, for the rail's badges. Accounts with
/// nothing unread are simply absent.
pub fn unread_counts(conn: &Connection) -> Result<Vec<UnreadCount>> {
    let mut stmt = conn.prepare(
        "SELECT account_id, count(*) FROM threads WHERE is_unread = 1 GROUP BY account_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UnreadCount {
            account_id: row.get(0)?,
            unread: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------------------------------------------------------------------------
// labels
// ---------------------------------------------------------------------------

pub fn upsert_label(conn: &Connection, new: &NewLabel) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO labels (account_id, gmail_label_id, name, label_type)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(account_id, gmail_label_id) DO UPDATE SET
             name       = excluded.name,
             label_type = excluded.label_type
         RETURNING id",
        params![
            new.account_id,
            new.gmail_label_id,
            new.name,
            new.label_type.as_str()
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn list_labels(conn: &Connection, account_id: Option<i64>) -> Result<Vec<Label>> {
    let mut sql = String::from(
        "SELECT id, account_id, gmail_label_id, name, label_type FROM labels",
    );
    let mut args: Vec<Value> = Vec::new();
    if let Some(id) = account_id {
        sql.push_str(" WHERE account_id = ?1");
        args.push(Value::Integer(id));
    }
    sql.push_str(" ORDER BY account_id, label_type DESC, name");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), |row| {
        let ty: String = row.get(4)?;
        Ok(Label {
            id: row.get(0)?,
            account_id: row.get(1)?,
            gmail_label_id: row.get(2)?,
            name: row.get(3)?,
            label_type: LabelType::from_str_lossy(&ty),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

/// Selected in this exact order by every thread-summary query; `map_thread`
/// depends on the positions.
const THREAD_COLUMNS: &str = "\
    t.id, t.account_id, a.email, a.colour_index, t.gmail_thread_id, \
    t.participants, t.subject, t.snippet, t.last_message_at, \
    t.is_unread, t.message_count, t.has_attachments, \
    (SELECT group_concat(tl.gmail_label_id) FROM thread_labels tl WHERE tl.thread_id = t.id)";

fn map_thread(row: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    let participants: String = row.get(5)?;
    let labels: Option<String> = row.get(12)?;
    Ok(ThreadSummary {
        id: row.get(0)?,
        account_id: row.get(1)?,
        account_email: row.get(2)?,
        account_colour_index: row.get(3)?,
        gmail_thread_id: row.get(4)?,
        participants: people_from_json(&participants),
        subject: row.get(6)?,
        snippet: row.get(7)?,
        last_message_at: row.get(8)?,
        is_unread: row.get(9)?,
        message_count: row.get(10)?,
        has_attachments: row.get(11)?,
        label_ids: csv_to_vec(labels),
    })
}

/// The hot query: the unified stream.
///
/// With `ThreadQuery::default()` this is every account interleaved by time,
/// newest first — an index scan of `idx_threads_stream` with no sort step.
/// Narrowing to one account switches it to `idx_threads_account_stream`;
/// narrowing to a label seeks through `idx_thread_labels_label`.
///
/// Pagination is keyset, not offset: see `ThreadCursor` for why.
pub fn list_threads(conn: &Connection, query: &ThreadQuery) -> Result<Vec<ThreadSummary>> {
    let mut sql = format!(
        "SELECT {THREAD_COLUMNS} FROM threads t JOIN accounts a ON a.id = t.account_id WHERE 1 = 1"
    );
    let mut args: Vec<Value> = Vec::new();

    if let Some(account_id) = query.account_id {
        args.push(Value::Integer(account_id));
        sql.push_str(&format!(" AND t.account_id = ?{}", args.len()));
    }
    if let Some(label) = &query.label_id {
        args.push(Value::Text(label.clone()));
        let by_label = format!(
            "EXISTS (SELECT 1 FROM thread_labels tl \
              WHERE tl.thread_id = t.id AND tl.gmail_label_id = ?{})",
            args.len()
        );
        // Drafts are the one mailbox the label set cannot answer on its own.
        //
        // `thread_labels` is derived: `sync_queries::recompute_thread` rebuilds
        // it from the per-message label union on every pass, which is what
        // makes a replayed history batch converge — and which also drops a
        // `DRAFT` row written locally for a draft Google has not been told
        // about yet. The draft appeared in the mailbox and then quietly left
        // it again, which is the original bug wearing a different hat.
        //
        // `messages.is_draft` is the same fact from the durable side: set when
        // Mach mirrors a draft locally (`compose::mirror`), and set by
        // `sync::convert` from Gmail's own `DRAFT` label on the way in. Either
        // is enough to be in Drafts. Indexed by migration 8.
        if label == DRAFT_LABEL {
            sql.push_str(&format!(
                " AND ({by_label} OR EXISTS (SELECT 1 FROM messages m \
                   WHERE m.thread_id = t.id AND m.is_draft = 1))"
            ));
        } else {
            sql.push_str(&format!(" AND {by_label}"));
        }
    }
    if query.unread_only {
        sql.push_str(" AND t.is_unread = 1");
    }
    if let Some(cursor) = query.after {
        // Row-value comparison so SQLite can drive it straight off the
        // (last_message_at DESC, id DESC) index instead of filtering after a scan.
        args.push(Value::Integer(cursor.last_message_at));
        args.push(Value::Integer(cursor.id));
        sql.push_str(&format!(
            " AND (t.last_message_at, t.id) < (?{}, ?{})",
            args.len() - 1,
            args.len()
        ));
    }

    let limit = match query.limit {
        0 => DEFAULT_PAGE_SIZE,
        n => n.min(MAX_PAGE_SIZE),
    };
    args.push(Value::Integer(limit as i64));
    sql.push_str(&format!(
        " ORDER BY t.last_message_at DESC, t.id DESC LIMIT ?{}",
        args.len()
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), map_thread)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Threads for one account, newest first. Sugar over `list_threads`.
pub fn list_threads_for_account(
    conn: &Connection,
    account_id: i64,
    limit: u32,
    after: Option<ThreadCursor>,
) -> Result<Vec<ThreadSummary>> {
    list_threads(
        conn,
        &ThreadQuery {
            account_id: Some(account_id),
            limit,
            after,
            ..Default::default()
        },
    )
}

pub fn thread_summary(conn: &Connection, thread_id: i64) -> Result<Option<ThreadSummary>> {
    let sql = format!(
        "SELECT {THREAD_COLUMNS} FROM threads t JOIN accounts a ON a.id = t.account_id \
         WHERE t.id = ?1"
    );
    Ok(conn.query_row(&sql, [thread_id], map_thread).optional()?)
}

pub fn thread_by_gmail_id(
    conn: &Connection,
    account_id: i64,
    gmail_thread_id: &str,
) -> Result<Option<ThreadSummary>> {
    let sql = format!(
        "SELECT {THREAD_COLUMNS} FROM threads t JOIN accounts a ON a.id = t.account_id \
         WHERE t.account_id = ?1 AND t.gmail_thread_id = ?2"
    );
    Ok(conn
        .query_row(&sql, params![account_id, gmail_thread_id], map_thread)
        .optional()?)
}

/// The reading pane: one thread and its whole conversation, oldest message
/// first, with attachments attached. Three statements, no N+1.
pub fn thread_with_messages(conn: &Connection, thread_id: i64) -> Result<Option<ThreadWithMessages>> {
    let Some(thread) = thread_summary(conn, thread_id)? else {
        return Ok(None);
    };
    let messages = messages_for_thread(conn, thread_id)?;
    Ok(Some(ThreadWithMessages { thread, messages }))
}

/// Insert or update a thread, keyed on `(account_id, gmail_thread_id)`, and
/// replace its label set. Returns the row id.
pub fn upsert_thread(conn: &Connection, new: &NewThread) -> Result<i64> {
    let id: i64 = conn.query_row(
        "INSERT INTO threads (account_id, gmail_thread_id, participants, subject, snippet,
                              last_message_at, is_unread, message_count, has_attachments)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(account_id, gmail_thread_id) DO UPDATE SET
             participants    = excluded.participants,
             subject         = excluded.subject,
             snippet         = excluded.snippet,
             last_message_at = excluded.last_message_at,
             is_unread       = excluded.is_unread,
             message_count   = excluded.message_count,
             has_attachments = excluded.has_attachments
         RETURNING id",
        params![
            new.account_id,
            new.gmail_thread_id,
            people_to_json(&new.participants),
            new.subject,
            new.snippet,
            new.last_message_at,
            new.is_unread,
            new.message_count,
            new.has_attachments,
        ],
        |row| row.get(0),
    )?;

    set_thread_labels(conn, id, &new.label_ids)?;
    Ok(id)
}

/// Replace a thread's label refs wholesale. Gmail always hands back the full
/// label set for a thread, so a diff would be more code for the same result.
pub fn set_thread_labels(conn: &Connection, thread_id: i64, label_ids: &[String]) -> Result<()> {
    conn.execute("DELETE FROM thread_labels WHERE thread_id = ?1", [thread_id])?;
    if label_ids.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO thread_labels (thread_id, gmail_label_id) VALUES (?1, ?2)",
    )?;
    for label in label_ids {
        stmt.execute(params![thread_id, label])?;
    }
    Ok(())
}

pub fn set_thread_unread(conn: &Connection, thread_id: i64, unread: bool) -> Result<()> {
    conn.execute(
        "UPDATE threads SET is_unread = ?2 WHERE id = ?1",
        params![thread_id, unread],
    )?;
    Ok(())
}

pub fn delete_thread(conn: &Connection, thread_id: i64) -> Result<()> {
    conn.execute("DELETE FROM threads WHERE id = ?1", [thread_id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

const MESSAGE_COLUMNS: &str = "\
    id, thread_id, account_id, gmail_message_id, rfc822_message_id, in_reply_to, \
    references_header, from_name, from_email, to_json, cc_json, bcc_json, subject, \
    body_html, body_text, snippet, internal_date, is_unread, is_draft, reply_to, \
    body_text_flowed, body_text_delsp, \
    (html_evicted_at IS NOT NULL AND body_html IS NULL)";

fn map_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    let to: String = row.get(9)?;
    let cc: String = row.get(10)?;
    let bcc: String = row.get(11)?;
    Ok(Message {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        account_id: row.get(2)?,
        gmail_message_id: row.get(3)?,
        rfc822_message_id: row.get(4)?,
        in_reply_to: row.get(5)?,
        references: row.get(6)?,
        from: Participant {
            name: row.get(7)?,
            email: row.get(8)?,
        },
        reply_to: people_from_json(&row.get::<_, Option<String>>(19)?.unwrap_or_default()),
        to: people_from_json(&to),
        cc: people_from_json(&cc),
        bcc: people_from_json(&bcc),
        subject: row.get(12)?,
        body_html: row.get(13)?,
        body_text: row.get(14)?,
        // NULL is a row written before migration 11, which is "we were never
        // told" and is read as not flowed. See [`Message::body_text_flowed`].
        body_text_flowed: row.get::<_, Option<bool>>(20)?.unwrap_or(false),
        body_text_delsp: row.get::<_, Option<bool>>(21)?.unwrap_or(false),
        // Computed in SQL rather than from the two columns here, because the
        // question is about the pair: sync's upsert can put `body_html` back
        // without clearing `html_evicted_at`, and that row is resident.
        html_evicted: row.get(22)?,
        snippet: row.get(15)?,
        internal_date: row.get(16)?,
        is_unread: row.get(17)?,
        is_draft: row.get(18)?,
        attachments: Vec::new(),
    })
}

/// A thread's messages, oldest first, with their attachments filled in.
pub fn messages_for_thread(conn: &Connection, thread_id: i64) -> Result<Vec<Message>> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages WHERE thread_id = ?1 ORDER BY internal_date, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut messages: Vec<Message> = stmt
        .query_map([thread_id], map_message)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // One extra statement for the whole thread rather than one per message.
    let mut stmt = conn.prepare(
        "SELECT id, message_id, gmail_attachment_id, filename, mime_type, size_bytes, local_path
         FROM attachments
         WHERE message_id IN (SELECT id FROM messages WHERE thread_id = ?1)
         ORDER BY id",
    )?;
    let attachments = stmt
        .query_map([thread_id], map_attachment)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for att in attachments {
        if let Some(msg) = messages.iter_mut().find(|m| m.id == att.message_id) {
            msg.attachments.push(att);
        }
    }

    Ok(messages)
}

pub fn message_by_gmail_id(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
) -> Result<Option<Message>> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages WHERE account_id = ?1 AND gmail_message_id = ?2"
    );
    Ok(conn
        .query_row(&sql, params![account_id, gmail_message_id], map_message)
        .optional()?)
}

/// Insert or update a message, keyed on `(account_id, gmail_message_id)`.
///
/// The `ON CONFLICT ... DO UPDATE` path fires the `messages_fts_au` trigger, so
/// re-syncing a message re-indexes it; there is no path that writes a body
/// without updating the search index.
pub fn upsert_message(conn: &Connection, new: &NewMessage) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO messages (thread_id, account_id, gmail_message_id, rfc822_message_id,
                               in_reply_to, references_header, from_name, from_email,
                               to_json, cc_json, bcc_json, subject, body_html, body_text,
                               snippet, internal_date, is_unread, is_draft, reply_to,
                               body_text_flowed, body_text_delsp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                 ?20, ?21)
         ON CONFLICT(account_id, gmail_message_id) DO UPDATE SET
             thread_id         = excluded.thread_id,
             rfc822_message_id = excluded.rfc822_message_id,
             in_reply_to       = excluded.in_reply_to,
             references_header = excluded.references_header,
             from_name         = excluded.from_name,
             from_email        = excluded.from_email,
             to_json           = excluded.to_json,
             cc_json           = excluded.cc_json,
             bcc_json          = excluded.bcc_json,
             subject           = excluded.subject,
             body_html         = excluded.body_html,
             body_text         = excluded.body_text,
             snippet           = excluded.snippet,
             internal_date     = excluded.internal_date,
             is_unread         = excluded.is_unread,
             is_draft          = excluded.is_draft,
             reply_to          = excluded.reply_to,
             body_text_flowed  = excluded.body_text_flowed,
             body_text_delsp   = excluded.body_text_delsp
         RETURNING id",
        params![
            new.thread_id,
            new.account_id,
            new.gmail_message_id,
            new.rfc822_message_id,
            new.in_reply_to,
            new.references,
            new.from.name,
            new.from.email,
            people_to_json(&new.to),
            people_to_json(&new.cc),
            people_to_json(&new.bcc),
            new.subject,
            new.body_html,
            new.body_text,
            new.snippet,
            new.internal_date,
            new.is_unread,
            new.is_draft,
            people_to_json(&new.reply_to),
            new.body_text_flowed,
            new.body_text_delsp,
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Record which Gmail draft a message is, as `users.drafts.list` reports it.
///
/// Returns whether a row was actually touched — `false` simply means the drafts
/// sweep ran ahead of the message sync and the message is not stored yet, which
/// the next pass resolves.
///
/// Deliberately not part of [`upsert_message`]: the ordinary message write knows
/// nothing about draft ids, and folding this into it would mean every sync of a
/// draft message overwrote the id with `NULL`.
pub fn set_message_draft_id(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
    gmail_draft_id: &str,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE messages SET gmail_draft_id = ?3
          WHERE account_id = ?1 AND gmail_message_id = ?2
            AND (gmail_draft_id IS NULL OR gmail_draft_id <> ?3)",
        params![account_id, gmail_message_id, gmail_draft_id],
    )?;
    Ok(changed > 0)
}

/// The Gmail draft id a message carries, if the sweep has learned one.
pub fn message_draft_id(conn: &Connection, message_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT gmail_draft_id FROM messages WHERE id = ?1",
            [message_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|id| !id.is_empty()))
}

/// Forget every draft id this account holds that Gmail no longer lists.
///
/// The sweep hands in the complete set, so an id that is missing from it is a
/// draft that was sent or deleted somewhere else. Clearing the column is what
/// stops Mach from later addressing a draft that is not there — and, because
/// `live` is the whole truth rather than a page of it, an empty list legitimately
/// means "this account has no drafts at all".
pub fn clear_missing_draft_ids(conn: &Connection, account_id: i64, live: &[String]) -> Result<usize> {
    // Built rather than bound because SQLite has no array parameter. The values
    // are Gmail draft ids, and they are still bound — only the number of
    // placeholders is interpolated.
    let placeholders = vec!["?"; live.len()].join(",");
    let sql = if live.is_empty() {
        "UPDATE messages SET gmail_draft_id = NULL
          WHERE account_id = ?1 AND gmail_draft_id IS NOT NULL"
            .to_string()
    } else {
        format!(
            "UPDATE messages SET gmail_draft_id = NULL
              WHERE account_id = ?1 AND gmail_draft_id IS NOT NULL
                AND gmail_draft_id NOT IN ({placeholders})"
        )
    };
    let mut values: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(live.len() + 1);
    values.push(&account_id);
    for id in live {
        values.push(id);
    }
    Ok(conn.execute(&sql, values.as_slice())?)
}

pub fn set_message_unread(conn: &Connection, message_id: i64, unread: bool) -> Result<()> {
    conn.execute(
        "UPDATE messages SET is_unread = ?2 WHERE id = ?1",
        params![message_id, unread],
    )?;
    Ok(())
}

pub fn delete_message(conn: &Connection, message_id: i64) -> Result<()> {
    conn.execute("DELETE FROM messages WHERE id = ?1", [message_id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// attachments
// ---------------------------------------------------------------------------

fn map_attachment(row: &Row<'_>) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: row.get(0)?,
        message_id: row.get(1)?,
        gmail_attachment_id: row.get(2)?,
        filename: row.get(3)?,
        mime_type: row.get(4)?,
        size_bytes: row.get(5)?,
        local_path: row.get(6)?,
    })
}

pub fn upsert_attachment(conn: &Connection, new: &NewAttachment) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO attachments (message_id, gmail_attachment_id, filename, mime_type,
                                  size_bytes, local_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(message_id, gmail_attachment_id) DO UPDATE SET
             filename   = excluded.filename,
             mime_type  = excluded.mime_type,
             size_bytes = excluded.size_bytes,
             -- Never clobber a path we already fetched with a NULL.
             local_path = COALESCE(excluded.local_path, attachments.local_path)
         RETURNING id",
        params![
            new.message_id,
            new.gmail_attachment_id,
            new.filename,
            new.mime_type,
            new.size_bytes,
            new.local_path,
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Record that an attachment's bytes now exist on disk.
pub fn set_attachment_local_path(conn: &Connection, attachment_id: i64, path: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE attachments SET local_path = ?2 WHERE id = ?1",
        params![attachment_id, path],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// calendar
// ---------------------------------------------------------------------------

pub(crate) const EVENT_COLUMNS: &str = "\
    id, account_id, calendar_id, google_event_id, title, description, location, \
    start_ts, end_ts, is_all_day, attendees, rsvp_status, recurring_event_id, \
    status, html_link, updated_at, recurrence, reminders, ical_uid, organizer, \
    organizer_self, guests_can_modify, conference, guests, creator, attachments, \
    visibility, transparency";

/// Read an event row selected with [`EVENT_COLUMNS`].
///
/// Shared with `command_queries`, which used to keep a byte-identical copy of
/// both this and the column list. Two copies of a positional mapper is a bug
/// waiting for the next column: adding one to a `SELECT` in one file and
/// forgetting the other reads a `TEXT` out of an `INTEGER` slot at runtime,
/// which is a panic rather than a compile error.
pub(crate) fn map_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    let attendees: String = row.get(10)?;
    let rsvp: Option<String> = row.get(11)?;
    let recurrence: Option<String> = row.get(16)?;
    let reminders: Option<String> = row.get(17)?;
    let organizer: Option<String> = row.get(19)?;
    let conference: Option<String> = row.get(22)?;
    let guests: Option<String> = row.get(23)?;
    let creator: Option<String> = row.get(24)?;
    let attachments: Option<String> = row.get(25)?;

    let attendees = people_from_json(&attendees);
    // A row written before migration 7, or one a local edit has just changed the
    // guest list of, knows the addresses and not the answers. Projecting the
    // addresses into guest rows means every reader gets one list to render
    // rather than two to reconcile — and a guest with no `response` says
    // "nobody told us", which is exactly true.
    let guests: Vec<crate::db::models::EventGuest> = guests
        .as_deref()
        .and_then(json_opt)
        .unwrap_or_else(|| attendees.iter().map(guest_from_participant).collect());

    Ok(Event {
        guests,
        conference: conference.as_deref().and_then(json_opt),
        creator: creator.as_deref().and_then(json_opt),
        attachments: json_or_default(attachments.as_deref()),
        visibility: row.get(26)?,
        transparency: row.get(27)?,
        id: row.get(0)?,
        account_id: row.get(1)?,
        calendar_id: row.get(2)?,
        google_event_id: row.get(3)?,
        title: row.get(4)?,
        description: row.get(5)?,
        location: row.get(6)?,
        start_ts: row.get(7)?,
        end_ts: row.get(8)?,
        is_all_day: row.get(9)?,
        attendees,
        rsvp_status: rsvp.as_deref().and_then(RsvpStatus::parse),
        recurring_event_id: row.get(12)?,
        recurrence: json_or_default(recurrence.as_deref()),
        reminders: reminders.as_deref().and_then(json_opt),
        ical_uid: row.get(18)?,
        organizer: organizer.as_deref().and_then(json_opt),
        organizer_self: row.get(20)?,
        guests_can_modify: row.get(21)?,
        status: row.get(13)?,
        html_link: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

/// A guest row for someone we know only as an address.
fn guest_from_participant(p: &crate::db::models::Participant) -> crate::db::models::EventGuest {
    crate::db::models::EventGuest {
        email: p.email.clone(),
        name: p.name.clone(),
        ..Default::default()
    }
}

/// Decode a JSON column, falling back to the type's default.
///
/// A column that will not parse is treated as absent rather than fatal: these
/// hold display data written by an older build, and refusing to render a week
/// because one event's reminder blob is malformed would be the wrong trade.
pub(crate) fn json_or_default<T: serde::de::DeserializeOwned + Default>(raw: Option<&str>) -> T {
    raw.and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default()
}

pub(crate) fn json_opt<T: serde::de::DeserializeOwned>(raw: &str) -> Option<T> {
    serde_json::from_str(raw).ok()
}

/// JSON for a value worth storing, or `None` for one that is not.
///
/// Empty lists become SQL `NULL` rather than `"[]"` so that the upsert's
/// `COALESCE` can tell "Google said nothing about this" from "Google said
/// there is nothing" — which is the whole mechanism that keeps a series' rule
/// alive across the expanded instances that never carry it.
pub(crate) fn json_if_present<T: serde::Serialize>(value: &[T]) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    serde_json::to_string(value).ok()
}

pub(crate) fn json_of<T: serde::Serialize>(value: Option<&T>) -> Option<String> {
    value.and_then(|v| serde_json::to_string(v).ok())
}

/// Every event overlapping `[from_ts, to_ts)`, across accounts unless one is
/// named. Overlap, not containment: a meeting that starts before the visible
/// week and ends inside it still has to be drawn.
pub fn events_in_range(
    conn: &Connection,
    from_ts: i64,
    to_ts: i64,
    account_id: Option<i64>,
) -> Result<Vec<Event>> {
    let mut sql = format!(
        "SELECT {EVENT_COLUMNS} FROM events WHERE start_ts < ?1 AND end_ts > ?2"
    );
    let mut args: Vec<Value> = vec![Value::Integer(to_ts), Value::Integer(from_ts)];
    if let Some(id) = account_id {
        args.push(Value::Integer(id));
        sql.push_str(&format!(" AND account_id = ?{}", args.len()));
    }
    sql.push_str(" ORDER BY start_ts, id");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), map_event)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Write an event row, or bring the existing one up to date.
///
/// # Why five of the columns are `COALESCE`d rather than overwritten
///
/// This is the statement sync runs for every event on every pass, and sync's
/// view of an event is *lossier than the store's*. `events.list` is called with
/// `singleEvents=true`, which returns concrete occurrences — and an occurrence
/// carries no `recurrence`, because the rule lives on the series master that
/// the expansion never returns. A plain `excluded.recurrence` would therefore
/// erase the rule of every series on the first sync after it was created, which
/// is exactly the "I made it weekly and it came back as a one-off" symptom.
///
/// So: a `NULL` from the caller means "I was not told", not "there is none",
/// and leaves what is already there alone. `json_if_present` is what turns an
/// empty list into that `NULL`. Clearing a rule for real is an `UPDATE` from
/// the command layer, which knows the difference and says so explicitly.
///
/// The series subquery is the other half. A recurring create writes the rule
/// onto one row; Google then expands the series into twenty siblings, none of
/// which know it. Reading the rule off any sibling that does costs one indexed
/// lookup and makes every occurrence of a series agree about how it repeats.
pub fn upsert_event(conn: &Connection, new: &NewEvent) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO events (account_id, calendar_id, google_event_id, title, description,
                             location, start_ts, end_ts, is_all_day, attendees, rsvp_status,
                             recurring_event_id, status, html_link, updated_at,
                             recurrence, reminders, ical_uid, organizer, organizer_self,
                             guests_can_modify, conference, guests, creator, attachments,
                             visibility, transparency)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 COALESCE(?16, (SELECT sibling.recurrence FROM events sibling
                                 WHERE sibling.account_id = ?1
                                   AND sibling.calendar_id = ?2
                                   AND sibling.recurring_event_id = ?12
                                   AND sibling.recurrence IS NOT NULL
                                 LIMIT 1)),
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
         ON CONFLICT(account_id, calendar_id, google_event_id) DO UPDATE SET
             title              = excluded.title,
             description        = excluded.description,
             location           = excluded.location,
             start_ts           = excluded.start_ts,
             end_ts             = excluded.end_ts,
             is_all_day         = excluded.is_all_day,
             attendees          = excluded.attendees,
             rsvp_status        = excluded.rsvp_status,
             recurring_event_id = excluded.recurring_event_id,
             status             = excluded.status,
             html_link          = excluded.html_link,
             updated_at         = excluded.updated_at,
             recurrence         = COALESCE(excluded.recurrence, events.recurrence),
             reminders          = COALESCE(excluded.reminders, events.reminders),
             ical_uid           = COALESCE(excluded.ical_uid, events.ical_uid),
             organizer          = COALESCE(excluded.organizer, events.organizer),
             organizer_self     = COALESCE(excluded.organizer_self, events.organizer_self),
             guests_can_modify  = COALESCE(excluded.guests_can_modify, events.guests_can_modify),
             -- Not COALESCEd, unlike the five above. Google puts all of these on
             -- every expanded occurrence, so a sync that says nothing is a sync
             -- that means nothing: a conference that has been removed, a guest
             -- who has been uninvited, an attachment that has been detached.
             -- Preserving those would be preserving a meeting that no longer
             -- exists, which is the opposite failure to the one the COALESCEs
             -- above are there for.
             conference         = excluded.conference,
             guests             = excluded.guests,
             creator            = COALESCE(excluded.creator, events.creator),
             attachments        = excluded.attachments,
             visibility         = excluded.visibility,
             transparency       = excluded.transparency
         RETURNING id",
        params![
            new.account_id,
            new.calendar_id,
            new.google_event_id,
            new.title,
            new.description,
            new.location,
            new.start_ts,
            new.end_ts,
            new.is_all_day,
            people_to_json(&new.attendees),
            new.rsvp_status.map(|r| r.as_str()),
            new.recurring_event_id,
            new.status,
            new.html_link,
            new.updated_at,
            json_if_present(&new.recurrence),
            json_of(new.reminders.as_ref()),
            new.ical_uid,
            json_of(new.organizer.as_ref()),
            new.organizer_self,
            new.guests_can_modify,
            json_of(new.conference.as_ref()),
            json_if_present(&new.guests),
            json_of(new.creator.as_ref()),
            json_if_present(&new.attachments),
            new.visibility,
            new.transparency,
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn delete_event(conn: &Connection, event_id: i64) -> Result<()> {
    conn.execute("DELETE FROM events WHERE id = ?1", [event_id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// calendar metadata
// ---------------------------------------------------------------------------

const CALENDAR_COLUMNS: &str = "\
    id, account_id, calendar_id, summary, summary_override, description, \
    time_zone, color_id, background_color, foreground_color, access_role, \
    is_primary, selected, deleted, synced_at";

fn map_calendar(row: &Row<'_>) -> rusqlite::Result<Calendar> {
    Ok(Calendar {
        id: row.get(0)?,
        account_id: row.get(1)?,
        calendar_id: row.get(2)?,
        summary: row.get(3)?,
        summary_override: row.get(4)?,
        description: row.get(5)?,
        time_zone: row.get(6)?,
        color_id: row.get(7)?,
        background_color: row.get(8)?,
        foreground_color: row.get(9)?,
        access_role: row.get(10)?,
        is_primary: row.get(11)?,
        selected: row.get(12)?,
        deleted: row.get(13)?,
        synced_at: row.get(14)?,
    })
}

/// Every calendar the store has metadata for, newest account first.
///
/// Includes tombstoned rows. Filtering them out here would make the caller
/// unable to name the events of a calendar that was unsubscribed this morning,
/// which is the one case the tombstone exists for; `ipc::reads::list_calendars`
/// is where that judgement belongs because it is the one that knows whether any
/// events are left.
pub fn list_calendars(conn: &Connection, account_id: Option<i64>) -> Result<Vec<Calendar>> {
    let mut sql = format!("SELECT {CALENDAR_COLUMNS} FROM calendars");
    let mut args: Vec<Value> = Vec::new();
    if let Some(id) = account_id {
        args.push(Value::Integer(id));
        sql.push_str(" WHERE account_id = ?1");
    }
    sql.push_str(" ORDER BY account_id, is_primary DESC, calendar_id");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), map_calendar)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// When this account's calendar metadata was last refreshed, as the oldest
/// `synced_at` across its rows — `None` when it has none at all.
///
/// The *oldest* rather than the newest, deliberately. A sweep stamps every row
/// it writes with the same instant, so the two agree in the ordinary case; they
/// diverge only when a row was written by something other than a full sweep, and
/// then the honest reading of "how stale is what I know" is the staleness of the
/// worst row rather than the best.
pub fn calendars_synced_at(conn: &Connection, account_id: i64) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT min(synced_at) FROM calendars WHERE account_id = ?1",
        [account_id],
        |row| row.get::<_, Option<i64>>(0),
    )?)
}

/// Write a calendar's metadata, or bring the existing row up to date.
///
/// Every column is overwritten, `COALESCE`d nothing. This is the opposite of
/// [`upsert_event`], and for the opposite reason: the caller here is always a
/// complete `calendarList.list` entry rather than a lossy expansion, so a `NULL`
/// genuinely means "this calendar has no description any more" and preserving
/// the old value would strand a name the user has just cleared.
pub fn upsert_calendar(conn: &Connection, new: &NewCalendar) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO calendars (account_id, calendar_id, summary, summary_override,
                                description, time_zone, color_id, background_color,
                                foreground_color, access_role, is_primary, selected,
                                deleted, synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(account_id, calendar_id) DO UPDATE SET
             summary          = excluded.summary,
             summary_override = excluded.summary_override,
             description      = excluded.description,
             time_zone        = excluded.time_zone,
             color_id         = excluded.color_id,
             background_color = excluded.background_color,
             foreground_color = excluded.foreground_color,
             access_role      = excluded.access_role,
             is_primary       = excluded.is_primary,
             selected         = excluded.selected,
             deleted          = excluded.deleted,
             synced_at        = excluded.synced_at
         RETURNING id",
        params![
            new.account_id,
            new.calendar_id,
            new.summary,
            new.summary_override,
            new.description,
            new.time_zone,
            new.color_id,
            new.background_color,
            new.foreground_color,
            new.access_role,
            new.is_primary,
            new.selected,
            new.deleted,
            new.synced_at,
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Tombstone every calendar of this account that `present` does not mention.
///
/// This is what an unsubscribe looks like from here: the calendar simply stops
/// appearing in `calendarList.list`, with no event and no farewell. Deleting the
/// row would be the tidy answer and the wrong one — its events are still in
/// `events`, still inside the visible window, and still drawn on the grid, so
/// removing the only thing that knows their name puts the sidebar back to
/// showing `c_8f3…@group.calendar.google.com`. Marking it keeps the name and
/// tells sync to stop asking.
///
/// Returns how many rows were newly tombstoned.
pub fn tombstone_missing_calendars(
    conn: &Connection,
    account_id: i64,
    present: &[String],
) -> Result<usize> {
    let mut sql = String::from(
        "UPDATE calendars SET deleted = 1 WHERE account_id = ?1 AND deleted = 0",
    );
    let mut args: Vec<Value> = vec![Value::Integer(account_id)];
    if !present.is_empty() {
        let placeholders: Vec<String> = present
            .iter()
            .map(|id| {
                args.push(Value::Text(id.clone()));
                format!("?{}", args.len())
            })
            .collect();
        sql.push_str(&format!(
            " AND calendar_id NOT IN ({})",
            placeholders.join(", ")
        ));
    }
    Ok(conn.execute(&sql, params_from_iter(args.iter()))?)
}

/// How many events the store holds per `(account_id, calendar_id)`.
///
/// Separate from [`list_calendars`] rather than a `LEFT JOIN` on it, because the
/// two answer different questions and only one of them can be empty: a calendar
/// with metadata and no events is a real answer, and so is a calendar with
/// events and no metadata. Reading both and merging is what lets
/// `ipc::reads::list_calendars` cover the second case at all.
pub fn event_counts_by_calendar(conn: &Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT account_id, calendar_id, count(*) FROM events
         GROUP BY account_id, calendar_id",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// Turn whatever ⌘K contains into a safe FTS5 MATCH expression.
///
/// The box takes arbitrary keystrokes, including half-typed quotes and words
/// that happen to be FTS operators (`AND`, `NEAR`, `*`). Rather than escape a
/// user-facing query language we do not expose, every run of alphanumerics
/// becomes a double-quoted prefix term and everything else is dropped. That
/// makes a syntax error impossible and makes search-as-you-type work: `veloci`
/// matches `velocipede` on the fifth keystroke.
///
/// Returns `None` when there is nothing to search for.
pub fn fts_match_expression(input: &str) -> Option<String> {
    const MAX_TERMS: usize = 16;
    let terms: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_TERMS)
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Local full-text search, collapsed to threads and ranked.
///
/// bm25 is computed per message; a thread's score is its best message's. The
/// subject column is weighted 10x the body, because a hit in the subject is
/// what the user usually means. Lower (more negative) is better, so the sort is
/// ascending.
pub fn search_threads(conn: &Connection, input: &str, limit: u32) -> Result<Vec<ThreadHit>> {
    let Some(expr) = fts_match_expression(input) else {
        return Ok(Vec::new());
    };
    let limit = limit.clamp(1, MAX_PAGE_SIZE);

    // The CTE is MATERIALIZED deliberately: bm25() is an FTS5 auxiliary
    // function and SQLite refuses it ("unable to use function bm25 in the
    // requested context") once the query is flattened into a join against
    // `messages`. Materialising the match first scores each hit inside a plain
    // FTS query, then the join is ordinary SQL.
    let mut stmt = conn.prepare(
        "WITH hits AS MATERIALIZED (
             SELECT rowid AS rid, bm25(messages_fts, 10.0, 1.0) AS score
             FROM messages_fts
             WHERE messages_fts MATCH ?1
         )
         SELECT m.thread_id, m.account_id, min(hits.score) AS best
         FROM hits JOIN messages m ON m.id = hits.rid
         GROUP BY m.thread_id, m.account_id
         ORDER BY best ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![expr, limit], |row| {
        Ok(ThreadHit {
            thread_id: row.get(0)?,
            account_id: row.get(1)?,
            score: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Search, then hydrate the hits into renderable rows in ranked order.
pub fn search_thread_summaries(
    conn: &Connection,
    input: &str,
    limit: u32,
) -> Result<Vec<ThreadSummary>> {
    let hits = search_threads(conn, input, limit)?;
    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        if let Some(summary) = thread_summary(conn, hit.thread_id)? {
            out.push(summary);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// search — the operator language
// ---------------------------------------------------------------------------

/*
 * Everything below compiles a parsed Gmail-style query into one SQL statement.
 *
 * The query is *parsed* in TypeScript (`src/lib/search-query.ts`) because the
 * box has to show its own interpretation as you type, and a round trip per
 * keystroke to find out what you typed would defeat the point. What crosses the
 * seam is the AST, not the text, so there is exactly one parser and this side
 * never has to guess what `older_than:2m` meant.
 *
 * # Why every predicate is pushed into SQL
 *
 * 61k messages. Fetching rows and filtering them in the frontend is not slow,
 * it is impossible — the page would have to arrive first. So each leaf below
 * compiles to a predicate SQLite can answer from an index, and the whole tree
 * becomes one statement with `ORDER BY last_message_at DESC LIMIT n`, which
 * lets SQLite walk `idx_threads_stream` newest-first and stop as soon as the
 * page is full.
 *
 * # The shapes that matter, measured against the real 61k-message store
 *
 *  * Full text goes through `messages_fts`, and the hit set is mapped back to
 *    threads through a *covering* scan of `idx_messages_thread` rather than by
 *    reading the matched `messages` rows. Message rows are fat (they hold the
 *    bodies), so 31k rowid lookups cost ~2.7s cold; the same answer off the
 *    covering index is ~60ms and does not grow with the number of hits.
 *  * `from:` is prefiltered on `threads.participants`, which is the sender
 *    rollup the list already renders, so the expensive per-message check only
 *    runs on threads that could possibly match.
 *  * `to:`/`cc:`/`bcc:` have nothing indexed to stand on — an unanchored LIKE
 *    cannot use a b-tree — so they are the one operator that can still walk the
 *    message table. Combined with anything else, or with a common address, the
 *    LIMIT stops it early. See the note on `SEARCH_UNINDEXED_FIELDS`.
 */

use serde::{Deserialize, Serialize};

/// Operators that carry a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchField {
    From,
    To,
    Cc,
    Bcc,
    Subject,
    Label,
    Filename,
}

/// Operators that are a state rather than a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchFlag {
    Unread,
    Read,
    Starred,
    Attachment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DateBound {
    Before,
    After,
}

/// The parsed query. Mirrors `SearchNode` in `src/lib/search-query.ts` field
/// for field; serde's tagged representation is the wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SearchNode {
    And {
        nodes: Vec<SearchNode>,
    },
    Or {
        nodes: Vec<SearchNode>,
    },
    Not {
        node: Box<SearchNode>,
    },
    /// The identity — what `in:anywhere` compiles to. Never narrows anything.
    All,
    Text {
        value: String,
        #[serde(default)]
        prefix: bool,
    },
    Field {
        field: SearchField,
        value: String,
        #[serde(default)]
        prefix: bool,
    },
    Flag {
        flag: SearchFlag,
    },
    /// Already an absolute epoch millisecond; the parser resolved `7d` for us.
    Date {
        bound: DateBound,
        ts: i64,
    },
}

/// One page of an operator search.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchRequest {
    /// `None` searches every account, which is what Gmail does.
    pub account_id: Option<i64>,
    /// `0` means the default page size.
    pub limit: u32,
    /// Keyset resume point, same `(last_message_at, id)` order as the stream.
    pub after: Option<ThreadCursor>,
}

/// How deep a tree may nest before the rest is ignored.
///
/// `((((((…` is a real thing to type, and both sides of the seam recurse over
/// this structure. The parser caps itself at the same order of magnitude; this
/// is the backstop for an AST that arrived from somewhere else.
const MAX_SEARCH_DEPTH: usize = 24;

/// `threads.participants` holds at most this many senders — see
/// `MAX_THREAD_PARTICIPANTS` in `sync_queries.rs`, which builds the rollup.
///
/// It is the reason the `from:` prefilter has to be an *or*: on a thread whose
/// rollup hit the cap the list is incomplete, so absence from it does not prove
/// absence from the thread. A rollup holding exactly the cap is precisely the
/// "might have been truncated" case, and it is rare — 27 threads out of 41,799
/// in the mailbox this was measured against — so the exact per-message check
/// still runs on almost nothing.
///
/// The `message_count` guard in front of the JSON call is not redundant: it
/// keeps `json_array_length` off 41,000 rows that cannot possibly have hit the
/// cap, and it is implied by the JSON test rather than adding to it (ten
/// distinct senders needs at least ten messages).
const PARTICIPANT_ROLLUP_CAP: i64 = 10;

/// The operators no index can serve, named so the comment above stays honest.
pub const SEARCH_UNINDEXED_FIELDS: &[SearchField] = &[SearchField::To, SearchField::Cc, SearchField::Bcc];

/// Quote a user's term as a single FTS5 string literal.
///
/// **This is the injection boundary.** Everything inside an FTS5 double-quoted
/// string is a literal token sequence: `*`, `NEAR`, `AND`, `OR`, `-`, `^`, `:`
/// and `(` all lose their meaning there, and the only character that can end
/// the string is `"`, which is escaped by doubling it. So a term is safe iff it
/// is wrapped and its quotes are doubled — which is what this does, and what
/// `tests/search.rs` proves against a corpus built to be broken out of.
///
/// Returns `None` when the term contains nothing the tokenizer would index. An
/// empty match expression is a *syntax error* in FTS5 rather than an empty
/// result, so callers must treat `None` as "matches nothing" themselves.
pub fn fts_escape(term: &str, prefix: bool) -> Option<String> {
    if !term.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    let escaped = term.replace('"', "\"\"");
    Some(if prefix {
        format!("\"{escaped}\"*")
    } else {
        format!("\"{escaped}\"")
    })
}

/// Escape a value for use inside `LIKE '%…%' ESCAPE '\'`.
fn like_contains(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Escape a value for an exact `LIKE` — case-insensitive equality, no wildcards.
fn like_exact(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

struct Compiler {
    args: Vec<Value>,
}

impl Compiler {
    /// Binds a value and returns the `?N` that refers to it.
    fn bind(&mut self, value: Value) -> String {
        self.args.push(value);
        format!("?{}", self.args.len())
    }

    fn text(&mut self, value: &str) -> String {
        self.bind(Value::Text(value.to_string()))
    }

    /// The set of threads holding a message that matches an FTS expression.
    ///
    /// Not correlated: SQLite evaluates it once and reuses it for every row of
    /// the driving scan. The `INDEXED BY` is load-bearing — without it the
    /// planner maps message ids back to threads with rowid lookups into the fat
    /// `messages` table, which is 45x slower on a cold cache. See the module
    /// note above for the measurements.
    fn fts_threads(&mut self, expr: &str) -> String {
        let param = self.text(expr);
        format!(
            "t.id IN (SELECT m.thread_id FROM messages m INDEXED BY idx_messages_thread \
             WHERE m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH {param}))"
        )
    }

    fn compile(&mut self, node: &SearchNode, depth: usize) -> String {
        if depth > MAX_SEARCH_DEPTH {
            return "1".to_string();
        }
        match node {
            SearchNode::All => "1".to_string(),
            SearchNode::And { nodes } => self.join(nodes, "AND", depth),
            SearchNode::Or { nodes } => self.join(nodes, "OR", depth),
            SearchNode::Not { node } => {
                let inner = self.compile(node, depth + 1);
                format!("NOT ({inner})")
            }
            SearchNode::Text { value, prefix } => match fts_escape(value, *prefix) {
                // A term the tokenizer would not index cannot be in the index,
                // so it matches nothing. Saying so is more honest than dropping
                // the term and returning the whole mailbox.
                None => "0".to_string(),
                Some(expr) => self.fts_threads(&expr),
            },
            SearchNode::Flag { flag } => match flag {
                SearchFlag::Unread => "t.is_unread = 1".to_string(),
                SearchFlag::Read => "t.is_unread = 0".to_string(),
                SearchFlag::Attachment => "t.has_attachments = 1".to_string(),
                SearchFlag::Starred => "EXISTS (SELECT 1 FROM thread_labels tl \
                     WHERE tl.thread_id = t.id AND tl.gmail_label_id = 'STARRED')"
                    .to_string(),
            },
            SearchNode::Date { bound, ts } => {
                let param = self.bind(Value::Integer(*ts));
                match bound {
                    DateBound::Before => format!("t.last_message_at < {param}"),
                    DateBound::After => format!("t.last_message_at >= {param}"),
                }
            }
            SearchNode::Field {
                field,
                value,
                prefix,
            } => self.field(*field, value, *prefix),
        }
    }

    fn join(&mut self, nodes: &[SearchNode], op: &str, depth: usize) -> String {
        if nodes.is_empty() {
            return "1".to_string();
        }
        let parts: Vec<String> = nodes.iter().map(|n| self.compile(n, depth + 1)).collect();
        format!("({})", parts.join(&format!(" {op} ")))
    }

    fn field(&mut self, field: SearchField, value: &str, prefix: bool) -> String {
        match field {
            SearchField::Subject => match fts_escape(value, prefix) {
                None => "0".to_string(),
                // The column filter is our literal and the term is quoted, so
                // the user cannot reach the `:` that separates them.
                Some(expr) => self.fts_threads(&format!("subject : {expr}")),
            },
            SearchField::From => {
                let like = self.text(&like_contains(value));
                format!(
                    "((t.participants LIKE {like} ESCAPE '\\' \
                       OR (t.message_count >= {PARTICIPANT_ROLLUP_CAP} \
                           AND json_array_length(t.participants) >= {PARTICIPANT_ROLLUP_CAP})) \
                     AND EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id \
                       AND (m.from_email LIKE {like} ESCAPE '\\' OR m.from_name LIKE {like} ESCAPE '\\')))"
                )
            }
            SearchField::To | SearchField::Cc | SearchField::Bcc => {
                let column = match field {
                    SearchField::To => "to_json",
                    SearchField::Cc => "cc_json",
                    _ => "bcc_json",
                };
                let like = self.text(&like_contains(value));
                format!(
                    "EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id \
                     AND m.{column} LIKE {like} ESCAPE '\\')"
                )
            }
            SearchField::Filename => {
                let like = self.text(&like_contains(value));
                // Driven from `attachments` (a few thousand rows) rather than
                // per thread, then mapped back through the covering index.
                format!(
                    "t.id IN (SELECT m.thread_id FROM messages m INDEXED BY idx_messages_thread \
                     WHERE m.id IN (SELECT att.message_id FROM attachments att \
                       WHERE att.filename LIKE {like} ESCAPE '\\'))"
                )
            }
            SearchField::Label => {
                // A label is either the Gmail id the thread carries (`INBOX`,
                // `Label_12`) or the name the user gave it, which only the
                // `labels` table knows. Both spellings are accepted because
                // `in:inbox` produces one and `label:receipts` the other.
                let id = self.text(value);
                let name = self.text(&like_exact(value));
                format!(
                    "EXISTS (SELECT 1 FROM thread_labels tl WHERE tl.thread_id = t.id \
                     AND (tl.gmail_label_id = {id} COLLATE NOCASE \
                          OR EXISTS (SELECT 1 FROM labels l WHERE l.account_id = t.account_id \
                                       AND l.gmail_label_id = tl.gmail_label_id \
                                       AND l.name LIKE {name} ESCAPE '\\')))"
                )
            }
        }
    }
}

/// Compile a query to `(sql predicate, bound values)`.
///
/// Exposed for tests, which assert on the SQL text rather than only on results
/// — an injection regression is much easier to see in the statement than in an
/// empty result set.
pub fn compile_search(node: &SearchNode) -> (String, Vec<Value>) {
    let mut compiler = Compiler { args: Vec::new() };
    let sql = compiler.compile(node, 0);
    (sql, compiler.args)
}

/// Run an operator search and hydrate the page.
///
/// Ordered newest-first, not by relevance. That is Gmail's order, it is the
/// order the list next to it is already in, and it is the only one that lets
/// the query keyset-paginate — `search_threads` above keeps bm25 for ⌘K, where
/// six results are all that will ever be shown.
pub fn search_threads_filtered(
    conn: &Connection,
    node: &SearchNode,
    request: &SearchRequest,
) -> Result<Vec<ThreadSummary>> {
    let (predicate, mut args) = compile_search(node);

    let mut sql = format!(
        "SELECT {THREAD_COLUMNS} FROM threads t JOIN accounts a ON a.id = t.account_id \
         WHERE ({predicate})"
    );

    if let Some(account_id) = request.account_id {
        args.push(Value::Integer(account_id));
        sql.push_str(&format!(" AND t.account_id = ?{}", args.len()));
    }
    if let Some(cursor) = request.after {
        // Same row-value comparison as the stream, so the same index drives it.
        args.push(Value::Integer(cursor.last_message_at));
        args.push(Value::Integer(cursor.id));
        sql.push_str(&format!(
            " AND (t.last_message_at, t.id) < (?{}, ?{})",
            args.len() - 1,
            args.len()
        ));
    }

    let limit = match request.limit {
        0 => DEFAULT_PAGE_SIZE,
        n => n.min(MAX_PAGE_SIZE),
    };
    args.push(Value::Integer(limit as i64));
    sql.push_str(&format!(
        " ORDER BY t.last_message_at DESC, t.id DESC LIMIT ?{}",
        args.len()
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), map_thread)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
