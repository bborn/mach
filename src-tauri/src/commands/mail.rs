//! The mail commands: archive, unarchive, read/unread, star, label, trash,
//! untrash, snooze, unsnooze.
//!
//! # Everything is a label delta
//!
//! Gmail has no "archive" call and no "star" call. It has `messages.modify`,
//! which adds and removes label ids. So this module computes, for each thread,
//! the **target label set** the command implies, diffs it against the set the
//! thread actually has, and turns that diff into one Gmail request.
//!
//! Doing it as a target-and-diff rather than a hard-coded add/remove pair buys
//! three properties that would otherwise each need their own special case:
//!
//!  * **Idempotence** is free. Archiving a thread that is already archived
//!    produces an empty diff, so no request is sent and nothing is written.
//!  * **Undo is exact.** The prior label set is captured before the write, so
//!    the inverse restores what was there — `[INBOX, Receipts, Family]` comes
//!    back as all three, never as a bare `INBOX`.
//!  * **Restore is a command**, not a code path. `Unarchive { restore }` and
//!    `Untrash { restore }` name a target set directly, which is what lets undo
//!    be a value the UI (or the agent) hands back to `execute`.
//!
//! # Order of operations
//!
//! 1. Read the prior state of every thread. (No writes yet — a bad thread id
//!    fails here, having changed nothing.)
//! 2. Write every local change, in **one transaction**, and commit.
//! 3. Only then talk to Google.
//! 4. If a call fails, put the affected threads back exactly as they were.
//!
//! Step 2 before step 3 is the entire point of the command layer: the UI
//! re-renders from SQLite the instant the transaction commits, so archiving 50
//! threads is visually instantaneous regardless of what Gmail is doing.
//!
//! # Batching, and what a partial failure means
//!
//! Threads are grouped by `(account, add-set, remove-set)` — batchModify is
//! per-account and applies one delta to every id — and each group is cut into
//! chunks of at most [`DEFAULT_MAX_BATCH_MESSAGE_IDS`] message ids.
//!
//! Chunking is **thread-wise**: a thread's messages never straddle two chunks.
//! That is what makes the failure unit meaningful. A chunk is all-or-nothing at
//! Google, so when one chunk of five fails:
//!
//!  * the threads in that chunk are rolled back locally, completely — a thread
//!    is never left half-modified;
//!  * the threads in the other four chunks keep their change and appear in
//!    `applied`;
//!  * `ok` is `false` and `failed` names exactly the ids that were reverted;
//!  * `undo` covers **only** the applied threads, so undoing after a partial
//!    failure cannot resurrect a change that never happened.
//!
//! # Conversations holding an unsent draft
//!
//! Saving a draft writes a message row into the conversation it answers, so the
//! thread repaints without waiting for Gmail (see `compose::mirror`).
//! Until the push to Gmail lands, that row is filed under
//! `mach-draft:<draft id>`, an id Mach mints for itself. A reply waiting out its
//! send delay is the same shape, under `mach-outbox:<entry id>`.
//!
//! Neither is a Gmail message id. `messages.batchModify` answers a request
//! containing one with `400 Invalid ids value`, and it rejects the whole
//! request, so a single unsent draft failed the archive of its entire
//! conversation. A command therefore names only the ids Google minted;
//! [`ThreadSnapshot`] separates them from Mach's own.
//!
//! **The draft itself is left alone.** That is what Gmail does: a draft message
//! carries `DRAFT` and never `INBOX`, so removing `INBOX` from the conversation
//! never reached it. The thread leaves the inbox and the draft stays in Drafts,
//! still editable and still sendable. Mach lands in the same state by the same
//! route, because archive only removes `INBOX` from the local label set, so the
//! thread keeps `DRAFT` and stays in the Drafts mailbox. Star, snooze,
//! mark-read and trash work the same way: whatever the conversation's labels
//! become, the draft's own row is untouched.
//!
//! The two alternatives were discarding the draft, and pushing it to Gmail
//! first so that it has an id to modify. Both give archive a side effect it
//! does not have in Gmail, and the second uploads unfinished text as a
//! consequence of a triage keystroke.
//!
//! # Snooze
//!
//! Gmail's API has no snooze. Google's own snooze is implemented with an
//! internal label the API does not return, so it is invisible to us. Mach's
//! representation is therefore:
//!
//!  * a real Gmail user label, `Mach/Snoozed` by default, applied while INBOX
//!    is removed — so the thread is out of the way in Gmail's web UI too, and
//!    visibly *why*;
//!  * a local `snoozed_threads` row holding the wake time and the label set the
//!    thread was snoozed *from*.
//!
//! The label alone would not be enough (no wake time), and the local row alone
//! would not be enough (the thread would still sit in the Gmail inbox on the
//! phone). Both together mean the state is legible from either side.
//!
//! Consequences, including the case where the user snoozes in the Gmail web UI:
//!
//!  * **Gmail-web snooze looks like an archive to Mach.** Google removes INBOX
//!    and applies an internal label the API never returns, so sync sees a
//!    thread leave the inbox and nothing else. Mach shows it as archived, with
//!    no wake badge. Nothing is corrupted; the information simply is not
//!    exposed by the API.
//!  * **Gmail wakes it on its own schedule.** When it does, `history.list`
//!    reports INBOX being added back and the thread reappears at the top of
//!    Mach's stream. The round trip self-heals.
//!  * **A Mach snooze is visible in Gmail** as the `Mach/Snoozed` label, but
//!    Gmail will not wake it — Mach's clock does, from the stored `wake_at`.
//!    That clock is [`crate::snooze`], which sweeps the due rows at launch and
//!    on a tick and dispatches [`Command::Unsnooze`] for each. Because the wake
//!    time is a row and not a timer, a snooze that comes due while Mach is
//!    closed fires at next launch instead of being lost.
//!  * **If the user strips the label in Gmail web**, sync removes it locally
//!    and the stale wake row simply un-snoozes a thread that is already
//!    un-snoozed, which is a no-op. Un-snooze is idempotent by construction.
//!
//! Creating the label is `users.labels.create`, which the Gmail client does not
//! expose yet, so a missing label is a typed
//! [`CommandError::MissingLabel`](super::CommandError::MissingLabel) rather
//! than a silently invented label id.

use crate::db::command_queries::{self as cq, SnoozeRow, ThreadSnapshot};
use crate::db::Db;
use crate::google::gmail::GmailClient;
use crate::google::GoogleError;

use super::drafts::{self, Discarded};
use super::error::{CommandError, CommandFailure};
use super::types::{plural, Command, CommandResult, ThreadLabelState};
use super::CommandDispatcher;

/// Gmail's system labels the command layer names directly.
pub const INBOX: &str = "INBOX";
pub const UNREAD: &str = "UNREAD";
pub const STARRED: &str = "STARRED";
pub const SPAM: &str = "SPAM";
pub const TRASH: &str = "TRASH";
pub const DRAFT: &str = "DRAFT";

/// Gmail's bulk tabs. Mach's Inbox is INBOX minus these — see `mailboxes.ts`.
const BULK_CATEGORIES: [&str; 4] = [
    "CATEGORY_PROMOTIONS",
    "CATEGORY_SOCIAL",
    "CATEGORY_UPDATES",
    "CATEGORY_FORUMS",
];

/// The Gmail user label Mach applies to snoozed threads.
pub const DEFAULT_SNOOZE_LABEL: &str = "Mach/Snoozed";

/// Gmail caps `messages.batchModify` at 1000 ids per call.
pub const DEFAULT_MAX_BATCH_MESSAGE_IDS: usize = 1000;

// ---------------------------------------------------------------------------
// plans
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteOp {
    Modify,
    /// Trash has a dedicated endpoint that is worth using when there is exactly
    /// one message; beyond that it is expressed as a label delta so it can be
    /// batched like everything else.
    Trash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnoozeAction {
    Leave,
    Set { wake_at: i64 },
    Clear,
}

/// One thread's worth of work: where it was, where it is going, and how.
struct ThreadPlan {
    snap: ThreadSnapshot,
    prior_snooze: Option<SnoozeRow>,
    target_labels: Vec<String>,
    target_unread: bool,
    add: Vec<String>,
    remove: Vec<String>,
    op: RemoteOp,
    snooze: SnoozeAction,
}

impl ThreadPlan {
    fn id(&self) -> i64 {
        self.snap.thread_id
    }

    fn labels_changed(&self) -> bool {
        !self.add.is_empty() || !self.remove.is_empty()
    }

    fn snooze_changed(&self) -> bool {
        match &self.snooze {
            SnoozeAction::Leave => false,
            SnoozeAction::Set { wake_at } => {
                self.prior_snooze.as_ref().map(|r| r.wake_at) != Some(*wake_at)
            }
            SnoozeAction::Clear => self.prior_snooze.is_some(),
        }
    }

    /// Whether anything about the local row actually differs.
    fn changed(&self) -> bool {
        self.labels_changed() || self.target_unread != self.snap.is_unread || self.snooze_changed()
    }

    /// Whether Google has to be told. A local-only difference (a snooze row)
    /// does not earn a round trip.
    fn remote_needed(&self) -> bool {
        self.labels_changed()
    }

    fn prior_state(&self) -> ThreadLabelState {
        ThreadLabelState {
            thread_id: self.snap.thread_id,
            label_ids: self.snap.label_ids.clone(),
            is_unread: self.snap.is_unread,
        }
    }
}

// ---------------------------------------------------------------------------
// tiny sorted-set helpers
// ---------------------------------------------------------------------------

fn normalise(mut labels: Vec<String>) -> Vec<String> {
    labels.sort();
    labels.dedup();
    labels
}

fn with(labels: &[String], add: &[&str]) -> Vec<String> {
    let mut out = labels.to_vec();
    for label in add {
        out.push((*label).to_string());
    }
    normalise(out)
}

fn without(labels: &[String], remove: &[&str]) -> Vec<String> {
    normalise(
        labels
            .iter()
            .filter(|l| !remove.contains(&l.as_str()))
            .cloned()
            .collect(),
    )
}

/// Everything in `a` that is not in `b`.
fn difference(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| !b.contains(x)).cloned().collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// planning
// ---------------------------------------------------------------------------

fn restore_state<'a>(
    restore: &'a [ThreadLabelState],
    thread_id: i64,
) -> Option<&'a ThreadLabelState> {
    restore.iter().find(|s| s.thread_id == thread_id)
}

/// Read the prior state of every named thread and work out what each one's
/// target state is. Nothing is written here.
async fn build_plans(
    dispatcher: &CommandDispatcher,
    command: &Command,
    thread_ids: &[i64],
) -> Result<Vec<ThreadPlan>, CommandError> {
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Snapshots first, so an unknown id fails before any write.
    let snapshots = dispatcher.db.read(|conn| {
        let mut out = Vec::with_capacity(thread_ids.len());
        for id in thread_ids {
            out.push((cq::thread_snapshot(conn, *id)?, cq::snooze_row(conn, *id)?));
        }
        Ok(out)
    })?;

    let mut pairs = Vec::with_capacity(snapshots.len());
    for (id, (snap, snooze)) in thread_ids.iter().zip(snapshots) {
        let snap = snap.ok_or(CommandError::UnknownThread { thread_id: *id })?;
        pairs.push((snap, snooze));
    }

    // The snooze label is per-account, so resolve it once per distinct account
    // rather than once per thread.
    let needs_label = matches!(command, Command::Snooze { .. } | Command::Unsnooze { .. });
    let mut snooze_labels: Vec<(i64, Option<String>)> = Vec::new();
    if needs_label {
        let name = dispatcher.snooze_label_name.clone();
        for (snap, _) in &pairs {
            if snooze_labels.iter().any(|(id, _)| *id == snap.account_id) {
                continue;
            }
            let mut label = dispatcher
                .db
                .read(|conn| cq::label_id_by_name(conn, snap.account_id, &name))?;

            // Gmail has no snooze primitive, so Mach represents it with a real
            // user label — the same thing Superhuman and Boomerang do. Refusing
            // to snooze because that label does not exist yet is a dead end the
            // user cannot resolve from inside the app, so create it on demand.
            if label.is_none() {
                let gmail = dispatcher.clients.gmail(snap.account_id)?;
                let created = match gmail.labels_create("me", &name).await {
                    Ok(l) => Some(l),
                    // Already there — Gmail rejects a duplicate name. Someone
                    // made it outside Mach, or two accounts raced; re-reading
                    // is the answer, not an error.
                    Err(GoogleError::InvalidRequest { .. }) => gmail
                        .labels_list("me")
                        .await
                        .map_err(|e| CommandError::Invalid {
                            message: format!("could not list labels: {e}"),
                        })?
                        .into_iter()
                        .find(|l| l.name == name),
                    Err(e) => {
                        return Err(CommandError::Invalid {
                            message: format!("could not create the \"{name}\" label: {e}"),
                        })
                    }
                };
                if let Some(created) = created {
                    dispatcher.db.write(|conn| {
                        crate::db::queries::upsert_label(
                            conn,
                            &crate::db::models::NewLabel {
                                account_id: snap.account_id,
                                gmail_label_id: created.id.clone(),
                                name: created.name.clone(),
                                label_type: crate::db::models::LabelType::User,
                            },
                        )
                    })?;
                    label = Some(created.id);
                }
            }

            snooze_labels.push((snap.account_id, label));
        }
    }
    let label_for = |account_id: i64| -> Option<String> {
        snooze_labels
            .iter()
            .find(|(id, _)| *id == account_id)
            .and_then(|(_, label)| label.clone())
    };

    let mut plans = Vec::with_capacity(pairs.len());
    for (snap, prior_snooze) in pairs {
        let prior = snap.label_ids.clone();
        let (target_labels, target_unread, op, snooze) = match command {
            Command::Archive { .. } => (
                without(&prior, &[INBOX]),
                snap.is_unread,
                RemoteOp::Modify,
                SnoozeAction::Leave,
            ),

            Command::Unarchive { restore, .. } => {
                match restore_state(restore, snap.thread_id) {
                    Some(state) => (
                        normalise(state.label_ids.clone()),
                        state.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                    None => (
                        with(&prior, &[INBOX]),
                        snap.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                }
            }

            Command::MarkRead { read, .. } => {
                let target = if *read {
                    without(&prior, &[UNREAD])
                } else {
                    with(&prior, &[UNREAD])
                };
                (target, !*read, RemoteOp::Modify, SnoozeAction::Leave)
            }

            Command::Star { starred, .. } => {
                let target = if *starred {
                    with(&prior, &[STARRED])
                } else {
                    without(&prior, &[STARRED])
                };
                (
                    target,
                    snap.is_unread,
                    RemoteOp::Modify,
                    SnoozeAction::Leave,
                )
            }

            Command::Label { label_id, add, .. } => {
                let target = if *add {
                    with(&prior, &[label_id.as_str()])
                } else {
                    without(&prior, &[label_id.as_str()])
                };
                (
                    target,
                    snap.is_unread,
                    RemoteOp::Modify,
                    SnoozeAction::Leave,
                )
            }

            Command::MoveToInbox { restore, .. } => {
                match restore_state(restore, snap.thread_id) {
                    Some(state) => (
                        normalise(state.label_ids.clone()),
                        state.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                    None => (
                        without(&with(&prior, &[INBOX]), &BULK_CATEGORIES),
                        snap.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                }
            }

            // Exactly what Gmail's own Report spam sends: SPAM on, INBOX off,
            // in one `batchModify`. Nothing else about the thread is touched —
            // a starred, labelled conversation keeps both — which is what makes
            // the prior state worth capturing rather than reconstructing.
            Command::ReportSpam { .. } => (
                without(&with(&prior, &[SPAM]), &[INBOX]),
                snap.is_unread,
                RemoteOp::Modify,
                SnoozeAction::Leave,
            ),

            Command::NotSpam { restore, .. } => {
                match restore_state(restore, snap.thread_id) {
                    Some(state) => (
                        normalise(state.label_ids.clone()),
                        state.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                    // Dispatched by hand from the Spam mailbox, where "not
                    // spam" means the inbox and there is no prior state to
                    // consult. Undo never reaches this arm: it always carries a
                    // `restore`.
                    None => (
                        without(&with(&prior, &[INBOX]), &[SPAM]),
                        snap.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                }
            }

            Command::Trash { .. } => (
                without(&with(&prior, &[TRASH]), &[INBOX]),
                snap.is_unread,
                RemoteOp::Trash,
                SnoozeAction::Leave,
            ),

            Command::Untrash { restore, .. } => {
                match restore_state(restore, snap.thread_id) {
                    Some(state) => (
                        normalise(state.label_ids.clone()),
                        state.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                    None => (
                        without(&with(&prior, &[INBOX]), &[TRASH]),
                        snap.is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Leave,
                    ),
                }
            }

            Command::Snooze { until, .. } => {
                let label =
                    label_for(snap.account_id).ok_or_else(|| CommandError::MissingLabel {
                        account_id: snap.account_id,
                        label_name: dispatcher.snooze_label_name.clone(),
                    })?;
                (
                    without(&with(&prior, &[label.as_str()]), &[INBOX]),
                    snap.is_unread,
                    RemoteOp::Modify,
                    SnoozeAction::Set { wake_at: *until },
                )
            }

            Command::Unsnooze { .. } => {
                // The stored row is the authority: it knows what the thread was
                // snoozed *from*. Without one (label stripped in Gmail web, or
                // a thread Mach never snoozed) fall back to the obvious guess.
                match &prior_snooze {
                    Some(row) => (
                        normalise(row.prior_label_ids.clone()),
                        row.prior_is_unread,
                        RemoteOp::Modify,
                        SnoozeAction::Clear,
                    ),
                    None => {
                        let target = match label_for(snap.account_id) {
                            Some(label) => without(&with(&prior, &[INBOX]), &[label.as_str()]),
                            None => with(&prior, &[INBOX]),
                        };
                        (
                            target,
                            snap.is_unread,
                            RemoteOp::Modify,
                            SnoozeAction::Clear,
                        )
                    }
                }
            }

            // The calendar half of the vocabulary never reaches this
            // function — `CommandDispatcher::execute` routes it away first —
            // but the match has to be total, and saying so is cheaper than a
            // wildcard that would swallow a future mail command.
            Command::Unsubscribe { .. }
            | Command::Rsvp { .. }
            | Command::CreateEvent { .. }
            | Command::UpdateEvent { .. }
            | Command::DeleteEvent { .. }
            | Command::MoveEvent { .. } => {
                return Err(CommandError::Invalid {
                    message: format!("{} is not a label change", command.kind()),
                })
            }
        };

        let add = difference(&target_labels, &prior);
        let remove = difference(&prior, &target_labels);
        plans.push(ThreadPlan {
            snap,
            prior_snooze,
            target_labels,
            target_unread,
            add,
            remove,
            op,
            snooze,
        });
    }

    Ok(plans)
}

// ---------------------------------------------------------------------------
// local writes
// ---------------------------------------------------------------------------

fn apply_local(db: &Db, plans: &[&ThreadPlan]) -> Result<(), CommandError> {
    db.write(|conn| {
        for plan in plans {
            cq::set_thread_state(
                conn,
                plan.id(),
                &plan.target_labels,
                plan.target_unread,
            )?;
            match &plan.snooze {
                SnoozeAction::Leave => {}
                SnoozeAction::Set { wake_at } => cq::upsert_snooze(
                    conn,
                    &SnoozeRow {
                        thread_id: plan.id(),
                        wake_at: *wake_at,
                        snoozed_at: now_ms(),
                        prior_label_ids: plan.snap.label_ids.clone(),
                        prior_is_unread: plan.snap.is_unread,
                    },
                )?,
                SnoozeAction::Clear => cq::delete_snooze(conn, plan.id())?,
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Put these threads back exactly as they were — labels, unread flag, and the
/// snooze row.
fn rollback_local(db: &Db, plans: &[&ThreadPlan]) -> Result<(), CommandError> {
    db.write(|conn| {
        for plan in plans {
            cq::set_thread_state(conn, plan.id(), &plan.snap.label_ids, plan.snap.is_unread)?;
            if !matches!(plan.snooze, SnoozeAction::Leave) {
                match &plan.prior_snooze {
                    Some(row) => cq::upsert_snooze(conn, row)?,
                    None => cq::delete_snooze(conn, plan.id())?,
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// remote calls
// ---------------------------------------------------------------------------

async fn run_chunk(
    client: &GmailClient,
    user_id: &str,
    op: RemoteOp,
    add: &[String],
    remove: &[String],
    chunk: &[&ThreadPlan],
) -> Result<(), GoogleError> {
    // `message_ids` is the Gmail-minted half of the snapshot; the placeholders
    // an unsent draft or a queued reply is filed under are in
    // `local_message_ids` and never reach here. One of them in this list is a
    // `400 Invalid ids value` for every id beside it.
    let ids: Vec<&str> = chunk
        .iter()
        .flat_map(|p| p.snap.message_ids.iter().map(String::as_str))
        .collect();
    if ids.is_empty() {
        return Ok(());
    }

    if op == RemoteOp::Trash && ids.len() == 1 {
        return client.messages_trash(user_id, ids[0]).await.map(|_| ());
    }

    let add: Vec<&str> = add.iter().map(String::as_str).collect();
    let remove: Vec<&str> = remove.iter().map(String::as_str).collect();

    if ids.len() == 1 {
        client
            .messages_modify(user_id, ids[0], &add, &remove)
            .await
            .map(|_| ())
    } else {
        client
            .messages_batch_modify(user_id, &ids, &add, &remove)
            .await
    }
}

// ---------------------------------------------------------------------------
// the inverse
// ---------------------------------------------------------------------------

/// The command that reverses what actually happened.
///
/// `changed` holds only the threads whose state really moved *and* whose remote
/// call succeeded. Narrowing to those is what stops undo from, say, adding
/// INBOX to a thread that was already archived before the command ran.
fn inverse(command: &Command, changed: &[&ThreadPlan]) -> Option<Command> {
    if changed.is_empty() {
        return None;
    }
    let thread_ids: Vec<i64> = changed.iter().map(|p| p.id()).collect();
    let priors: Vec<ThreadLabelState> = changed.iter().map(|p| p.prior_state()).collect();

    Some(match command {
        // Archive only ever removes INBOX, so adding it back is faithful — the
        // other labels were never touched.
        Command::Archive { .. } => Command::Unarchive {
            thread_ids,
            restore: Vec::new(),
        },
        // Unarchive can be either form, so its inverse always names the exact
        // prior state.
        Command::Unarchive { .. } => Command::Unarchive {
            thread_ids,
            restore: priors,
        },
        Command::MarkRead { read, .. } => Command::MarkRead {
            thread_ids,
            read: !read,
        },
        Command::Star { starred, .. } => Command::Star {
            thread_ids,
            starred: !starred,
        },
        Command::Label { label_id, add, .. } => Command::Label {
            thread_ids,
            label_id: label_id.clone(),
            add: !add,
        },
        // Both directions invert to the restore form: putting the bulk tabs
        // back is only faithful if they were the ones that were there, and
        // taking INBOX off again is only faithful if the forward added it.
        Command::MoveToInbox { .. } => Command::MoveToInbox {
            thread_ids,
            restore: priors,
        },
        Command::Trash { .. } | Command::Untrash { .. } => Command::Untrash {
            thread_ids,
            restore: priors,
        },
        // Both directions invert to the restore form, exactly as trash does.
        // Taking a report back is not "remove SPAM, add INBOX" — see the doc on
        // `Command::NotSpam` — and taking back a *rescue* is not "add SPAM"
        // either, because the thread may have carried other labels in spam.
        // Naming the prior set covers both without a special case.
        Command::ReportSpam { .. } | Command::NotSpam { .. } => Command::NotSpam {
            thread_ids,
            restore: priors,
        },
        Command::Snooze { .. } => Command::Unsnooze { thread_ids },
        Command::Unsnooze { .. } => {
            // Re-snoozing needs one wake time. If the threads were snoozed to
            // different times there is no single `Snooze` that restores them,
            // so this is honestly not undoable rather than approximately so.
            let wakes: Vec<i64> = changed
                .iter()
                .filter_map(|p| p.prior_snooze.as_ref().map(|r| r.wake_at))
                .collect();
            if wakes.len() != changed.len() || wakes.iter().any(|w| *w != wakes[0]) {
                return None;
            }
            Command::Snooze {
                thread_ids,
                until: wakes[0],
            }
        }
        // Unsubscribe has no inverse and never reaches here; the others are
        // the calendar half, which builds its own.
        Command::Unsubscribe { .. }
        | Command::Rsvp { .. }
        | Command::CreateEvent { .. }
        | Command::UpdateEvent { .. }
        | Command::DeleteEvent { .. }
        | Command::MoveEvent { .. } => return None,
    })
}

/// The sentence for a trash that also threw drafts away.
///
/// Two clauses rather than one number, because they are two different events
/// and only one of them can be taken back: "Trashed 3 conversations · discarded
/// 1 draft". The conversation clause is dropped when there were none, so
/// deleting a selection of drafts reads as "Discarded 4 drafts" and not as a
/// trash of nothing.
fn describe_trash(conversations: usize, drafts: usize) -> String {
    let trashed = format!("Trashed {}", plural(conversations, "conversation"));
    match (conversations, drafts) {
        (_, 0) => trashed,
        (0, _) => format!("Discarded {}", plural(drafts, "draft")),
        _ => format!("{trashed} · discarded {}", plural(drafts, "draft")),
    }
}

fn describe(command: &Command, n: usize) -> String {
    let subject = plural(n, "conversation");
    match command {
        Command::Archive { .. } => format!("Archived {subject}"),
        Command::Unarchive { .. } => format!("Moved {subject} back to the inbox"),
        Command::MarkRead { read: true, .. } => format!("Marked {subject} read"),
        Command::MarkRead { read: false, .. } => format!("Marked {subject} unread"),
        Command::Star { starred: true, .. } => format!("Starred {subject}"),
        Command::Star { starred: false, .. } => format!("Unstarred {subject}"),
        Command::Label { add: true, .. } => format!("Labelled {subject}"),
        Command::Label { add: false, .. } => format!("Removed the label from {subject}"),
        Command::MoveToInbox { restore, .. } => {
            if restore.is_empty() {
                format!("Moved {subject} to Inbox")
            } else {
                format!("Moved {subject} back")
            }
        }
        Command::ReportSpam { .. } => format!("Reported {subject} as spam"),
        Command::NotSpam { .. } => format!("Marked {subject} not spam"),
        Command::Trash { .. } => format!("Trashed {subject}"),
        Command::Untrash { .. } => format!("Restored {subject} from the trash"),
        Command::Snooze { .. } => format!("Snoozed {subject}"),
        Command::Unsnooze { .. } => format!("Un-snoozed {subject}"),
        Command::Rsvp { .. } => "RSVP sent".to_string(),
        // Not reachable: `commands::unsubscribe` writes its own message, and
        // this one counts conversations, which an unsubscribe does not touch.
        Command::Unsubscribe { .. } => "Unsubscribed".to_string(),
        // Not reachable: `commands::calendar` writes its own messages.
        Command::CreateEvent { .. }
        | Command::UpdateEvent { .. }
        | Command::DeleteEvent { .. }
        | Command::MoveEvent { .. } => command.kind().to_string(),
    }
}

// ---------------------------------------------------------------------------
// execution
// ---------------------------------------------------------------------------

/// Group key: batchModify is per-account and applies one delta to every id, so
/// threads can only share a call when all three of these match.
type GroupKey = (i64, Vec<String>, Vec<String>, RemoteOp);

pub(crate) async fn execute(
    dispatcher: &CommandDispatcher,
    command: &Command,
) -> Result<CommandResult, CommandError> {
    let targets = command.target_ids();

    // Drafts first, and only for trash.
    //
    // A draft has no message id `batchModify` will take, so the label-delta
    // engine below cannot touch one; `commands::drafts` deletes it through
    // `drafts.delete` instead. Doing it *before* the snapshots is what keeps the
    // two halves from disagreeing: a conversation that held a draft and three
    // real messages is planned from a store the draft has already left, so the
    // draft's own message id is not in the batch that trashes the rest of it.
    //
    // Only trash. Archiving, starring or labelling a conversation leaves its
    // draft alone in Gmail, and Mach lands in the same place by the same route —
    // see the module docs above.
    let discarded = if matches!(command, Command::Trash { .. }) {
        drafts::discard_in_threads(dispatcher, &targets, now_ms()).await?
    } else {
        Discarded::default()
    };

    // A conversation whose whole content was a draft no longer has a row to
    // plan against, and one whose draft could not be deleted is left entirely
    // alone rather than trashed around a draft that is still there.
    let remaining: Vec<i64> = targets
        .iter()
        .copied()
        .filter(|id| !discarded.vanished.contains(id) && !discarded.failed.contains(id))
        .collect();

    let plans = build_plans(dispatcher, command, &remaining).await?;
    if plans.is_empty() && discarded.is_empty() {
        return Ok(CommandResult::noop(describe(command, 0)));
    }
    if plans.is_empty() {
        return Ok(CommandResult {
            ok: discarded.failures.is_empty(),
            message: describe_trash(0, discarded.drafts),
            // Nothing here can be undone: see `commands::drafts`.
            undo: None,
            undo_label: None,
            applied: discarded.threads.clone(),
            failed: discarded.failures,
        });
    }

    // A thread with nothing Gmail will accept as an id cannot be named in a
    // request. Rather than write locally and let the store drift, it is reported
    // as a failure and left untouched.
    //
    // There are two ways to get here and they are not the same news, so they are
    // reported separately: a conversation nobody has synced yet, and one whose
    // only messages are Mach's own — a draft that has not been pushed, a reply
    // still in the outbox. The second is not a sync problem and telling the
    // owner to sync would send them after a fix that does not exist.
    let mut failures: Vec<CommandFailure> = discarded.failures;
    let stranded = |p: &&ThreadPlan| p.remote_needed() && p.snap.message_ids.is_empty();
    let unsynced: Vec<i64> = plans
        .iter()
        .filter(|p| stranded(p) && p.snap.local_message_ids.is_empty())
        .map(|p| p.id())
        .collect();
    let unsent_only: Vec<i64> = plans
        .iter()
        .filter(|p| stranded(p) && !p.snap.local_message_ids.is_empty())
        .map(|p| p.id())
        .collect();
    if !unsynced.is_empty() {
        failures.push(CommandFailure::invalid(
            unsynced.clone(),
            "thread has no locally known Gmail message ids; sync it before acting on it",
        ));
    }
    if !unsent_only.is_empty() {
        failures.push(CommandFailure::invalid(
            unsent_only.clone(),
            "conversation holds only unsent mail; there is nothing in Gmail to change yet",
        ));
    }
    let unaddressable: Vec<i64> = unsynced.into_iter().chain(unsent_only).collect();

    let actionable: Vec<&ThreadPlan> = plans
        .iter()
        .filter(|p| !unaddressable.contains(&p.id()))
        .collect();

    // ---------------------------------------------------------- local first
    let to_write: Vec<&ThreadPlan> = actionable
        .iter()
        .copied()
        .filter(|p| p.changed())
        .collect();
    apply_local(&dispatcher.db, &to_write)?;

    // -------------------------------------------------------- then the wire
    let mut groups: Vec<(GroupKey, Vec<&ThreadPlan>)> = Vec::new();
    for plan in actionable.iter().copied().filter(|p| p.remote_needed()) {
        let key: GroupKey = (
            plan.snap.account_id,
            plan.add.clone(),
            plan.remove.clone(),
            plan.op,
        );
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(plan),
            None => groups.push((key, vec![plan])),
        }
    }

    let mut failed_ids: Vec<i64> = unaddressable;
    for ((account_id, add, remove, op), members) in groups {
        // Not `?`. The local write for this group has already committed, and
        // returning here would leave it standing with nothing having been sent
        // and nothing said — the store quietly disagreeing with Gmail, which is
        // the one outcome the whole layer exists to prevent. A client that
        // cannot be built (no OAuth client configured, an account row that has
        // gone) is reported like any other refusal: rolled back, named, and
        // retriable once the cause is fixed.
        let client = match dispatcher.clients.gmail(account_id) {
            Ok(client) => client,
            Err(error) => {
                rollback_local(&dispatcher.db, &members)?;
                let ids: Vec<i64> = members.iter().map(|p| p.id()).collect();
                failed_ids.extend(ids.iter().copied());
                failures.push(CommandFailure::invalid(ids, error.to_string()));
                continue;
            }
        };
        for chunk in chunk_threads(&members, dispatcher.max_batch_message_ids) {
            if let Err(error) =
                run_chunk(&client, &dispatcher.user_id, op, &add, &remove, &chunk).await
            {
                rollback_local(&dispatcher.db, &chunk)?;
                let ids: Vec<i64> = chunk.iter().map(|p| p.id()).collect();
                failed_ids.extend(ids.iter().copied());
                failures.push(CommandFailure::from_google(ids, &error));
            }
        }
    }

    // ------------------------------------------------------------- report
    let mut applied: Vec<i64> = actionable
        .iter()
        .map(|p| p.id())
        .filter(|id| !failed_ids.contains(id))
        .collect();
    let changed: Vec<&ThreadPlan> = actionable
        .iter()
        .copied()
        .filter(|p| p.changed() && !failed_ids.contains(&p.id()))
        .collect();

    // Two counts, because the summary says two things: how many conversations
    // were trashed, and how many drafts were thrown away. A conversation that
    // held a draft *and* real messages is in both — it was trashed, and its
    // draft went with it — so the conversation count is the label-delta half
    // alone rather than the union.
    let conversations = applied.len();
    let message = if discarded.drafts > 0 {
        describe_trash(conversations, discarded.drafts)
    } else {
        describe(command, conversations)
    };
    for id in &discarded.threads {
        if !applied.contains(id) {
            applied.push(*id);
        }
    }

    Ok(CommandResult {
        ok: failures.is_empty(),
        // ⌘Z restores the conversations and cannot restore the drafts, so the
        // undo entry names only the conversations. Reusing `message` there would
        // put "undo … discarded 1 draft" on a button that does no such thing.
        undo_label: (discarded.drafts > 0).then(|| describe(command, conversations)),
        undo: inverse(command, &changed),
        message,
        applied,
        failed: failures,
    })
}

/// Cut a group into chunks of at most `max` message ids, never splitting a
/// thread. A thread with more messages than `max` still travels alone rather
/// than being broken up.
fn chunk_threads<'a>(plans: &[&'a ThreadPlan], max: usize) -> Vec<Vec<&'a ThreadPlan>> {
    let max = max.max(1);
    let mut out: Vec<Vec<&ThreadPlan>> = Vec::new();
    let mut current: Vec<&ThreadPlan> = Vec::new();
    let mut count = 0usize;
    for plan in plans {
        let size = plan.snap.message_ids.len();
        if !current.is_empty() && count + size > max {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
        current.push(plan);
        count += size;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}
