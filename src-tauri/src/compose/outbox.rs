//! The ten-second undo window, and the queue behind it.
//!
//! # Why the window is a row and not a timer
//!
//! The obvious implementation of "undo send" is a `setTimeout` that fires the
//! API call ten seconds later. It is wrong in a way that only shows up on a bad
//! day: close the window, quit the app, or crash inside those ten seconds and
//! the message is gone — not delayed, *gone*, with no trace anywhere and no way
//! for the user to know. A mail client may lose a lot of things. It may not
//! lose a message the user was told had been sent.
//!
//! So the order is: build the RFC822 bytes, **commit them to SQLite**, write the
//! optimistic local copy so the thread repaints, and only then start counting.
//! Undo deletes the row before anything has left. A crash leaves the row, and
//! the next [`Outbox::flush_due`] — which any compose IPC call performs — sends
//! it. The window is advisory; the row is the truth.
//!
//! # Exactly once
//!
//! Two flushes can overlap (a timer and a manual one, or two windows). A row is
//! claimed with `UPDATE … SET state='sending' WHERE id=? AND state='holding'`
//! and only sent if that update changed a row, so the claim is the same
//! compare-and-swap SQLite already gives us for free. The same statement is
//! what makes undo safe against a flush that started in the same millisecond:
//! whichever ran first wins, and the other finds nothing to do.
//!
//! # Failure
//!
//! A retriable failure (rate limit, network) goes back to `holding` with a
//! later `send_after`, so it leaves on the next flush. A permanent one (a dead
//! grant, a rejected address) becomes `failed` and *keeps the bytes*: the user
//! can retry it after re-authorizing, which is only possible because the queue
//! stores the finished message rather than a reference to a draft that may have
//! been edited since.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::commands::GoogleClients;
use crate::db::models::{NewMessage, Participant};
use crate::db::{queries, Db};
use crate::google::GoogleError;

use super::draft::Built;
use super::mime::build_rfc822;
use super::{ensure_compose_schema, Result};

/// The spec's number. Long enough to notice the mistake, short enough that the
/// reply feels sent.
pub const UNDO_WINDOW_MS: i64 = 10_000;

/// How long a retriable failure waits before the next attempt. The API client's
/// own [`RetryPolicy`](crate::google::RetryPolicy) has already spent its budget
/// by the time we see the error, so this is the outer loop.
const RETRY_BACKOFF_MS: i64 = 30_000;

/// After this many failures the message stops trying and waits for a human.
const MAX_ATTEMPTS: i64 = 5;

/// Above this many raw bytes, a message goes to Gmail's upload host instead of
/// being base64'd into a JSON field.
///
/// Five megabytes is well under where the JSON endpoint starts refusing and
/// well over any message without a file attached, which is the property that
/// matters: the ordinary path stays the ordinary path, and the one that only
/// exists for attachments is only taken by messages that have them.
pub const UPLOAD_ABOVE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutboxState {
    /// Inside the undo window, or scheduled for later. Nothing has left.
    Holding,
    /// Claimed by a flush; a request is in flight.
    Sending,
    Sent,
    Failed,
}

impl OutboxState {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxState::Holding => "holding",
            OutboxState::Sending => "sending",
            OutboxState::Sent => "sent",
            OutboxState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "sending" => OutboxState::Sending,
            "sent" => OutboxState::Sent,
            "failed" => OutboxState::Failed,
            _ => OutboxState::Holding,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxEntry {
    pub id: String,
    pub account_id: i64,
    #[serde(default)]
    pub thread_id: Option<i64>,
    #[serde(default)]
    pub gmail_thread_id: Option<String>,
    pub subject: String,
    pub state: OutboxState,
    /// Unix millis. Until this instant the message can still be recalled.
    pub send_after: i64,
    pub created_at: i64,
    pub attempts: i64,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub sent_message_id: Option<String>,
}

impl OutboxEntry {
    /// True while undo is still available.
    pub fn is_recallable(&self) -> bool {
        matches!(self.state, OutboxState::Holding)
    }
}

/// What one flush pass did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlushOutcome {
    pub id: String,
    pub sent: bool,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Whether this will be tried again without the user asking.
    pub will_retry: bool,
}

/// The queue. Holds the store and the per-account client factory, nothing else —
/// no timer, no task, no channel, because the schedule lives in `send_after`.
pub struct Outbox {
    db: Db,
    clients: Arc<dyn GoogleClients>,
    user_id: String,
}

impl Outbox {
    pub fn new(db: Db, clients: Arc<dyn GoogleClients>) -> Result<Self> {
        db.write(ensure_compose_schema)?;
        Ok(Outbox {
            db,
            clients,
            user_id: "me".to_string(),
        })
    }

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = user_id.into();
        self
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The per-account client factory this queue was built with.
    ///
    /// Exposed so the draft push in [`super::remote`] reaches Gmail through the
    /// same factory the send path uses, rather than being handed a second one
    /// that could be configured differently.
    pub fn clients(&self) -> Arc<dyn GoogleClients> {
        Arc::clone(&self.clients)
    }

    // ----------------------------------------------------------------- queue

    /// Build, commit, and start the clock.
    ///
    /// `send_after` is `now + `[`UNDO_WINDOW_MS`] for an ordinary send and the
    /// chosen instant for a scheduled one — the two are the same mechanism,
    /// which is why `⌃S` is three lines of UI rather than a feature.
    pub fn queue(&self, built: &Built, now_ms: i64, send_after: i64) -> Result<OutboxEntry> {
        let rfc822 = build_rfc822(&built.outgoing)?;

        let entry = OutboxEntry {
            id: format!("ob-{now_ms:x}-{:x}", fnv(&built.outgoing.message_id)),
            account_id: built.account_id,
            thread_id: built.thread_id,
            gmail_thread_id: built.gmail_thread_id.clone(),
            subject: built.outgoing.subject.clone(),
            state: OutboxState::Holding,
            send_after,
            created_at: now_ms,
            attempts: 0,
            last_error: None,
            sent_message_id: None,
        };

        // The bytes go in first, on their own. If the process dies between this
        // commit and the optimistic copy below, the message still sends and the
        // thread simply learns about it from the next sync pass — the failure
        // mode is a repaint that is late, not a message that is lost. The other
        // order would give the reverse.
        self.db.write(|conn| {
            conn.execute(
                "INSERT INTO compose_outbox
                     (id, account_id, thread_id, gmail_thread_id, subject, rfc822, state,
                      send_after, created_at, attempts, last_error, sent_message_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, NULL, NULL)",
                rusqlite::params![
                    entry.id,
                    entry.account_id,
                    entry.thread_id,
                    entry.gmail_thread_id,
                    entry.subject,
                    rfc822,
                    entry.state.as_str(),
                    entry.send_after,
                    entry.created_at,
                ],
            )?;
            Ok(())
        })?;

        self.write_local_copy(&entry, built, now_ms)?;
        Ok(entry)
    }

    /// The optimistic local write. This is what makes the reply appear in the
    /// thread the instant `⌘⏎` is pressed, before Google has heard anything.
    ///
    /// The placeholder Gmail id is replaced with the real one on send, so the
    /// row the sync engine later upserts is this same row rather than a
    /// duplicate underneath it.
    fn write_local_copy(&self, entry: &OutboxEntry, built: &Built, now_ms: i64) -> Result<()> {
        let Some(thread_id) = built.thread_id else {
            return Ok(());
        };
        let out = &built.outgoing;
        let new = NewMessage {
            thread_id,
            account_id: entry.account_id,
            gmail_message_id: format!("mach-outbox:{}", entry.id),
            rfc822_message_id: Some(format!("<{}>", out.message_id)),
            in_reply_to: out.in_reply_to.as_ref().map(|id| format!("<{id}>")),
            // Our own outgoing mail never sets Reply-To.
            reply_to: Vec::new(),
            references: (!out.references.is_empty()).then(|| {
                out.references
                    .iter()
                    .map(|r| format!("<{r}>"))
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            from: out.from.to_participant(),
            to: participants(&out.to),
            cc: participants(&out.cc),
            bcc: participants(&out.bcc),
            subject: out.subject.clone(),
            body_html: Some(out.html.clone()),
            body_text: Some(out.text.clone()),
            snippet: snippet(&out.text),
            internal_date: now_ms,
            is_unread: false,
            is_draft: false,
        };

        self.db.write(|conn| {
            queries::upsert_message(conn, &new)?;
            // The list renders from `threads`, so a reply that does not touch
            // these two columns lands in the reading pane and nowhere else.
            conn.execute(
                "UPDATE threads
                    SET last_message_at = MAX(last_message_at, ?2),
                        message_count   = message_count + 1
                  WHERE id = ?1",
                rusqlite::params![thread_id, now_ms],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    // ------------------------------------------------------------------ undo

    /// Recall a message. Returns false if it had already gone.
    ///
    /// Deleting the row is what guarantees "no API call at all": the flusher
    /// only ever reads rows, so a row that is not there cannot be sent, and
    /// there is no in-flight timer holding a copy of the bytes.
    pub fn cancel(&self, id: &str) -> Result<bool> {
        let removed = self.db.write(|conn| {
            let changed = conn.execute(
                "DELETE FROM compose_outbox WHERE id = ?1 AND state = 'holding'",
                [id],
            )?;
            if changed > 0 {
                // Take the optimistic copy back out of the thread, and undo the
                // count it added.
                let thread_id: Option<i64> = conn
                    .query_row(
                        "SELECT thread_id FROM messages WHERE gmail_message_id = ?1",
                        [format!("mach-outbox:{id}")],
                        |row| row.get(0),
                    )
                    .optional()?;
                conn.execute(
                    "DELETE FROM messages WHERE gmail_message_id = ?1",
                    [format!("mach-outbox:{id}")],
                )?;
                if let Some(thread_id) = thread_id {
                    conn.execute(
                        "UPDATE threads SET message_count = MAX(message_count - 1, 0) WHERE id = ?1",
                        [thread_id],
                    )?;
                }
            }
            Ok(changed > 0)
        })?;
        Ok(removed)
    }

    // ----------------------------------------------------------------- reads

    pub fn list(&self) -> Result<Vec<OutboxEntry>> {
        Ok(self.db.read(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM compose_outbox ORDER BY send_after, id"
            ))?;
            let rows = stmt.query_map([], map_entry)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?)
    }

    /// Everything that has not been delivered — what the UI shows as pending.
    pub fn pending(&self) -> Result<Vec<OutboxEntry>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|e| !matches!(e.state, OutboxState::Sent))
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<Option<OutboxEntry>> {
        Ok(self.db.read(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {ENTRY_COLUMNS} FROM compose_outbox WHERE id = ?1"),
                    [id],
                    map_entry,
                )
                .optional()?)
        })?)
    }

    /// Forget a delivered message. Purely housekeeping — sent rows are kept
    /// only so the UI can say "sent" for a moment.
    pub fn forget_sent(&self, before_ms: i64) -> Result<usize> {
        Ok(self.db.write(|conn| {
            Ok(conn.execute(
                "DELETE FROM compose_outbox WHERE state = 'sent' AND created_at < ?1",
                [before_ms],
            )?)
        })?)
    }

    // ----------------------------------------------------------------- flush

    /// Send everything whose window has closed.
    ///
    /// Called from the frontend on a timer and from every compose IPC call, so
    /// a message queued in a window that then closed leaves on the next thing
    /// the app does. Nothing here reads the clock: `now_ms` is passed in, which
    /// is how the undo-window tests can move time without sleeping.
    pub async fn flush_due(&self, now_ms: i64) -> Result<Vec<FlushOutcome>> {
        let due = self.due_ids(now_ms)?;
        let mut out = Vec::with_capacity(due.len());
        for id in due {
            if let Some(outcome) = self.send_one(&id, now_ms).await? {
                out.push(outcome);
            }
        }
        Ok(out)
    }

    fn due_ids(&self, now_ms: i64) -> Result<Vec<String>> {
        Ok(self.db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id FROM compose_outbox
                  WHERE state = 'holding' AND send_after <= ?1
                  ORDER BY send_after, id",
            )?;
            let rows = stmt.query_map([now_ms], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?)
    }

    /// Returns `None` when the row was claimed by somebody else (or cancelled)
    /// between the read and the claim — not an error, just nothing to report.
    async fn send_one(&self, id: &str, now_ms: i64) -> Result<Option<FlushOutcome>> {
        let claimed = self.db.write(|conn| {
            Ok(conn.execute(
                "UPDATE compose_outbox SET state = 'sending', attempts = attempts + 1
                  WHERE id = ?1 AND state = 'holding'",
                [id],
            )?)
        })?;
        if claimed == 0 {
            return Ok(None);
        }

        let Some(entry) = self.get(id)? else {
            return Ok(None);
        };
        let rfc822: Vec<u8> = self.db.read(|conn| {
            Ok(conn.query_row(
                "SELECT rfc822 FROM compose_outbox WHERE id = ?1",
                [id],
                |row| row.get(0),
            )?)
        })?;

        let client = self.clients.gmail(entry.account_id)?;
        // Which road the bytes take is decided here and nowhere else. Below the
        // threshold the JSON endpoint is one request with one encoding; above
        // it, base64 inside a JSON string is both slower and — past a few
        // megabytes — refused outright, so the upload host takes the message as
        // bytes. Nothing else changes: the response is the same `Message`.
        let result = if rfc822.len() > UPLOAD_ABOVE_BYTES {
            client
                .messages_send_upload(&self.user_id, &rfc822, entry.gmail_thread_id.as_deref())
                .await
        } else {
            client
                .messages_send(&self.user_id, &rfc822, entry.gmail_thread_id.as_deref())
                .await
        };

        match result {
            Ok(message) => {
                self.mark_sent(&entry, &message.id)?;
                Ok(Some(FlushOutcome {
                    id: entry.id,
                    sent: true,
                    message_id: Some(message.id),
                    error: None,
                    will_retry: false,
                }))
            }
            Err(error) => {
                let retry = error.is_retriable() && entry.attempts + 1 < MAX_ATTEMPTS;
                self.mark_failed(&entry, &error, now_ms, retry)?;
                Ok(Some(FlushOutcome {
                    id: entry.id,
                    sent: false,
                    message_id: None,
                    error: Some(error.to_string()),
                    will_retry: retry,
                }))
            }
        }
    }

    fn mark_sent(&self, entry: &OutboxEntry, gmail_message_id: &str) -> Result<()> {
        self.db.write(|conn| {
            conn.execute(
                "UPDATE compose_outbox
                    SET state = 'sent', last_error = NULL, sent_message_id = ?2
                  WHERE id = ?1",
                rusqlite::params![entry.id, gmail_message_id],
            )?;
            // Adopt the real id, so the sync engine's upsert lands on this row
            // instead of inserting the same message a second time.
            if !gmail_message_id.is_empty() {
                conn.execute(
                    "UPDATE messages SET gmail_message_id = ?2 WHERE gmail_message_id = ?1",
                    rusqlite::params![format!("mach-outbox:{}", entry.id), gmail_message_id],
                )?;
            }
            Ok(())
        })?;
        Ok(())
    }

    fn mark_failed(
        &self,
        entry: &OutboxEntry,
        error: &GoogleError,
        now_ms: i64,
        retry: bool,
    ) -> Result<()> {
        let state = if retry {
            OutboxState::Holding
        } else {
            OutboxState::Failed
        };
        let send_after = if retry {
            now_ms + RETRY_BACKOFF_MS
        } else {
            entry.send_after
        };
        self.db.write(|conn| {
            conn.execute(
                "UPDATE compose_outbox SET state = ?2, last_error = ?3, send_after = ?4
                  WHERE id = ?1",
                rusqlite::params![entry.id, state.as_str(), error.to_string(), send_after],
            )?;
            if !retry {
                // A message that will not be retried must stop looking sent.
                // The bytes stay in the outbox, so retrying after fixing the
                // account is still possible.
                let thread_id: Option<i64> = conn
                    .query_row(
                        "SELECT thread_id FROM messages WHERE gmail_message_id = ?1",
                        [format!("mach-outbox:{}", entry.id)],
                        |row| row.get(0),
                    )
                    .optional()?;
                conn.execute(
                    "DELETE FROM messages WHERE gmail_message_id = ?1",
                    [format!("mach-outbox:{}", entry.id)],
                )?;
                if let Some(thread_id) = thread_id {
                    conn.execute(
                        "UPDATE threads SET message_count = MAX(message_count - 1, 0) WHERE id = ?1",
                        [thread_id],
                    )?;
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Put a failed message back in the queue.
    pub fn retry(&self, id: &str, now_ms: i64) -> Result<bool> {
        Ok(self.db.write(|conn| {
            Ok(conn.execute(
                "UPDATE compose_outbox SET state = 'holding', send_after = ?2, attempts = 0
                  WHERE id = ?1 AND state = 'failed'",
                rusqlite::params![id, now_ms],
            )? > 0)
        })?)
    }

    /// Throw a failed message away, bytes and all.
    pub fn discard(&self, id: &str) -> Result<bool> {
        Ok(self.db.write(|conn| {
            Ok(conn.execute(
                "DELETE FROM compose_outbox WHERE id = ?1 AND state != 'sending'",
                [id],
            )? > 0)
        })?)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

const ENTRY_COLUMNS: &str = "id, account_id, thread_id, gmail_thread_id, subject, state, \
                             send_after, created_at, attempts, last_error, sent_message_id";

fn map_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxEntry> {
    let state: String = row.get(5)?;
    Ok(OutboxEntry {
        id: row.get(0)?,
        account_id: row.get(1)?,
        thread_id: row.get(2)?,
        gmail_thread_id: row.get(3)?,
        subject: row.get(4)?,
        state: OutboxState::parse(&state),
        send_after: row.get(6)?,
        created_at: row.get(7)?,
        attempts: row.get(8)?,
        last_error: row.get(9)?,
        sent_message_id: row.get(10)?,
    })
}

fn participants(list: &[super::mime::Mailbox]) -> Vec<Participant> {
    list.iter().map(|m| m.to_participant()).collect()
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(200).collect()
}

/// A tiny non-cryptographic hash, so an outbox id is stable for a given
/// message without pulling in a uuid crate for one string.
fn fnv(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
