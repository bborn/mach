//! Cooperative cancellation.
//!
//! The sync loop must stop when the window closes, and it must stop *promptly*
//! — a 12-month backfill left running past shutdown would keep writing into a
//! database the app is about to close, and `tokio`'s runtime drop would then
//! block on it.
//!
//! This is a `watch` channel rather than an `AtomicBool` + `Notify` because
//! `Notify::notify_waiters` only wakes tasks that have already registered: a
//! task that checks the flag, sees `false`, and is cancelled before it polls
//! `notified()` would sleep forever. `watch` carries the value itself, so
//! `borrow_and_update` closes that race by construction.

use std::sync::Arc;

use tokio::sync::watch;

/// A clonable handle to one cancellation signal. All clones share it.
#[derive(Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
}

impl CancelToken {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx: Arc::new(tx) }
    }

    /// Signal every holder. Idempotent.
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolves as soon as the token is cancelled, immediately if it already is.
    ///
    /// Cancel-safe: dropping the future mid-`select!` loses nothing, because the
    /// state lives in the channel rather than in the future.
    pub async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                // The sender is owned by this handle, so it cannot have been
                // dropped while we hold `self`. Park rather than claim a
                // cancellation that never happened.
                std::future::pending::<()>().await;
            }
        }
    }

    /// `Err(())` if cancelled, for use with `?` at a checkpoint.
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Marker returned by [`CancelToken::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;
