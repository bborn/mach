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
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM thread_labels tl \
               WHERE tl.thread_id = t.id AND tl.gmail_label_id = ?{})",
            args.len()
        ));
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
    body_html, body_text, snippet, internal_date, is_unread, is_draft, reply_to";

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
                               snippet, internal_date, is_unread, is_draft, reply_to)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
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
             reply_to          = excluded.reply_to
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
        ],
        |row| row.get(0),
    )?;
    Ok(id)
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

const EVENT_COLUMNS: &str = "\
    id, account_id, calendar_id, google_event_id, title, description, location, \
    start_ts, end_ts, is_all_day, attendees, rsvp_status, recurring_event_id, \
    status, html_link, updated_at";

fn map_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    let attendees: String = row.get(10)?;
    let rsvp: Option<String> = row.get(11)?;
    Ok(Event {
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
        attendees: people_from_json(&attendees),
        rsvp_status: rsvp.as_deref().and_then(RsvpStatus::parse),
        recurring_event_id: row.get(12)?,
        status: row.get(13)?,
        html_link: row.get(14)?,
        updated_at: row.get(15)?,
    })
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

pub fn upsert_event(conn: &Connection, new: &NewEvent) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO events (account_id, calendar_id, google_event_id, title, description,
                             location, start_ts, end_ts, is_all_day, attendees, rsvp_status,
                             recurring_event_id, status, html_link, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
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
             updated_at         = excluded.updated_at
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
