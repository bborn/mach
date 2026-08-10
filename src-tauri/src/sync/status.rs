//! Observable sync progress.
//!
//! # Why a `watch` channel rather than an event stream
//!
//! The UI's sync indicator only ever asks one question: *what is happening right
//! now?* It never needs the history of how it got there. A `broadcast` or `mpsc`
//! of progress events would make the consumer responsible for draining a queue
//! that the backfill fills tens of thousands of times, and a slow UI would
//! either lag or drop messages — and "dropped the last message" is exactly the
//! event you cannot afford to lose, because it is the one that says *done*.
//!
//! `tokio::sync::watch` keeps only the latest value, which is precisely the
//! semantics wanted: coalescing is a feature, the final state is always
//! delivered, and there is no unbounded buffer. It also reads *synchronously*
//! (`borrow()`), so a Tauri command can answer `sync_status()` without awaiting
//! anything — the same invariant the whole app is built on. `changed()` is there
//! for the push path when the UI would rather be told than poll.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Where one account is in its sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncPhase {
    /// Never synced in this process.
    Idle,
    /// Fetching the label list.
    Labels,
    /// Pulling the last 12 months. `backfill_done`/`backfill_total` are live.
    Backfill,
    /// Replaying `users.history.list` from the watermark.
    Incremental,
    /// `events.list`, initial window or incremental.
    Calendar,
    /// This pass finished cleanly.
    Done,
    /// This pass stopped on an error; see `last_error`.
    Failed,
    /// Shut down mid-pass. Whatever was committed is committed.
    Cancelled,
}

impl SyncPhase {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            SyncPhase::Labels | SyncPhase::Backfill | SyncPhase::Incremental | SyncPhase::Calendar
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub account_id: i64,
    pub email: String,
    pub phase: SyncPhase,
    /// Messages enumerated for the backfill; 0 when not backfilling.
    pub backfill_total: i64,
    pub backfill_done: i64,
    /// Messages written to the store during this pass.
    pub messages_written: i64,
    /// Events written during this pass.
    pub events_written: i64,
    /// The most recent failure, cleared when a pass succeeds. A rate-limited or
    /// de-authorised account keeps this set while its four siblings carry on.
    pub last_error: Option<String>,
    /// This account's credential is dead — Google refused the refresh token —
    /// and no further pass can fix it. Set by the pass that saw it, cleared by
    /// the pass that succeeds or by a completed re-authorization.
    ///
    /// Separate from `last_error` because the two answer different questions.
    /// `last_error` is *what happened*, in Google's words, and it is worth
    /// showing whatever the cause. This is *whose problem it is*, and it is the
    /// only thing that decides whether the recovery offered is "Sync now" or
    /// "Sign in again".
    ///
    /// It is not persisted. A dead refresh token is still *present* in the
    /// Keychain, so the startup credential check cannot see it — but the first
    /// pass after launch asks Google and is told again within seconds, which
    /// makes rebuilding this cheaper and more honest than storing it.
    #[serde(default)]
    pub needs_reauthorization: bool,
    pub last_success_at: Option<i64>,
    pub updated_at: i64,
}

impl AccountStatus {
    fn new(account_id: i64, email: String) -> Self {
        Self {
            account_id,
            email,
            phase: SyncPhase::Idle,
            backfill_total: 0,
            backfill_done: 0,
            messages_written: 0,
            events_written: 0,
            last_error: None,
            needs_reauthorization: false,
            last_success_at: None,
            updated_at: now_ms(),
        }
    }

    /// True while the backfill is still working through its queue.
    pub fn is_backfilling(&self) -> bool {
        self.phase == SyncPhase::Backfill
    }
}

/// The whole picture, in one value the UI can render directly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// Whether the background loop is alive.
    pub running: bool,
    pub accounts: Vec<AccountStatus>,
    pub last_pass_started_at: Option<i64>,
    pub last_pass_finished_at: Option<i64>,
}

impl SyncStatus {
    pub fn account(&self, account_id: i64) -> Option<&AccountStatus> {
        self.accounts.iter().find(|a| a.account_id == account_id)
    }

    /// True while any account is mid-pass — what drives the spinner.
    pub fn is_syncing(&self) -> bool {
        self.accounts.iter().any(|a| a.phase.is_active())
    }

    pub fn errors(&self) -> impl Iterator<Item = (&str, &str)> {
        self.accounts
            .iter()
            .filter_map(|a| a.last_error.as_deref().map(|e| (a.email.as_str(), e)))
    }

    /// Addresses whose credential Google has refused during this run.
    pub fn needs_reauthorization(&self) -> impl Iterator<Item = &str> {
        self.accounts
            .iter()
            .filter(|a| a.needs_reauthorization)
            .map(|a| a.email.as_str())
    }
}

/// Write side of the status channel. Cheap to clone; every account task holds
/// one.
#[derive(Clone)]
pub struct StatusSink {
    tx: Arc<watch::Sender<SyncStatus>>,
}

impl StatusSink {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(SyncStatus::default());
        Self { tx: Arc::new(tx) }
    }

    /// Subscribe for push updates. `borrow()` on the receiver is a synchronous
    /// read of the latest value.
    pub fn subscribe(&self) -> watch::Receiver<SyncStatus> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> SyncStatus {
        self.tx.borrow().clone()
    }

    pub fn set_running(&self, running: bool) {
        self.tx.send_modify(|s| s.running = running);
    }

    pub fn begin_pass(&self) {
        self.tx.send_modify(|s| {
            s.last_pass_started_at = Some(now_ms());
            s.last_pass_finished_at = None;
        });
    }

    pub fn end_pass(&self) {
        self.tx
            .send_modify(|s| s.last_pass_finished_at = Some(now_ms()));
    }

    /// A handle scoped to one account, created (or found) by id.
    pub fn account(&self, account_id: i64, email: &str) -> AccountReporter {
        self.tx.send_modify(|s| match s.account(account_id) {
            Some(_) => {
                if let Some(a) = s.accounts.iter_mut().find(|a| a.account_id == account_id) {
                    a.email = email.to_string();
                }
            }
            None => s
                .accounts
                .push(AccountStatus::new(account_id, email.to_string())),
        });
        AccountReporter {
            sink: self.clone(),
            account_id,
        }
    }

    /// Forget that an address ever needed authorizing.
    ///
    /// Called when a sign-in completes, so the label and the status bar clear in
    /// the same render rather than at the next pass — the property `73bc4af`
    /// established for the Keychain-missing case, held to here as well.
    /// Addressed by email because that is what the authorization knows; the
    /// account id it belongs to is a detail of a row it may just have created.
    pub fn clear_reauthorization(&self, email: &str) {
        self.tx.send_modify(|s| {
            for account in s.accounts.iter_mut().filter(|a| a.email == email) {
                account.needs_reauthorization = false;
                account.last_error = None;
                account.updated_at = now_ms();
            }
        });
    }

    fn update(&self, account_id: i64, f: impl FnOnce(&mut AccountStatus)) {
        self.tx.send_modify(|s| {
            if let Some(a) = s.accounts.iter_mut().find(|a| a.account_id == account_id) {
                f(a);
                a.updated_at = now_ms();
            }
        });
    }
}

impl Default for StatusSink {
    fn default() -> Self {
        Self::new()
    }
}

/// The per-account write handle. Every mutation is a single `send_modify`, so a
/// reader never observes a half-updated struct.
#[derive(Clone)]
pub struct AccountReporter {
    sink: StatusSink,
    account_id: i64,
}

impl AccountReporter {
    pub fn account_id(&self) -> i64 {
        self.account_id
    }

    pub fn phase(&self, phase: SyncPhase) {
        self.sink.update(self.account_id, |a| a.phase = phase);
    }

    /// Reset the per-pass counters. Called at the top of each pass so the UI
    /// shows "this sync", not "since launch".
    pub fn begin_pass(&self) {
        self.sink.update(self.account_id, |a| {
            a.messages_written = 0;
            a.events_written = 0;
            a.backfill_total = 0;
            a.backfill_done = 0;
        });
    }

    pub fn backfill_progress(&self, done: i64, total: i64) {
        self.sink.update(self.account_id, |a| {
            a.backfill_done = done;
            a.backfill_total = total;
        });
    }

    pub fn add_messages(&self, n: i64) {
        self.sink
            .update(self.account_id, |a| a.messages_written += n);
    }

    pub fn add_events(&self, n: i64) {
        self.sink.update(self.account_id, |a| a.events_written += n);
    }

    /// A pass that stopped on something another pass could get past.
    ///
    /// Leaves `needs_reauthorization` alone rather than clearing it: an account
    /// whose credential is dead will also fail to reach the network, be rate
    /// limited, and time out, and none of those are evidence that the
    /// credential came back. Only a success is.
    pub fn failed(&self, message: impl Into<String>) {
        let message = message.into();
        self.sink.update(self.account_id, |a| {
            a.phase = SyncPhase::Failed;
            a.last_error = Some(message);
        });
    }

    /// A pass that stopped because Google refused the stored credential.
    ///
    /// The message is still recorded — it is Google's own text, and it is the
    /// only thing that says whether this was a password change or the seven-day
    /// expiry — but the account is flagged as well, which is what puts "Sign in
    /// again" next to it.
    pub fn credential_rejected(&self, message: impl Into<String>) {
        let message = message.into();
        self.sink.update(self.account_id, |a| {
            a.phase = SyncPhase::Failed;
            a.last_error = Some(message);
            a.needs_reauthorization = true;
        });
    }

    pub fn cancelled(&self) {
        self.sink
            .update(self.account_id, |a| a.phase = SyncPhase::Cancelled);
    }

    pub fn succeeded(&self) {
        self.sink.update(self.account_id, |a| {
            a.phase = SyncPhase::Done;
            a.last_error = None;
            // A pass that reached Google and came back proves the credential is
            // alive, whatever the last one concluded. This is the ordinary way
            // the flag clears after a re-authorization the app did not perform
            // itself — a token minted on the phone, say.
            a.needs_reauthorization = false;
            a.last_success_at = Some(now_ms());
        });
    }
}
