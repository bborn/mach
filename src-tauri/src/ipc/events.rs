//! Push, not poll.
//!
//! The UI must never sit on a timer asking whether anything changed — that is
//! the same "wait for an answer" the whole design exists to avoid, just aimed at
//! the backend instead of at Google. Four events carry everything the shell
//! needs:
//!
//! | event | when | payload |
//! |---|---|---|
//! | `sync-status` | the engine's watch channel changed | [`SyncStatusPayload`] |
//! | `threads-changed` | a pass or a command wrote threads | `null` |
//! | `wake-failed` | Google refused a snooze wake | [`WakeFailedPayload`] |
//! | `send-failed` | a queued message stopped trying | [`SendFailedPayload`] |
//!
//! `wake-failed` exists because a wake has no gesture behind it. Every other
//! write in the app was asked for a moment ago and its refusal lands on the
//! status line of the person who asked; a sweep runs on a tick, so a refusal
//! with nowhere to go would be exactly the silent failure this project has paid
//! for before. The conversation stays snoozed and the next tick retries it, and
//! this says so once.
//!
//! `send-failed` is the same argument about a different tick. A message leaves
//! the outbox when its undo window lapses, which can be ten seconds after `⌘⏎`
//! or the following morning, and the flush that carries it runs from the CLI
//! and from the agent as well as from the window. A refusal on any of those
//! paths had nowhere to land: four of them accumulated in the owner's store
//! over eighteen days, the newest a business reply, and nothing on screen ever
//! mentioned one. This is how the window hears. The durable half is the queue
//! itself — see [`FailedSend`](crate::ipc::compose::engine::FailedSend).
//!
//! `threads-changed` deliberately carries nothing. The list is keyset-paginated
//! and the reading pane is a point read, so "something changed, re-read what you
//! are showing" is both sufficient and immune to the ordering problems a diff
//! payload would bring.
//!
//! # Why the bridge derives `threads-changed` from status
//!
//! The sync engine has no completion callback — by design, since its only
//! observable is the `watch` channel. That channel already carries what a write
//! looks like: `messagesWritten` climbing within a pass, and
//! `lastPassFinishedAt` moving at its end. Watching those two is enough to
//! notice every write without the engine growing an API for it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::snooze::WakeReport;
use crate::sync::SyncEngine;

use super::state::AppState;
use super::types::SyncStatusPayload;

pub const SYNC_STATUS_EVENT: &str = "sync-status";
pub const THREADS_CHANGED_EVENT: &str = "threads-changed";
pub const WAKE_FAILED_EVENT: &str = "wake-failed";
pub const SEND_FAILED_EVENT: &str = "send-failed";

/// One refused wake, as the frontend reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeFailedPayload {
    /// The conversations that are still snoozed.
    pub thread_ids: Vec<i64>,
    /// Google's reason, verbatim.
    pub message: String,
    /// Whether the sweep that follows could plausibly succeed.
    pub retriable: bool,
}

/// One message that stopped trying, as the frontend reads it.
///
/// Deliberately thin. The window's own copy of the queue is the durable list —
/// it re-reads `compose_outbox` and gets the recipients and the words with it —
/// so this carries only enough to name the message in a status line, and to
/// stop the surface having to guess whether it needs to re-read at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendFailedPayload {
    /// The `compose_outbox` row. What a "show me" gesture would address.
    pub id: String,
    pub subject: String,
    /// Google's reason, verbatim.
    pub message: String,
}

/// The window's ear on the outbox. See
/// [`FailedSends`](crate::ipc::compose::engine::FailedSends).
///
/// Generic over the runtime so the CLI door, which is generic, can build one —
/// and so a mock-runtime test can drive it without a real webview.
pub struct SendFailures<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> SendFailures<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        SendFailures { app }
    }
}

impl<R: Runtime> crate::ipc::compose::engine::FailedSends for SendFailures<R> {
    fn send_failed(&self, entry: &crate::ipc::compose::engine::OutboxEntry, error: &str) {
        // Best-effort, like every other emit here: no window is not a reason to
        // fail a send that has already failed.
        let _ = self.app.emit(
            SEND_FAILED_EVENT,
            SendFailedPayload {
                id: entry.id.clone(),
                subject: entry.subject.clone(),
                message: error.to_string(),
            },
        );
    }
}

/// Tell the UI the thread list is stale. Best-effort: a failed emit means the
/// window is gone, which is not something a command should fail for.
pub fn emit_threads_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(THREADS_CHANGED_EVENT, ());
}

pub fn emit_sync_status<R: Runtime>(app: &AppHandle<R>, status: &SyncStatusPayload) {
    let _ = app.emit(SYNC_STATUS_EVENT, status);
}

/// Push one sweep's outcome to the window.
///
/// A conversation that woke says nothing: it is simply back at the top of the
/// inbox, which is what the owner asked for when they snoozed it and what Gmail
/// does. `threads-changed` is enough to put it there. A conversation that could
/// not be woken says so, once.
pub fn emit_wake_report<R: Runtime>(app: &AppHandle<R>, report: &WakeReport) {
    if !report.woken.is_empty() {
        emit_threads_changed(app);
    }
    for failure in &report.failed {
        let _ = app.emit(
            WAKE_FAILED_EVENT,
            WakeFailedPayload {
                thread_ids: failure.ids.clone(),
                message: failure.message.clone(),
                retriable: failure.retriable,
            },
        );
    }
}

/// Start the sync loop (if there is anything to sync) and forward its progress
/// to the webview for as long as the app lives.
pub async fn run<R: Runtime>(app: AppHandle<R>, sync: Arc<SyncEngine>, start: bool) {
    if start {
        sync.start();
    }

    let mut rx = sync.status();
    let mut last_written: i64 = 0;
    let mut last_finished: Option<i64> = None;

    loop {
        // Scoped so the `State` borrow is released before the await below —
        // holding it across a suspension point would pin the app handle.
        let payload = {
            let state = app.state::<AppState>();
            state.status_payload()
        };

        let written: i64 = payload.accounts.iter().map(|a| a.messages_written).sum();
        let finished = payload.last_pass_finished_at;

        emit_sync_status(&app, &payload);

        // Counters reset at the top of each pass, so a climb inside a pass and
        // a new finish timestamp between passes together cover every write.
        if written > last_written || (finished.is_some() && finished != last_finished) {
            emit_threads_changed(&app);
        }
        last_written = written;
        last_finished = finished;

        // The sender lives as long as the engine; an error here means shutdown.
        if rx.changed().await.is_err() {
            break;
        }
    }
}
