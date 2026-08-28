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
use crate::db::models::{NewMessage, Participant, OUTBOX_ID_PREFIX};
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
    /// The `compose_drafts` row this message was written as, and what it was on
    /// Gmail. Carried because the draft row is deleted the moment the message
    /// is queued: by the time this leaves, these three strings are all that is
    /// left of it. See [`Outbox::send_one`].
    #[serde(default)]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub gmail_draft_id: Option<String>,
    #[serde(default)]
    pub gmail_draft_message_id: Option<String>,
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
        ensure_compose_schema(&db)?;
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
            draft_id: Some(built.draft_id.clone()),
            gmail_draft_id: built.gmail_draft_id.clone(),
            gmail_draft_message_id: built.gmail_draft_message_id.clone(),
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
                      send_after, created_at, attempts, last_error, sent_message_id,
                      draft_id, gmail_draft_id, gmail_draft_message_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, NULL, NULL, ?10, ?11, ?12)",
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
                    entry.draft_id,
                    entry.gmail_draft_id,
                    entry.gmail_draft_message_id,
                ],
            )?;
            Ok(())
        })?;

        // The draft leaves the conversation here, in the same breath as the
        // reply arriving in it, because those two are one event: the message the
        // owner was writing is now the message he has sent. Doing it here rather
        // than in the caller is what makes the repaint immediate — `⌘⏎` used to
        // show the reply *and* a `DRAFT` row of the same words, and the draft
        // went several seconds later when a sync pass got round to it.
        //
        // Its inverse is in [`Outbox::cancel`], which is the only other thing
        // that ever undoes this write.
        if !built.draft_id.is_empty() {
            super::mirror::unmirror_ids(
                &self.db,
                &built.draft_id,
                built.gmail_draft_message_id.as_deref(),
            )?;
        }
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
            gmail_message_id: placeholder_message_id(&entry.id),
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
            // See the mirror: our own text alternative is not flowed, and the
            // MIME we send does not say it is. No `search_text` either; that
            // column is about what a stranger's markup adds.
            body_text_flowed: false,
            body_text_delsp: false,
            search_text: None,
            snippet: snippet(&out.text),
            internal_date: now_ms,
            is_unread: false,
            is_draft: false,
            // Mail Mach sent carries no list headers.
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            list_id: None,
            precedence: None,
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
    ///
    /// # The inverse, exactly
    ///
    /// [`Outbox::queue`] does three things to the conversation: it takes the
    /// draft's mirror out, puts the outgoing message in, and retires the draft
    /// row. This undoes all three, in the opposite order, before it returns — so
    /// the repaint after `⌘Z` is one refetch and shows what was on screen before
    /// `⌘⏎`, whether or not the composer that sent it still exists. The window
    /// can have been closed, or the app relaunched: the text comes back from the
    /// tombstone rather than from the UI.
    pub fn cancel(&self, id: &str, now_ms: i64) -> Result<bool> {
        // Read before the delete, because the draft this was written as is only
        // named on the row that is about to go.
        let draft_id = self.get(id)?.and_then(|entry| entry.draft_id);
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
                        [placeholder_message_id(id)],
                        |row| row.get(0),
                    )
                    .optional()?;
                conn.execute(
                    "DELETE FROM messages WHERE gmail_message_id = ?1",
                    [placeholder_message_id(id)],
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
        // The draft is a draft again. Nothing was sent, so the Gmail draft this
        // was going to be sent *as* is still there — and the row has to point
        // at it again, or the composer's next save creates a second one beside
        // it. See `draft::revive`.
        if removed {
            if let Some(draft_id) = draft_id {
                super::draft::revive(&self.db, &draft_id)?;
                // And back into the conversation it answers, which is where the
                // owner is looking. The composer saves the text again a moment
                // later and lands on this same row — one draft, one mirror.
                if let Some(draft) = super::draft::load_draft(&self.db, &draft_id)? {
                    super::mirror::mirror(&self.db, &draft, now_ms)?;
                }
            }
        }
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

    /// The row still waiting to leave that was written as `draft_id`, if any.
    ///
    /// This is what separates a draft that was *sent* from one that was
    /// *discarded*. Both delete the `compose_drafts` row and both write a
    /// tombstone, so from `compose::remote`'s side they are the same event —
    /// and it used to treat them the same, deleting the Gmail draft behind
    /// both. For a discard that is right. For a send it destroys the very
    /// draft the outbox is about to call `drafts.send` on, and the send comes
    /// back 404 ten seconds later with the message never delivered.
    ///
    /// A live row here means the outbox owns that Gmail draft until it leaves.
    pub fn owner_of_draft(db: &Db, draft_id: &str) -> Result<Option<OutboxEntry>> {
        Ok(db.read(|conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {ENTRY_COLUMNS} FROM compose_outbox \
                         WHERE draft_id = ?1 AND state IN ('holding', 'sending') \
                         ORDER BY created_at DESC LIMIT 1"
                    ),
                    [draft_id],
                    map_entry,
                )
                .optional()?)
        })?)
    }

    /// Point a waiting row at the Gmail draft that actually exists now.
    ///
    /// A push that was in flight when the message was queued can come back
    /// having *created* a draft rather than updated one, and its id is then
    /// not the id the outbox captured. Sending the captured one would 404 just
    /// as surely, so the newer id replaces it.
    pub fn repoint_draft(
        db: &Db,
        id: &str,
        gmail_draft_id: &str,
        gmail_draft_message_id: Option<&str>,
    ) -> Result<()> {
        db.write(|conn| {
            conn.execute(
                "UPDATE compose_outbox \
                 SET gmail_draft_id = ?2, gmail_draft_message_id = ?3 \
                 WHERE id = ?1 AND state IN ('holding', 'sending')",
                rusqlite::params![id, gmail_draft_id, gmail_draft_message_id],
            )?;
            Ok(())
        })?;
        Ok(())
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
        let thread = entry.gmail_thread_id.as_deref();
        // Two decisions, and they are independent.
        //
        // **Which operation** depends on whether this message already exists as
        // a Gmail draft. If it does, `drafts.send` sends *that draft* and
        // removes it in one request; anything else leaves the draft behind, and
        // a draft of a message you have just sent is the duplicate this whole
        // path exists to stop. If it does not — a reply sent before the push
        // landed, or one written with the network down — `messages.send` is
        // still the only thing that can send it.
        //
        // **Which host** depends on size, exactly as it did before. Below the
        // threshold the JSON endpoint is one request with one encoding; above
        // it, base64 inside a JSON string is both slower and — past a few
        // megabytes — refused outright, so the upload host takes the message as
        // bytes. The draft-backed send needed its own upload form for this
        // reason: routing by draft id must not quietly cost a reply its
        // attachment. Every arm answers with the same `Message`.
        let big = rfc822.len() > UPLOAD_ABOVE_BYTES;
        let result = match entry.gmail_draft_id.as_deref().filter(|id| !id.is_empty()) {
            Some(draft_id) if big => {
                client
                    .drafts_send_upload(&self.user_id, draft_id, &rfc822, thread)
                    .await
            }
            Some(draft_id) => {
                client
                    .drafts_send(&self.user_id, draft_id, &rfc822, thread)
                    .await
            }
            None if big => {
                client
                    .messages_send_upload(&self.user_id, &rfc822, thread)
                    .await
            }
            None => client.messages_send(&self.user_id, &rfc822, thread).await,
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

    /// The message has left. Take the draft's local remains with it, then let
    /// the optimistic copy adopt the id Gmail gave the message.
    ///
    /// # Why the mirror is removed here as well as at queue time
    ///
    /// The draft's mirror goes when the message is queued — that is what makes
    /// the conversation repaint with the reply instead of the draft. But the
    /// Gmail draft is still there for the length of the undo window, so a sync
    /// pass inside those ten seconds can pull it back down as an ordinary
    /// `DRAFT` message. `drafts.send` then consumes the draft and the row it
    /// left behind is a mirror of something that no longer exists.
    ///
    /// This runs before the rename for one specific reason: `drafts.send` can
    /// hand back the same message id the draft was filed under, and two rows
    /// cannot hold one `(account_id, gmail_message_id)`. Clearing the draft
    /// first is what leaves the sent message somewhere to land — and
    /// [`mirror::unmirror_ids`](super::mirror::unmirror_ids) will only ever
    /// delete a row that is still a draft, so the sent message itself is not
    /// something this can reach.
    fn mark_sent(&self, entry: &OutboxEntry, gmail_message_id: &str) -> Result<()> {
        if let Some(draft_id) = entry.draft_id.as_deref().filter(|id| !id.is_empty()) {
            // Its own transaction, and it must be its own: `Db::write` takes the
            // writer mutex, so this cannot be folded into the one below.
            super::mirror::unmirror_ids(
                &self.db,
                draft_id,
                entry.gmail_draft_message_id.as_deref(),
            )?;
        }
        let placeholder = placeholder_message_id(&entry.id);
        self.db.write(|conn| {
            conn.execute(
                "UPDATE compose_outbox
                    SET state = 'sent', last_error = NULL, sent_message_id = ?2
                  WHERE id = ?1",
                rusqlite::params![entry.id, gmail_message_id],
            )?;
            // Adopt the real id, so the sync engine's upsert lands on this row
            // instead of inserting the same message a second time.
            //
            // `OR IGNORE`, as in `mirror::adopt`, because a sync pass can have
            // imported the sent message already — between `drafts.send`
            // returning and this write. The rename is then refused rather than
            // taking the whole flush down with a constraint error, and the copy
            // that has to go is the optimistic one.
            if !gmail_message_id.is_empty() {
                conn.execute(
                    "UPDATE OR IGNORE messages SET gmail_message_id = ?2
                      WHERE gmail_message_id = ?1",
                    rusqlite::params![placeholder, gmail_message_id],
                )?;
                let thread_id: Option<i64> = conn
                    .query_row(
                        "SELECT thread_id FROM messages WHERE gmail_message_id = ?1",
                        [&placeholder],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(thread_id) = thread_id {
                    conn.execute(
                        "DELETE FROM messages WHERE gmail_message_id = ?1",
                        [&placeholder],
                    )?;
                    conn.execute(
                        "UPDATE threads
                            SET message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?1)
                          WHERE id = ?1",
                        [thread_id],
                    )?;
                }
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
                        [placeholder_message_id(&entry.id)],
                        |row| row.get(0),
                    )
                    .optional()?;
                conn.execute(
                    "DELETE FROM messages WHERE gmail_message_id = ?1",
                    [placeholder_message_id(&entry.id)],
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
                             send_after, created_at, attempts, last_error, sent_message_id, \
                             draft_id, gmail_draft_id, gmail_draft_message_id";

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
        draft_id: row.get(11)?,
        gmail_draft_id: row.get(12)?,
        gmail_draft_message_id: row.get(13)?,
    })
}

/// The id the optimistic local copy is filed under until Gmail answers with a
/// real one. See [`OUTBOX_ID_PREFIX`]: the command layer recognises the
/// namespace and keeps these out of Gmail requests, which it can only do while
/// every mint of one goes through here.
fn placeholder_message_id(entry_id: &str) -> String {
    format!("{OUTBOX_ID_PREFIX}{entry_id}")
}

/// Is this Gmail draft message one the outbox is holding — a message that has
/// been sent as far as the owner is concerned?
///
/// # The `DRAFT` row above the reply he had just sent
///
/// Queuing takes the draft out of the conversation locally and **leaves it on
/// Gmail**, because `drafts.send` is what will send it when the undo window
/// lapses; deleting it at `⌘⏎` would leave nothing to send. So between those
/// two moments Gmail still holds — and still lists — a draft that Mach has
/// already removed, and the history record the draft's own push wrote is still
/// sitting there waiting to be replayed.
///
/// The next sync pass replays it, fetches the message, sees the `DRAFT` label
/// and stores it. The mirror it would have upserted onto is gone, so it lands
/// as a **new row**: no `mach_draft_id`, no `compose_drafts` row behind it,
/// nothing that any removal path addresses. It renders exactly like the draft
/// the owner just sent, in the conversation, above the reply, stamped the same
/// minute — and it stays there until the send actually happens.
/// [`Outbox::mark_sent`] does clear it, which is why it always went away
/// eventually; the ten seconds before that is what he kept reporting, and a
/// scheduled send makes those ten seconds an afternoon.
///
/// So the row is refused at the door instead. `holding` and `sending` only:
/// a send that has permanently failed is not going to consume that draft, and
/// its text is no longer anywhere else, so Gmail's copy coming back is then the
/// correct outcome rather than the bug.
pub fn holds_draft_message(
    conn: &rusqlite::Connection,
    account_id: i64,
    gmail_message_id: &str,
) -> rusqlite::Result<bool> {
    if gmail_message_id.is_empty() {
        return Ok(false);
    }
    // The composer owns this table and creates it lazily, so a store whose
    // owner has never written a message does not have it — and `pragma_table_info`
    // of a table that is not there is zero rows rather than an error, which
    // answers "is there a column to read?" and "is there a table?" at once.
    let column: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('compose_outbox') \
              WHERE name = 'gmail_draft_message_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if column.is_none() {
        return Ok(false);
    }
    Ok(conn
        .query_row(
            "SELECT 1 FROM compose_outbox \
              WHERE account_id = ?1 AND gmail_draft_message_id = ?2 \
                AND state IN ('holding', 'sending')",
            rusqlite::params![account_id, gmail_message_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
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
