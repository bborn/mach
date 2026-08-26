//! The background sync loop — the component that makes "the UI never waits on
//! Google" true.
//!
//! Nothing in here is ever on a user's critical path. The engine's only job is
//! to keep local SQLite an accurate mirror of five Gmail mailboxes and their
//! calendars, so that every read the UI performs is a local read.
//!
//! # Shape
//!
//! ```text
//!   SyncEngine::start()
//!        │
//!        └── loop task ──┬── account 1 task ── MailSync ─┐
//!                        ├── account 2 task ── MailSync ─┤──► db::queries ──► SQLite
//!                        ├── …                CalendarSync ┘
//!                        └── (bounded by two semaphores)
//! ```
//!
//! * **Per-account isolation.** Each account is its own task with its own
//!   watermark. A revoked token or a rate-limited account records an error in
//!   its own [`status::AccountStatus`] and the other four finish normally.
//! * **Two bounds, not one.** `account_concurrency` caps how many mailboxes are
//!   in flight; `request_concurrency` caps how many HTTP requests exist across
//!   *all* of them. Without the second, five accounts each running a wide
//!   backfill would put a hundred requests on the wire at once and earn a 429
//!   for everyone.
//! * **A pipeline, not a lockstep.** The backfill enumerates ids into a durable
//!   queue, then keeps `backfill_fetch_concurrency` `messages.get` calls in
//!   flight continuously while a single writer task commits completed messages
//!   `message_batch_size` at a time. Fetching does not stop while a transaction
//!   is open. That mattered more when Gmail's quota allowed 50 fetches a second;
//!   it now allows five, and the width is set by the quota rather than by how
//!   much the wire can be kept full. See [`GMAIL_QUOTA_UNITS_PER_USER_PER_MINUTE`].
//! * **Cancellation is structural.** Every task holds a [`CancelToken`]; the
//!   engine aborts its loop on drop. No detached work outlives shutdown.
//!
//! The two hard correctness properties — watermark ordering and backfill
//! resumability — are documented where they live, in [`mail`].

pub mod cancel;
pub mod calendar;
pub mod convert;
pub mod mail;
pub mod status;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use crate::db::models::Account;
use crate::db::{queries, sync_queries, Db, DbError};
use crate::google::calendar::CalendarClient;
use crate::google::gmail::GmailClient;
use crate::google::{GoogleError, HttpTransport, RetryPolicy, Sleeper, TokenProvider};

pub use cancel::{CancelToken, Cancelled};
pub use status::{AccountReporter, AccountStatus, StatusSink, SyncPhase, SyncStatus};

// ===========================================================================
// errors
// ===========================================================================

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("store: {0}")]
    Db(#[from] DbError),
    #[error("google: {0}")]
    Google(#[from] GoogleError),
    /// Shutdown was requested mid-pass. Everything committed stays committed.
    #[error("sync cancelled")]
    Cancelled,
    #[error("{0}")]
    Config(String),
}

impl From<Cancelled> for SyncError {
    fn from(_: Cancelled) -> Self {
        SyncError::Cancelled
    }
}

impl SyncError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, SyncError::Cancelled)
    }

    /// Google refused the stored credential, so no later pass can get past this
    /// without a person authorizing the account again.
    ///
    /// The whole reason it is asked here rather than at the token layer: by the
    /// time a pass fails, the cause has been through a `TokenProvider`, a
    /// `GoogleError` and a `SyncError`, and this is the last place that still
    /// knows which account it belongs to.
    pub fn is_credential_rejected(&self) -> bool {
        matches!(self, SyncError::Google(e) if e.is_credential_rejected())
    }
}

// ===========================================================================
// Gmail's quota
// ===========================================================================
//
// Google revised the Gmail quota on 1 May 2026. `messages.get` went from 5
// units to 20 and the per-user ceiling became 6,000 units a minute, so the
// width that used to sit just under the ceiling is now eight times over it.
// The width had been a literal with a paragraph of arithmetic beside it, and
// the paragraph is what went stale. It is constants and a `const fn` now, and a
// test pins the relationship between them.

/// Gmail's per-user ceiling.
///
/// > Per minute per user per project — 6,000 quota units
///
/// — [Gmail API usage limits][quota]
///
/// That page also says a Cloud project which used the API between November 2025
/// and April 2026 keeps its previous quotas, and that projects created on or
/// after 1 May 2026 get these. Mach ships no credentials: every user registers
/// their own Cloud project (see the README), so anybody installing it now has a
/// project created after the revision. The default has to assume the new
/// numbers, and an older grandfathered project simply runs under its budget.
///
/// [quota]: https://developers.google.com/workspace/gmail/api/reference/quota
pub const GMAIL_QUOTA_UNITS_PER_USER_PER_MINUTE: u32 = 6_000;

/// What one `users.messages.get` costs, from the same table.
///
/// This is the number that moved. It was 5, which is why 16 requests in flight
/// used to be a reasonable width and now is eight times the whole budget.
pub const GMAIL_MESSAGES_GET_UNITS: u32 = 20;

/// The share of a user's per-minute quota the backfill may spend.
///
/// The rest pays for everything the same account does while a backfill runs:
/// `history.list` at 2 units, `drafts.list` at 5, `messages.modify` at 5 for
/// every archive or star, `messages.attachments.get` at 20, and whatever the
/// person reading their mail asks for. Spending the whole ceiling on the
/// backfill would make the app unusable for as long as the backfill lasts.
pub const BACKFILL_QUOTA_SHARE_PERCENT: u32 = 80;

/// Round trip of one `messages.get`, measured against a real mailbox.
///
/// Only used to turn a rate into a width. Throughput of a pure-latency pipeline
/// is *concurrency ÷ round trip* and nothing else, so this is the other half of
/// the sum. It is the half that is a measurement rather than a published fact,
/// which is why the width rounds up: overshooting costs a 429 the client
/// already backs off from, and undershooting costs throughput silently.
pub const BACKFILL_ROUND_TRIP_MS: u32 = 400;

/// `messages.get` calls a minute the backfill is allowed to make.
///
/// 6,000 units × 80% ÷ 20 units = 240 a minute, four a second. The same sum
/// under the old quota gave forty a second.
pub const fn backfill_fetches_per_minute() -> u32 {
    GMAIL_QUOTA_UNITS_PER_USER_PER_MINUTE * BACKFILL_QUOTA_SHARE_PERCENT
        / 100
        / GMAIL_MESSAGES_GET_UNITS
}

/// The width that spends [`backfill_fetches_per_minute`] and no more.
///
/// `rate × round trip`, rounded up: 240 a minute over a 400 ms round trip is
/// 1.6 requests in flight, so 2.
pub const fn default_backfill_fetch_concurrency() -> usize {
    let per_minute = backfill_fetches_per_minute() as u64;
    let width = (per_minute * BACKFILL_ROUND_TRIP_MS as u64 + 59_999) / 60_000;
    if width < 1 {
        1
    } else {
        width as usize
    }
}

// ===========================================================================
// configuration
// ===========================================================================

#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// How far back the initial backfill reaches. Older mail stays reachable
    /// through server-side search.
    pub backfill_window_days: i64,
    /// Fetched messages written per transaction. This is a *write* batch, not a
    /// fetch batch: the backfill no longer waits for a batch to be written
    /// before asking for more.
    ///
    /// It is also the length of the longest wait a user command can inherit
    /// from the sync loop, which is what sets it. A background writer stands
    /// aside for a queued command at its next batch boundary, so a command
    /// waits for at most one batch in progress — and against the owner's
    /// mailbox a batch of 25 messages measured p99 111 ms, versus 27 ms at 10.
    /// The cost is 14% fewer messages a second written locally, which buys
    /// nothing: the backfill is bounded by Gmail's quota at four messages a
    /// second, and this writes over a thousand.
    pub message_batch_size: usize,
    /// `maxResults` for `messages.list` during the backfill.
    pub list_page_size: u32,
    /// `maxResults` for `users.history.list`.
    pub history_page_size: u32,
    /// Mailboxes synced at once.
    pub account_concurrency: usize,
    /// HTTP requests in flight across every account. The stampede guard.
    pub request_concurrency: usize,
    /// `messages.get` calls one account's backfill keeps in flight.
    ///
    /// This is the throughput knob, and it is not a free choice: it is Gmail's
    /// per-user quota divided by what a `messages.get` costs, times the round
    /// trip. [`default_backfill_fetch_concurrency`] does that sum from named
    /// constants so the next revision to the quota is one edit to one number.
    ///
    /// Never effectively larger than `request_concurrency`, which bounds every
    /// account together — going wider than the global bound would only park
    /// tasks on the shared semaphore.
    ///
    /// Not paired with a rate limiter. The client's [`RetryPolicy`] already
    /// backs off with jitter on a 429 and honours a `Retry-After`, so the
    /// consequence of the round-trip estimate being optimistic is a retry, not
    /// a stampede. The estimate is the weak part of this, and if it ever needs
    /// to stop being a guess the answer is a token bucket metered in quota
    /// units rather than a wider or narrower semaphore — concurrency cannot
    /// express "four a second" at all when four a second is two requests.
    pub backfill_fetch_concurrency: usize,
    pub calendar_past_days: i64,
    pub calendar_future_days: i64,
    pub calendar_page_size: u32,
    /// Empty means "ask Google for the calendar list".
    pub calendar_ids: Vec<String>,
    /// Gap between passes of the background loop.
    pub poll_interval: Duration,
    pub sync_mail: bool,
    pub sync_calendar: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            backfill_window_days: 365,
            message_batch_size: 10,
            list_page_size: 500,
            history_page_size: 500,
            account_concurrency: 5,
            // Five mailboxes onboarding at once share this; one mailbox on its
            // own is bounded by `backfill_fetch_concurrency` below. The quota
            // is per user, so five backfills at their own width are within
            // budget individually and this only has to stop the pathological
            // case — every account also fetching attachments and drafts at once.
            request_concurrency: 32,
            backfill_fetch_concurrency: default_backfill_fetch_concurrency(),
            calendar_past_days: 90,
            calendar_future_days: 365,
            calendar_page_size: 250,
            calendar_ids: Vec::new(),
            poll_interval: Duration::from_secs(60),
            sync_mail: true,
            sync_calendar: true,
        }
    }
}

// ===========================================================================
// clients
// ===========================================================================

/// How the engine gets an API client for an account.
///
/// A trait rather than a concrete type because the engine must not know how
/// tokens are stored: production hands it a Keychain-backed `TokenManager`,
/// tests hand it a static string and a scripted transport.
pub trait ClientFactory: Send + Sync + 'static {
    fn gmail(&self, account: &Account) -> Result<GmailClient, SyncError>;
    fn calendar(&self, account: &Account) -> Result<CalendarClient, SyncError>;
}

/// Resolves an account to the token provider that signs its requests.
pub type TokenProviderFor = dyn Fn(&Account) -> Arc<dyn TokenProvider> + Send + Sync;

/// The ordinary implementation: one shared transport (and therefore one
/// connection pool) plus a per-account token provider.
pub struct TransportClients {
    transport: Arc<dyn HttpTransport>,
    tokens: Box<TokenProviderFor>,
    gmail_base: Option<String>,
    calendar_base: Option<String>,
    retry: Option<RetryPolicy>,
    sleeper: Option<Arc<dyn Sleeper>>,
}

impl TransportClients {
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        tokens: impl Fn(&Account) -> Arc<dyn TokenProvider> + Send + Sync + 'static,
    ) -> Self {
        Self {
            transport,
            tokens: Box::new(tokens),
            gmail_base: None,
            calendar_base: None,
            retry: None,
            sleeper: None,
        }
    }

    /// Point both clients somewhere other than Google — the seam the tests use.
    pub fn with_base_urls(
        mut self,
        gmail: impl Into<String>,
        calendar: impl Into<String>,
    ) -> Self {
        self.gmail_base = Some(gmail.into());
        self.calendar_base = Some(calendar.into());
        self
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = Some(sleeper);
        self
    }
}

impl ClientFactory for TransportClients {
    fn gmail(&self, account: &Account) -> Result<GmailClient, SyncError> {
        let mut client = GmailClient::new(Arc::clone(&self.transport), (self.tokens)(account));
        if let Some(base) = &self.gmail_base {
            client = client.with_base_url(base.clone());
        }
        if let Some(retry) = self.retry {
            client = client.with_retry_policy(retry);
        }
        if let Some(sleeper) = &self.sleeper {
            client = client.with_sleeper(Arc::clone(sleeper));
        }
        Ok(client)
    }

    fn calendar(&self, account: &Account) -> Result<CalendarClient, SyncError> {
        let mut client = CalendarClient::new(Arc::clone(&self.transport), (self.tokens)(account));
        if let Some(base) = &self.calendar_base {
            client = client.with_base_url(base.clone());
        }
        if let Some(retry) = self.retry {
            client = client.with_retry_policy(retry);
        }
        if let Some(sleeper) = &self.sleeper {
            client = client.with_sleeper(Arc::clone(sleeper));
        }
        Ok(client)
    }
}

// ===========================================================================
// pass results
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountOutcome {
    pub account_id: i64,
    pub email: String,
    pub messages_written: u64,
    pub events_written: u64,
    /// `None` on success. An account that failed still leaves its siblings'
    /// outcomes untouched.
    pub error: Option<String>,
    /// The failure was Google refusing the stored credential. Retrying is not
    /// the recovery; signing in again is.
    #[serde(default)]
    pub needs_reauthorization: bool,
    pub cancelled: bool,
    /// This account was already mid-pass, so this pass left it alone.
    ///
    /// Not an error and not a success: nothing was asked of Google and nothing
    /// was written. It is the honest answer to a second "Sync now" pressed
    /// while the first one is still talking, and it is what stops two passes
    /// replaying the same history window against the same watermark.
    #[serde(default)]
    pub skipped: bool,
}

impl AccountOutcome {
    fn new(account_id: i64, email: String) -> Self {
        AccountOutcome {
            account_id,
            email,
            messages_written: 0,
            events_written: 0,
            error: None,
            needs_reauthorization: false,
            cancelled: false,
            skipped: false,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none() && !self.cancelled
    }

    /// Record a failure, keeping the first reason and the strongest verdict.
    ///
    /// Mail and calendar are synced independently against the same credential,
    /// so a dead token fails both. The *first* message is kept, because it is
    /// the one that describes what the pass was doing when it stopped; the
    /// reauthorization flag is kept if either half raised it, because whether
    /// the credential is dead is not a matter of which request noticed.
    fn record(&mut self, error: SyncError) {
        self.needs_reauthorization |= error.is_credential_rejected();
        if self.error.is_none() {
            self.error = Some(error.to_string());
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPass {
    pub started_at: i64,
    pub finished_at: i64,
    pub accounts: Vec<AccountOutcome>,
}

impl SyncPass {
    pub fn messages_written(&self) -> u64 {
        self.accounts.iter().map(|a| a.messages_written).sum()
    }

    pub fn events_written(&self) -> u64 {
        self.accounts.iter().map(|a| a.events_written).sum()
    }

    pub fn account(&self, account_id: i64) -> Option<&AccountOutcome> {
        self.accounts.iter().find(|a| a.account_id == account_id)
    }

    pub fn failures(&self) -> impl Iterator<Item = &AccountOutcome> {
        self.accounts.iter().filter(|a| a.error.is_some())
    }
}

// ===========================================================================
// forcing a pass
// ===========================================================================

/// Which mailboxes a pass covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncScope {
    /// Every account. What the background loop always does, and what "Sync
    /// now" with nothing selected means.
    All,
    /// One account. What the per-account retry beside a failure means — that
    /// button names an address, so it had better only sync that address.
    Account(i64),
}

impl SyncScope {
    fn covers(self, account_id: i64) -> bool {
        match self {
            SyncScope::All => true,
            SyncScope::Account(id) => id == account_id,
        }
    }
}

/// Why a pass is running.
///
/// The two are the *same pass* — see [`SyncEngine::force_sync`] — and this
/// changes exactly one thing, in [`calendar::CalendarSync`]: whether the
/// six-hour-old cached calendar list is trusted. Everything else a forced pass
/// does is byte for byte what the loop does a minute later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The background loop's own tick.
    Scheduled,
    /// Somebody pressed something.
    Forced,
}

impl Trigger {
    pub fn is_forced(self) -> bool {
        matches!(self, Trigger::Forced)
    }
}

/// What a forced pass reports back to whoever asked for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForcedPass {
    /// False when every account asked for was already syncing, so nothing new
    /// was started. Pressing twice is not an error; it is this.
    pub started: bool,
    /// One entry per account the request covered, including the ones that were
    /// already in flight (`skipped`).
    pub accounts: Vec<AccountOutcome>,
}

impl ForcedPass {
    fn from_pass(pass: SyncPass) -> Self {
        // "Started" is about work, not about accounts: a request that covered
        // nothing at all (no accounts yet, or an id that has since been
        // removed) did not fail to start, it had nothing to do.
        let started = pass.accounts.is_empty() || pass.accounts.iter().any(|a| !a.skipped);
        ForcedPass {
            started,
            accounts: pass.accounts,
        }
    }

    pub fn failures(&self) -> impl Iterator<Item = &AccountOutcome> {
        self.accounts.iter().filter(|a| a.error.is_some())
    }
}

/// One account's place in the in-flight set, released on drop.
///
/// The claim is what makes a forced pass safe to run beside the background
/// loop. Two passes replaying `users.history.list` from the same watermark at
/// the same time would both write, both promote a watermark, and race over
/// which one lands last — and two backfills would drain one durable queue from
/// both ends. Rather than locking the whole engine (which would make "Sync
/// now" during a thirteen-minute backfill mean "wait thirteen minutes"), each
/// account is claimed on its own, and whoever gets there second leaves it
/// alone and says so.
struct AccountClaim {
    inner: Arc<Inner>,
    account_id: i64,
}

impl AccountClaim {
    /// Claim `account_id`, or `None` when a pass already holds it.
    fn acquire(inner: &Arc<Inner>, account_id: i64) -> Option<Self> {
        let mut set = inner
            .in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !set.insert(account_id) {
            return None;
        }
        Some(AccountClaim {
            inner: Arc::clone(inner),
            account_id,
        })
    }
}

impl Drop for AccountClaim {
    fn drop(&mut self) {
        self.inner
            .in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.account_id);
    }
}

// ===========================================================================
// the engine
// ===========================================================================

struct Inner {
    db: Db,
    clients: Arc<dyn ClientFactory>,
    config: Arc<SyncConfig>,
    cancel: CancelToken,
    status: StatusSink,
    /// Nudge the loop into an immediate pass.
    wake: Notify,
    /// Global in-flight request bound, shared by every account across every
    /// pass so it actually bounds anything.
    limiter: Arc<Semaphore>,
    /// The live value of [`SyncConfig::poll_interval`], as milliseconds.
    ///
    /// Seeded from the config and then owned by whoever changes the preference.
    /// It is an atomic rather than a field on the `Arc<SyncConfig>` because the
    /// loop reads it between passes on a task nobody holds a handle to — there
    /// is no lock to take and nothing to wake. A change lands on the next tick,
    /// which is the worst case one old interval away.
    poll_interval_ms: AtomicU64,
    /// Accounts with a pass in flight, whichever path started it. See
    /// [`AccountClaim`].
    in_flight: Mutex<HashSet<i64>>,
}

impl Inner {
    fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms.load(Ordering::Relaxed))
    }
}

/// Start it, read its status, stop it. This is the whole surface the Tauri
/// layer needs.
pub struct SyncEngine {
    inner: Arc<Inner>,
    loop_handle: Mutex<Option<JoinHandle<()>>>,
}

impl SyncEngine {
    pub fn new(
        db: Db,
        clients: Arc<dyn ClientFactory>,
        config: SyncConfig,
    ) -> Result<Self, SyncError> {
        db.write_background(sync_queries::ensure_schema)?;
        let request_concurrency = config.request_concurrency.max(1);
        let poll_interval_ms = AtomicU64::new(config.poll_interval.as_millis() as u64);
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                clients,
                config: Arc::new(config),
                cancel: CancelToken::new(),
                status: StatusSink::new(),
                wake: Notify::new(),
                limiter: Arc::new(Semaphore::new(request_concurrency)),
                poll_interval_ms,
                in_flight: Mutex::new(HashSet::new()),
            }),
            loop_handle: Mutex::new(None),
        })
    }

    /// Subscribe to progress. `Receiver::borrow()` is a synchronous read of the
    /// latest value, so a Tauri command can answer without awaiting; `changed()`
    /// drives a push feed if the UI prefers one.
    pub fn status(&self) -> tokio::sync::watch::Receiver<SyncStatus> {
        self.inner.status.subscribe()
    }

    /// The current picture, copied.
    pub fn status_snapshot(&self) -> SyncStatus {
        self.inner.status.snapshot()
    }

    /// Forget that an address needed authorizing, without waiting for a pass.
    ///
    /// What a completed sign-in calls, so the row and the status bar clear
    /// together. The next pass either confirms it by succeeding or sets the flag
    /// again, so an optimistic clear costs at most one interval of quiet.
    pub fn clear_reauthorization(&self, email: &str) {
        self.inner.status.clear_reauthorization(email);
    }

    /// The token every task in this engine watches. Handy for wiring an app
    /// shutdown hook that must also stop other work.
    pub fn cancel_token(&self) -> CancelToken {
        self.inner.cancel.clone()
    }

    pub fn config(&self) -> &SyncConfig {
        &self.inner.config
    }

    /// Spawn the background loop. Idempotent — calling it twice does not start
    /// a second loop.
    pub fn start(&self) {
        let mut slot = self
            .loop_handle
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        *slot = Some(tokio::spawn(run_loop(inner)));
    }

    /// Ask the running loop to start a pass now rather than at the next tick.
    pub fn sync_now(&self) {
        self.inner.wake.notify_one();
    }

    /// The gap the loop is currently sleeping for between passes.
    pub fn poll_interval(&self) -> Duration {
        self.inner.poll_interval()
    }

    /// Change that gap without restarting anything.
    ///
    /// This is what the sync-interval preference writes through. It takes effect
    /// on the next tick rather than interrupting the sleep in progress: waking
    /// the loop would start a *pass*, and "I made syncing less frequent" should
    /// not put a request on the wire.
    pub fn set_poll_interval(&self, interval: Duration) {
        self.inner
            .poll_interval_ms
            .store(interval.as_millis().max(1) as u64, Ordering::Relaxed);
    }

    /// Run exactly one pass over every account, inline. This is what the tests
    /// drive.
    pub async fn sync_once(&self) -> SyncPass {
        pass(Arc::clone(&self.inner), SyncScope::All, Trigger::Scheduled).await
    }

    /// Go and look now.
    ///
    /// # What "force" means here, and what it deliberately does not
    ///
    /// It is the ordinary pass, run this instant, awaited so the caller can
    /// say what happened. It is **not** a rebuild: the watermark is left
    /// exactly where it is.
    ///
    /// That is the whole decision, and it is not a compromise. The incremental
    /// pass is complete by construction — `users.history.list` replayed from
    /// the stored `historyId` returns *every* change since it, and a calendar
    /// `syncToken` sweep returns every event that moved since the token — so
    /// there is nothing a from-scratch rebuild would find that running the
    /// normal pass now would miss. What a rebuild would add is a 365-day
    /// backfill: tens of thousands of `messages.get` calls and, measured
    /// against the owner's mailbox, about thirteen minutes. Pressing a key
    /// because a message you sent from your phone thirty seconds ago has not
    /// appeared yet, and getting a thirteen-minute re-download of the year, is
    /// the wrong trade in every direction.
    ///
    /// The case for the stronger thing does exist, and it is already built and
    /// already automatic: when Gmail says the watermark has aged out of its
    /// retention window, [`mail::MailSync::run_messages`] throws it away and
    /// rebuilds, and a 410 on a calendar token re-lists the window. Those are
    /// repairs for a watermark that is *wrong*, and they are not something to
    /// put behind a key — a person cannot tell from the outside whether that is
    /// the situation, and offering the choice would only mean offering the
    /// expensive answer to a question that already answers itself.
    ///
    /// The one place a forced pass does more than the scheduled one is the
    /// calendar list, where [`calendar::CALENDAR_LIST_MAX_AGE_MS`] normally
    /// lets a six-hour-old cached list stand. "Go and look now" ought to notice
    /// a calendar shared with you this morning, and that costs one request.
    ///
    /// # Pressing it twice
    ///
    /// Each account is claimed for the duration of its sync ([`AccountClaim`]),
    /// so a second request — or the background loop arriving mid-force —
    /// finds the account taken, leaves it alone, and reports it `skipped`.
    /// Nothing queues, nothing doubles, and no two tasks ever replay history
    /// against the same watermark.
    pub async fn force_sync(&self, scope: SyncScope) -> ForcedPass {
        ForcedPass::from_pass(pass(Arc::clone(&self.inner), scope, Trigger::Forced).await)
    }

    /// Stop promptly and wait for the loop to actually be gone.
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        self.inner.wake.notify_waiters();
        let handle = self
            .loop_handle
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        self.inner.status.set_running(false);
    }
}

/// Dropping the engine cancels it and aborts the loop, so a forgotten
/// `shutdown()` cannot leave work running against a database that is closing.
impl Drop for SyncEngine {
    fn drop(&mut self) {
        self.inner.cancel.cancel();
        if let Some(handle) = self
            .loop_handle
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            handle.abort();
        }
    }
}

async fn run_loop(inner: Arc<Inner>) {
    inner.status.set_running(true);
    loop {
        if inner.cancel.is_cancelled() {
            break;
        }
        pass(Arc::clone(&inner), SyncScope::All, Trigger::Scheduled).await;
        if inner.cancel.is_cancelled() {
            break;
        }
        checkpoint(&inner.db);
        tokio::select! {
            biased;
            () = inner.cancel.cancelled() => break,
            () = inner.wake.notified() => {}
            () = tokio::time::sleep(inner.poll_interval()) => {}
        }
    }
    inner.status.set_running(false);
}

/// How large the write-ahead log is allowed to get before the sync loop stops
/// to fold it back into the database file.
///
/// The automatic checkpoint cannot be relied on to do this. It is passive, so
/// it abandons the attempt whenever a reader is using the log, and this app's
/// reader pool answers the UI continuously; measured against a generated store
/// of the owner's shape with the pool busy, the log grew linearly to 139 MB
/// over 3,200 messages written and never once fell. The owner's had reached
/// 814 MB, about a third of his store's footprint on disk.
///
/// 32 MB is the size at which truncating costs about 15 ms — small enough to
/// happen inside a sync pass without a user noticing, and small enough that the
/// log stops being a meaningful share of the disk.
const WAL_CHECKPOINT_OVER_BYTES: u64 = 32 * 1024 * 1024;

/// Fold the log back in, if it has grown enough to be worth the stall.
///
/// Called between passes, and from the backfill writer between batches: a
/// backfill is a single pass that runs for hours, so waiting for the end of one
/// would be waiting for the end of the only thing that fills the log.
///
/// Synchronous, on whichever task calls it. Under the threshold it is one
/// `stat`; over it, it is tens of milliseconds of SQLite — which is the same
/// kind of call, on the same thread, as the transactions this loop already
/// commits, so there is nothing to hand to a blocking pool. A failure is not
/// worth reporting: the log is a little longer and there is another gap in a
/// minute.
pub(crate) fn checkpoint(db: &Db) {
    let _ = db.checkpoint_if_large(WAL_CHECKPOINT_OVER_BYTES);
}

/// One pass over the accounts in `scope`. Accounts run concurrently and
/// independently; a failure — or a panic — in one is contained to its own
/// outcome.
async fn pass(inner: Arc<Inner>, scope: SyncScope, trigger: Trigger) -> SyncPass {
    let started_at = now_ms();

    // A store that will not answer is not a reason to spin: report an empty
    // pass and let the next tick try again.
    let accounts: Vec<Account> = inner
        .db
        .read(queries::list_accounts)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| scope.covers(a.id))
        .collect();

    // Claim before announcing anything.
    //
    // The order matters: a pass that turns out to have no work — every account
    // it covers is already syncing — must not touch the pass timestamps, or a
    // second "Sync now" pressed during a backfill would stamp the backfill as
    // finished while it is still running.
    let mut claimed = Vec::new();
    let mut skipped = Vec::new();
    for account in accounts {
        match AccountClaim::acquire(&inner, account.id) {
            Some(claim) => claimed.push((account, claim)),
            None => {
                let mut outcome = AccountOutcome::new(account.id, account.email);
                outcome.skipped = true;
                skipped.push(outcome);
            }
        }
    }
    if claimed.is_empty() && !skipped.is_empty() {
        skipped.sort_by_key(|o| o.account_id);
        return SyncPass {
            started_at,
            finished_at: now_ms(),
            accounts: skipped,
        };
    }

    inner.status.begin_pass();

    let gate = Arc::new(Semaphore::new(inner.config.account_concurrency.max(1)));
    let mut set: JoinSet<AccountOutcome> = JoinSet::new();
    for (account, claim) in claimed {
        let inner = Arc::clone(&inner);
        let gate = Arc::clone(&gate);
        set.spawn(async move {
            // Held for the whole account task, released when it returns —
            // including on a panic, because the guard unwinds with it.
            let _claim = claim;
            let _permit = gate.acquire_owned().await;
            sync_account(inner, account, trigger).await
        });
    }

    let mut outcomes = skipped;
    while let Some(joined) = set.join_next().await {
        // A panicked account task is a bug, but it must not abort the pass for
        // the other four mailboxes.
        if let Ok(outcome) = joined {
            outcomes.push(outcome);
        }
    }
    outcomes.sort_by_key(|o| o.account_id);

    inner.status.end_pass();
    SyncPass {
        started_at,
        finished_at: now_ms(),
        accounts: outcomes,
    }
}

async fn sync_account(inner: Arc<Inner>, account: Account, trigger: Trigger) -> AccountOutcome {
    let report = inner.status.account(account.id, &account.email);
    report.begin_pass();

    let mut outcome = AccountOutcome::new(account.id, account.email.clone());

    if inner.cancel.is_cancelled() {
        report.cancelled();
        outcome.cancelled = true;
        return outcome;
    }

    if inner.config.sync_mail {
        match inner.clients.gmail(&account) {
            Ok(gmail) => {
                let sync = mail::MailSync {
                    db: inner.db.clone(),
                    gmail,
                    account_id: account.id,
                    config: Arc::clone(&inner.config),
                    cancel: inner.cancel.clone(),
                    report: report.clone(),
                    limiter: Arc::clone(&inner.limiter),
                };
                match sync.run().await {
                    Ok(n) => outcome.messages_written = n,
                    Err(SyncError::Cancelled) => outcome.cancelled = true,
                    Err(e) => outcome.record(e),
                }
            }
            Err(e) => outcome.record(e),
        }
    }

    // Calendar runs even when mail failed: a revoked Gmail scope should not
    // freeze the week grid, and vice versa.
    if inner.config.sync_calendar && !outcome.cancelled {
        match inner.clients.calendar(&account) {
            Ok(calendar) => {
                let sync = calendar::CalendarSync {
                    db: inner.db.clone(),
                    calendar,
                    account_id: account.id,
                    config: Arc::clone(&inner.config),
                    cancel: inner.cancel.clone(),
                    report: report.clone(),
                    // The one difference a forced pass makes. See
                    // `SyncEngine::force_sync`.
                    refresh_calendar_list: trigger.is_forced(),
                };
                match sync.run().await {
                    Ok(n) => outcome.events_written = n,
                    Err(SyncError::Cancelled) => outcome.cancelled = true,
                    Err(e) => outcome.record(e),
                }
            }
            Err(e) => outcome.record(e),
        }
    }

    match (&outcome.error, outcome.cancelled) {
        // A dead credential and an ordinary failure are both failures, and only
        // one of them has a recovery the owner can perform. Splitting them here
        // is what makes "Sign in again" appear beside the account it belongs to
        // rather than "Sync failed" appearing in the corner of the window.
        (Some(error), _) if outcome.needs_reauthorization => {
            report.credential_rejected(error.clone())
        }
        (Some(error), _) => report.failed(error.clone()),
        (None, true) => report.cancelled(),
        (None, false) => report.succeeded(),
    }
    outcome
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sum these constants exist to make visible, spelled out. Change a
    /// quota number and this is the test that tells you what it did.
    #[test]
    fn the_backfill_budget_is_the_published_quota_divided_by_the_published_cost() {
        assert_eq!(GMAIL_QUOTA_UNITS_PER_USER_PER_MINUTE, 6_000);
        assert_eq!(GMAIL_MESSAGES_GET_UNITS, 20);
        assert_eq!(backfill_fetches_per_minute(), 240);
        assert_eq!(default_backfill_fetch_concurrency(), 2);
    }

    /// The property, independent of today's numbers: the backfill's own
    /// spending is inside the share it was given, and the share is inside the
    /// quota.
    #[test]
    fn the_backfill_never_budgets_more_than_its_share_of_the_quota() {
        assert!(BACKFILL_QUOTA_SHARE_PERCENT <= 100);
        let spent = backfill_fetches_per_minute() * GMAIL_MESSAGES_GET_UNITS;
        assert!(
            spent <= GMAIL_QUOTA_UNITS_PER_USER_PER_MINUTE / 100 * BACKFILL_QUOTA_SHARE_PERCENT,
            "backfill budgets {spent} units a minute, above its \
             {BACKFILL_QUOTA_SHARE_PERCENT}% share of \
             {GMAIL_QUOTA_UNITS_PER_USER_PER_MINUTE}"
        );
        assert!(spent < GMAIL_QUOTA_UNITS_PER_USER_PER_MINUTE);
    }

    /// The width is the *smallest* one that reaches the budgeted rate at the
    /// measured round trip. One narrower would leave quota unspent; one wider
    /// buys nothing but 429s. This is what pins the rounding direction, which
    /// is the part of the sum a future edit is most likely to get wrong.
    #[test]
    fn the_width_is_the_narrowest_that_spends_the_budget() {
        let width = default_backfill_fetch_concurrency() as u64;
        let round_trip = BACKFILL_ROUND_TRIP_MS as u64;
        let budget = backfill_fetches_per_minute() as u64;

        let reached = width * 60_000 / round_trip;
        assert!(
            reached >= budget,
            "{width} in flight over {round_trip} ms reaches {reached} fetches a \
             minute, short of the {budget} the quota allows"
        );
        let narrower = width.saturating_sub(1) * 60_000 / round_trip;
        assert!(
            narrower < budget,
            "{} in flight already reaches {narrower} fetches a minute, so \
             {width} is wider than the quota justifies",
            width - 1
        );
    }

    /// The default config uses the derived number rather than a copy of it, and
    /// the global bound does not quietly override it.
    #[test]
    fn the_default_config_carries_the_derived_width() {
        let config = SyncConfig::default();
        assert_eq!(
            config.backfill_fetch_concurrency,
            default_backfill_fetch_concurrency()
        );
        assert!(config.backfill_fetch_concurrency >= 1);
        assert!(config.backfill_fetch_concurrency <= config.request_concurrency);
        // Five accounts at full width still has to fit the stampede guard, or
        // the width in the config is not the width on the wire.
        assert!(
            config.account_concurrency * config.backfill_fetch_concurrency
                <= config.request_concurrency
        );
    }
}
