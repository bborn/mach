//! Push, not poll.
//!
//! The UI must never sit on a timer asking whether anything changed — that is
//! the same "wait for an answer" the whole design exists to avoid, just aimed at
//! the backend instead of at Google. Two events carry everything the shell
//! needs:
//!
//! | event | when | payload |
//! |---|---|---|
//! | `sync-status` | the engine's watch channel changed | [`SyncStatusPayload`] |
//! | `threads-changed` | a pass or a command wrote threads | `null` |
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

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::sync::SyncEngine;

use super::state::AppState;
use super::types::SyncStatusPayload;

pub const SYNC_STATUS_EVENT: &str = "sync-status";
pub const THREADS_CHANGED_EVENT: &str = "threads-changed";

/// Tell the UI the thread list is stale. Best-effort: a failed emit means the
/// window is gone, which is not something a command should fail for.
pub fn emit_threads_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(THREADS_CHANGED_EVENT, ());
}

pub fn emit_sync_status<R: Runtime>(app: &AppHandle<R>, status: &SyncStatusPayload) {
    let _ = app.emit(SYNC_STATUS_EVENT, status);
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
