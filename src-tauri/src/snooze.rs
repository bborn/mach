//! Waking snoozed conversations.
//!
//! [`commands::mail`](crate::commands::mail) explains how a snooze is stored: a
//! real Gmail label while INBOX is removed, plus a `snoozed_threads` row holding
//! the wake time and the label set the thread was snoozed *from*. This module is
//! the other half — the clock that reads those rows back and un-snoozes the ones
//! whose time has come.
//!
//! # Why a stored row and a sweep rather than a timer
//!
//! A timer only fires while the process that armed it is alive, and this is a
//! desktop app that is closed most nights. The wake time is a row on disk, so a
//! sweep at launch finds every snooze that came due while Mach was shut and
//! wakes it then. Nothing is skipped for having been missed; a thread snoozed to
//! 08:00 on a laptop that opens at 09:30 comes back at 09:30.
//!
//! Between launches the same sweep runs on a fixed tick. The gap is a constant
//! rather than the sync interval preference: the sync gap is about how often to
//! ask Google for news, and lengthening it should not make a snooze late.
//!
//! A closed lid is the same case as a closed app. The tick is a monotonic
//! timer, which does not advance while macOS is asleep, so a machine that
//! sleeps at 23:00 and opens at 09:30 resumes the tick it was part-way through
//! and sweeps within a minute. What decides whether a row is due is `wake_at`
//! against the wall clock, and that comparison does not care how long the gap
//! was.
//!
//! # Waking is a command
//!
//! The sweep does not write labels itself. It reads the due rows and dispatches
//! one [`Command::Unsnooze`], which is the *exact* inverse of the
//! [`Command::Snooze`](crate::commands::Command::Snooze) that made them: the
//! stored row names the label set the thread carried before it was snoozed, and
//! un-snooze restores that set. So a thread snoozed out of the inbox comes back
//! to the inbox with `Receipts` and `Family` still on it, a thread snoozed while
//! already archived does not gain INBOX it never had, and the `Mach/Snoozed`
//! label comes off because it is not in the set the thread was snoozed from.
//!
//! Going through the command layer also buys the sweep everything that layer
//! guarantees: the local write commits first so the list repaints immediately,
//! the Gmail call follows, and a refusal rolls the thread back *including its
//! snooze row*. A wake Google refuses therefore leaves the conversation snoozed
//! and still due, which is what makes the next tick a retry.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::commands::{Command, CommandDispatcher, CommandError, CommandFailure};
use crate::db::command_queries;
use crate::sync::CancelToken;

/// How often the sweep runs while the app is open.
///
/// A minute is the resolution of the wake times the picker offers — every option
/// in `src/lib/snooze.ts` lands on a whole minute — so this is as punctual as
/// the promise the UI made.
pub const DEFAULT_WAKE_INTERVAL: Duration = Duration::from_secs(60);

/// What one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WakeReport {
    /// Every thread whose wake time had arrived.
    pub due: Vec<i64>,
    /// Those that are now back where they were snoozed from.
    pub woken: Vec<i64>,
    /// Those Google refused, already rolled back and still snoozed.
    pub failed: Vec<CommandFailure>,
}

impl WakeReport {
    pub fn is_empty(&self) -> bool {
        self.due.is_empty()
    }
}

/// Wake everything due at `now_ms`.
///
/// One command for the whole set: the command layer groups threads by
/// `(account, label delta)` and batches each group, so twenty snoozes coming due
/// together are a handful of requests rather than twenty. Grouping also gives
/// per-thread failure granularity for free — a chunk Google refuses is rolled
/// back and named in [`WakeReport::failed`] while the rest stay woken.
///
/// `Err` means the sweep never ran and nothing was written.
pub async fn wake_due(
    dispatcher: &CommandDispatcher,
    now_ms: i64,
) -> Result<WakeReport, CommandError> {
    let due = dispatcher
        .db()
        .read(|conn| command_queries::due_snoozes(conn, now_ms))?;
    if due.is_empty() {
        return Ok(WakeReport::default());
    }

    let thread_ids: Vec<i64> = due.iter().map(|row| row.thread_id).collect();
    let result = dispatcher
        .execute(Command::Unsnooze {
            thread_ids: thread_ids.clone(),
        })
        .await?;

    Ok(WakeReport {
        due: thread_ids,
        woken: result.applied,
        failed: result.failed,
    })
}

/// Sweep now, then every `interval` until cancelled.
///
/// The first sweep is immediate and unconditional, which is the launch case:
/// whatever came due while the app was closed wakes as soon as there is a
/// process to wake it.
///
/// `observe` is called only when a sweep did something worth telling the app
/// about — threads woken, or a failure that has not already been reported. A
/// wake that keeps being refused is retried on every tick, because the failure
/// may well be transient, but it is only *said* once: an error message every
/// minute for the same conversation is noise the owner cannot act on.
pub async fn run<F>(
    dispatcher: Arc<CommandDispatcher>,
    cancel: CancelToken,
    interval: Duration,
    mut observe: F,
) where
    F: FnMut(WakeReport) + Send + 'static,
{
    let mut reported: HashSet<i64> = HashSet::new();
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match wake_due(&dispatcher, now_ms()).await {
            Ok(mut report) => {
                for id in &report.woken {
                    reported.remove(id);
                }
                report.failed.retain(|failure| {
                    let fresh = failure.ids.iter().any(|id| !reported.contains(id));
                    reported.extend(failure.ids.iter().copied());
                    fresh
                });
                if !report.woken.is_empty() || !report.failed.is_empty() {
                    observe(report);
                }
            }
            // A sweep that could not run at all — a store that will not answer,
            // an account row that has gone — is not a reason to stop sweeping.
            // Nothing was written, and the rows are still there for next time.
            Err(error) => eprintln!("could not wake snoozed conversations: {error}"),
        }

        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(interval) => {}
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
