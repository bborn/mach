//! Gmail sync for one account: backfill, then history replay.
//!
//! # The watermark ordering
//!
//! This is the bug the whole file is arranged around. A backfill takes minutes;
//! mail arrives while it runs. If the account's `historyId` is read *after* the
//! backfill, every change that happened during it falls into the gap between
//! "the backfill's snapshot" and "the watermark" and is lost permanently —
//! silently, because the next incremental sync will happily report no changes.
//!
//! So the order is: read `users.getProfile` **first**, persist that `historyId`
//! into the backfill checkpoint, run the backfill, and only then promote the
//! *saved* value into `accounts.history_id` and immediately replay history from
//! it. The replay re-visits everything that moved during the backfill. Re-adding
//! a message the backfill already stored is an upsert, so the overlap costs
//! nothing.
//!
//! # Resumability
//!
//! The checkpoint (`db::sync_queries`) holds the pre-backfill watermark, the
//! `messages.list` page token, and a queue of enumerated-but-unfetched ids. A
//! queue row is deleted in the *same transaction* that writes its message, so
//! the only failure mode a crash can produce is re-fetching a message that was
//! already stored — which upserts to the same row.
//!
//! `accounts.history_id` stays `NULL` for the whole backfill. A half-finished
//! backfill therefore cannot be mistaken for a synced account.
//!
//! # Throughput
//!
//! The backfill used to run in lockstep: read a batch of ids, fetch that batch,
//! write it, repeat. Two things wasted most of the wire. The batch was a
//! barrier, so the tail of every batch ran at falling concurrency and the whole
//! batch waited on its slowest response; and no request at all was in flight
//! while the transaction was open. Measured against a real mailbox that came to
//! 11 messages a second against a ceiling of 50.
//!
//! [`MailSync::drain_queue`] is now a pipeline instead. One task keeps
//! `backfill_fetch_concurrency` `messages.get` calls in flight without ever
//! draining to zero, and hands completed messages to a *single* writer task —
//! single because SQLite has one writer, and fanning transactions out would only
//! move the queue from the wire to the mutex. Neither side waits for the other.
//!
//! Nothing about the correctness properties changed: the writer still deletes a
//! queue row in the same transaction that stores its message, and the watermark
//! is still read before enumeration and promoted only once the queue is empty.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use crate::db::{queries, sync_queries, Db, DbError};
use crate::google::gmail::{
    GmailClient, HistoryListQuery, HistoryType, MessageFormat, MessagesListQuery,
};
use crate::google::types as g;
use crate::google::GoogleError;

use super::cancel::CancelToken;
use super::convert;
use super::status::{AccountReporter, SyncPhase};
use super::{SyncConfig, SyncError};

/// One transaction's worth of fetched messages, on its way to the writer.
///
/// `None` is a message Gmail no longer has. It travels with the batch rather
/// than being dropped because its queue row still has to go, or a message
/// deleted between listing and fetching stalls the backfill forever.
type FetchedBatch = Vec<(String, Option<g::Message>)>;

/// How many write batches may be waiting on the writer before the fetcher is
/// made to wait. Two is enough to cover one transaction's duration; more would
/// just hold more parsed messages in memory for no extra overlap.
const WRITE_QUEUE_DEPTH: usize = 2;

/// How far ahead of the wire the id lease runs, as a multiple of the fetch
/// width. The fetcher must never stop to ask SQLite for its next id, and a
/// keyset read of a few hundred rows costs about as much as one of ten.
const LEASE_AHEAD: usize = 4;

/// Everything one account's Gmail sync needs. Cheap to construct per pass.
pub struct MailSync {
    pub db: Db,
    pub gmail: GmailClient,
    pub account_id: i64,
    pub config: Arc<SyncConfig>,
    pub cancel: CancelToken,
    pub report: AccountReporter,
    /// Shared across every account, so five accounts cannot together open fifty
    /// connections to Google.
    pub limiter: Arc<Semaphore>,
}

impl MailSync {
    /// One full pass: labels, then either a backfill or a history replay.
    pub async fn run(&self) -> Result<u64, SyncError> {
        self.cancel.check()?;
        self.sync_labels().await?;

        let watermark = self.db.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT history_id FROM accounts WHERE id = ?1",
                    [self.account_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten())
        })?;

        match watermark {
            Some(watermark) => match self.incremental(&watermark).await {
                Err(SyncError::Google(e)) if e.requires_full_resync() => {
                    // Expected, not exceptional: the watermark aged out of
                    // Gmail's retention window. Throw it away and rebuild.
                    self.db.write(|conn| {
                        queries::set_history_id(conn, self.account_id, None)?;
                        sync_queries::clear_backfill(conn, self.account_id)
                    })?;
                    self.backfill().await
                }
                other => other,
            },
            None => self.backfill().await,
        }
    }

    // ----------------------------------------------------------------- labels

    async fn sync_labels(&self) -> Result<(), SyncError> {
        self.report.phase(SyncPhase::Labels);
        let labels = self.gmail.labels_list("me").await?;
        let account_id = self.account_id;
        self.db.write(|conn| {
            for label in &labels {
                if label.id.is_empty() {
                    continue;
                }
                queries::upsert_label(conn, &convert::prepare_label(account_id, label))?;
            }
            Ok(())
        })?;
        Ok(())
    }

    // --------------------------------------------------------------- backfill

    /// The last N months, resumably. Returns the number of messages written.
    async fn backfill(&self) -> Result<u64, SyncError> {
        self.report.phase(SyncPhase::Backfill);
        let account_id = self.account_id;

        let existing = self
            .db
            .read(|conn| sync_queries::backfill_checkpoint(conn, account_id))?;

        let checkpoint = match existing {
            // Resume. Note we deliberately reuse the *stored* watermark rather
            // than asking Google for a fresh one — a fresh one would skip
            // everything that changed while the app was closed.
            Some(cp) if cp.start_history_id.is_some() => cp,
            _ => {
                // Read the watermark BEFORE the first messages.list call.
                let profile = self.gmail.get_profile("me").await?;
                let window_start_ms = self.window_start_ms();
                self.db.write(|conn| {
                    sync_queries::begin_backfill(
                        conn,
                        account_id,
                        &profile.history_id,
                        window_start_ms,
                    )
                })?
            }
        };

        let start_history_id = checkpoint
            .start_history_id
            .clone()
            .ok_or_else(|| SyncError::Config("backfill checkpoint has no watermark".into()))?;

        self.enumerate(&checkpoint).await?;
        let written = self.drain_queue().await?;

        // Promote the pre-backfill watermark and drop the checkpoint together,
        // so there is no instant in which the account looks synced without one.
        self.db
            .write(|conn| sync_queries::finish_backfill(conn, account_id, &start_history_id))?;

        // Everything that arrived *during* the backfill is now replayed.
        let caught_up = match self.incremental(&start_history_id).await {
            Ok(n) => n,
            Err(SyncError::Google(e)) if e.requires_full_resync() => {
                // The backfill outlived Gmail's history retention. Drop the
                // watermark; the next pass rebuilds. Do not recurse.
                self.db
                    .write(|conn| queries::set_history_id(conn, account_id, None))?;
                0
            }
            Err(e) => return Err(e),
        };

        Ok(written + caught_up)
    }

    /// Phase one: walk `messages.list` and park every id in the queue.
    async fn enumerate(
        &self,
        checkpoint: &sync_queries::BackfillCheckpoint,
    ) -> Result<(), SyncError> {
        if checkpoint.enumeration_done {
            return Ok(());
        }
        let account_id = self.account_id;
        let query = MessagesListQuery::new()
            .q(format!("after:{}", checkpoint.window_start_ms / 1000))
            .max_results(self.config.list_page_size)
            .include_spam_trash(false);

        let mut page_token = checkpoint.page_token.clone();
        loop {
            self.cancel.check()?;
            let page = self
                .gmail
                .messages_list_page("me", &query, page_token.as_deref())
                .await?;
            let refs: Vec<(String, String)> = page
                .items
                .iter()
                .filter(|m| !m.id.is_empty())
                .map(|m| (m.id.clone(), m.thread_id.clone()))
                .collect();
            let next = page.next_page_token.clone();

            self.db.write(|conn| {
                sync_queries::enqueue_backfill(conn, account_id, &refs)?;
                sync_queries::set_backfill_cursor(
                    conn,
                    account_id,
                    next.as_deref(),
                    next.is_none(),
                )
            })?;

            self.publish_backfill_progress()?;

            match next {
                Some(token) => page_token = Some(token),
                None => return Ok(()),
            }
        }
    }

    /// Phase two: fetch the queued messages, as fast as the quota allows.
    ///
    /// A fetcher and a writer running concurrently, joined by a short bounded
    /// channel. Returns the number of messages actually committed — which is a
    /// count the writer produced, not one the fetcher hoped for, so the status
    /// the UI renders can never run ahead of the store.
    async fn drain_queue(&self) -> Result<u64, SyncError> {
        let (tx, rx) = mpsc::channel::<FetchedBatch>(WRITE_QUEUE_DEPTH);
        let writer = spawn_backfill_writer(
            self.db.clone(),
            self.account_id,
            self.report.clone(),
            rx,
        );

        let fetched = self.pump_fetches(&tx).await;
        // Closing the channel is how the writer is told there is no more work:
        // it commits what it still holds and exits.
        drop(tx);

        let written = match writer.await {
            Ok(Ok(written)) => written,
            // A store failure is the cause; the fetcher failing to send into the
            // channel it then closed is only the shadow of it.
            Ok(Err(e)) => return Err(SyncError::Db(e)),
            Err(join) => {
                return Err(SyncError::Db(DbError::Other(format!(
                    "backfill writer: {join}"
                ))))
            }
        };

        fetched?;
        Ok(written)
    }

    /// Keep the wire full until the queue is empty.
    ///
    /// The loop holds exactly one invariant: while there is work left and no
    /// failure, there are `width` requests in flight. Ids are leased from SQLite
    /// well ahead of the wire, and completed messages are handed off in
    /// `message_batch_size` lots without waiting for them to be stored.
    async fn pump_fetches(&self, tx: &mpsc::Sender<FetchedBatch>) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let batch_size = self.config.message_batch_size.max(1);
        let width = self.fetch_width();
        let lease_size = (width * LEASE_AHEAD).max(batch_size);

        let mut cursor: Option<String> = None;
        let mut exhausted = false;
        let mut lease: VecDeque<String> = VecDeque::new();
        let mut inflight: JoinSet<(String, Result<g::Message, GoogleError>)> = JoinSet::new();
        let mut pending: FetchedBatch = Vec::with_capacity(batch_size);
        let mut failure: Option<GoogleError> = None;

        loop {
            if self.cancel.is_cancelled() {
                inflight.shutdown().await;
                return Err(SyncError::Cancelled);
            }

            if failure.is_none() && !exhausted && lease.len() < width {
                let rows = self.db.read(|conn| {
                    sync_queries::next_backfill_batch_after(
                        conn,
                        account_id,
                        cursor.as_deref(),
                        lease_size,
                    )
                })?;
                match rows.last() {
                    Some((last, _)) => {
                        cursor = Some(last.clone());
                        lease.extend(rows.into_iter().map(|(id, _)| id));
                    }
                    None => exhausted = true,
                }
            }

            while failure.is_none() && inflight.len() < width {
                let Some(id) = lease.pop_front() else { break };
                let gmail = self.gmail.clone();
                let limiter = Arc::clone(&self.limiter);
                inflight.spawn(async move {
                    let _permit = limiter.acquire_owned().await;
                    let out = gmail.messages_get("me", &id, MessageFormat::Full).await;
                    (id, out)
                });
            }

            // Nothing on the wire and nothing left to put there.
            if inflight.is_empty() {
                break;
            }

            let joined = tokio::select! {
                biased;
                () = self.cancel.cancelled() => {
                    // Nothing outlives the shutdown: abort the in-flight gets
                    // and wait for the tasks to actually stop. Whatever is in
                    // `pending` was never handed to the writer, so its queue
                    // rows are still there and the next pass re-fetches it.
                    inflight.shutdown().await;
                    return Err(SyncError::Cancelled);
                }
                joined = inflight.join_next() => joined,
            };
            let Some(joined) = joined else { break };
            match joined {
                Ok((id, Ok(message))) => pending.push((id, Some(message))),
                // A 404 means the message vanished between listing and fetching,
                // which is ordinary.
                Ok((id, Err(e))) if e.is_not_found() => pending.push((id, None)),
                Ok((_, Err(e))) => {
                    if failure.is_none() {
                        failure = Some(e);
                    }
                    inflight.abort_all();
                }
                Err(join) if join.is_cancelled() => {}
                Err(join) => {
                    if failure.is_none() {
                        failure = Some(GoogleError::Network {
                            message: join.to_string(),
                        });
                    }
                    inflight.abort_all();
                }
            }

            if pending.len() >= batch_size {
                send_batch(tx, std::mem::take(&mut pending)).await?;
                pending.reserve(batch_size);
            }
        }

        // Even a pass that died has already paid for what it fetched, so the
        // tail is written before the error is reported. Storing it is what stops
        // the retry from asking Google for it a second time.
        if !pending.is_empty() {
            send_batch(tx, pending).await?;
        }

        match failure {
            Some(e) => Err(SyncError::Google(e)),
            None => Ok(()),
        }
    }

    /// How many `messages.get` calls this account's backfill keeps in flight.
    ///
    /// Never above the global bound: the shared semaphore would hold the extra
    /// tasks anyway, and pretending otherwise would only make the number in the
    /// config a lie.
    fn fetch_width(&self) -> usize {
        self.config
            .backfill_fetch_concurrency
            .min(self.config.request_concurrency)
            .max(1)
    }

    fn publish_backfill_progress(&self) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let checkpoint = self
            .db
            .read(|conn| sync_queries::backfill_checkpoint(conn, account_id))?;
        if let Some(cp) = checkpoint {
            self.report
                .backfill_progress(cp.fetched_total, cp.queued_total);
        }
        Ok(())
    }

    fn window_start_ms(&self) -> i64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        now - self.config.backfill_window_days * 24 * 60 * 60 * 1000
    }

    // ------------------------------------------------------------ incremental

    /// Replay `users.history.list` from `start` and persist the new watermark
    /// only once the whole sweep is durably applied.
    async fn incremental(&self, start: &str) -> Result<u64, SyncError> {
        self.report.phase(SyncPhase::Incremental);
        let account_id = self.account_id;

        let query = HistoryListQuery::new(start)
            .history_types(HistoryType::all())
            .max_results(self.config.history_page_size);
        // No limit: truncating a sweep while keeping its watermark would skip
        // records permanently.
        let sweep = self.gmail.history_list_all("me", &query, None).await?;
        self.cancel.check()?;

        // Which message bodies do we actually have to go and get? Anything
        // added, plus anything whose labels moved that we have never seen. Not
        // anything deleted later in the same sweep.
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        let mut deleted: BTreeSet<String> = BTreeSet::new();
        for record in &sweep.records {
            for added in &record.messages_added {
                if !added.message.id.is_empty() {
                    wanted.insert(added.message.id.clone());
                }
            }
            for change in record.labels_added.iter().chain(record.labels_removed.iter()) {
                if !change.message.id.is_empty() {
                    wanted.insert(change.message.id.clone());
                }
            }
            for gone in &record.messages_deleted {
                if !gone.message.id.is_empty() {
                    deleted.insert(gone.message.id.clone());
                }
            }
        }
        for id in &deleted {
            wanted.remove(id);
        }
        let already: HashSet<String> = {
            let ids: Vec<String> = wanted.iter().cloned().collect();
            self.db.read(|conn| {
                let mut out = HashSet::new();
                for id in &ids {
                    if queries::message_by_gmail_id(conn, account_id, id)?.is_some() {
                        out.insert(id.clone());
                    }
                }
                Ok(out)
            })?
        };
        let to_fetch: Vec<String> = wanted
            .iter()
            .filter(|id| !already.contains(*id))
            .cloned()
            .collect();

        let mut fetched: HashMap<String, g::Message> = HashMap::new();
        for chunk in to_fetch.chunks(self.config.message_batch_size.max(1)) {
            self.cancel.check()?;
            for (id, message) in self.fetch_messages(chunk).await? {
                if let Some(message) = message {
                    fetched.insert(id, message);
                }
            }
        }

        self.cancel.check()?;

        let records = sweep.records;
        let watermark = sweep.history_id.clone();
        let written = self.db.write(|conn| {
            let mut touched: HashSet<i64> = HashSet::new();
            let mut stored = 0u64;

            for record in &records {
                for added in &record.messages_added {
                    if let Some(message) = fetched.get(&added.message.id) {
                        touched.insert(store_message(conn, account_id, message)?);
                        stored += 1;
                    }
                }
                for change in &record.labels_added {
                    if let Some(thread_id) = apply_label_change(
                        conn,
                        account_id,
                        &change.message.id,
                        &change.label_ids,
                        true,
                        &fetched,
                    )? {
                        touched.insert(thread_id);
                    }
                }
                for change in &record.labels_removed {
                    if let Some(thread_id) = apply_label_change(
                        conn,
                        account_id,
                        &change.message.id,
                        &change.label_ids,
                        false,
                        &fetched,
                    )? {
                        touched.insert(thread_id);
                    }
                }
                for gone in &record.messages_deleted {
                    if let Some(thread_id) = sync_queries::delete_message_by_gmail_id(
                        conn,
                        account_id,
                        &gone.message.id,
                    )? {
                        touched.insert(thread_id);
                    }
                }
            }

            for thread_id in touched {
                sync_queries::recompute_thread(conn, thread_id)?;
            }

            // The watermark moves last, inside the same transaction as the
            // changes it accounts for. A crash one statement earlier replays
            // the batch; replaying is idempotent.
            if let Some(watermark) = &watermark {
                queries::set_history_id(conn, account_id, Some(watermark))?;
            }
            Ok(stored)
        })?;

        self.report.add_messages(written as i64);
        Ok(written)
    }

    // ------------------------------------------------------------- fetching

    /// `messages.get` for a batch, with global concurrency bounded by the
    /// shared semaphore. A `404` means the message vanished between listing and
    /// fetching, which is ordinary; it comes back as `None`.
    async fn fetch_messages(
        &self,
        ids: &[String],
    ) -> Result<Vec<(String, Option<g::Message>)>, SyncError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.cancel.check()?;

        let mut set: JoinSet<(String, Result<g::Message, GoogleError>)> = JoinSet::new();
        for id in ids {
            let gmail = self.gmail.clone();
            let limiter = Arc::clone(&self.limiter);
            let id = id.clone();
            set.spawn(async move {
                let _permit = limiter.acquire_owned().await;
                let out = gmail.messages_get("me", &id, MessageFormat::Full).await;
                (id, out)
            });
        }

        let mut out = Vec::with_capacity(ids.len());
        let mut failure: Option<GoogleError> = None;
        loop {
            let joined = tokio::select! {
                biased;
                () = self.cancel.cancelled() => {
                    // Nothing outlives the shutdown: abort the in-flight gets
                    // and wait for the tasks to actually stop.
                    set.shutdown().await;
                    return Err(SyncError::Cancelled);
                }
                joined = set.join_next() => joined,
            };
            let Some(joined) = joined else { break };
            match joined {
                Ok((id, Ok(message))) => out.push((id, Some(message))),
                Ok((id, Err(e))) if e.is_not_found() => out.push((id, None)),
                Ok((_, Err(e))) => {
                    if failure.is_none() {
                        failure = Some(e);
                    }
                    set.abort_all();
                }
                Err(join) if join.is_cancelled() => {}
                Err(join) => {
                    if failure.is_none() {
                        failure = Some(GoogleError::Network {
                            message: join.to_string(),
                        });
                    }
                }
            }
        }

        match failure {
            Some(e) => Err(SyncError::Google(e)),
            None => Ok(out),
        }
    }
}

// ---------------------------------------------------------------------------
// the backfill writer
// ---------------------------------------------------------------------------

/// Hand a batch to the writer, waiting if it is still behind.
///
/// The wait is the backpressure that keeps memory bounded: a fetcher that can
/// outrun the store is told to stop asking for more, rather than piling parsed
/// messages up in a channel.
async fn send_batch(
    tx: &mpsc::Sender<FetchedBatch>,
    batch: FetchedBatch,
) -> Result<(), SyncError> {
    tx.send(batch)
        .await
        .map_err(|_| SyncError::Db(DbError::Other("backfill writer stopped".into())))
}

/// The store side of the backfill pipeline.
///
/// One task, because SQLite has one writer: fanning these transactions out over
/// several tasks would move the queue from the wire onto the writer mutex and
/// buy nothing. Each batch is one transaction, and **the queue rows are deleted
/// inside it** — that single fact is what makes a crash re-fetch a message
/// rather than skip it, and it is why the deletion cannot be hoisted out to
/// "after the write succeeded".
///
/// Progress is published from the counters the same transaction just updated,
/// so `messagesWritten` and the percentage the UI shows are never ahead of what
/// is durable, and cost no extra read.
fn spawn_backfill_writer(
    db: Db,
    account_id: i64,
    report: AccountReporter,
    mut rx: mpsc::Receiver<FetchedBatch>,
) -> JoinHandle<Result<u64, DbError>> {
    tokio::spawn(async move {
        let mut written = 0u64;
        while let Some(batch) = rx.recv().await {
            let ids: Vec<String> = batch.iter().map(|(id, _)| id.clone()).collect();
            let (stored, done, total) = db.write(|conn| {
                let mut touched: HashSet<i64> = HashSet::new();
                let mut stored = 0u64;
                for (_, message) in &batch {
                    if let Some(message) = message {
                        touched.insert(store_message(conn, account_id, message)?);
                        stored += 1;
                    }
                }
                // Dequeue everything the batch covers, including ids Google no
                // longer has — otherwise a deleted message stalls the queue.
                sync_queries::dequeue_backfill(conn, account_id, &ids)?;
                for thread_id in touched {
                    sync_queries::recompute_thread(conn, thread_id)?;
                }
                let (done, total) = sync_queries::backfill_checkpoint(conn, account_id)?
                    .map(|cp| (cp.fetched_total, cp.queued_total))
                    .unwrap_or((0, 0));
                Ok((stored, done, total))
            })?;

            written += stored;
            report.add_messages(stored as i64);
            report.backfill_progress(done, total);
        }
        Ok(written)
    })
}

// ---------------------------------------------------------------------------
// write helpers (inside a transaction)
// ---------------------------------------------------------------------------

/// Write one Gmail message and everything hanging off it. Returns the thread
/// row it belongs to, so the caller can recompute that thread once per batch
/// rather than once per message.
pub(crate) fn store_message(
    conn: &rusqlite::Connection,
    account_id: i64,
    message: &g::Message,
) -> Result<i64, DbError> {
    let prepared = convert::prepare_message(account_id, message);
    let thread_id = sync_queries::ensure_thread(conn, account_id, &prepared.gmail_thread_id)?;

    let mut row = prepared.message;
    row.thread_id = thread_id;
    let message_id = queries::upsert_message(conn, &row)?;

    sync_queries::clear_message_attachments(conn, message_id)?;
    for attachment in prepared.attachments {
        queries::upsert_attachment(
            conn,
            &crate::db::models::NewAttachment {
                message_id,
                ..attachment
            },
        )?;
    }

    sync_queries::set_message_labels(
        conn,
        account_id,
        &row.gmail_message_id,
        &prepared.label_ids,
    )?;
    Ok(thread_id)
}

/// Apply one `labelsAdded` / `labelsRemoved` record.
///
/// Set semantics on the per-message label list, so replaying the same record is
/// a no-op rather than a double-add or a double-remove. Returns the thread row
/// to recompute, or `None` when the message is unknown and unfetchable (it was
/// deleted before we could get it).
fn apply_label_change(
    conn: &rusqlite::Connection,
    account_id: i64,
    gmail_message_id: &str,
    labels: &[String],
    add: bool,
    fetched: &HashMap<String, g::Message>,
) -> Result<Option<i64>, DbError> {
    if gmail_message_id.is_empty() {
        return Ok(None);
    }
    if queries::message_by_gmail_id(conn, account_id, gmail_message_id)?.is_none() {
        // A label moved on a message we have never stored — usually one that
        // arrived and was labelled in the same sweep.
        let Some(message) = fetched.get(gmail_message_id) else {
            return Ok(None);
        };
        store_message(conn, account_id, message)?;
    }

    let resulting = if add {
        sync_queries::add_message_labels(conn, account_id, gmail_message_id, labels)?
    } else {
        sync_queries::remove_message_labels(conn, account_id, gmail_message_id, labels)?
    };
    let Some(resulting) = resulting else {
        return Ok(None);
    };
    sync_queries::set_message_unread_from_labels(conn, account_id, gmail_message_id, &resulting)?;

    let thread_id: Option<i64> = {
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT thread_id FROM messages WHERE account_id = ?1 AND gmail_message_id = ?2",
            rusqlite::params![account_id, gmail_message_id],
            |row| row.get(0),
        )
        .optional()?
    };
    Ok(thread_id)
}
