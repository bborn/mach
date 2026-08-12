//! Throwing a draft away — the one mail action that is not a label delta.
//!
//! # Why `trash` could not do this on its own
//!
//! Every other mail command reaches Gmail through `messages.batchModify`, which
//! is addressed by **message** ids. A draft is not an ordinary message: it lives
//! behind `users.drafts.*`, it is addressed by a **draft** id that appears on no
//! message resource, and the only call that removes one is `drafts.delete`.
//!
//! So a conversation whose only content is a draft has nothing `batchModify` can
//! be told about, and `commands::mail` refused it — correctly, for the path it
//! was on. Selecting four drafts and pressing delete reported "thread has no
//! locally known Gmail message ids; sync it before acting on it" and deleted
//! nothing. Syncing would not have helped: there was never a message id to find.
//!
//! `#` (and ⌘⌫) therefore run this first. It is what Gmail does with the same
//! keystroke: on a draft, delete means *discard the draft*, not "move a
//! conversation to Trash".
//!
//! # Three shapes of "the conversation has a draft"
//!
//! Which local row stands for a draft depends on where it was written, and all
//! three shapes are live in the owner's store:
//!
//!  1. **Written here, never pushed.** A `compose_drafts` row and a mirror
//!     message under `mach-draft:<id>`. Nothing exists at Gmail, so discarding
//!     it is purely local and costs no request.
//!  2. **Written here or elsewhere, and at Gmail.** The message row carries a
//!     Gmail-minted id, and `gmail_draft_id` is on the message (put there by the
//!     drafts sweep in `sync::mail`) or on the composer's row. `drafts.delete`
//!     takes it.
//!  3. **The conversation claims a draft and holds no message at all.** Four of
//!     these are in the owner's store right now: a `DRAFT` row in
//!     `thread_labels`, a `message_count` of 1 or 2, and nothing in `messages`
//!     behind it. No draft id exists locally, so it is fetched from
//!     `drafts.list` — the only endpoint that pairs a draft id with the thread
//!     it sits in, and the one `sync::mail` already calls every pass. It costs a
//!     few hundred bytes and it is what makes those four rows deletable at all.
//!
//! The lookup happens once per account per command, not once per draft, and only
//! when something targeted actually needs it.
//!
//! # Gmail first, then the store — the one command that inverts the order
//!
//! The rest of the layer writes locally and then tells Google, so the UI
//! repaints before Google answers. The repaint is not what is at stake here:
//! `project()` on the frontend hides the row in the frame the keystroke
//! produced, and this write only has to beat the refetch behind it.
//!
//! What local-first cannot give a draft is a **rollback**. `drafts.delete` is
//! permanent, and a local removal Google then refuses leaves a draft that is
//! gone here and alive on his phone — where the next sync pass, finding a
//! `DRAFT` message with no local draft row, adopts it straight back into the
//! conversation he just cleared. Deleting at Gmail first means a refusal leaves
//! everything exactly as it was, which is the state a retry can start from.
//!
//! # There is no undo
//!
//! Gmail has no endpoint that puts a deleted draft back, and no trash for it to
//! have gone to. So a discarded draft contributes nothing to the inverse `trash`
//! hands back, and [`CommandResult::undo_label`](super::CommandResult) names only
//! the half ⌘Z can honour. Claiming otherwise would be a button that lies.

use std::collections::HashMap;

use crate::db::command_queries::{self as cq, ThreadDraft};
use crate::db::Db;
use crate::google::gmail::GmailClient;
use crate::google::GoogleError;
use crate::ipc::compose::engine::{draft as compose_draft, mirror, ComposeError};

use super::error::{CommandError, CommandFailure};
use super::mail::DRAFT;
use super::CommandDispatcher;

/// What the drafts pass did, for the command that asked for it.
#[derive(Debug, Default)]
pub(crate) struct Discarded {
    /// Conversations that were holding a draft and are not any more.
    pub threads: Vec<i64>,
    /// How many drafts that was. A conversation can hold two.
    pub drafts: usize,
    /// Conversations whose row went with the draft, because the draft was all
    /// there was. Nothing downstream may ask the store about these.
    pub vanished: Vec<i64>,
    /// Conversations whose draft is still there. They keep it, and the command
    /// that called this leaves them alone rather than half-acting on them.
    pub failed: Vec<i64>,
    pub failures: Vec<CommandFailure>,
}

impl Discarded {
    pub fn is_empty(&self) -> bool {
        self.drafts == 0 && self.failures.is_empty()
    }

    fn note_failure(&mut self, thread_id: i64, error: &GoogleError) {
        if !self.failed.contains(&thread_id) {
            self.failed.push(thread_id);
        }
        self.failures.push(CommandFailure {
            ids: vec![thread_id],
            kind: super::error::FailureKind::from_google(error),
            message: format!("the draft is still at Gmail: {error}"),
            retriable: error.is_retriable(),
            // Nothing local was written for this conversation, so there was
            // nothing to put back. See the ordering note in the module docs.
            rolled_back: false,
        });
    }
}

/// Discard every draft held by any of these conversations.
///
/// Threads with no draft are not touched and are not mentioned in the result — a
/// selection of forty ordinary conversations passes through here having cost one
/// read each and no requests at all.
pub(crate) async fn discard_in_threads(
    dispatcher: &CommandDispatcher,
    thread_ids: &[i64],
    now: i64,
) -> Result<Discarded, CommandError> {
    let mut out = Discarded::default();
    if thread_ids.is_empty() {
        return Ok(out);
    }

    // Every read first, before a single write, so an unknown id fails here
    // having changed nothing — the order `mail::build_plans` uses, for the same
    // reason.
    let mut work: Vec<(i64, Vec<ThreadDraft>)> = Vec::new();
    let mut shells: Vec<(i64, i64, String)> = Vec::new();
    for id in thread_ids {
        let (snapshot, drafts) = dispatcher
            .db
            .read(|conn| Ok((cq::thread_snapshot(conn, *id)?, cq::thread_drafts(conn, *id)?)))?;
        let Some(snapshot) = snapshot else {
            return Err(CommandError::UnknownThread { thread_id: *id });
        };
        if !drafts.is_empty() {
            work.push((*id, drafts));
        } else if snapshot.label_ids.iter().any(|l| l == DRAFT) {
            // Shape 3: the mailbox says there is a draft here and no local row
            // says which. The thread's own Gmail id is the handle that is left.
            shells.push((*id, snapshot.account_id, snapshot.gmail_thread_id));
        }
    }
    if work.is_empty() && shells.is_empty() {
        return Ok(out);
    }

    let mut listings = DraftListings::default();

    for (thread_id, drafts) in work {
        let mut forgotten = 0usize;
        for draft in drafts {
            let account_id = draft.account_id;
            let remote_id = match draft.gmail_draft_id.clone() {
                Some(id) => Some(id),
                // A draft Gmail has never been told about has nothing to look
                // up and nothing to delete there.
                None if !draft.exists_at_gmail() => None,
                None => {
                    let key = draft.gmail_message_id.clone().unwrap_or_default();
                    match listings
                        .draft_id(dispatcher, account_id, Wanted::ByMessage(&key))
                        .await
                    {
                        Ok(found) => found,
                        Err(error) => {
                            out.note_failure(thread_id, &error);
                            continue;
                        }
                    }
                }
            };
            if let Some(remote_id) = remote_id {
                if let Err(error) = delete_remote(dispatcher, account_id, &remote_id).await {
                    out.note_failure(thread_id, &error);
                    continue;
                }
            }
            forget_locally(&dispatcher.db, &draft, now)?;
            forgotten += 1;
            out.drafts += 1;
        }
        if forgotten > 0 {
            finish(&dispatcher.db, &mut out, thread_id)?;
        }
    }

    for (thread_id, account_id, gmail_thread_id) in shells {
        let remote_id = match listings
            .draft_id(dispatcher, account_id, Wanted::ByThread(&gmail_thread_id))
            .await
        {
            Ok(found) => found,
            Err(error) => {
                out.note_failure(thread_id, &error);
                continue;
            }
        };
        if let Some(remote_id) = remote_id {
            if let Err(error) = delete_remote(dispatcher, account_id, &remote_id).await {
                out.note_failure(thread_id, &error);
                continue;
            }
        }
        // Gmail lists no draft for this conversation and neither does the
        // store: the row is a leftover claiming a draft that stopped existing
        // somewhere else. Clearing it is the whole job, and it is what the
        // owner asked for.
        out.drafts += 1;
        finish(&dispatcher.db, &mut out, thread_id)?;
    }

    Ok(out)
}

/// Take the draft out of the conversation and out of the composer.
///
/// In that order: removing the composer's row first would leave `unmirror`
/// nothing to look the message up by, which is the ordering
/// `ipc::compose::forget_draft_locally` documents. `delete_draft` also writes a
/// tombstone, so an autosave already on the wire cannot write the draft back
/// half a second later.
fn forget_locally(db: &Db, draft: &ThreadDraft, now: i64) -> Result<(), CommandError> {
    match draft.local_draft_id.as_deref() {
        Some(id) => {
            if let Some(loaded) = compose_draft::load_draft(db, id).map_err(compose_error)? {
                mirror::unmirror(db, &loaded).map_err(compose_error)?;
            }
            compose_draft::delete_draft(db, id, now).map_err(compose_error)?;
        }
        // A draft written elsewhere has no composer row; the mirror in the
        // conversation is the only thing standing for it here.
        None => mirror::unmirror_ids(db, "", draft.gmail_message_id.as_deref())
            .map_err(compose_error)?,
    }
    Ok(())
}

/// Record a conversation as done with, and note whether its row survived.
fn finish(db: &Db, out: &mut Discarded, thread_id: i64) -> Result<(), CommandError> {
    let vanished = db.write(|conn| cq::forget_thread_draft_label(conn, thread_id))?;
    if !out.threads.contains(&thread_id) {
        out.threads.push(thread_id);
    }
    if vanished && !out.vanished.contains(&thread_id) {
        out.vanished.push(thread_id);
    }
    Ok(())
}

async fn delete_remote(
    dispatcher: &CommandDispatcher,
    account_id: i64,
    draft_id: &str,
) -> Result<(), GoogleError> {
    let client = gmail(dispatcher, account_id)?;
    match client.drafts_delete(&dispatcher.user_id, draft_id).await {
        // Already gone at Gmail is the state that was asked for. Reporting it
        // would be a red toast about a draft that is, in fact, deleted.
        Err(GoogleError::NotFound { .. }) => Ok(()),
        other => other,
    }
}

/// A client that could not be built, in the shape the remote calls return, so
/// one failure path covers both.
fn gmail(dispatcher: &CommandDispatcher, account_id: i64) -> Result<GmailClient, GoogleError> {
    dispatcher
        .clients
        .gmail(account_id)
        .map_err(|error| GoogleError::InvalidRequest {
            message: error.to_string(),
        })
}

// ---------------------------------------------------------------------------
// finding a draft id
// ---------------------------------------------------------------------------

/// Which way a caller knows the draft it is after.
enum Wanted<'a> {
    ByMessage(&'a str),
    ByThread(&'a str),
}

/// `drafts.list` for one account, indexed both ways it gets asked about.
#[derive(Debug, Default)]
struct DraftListing {
    by_message: HashMap<String, String>,
    by_thread: HashMap<String, String>,
}

/// One listing per account, fetched at most once per command.
#[derive(Debug, Default)]
struct DraftListings {
    fetched: HashMap<i64, DraftListing>,
}

impl DraftListings {
    async fn draft_id(
        &mut self,
        dispatcher: &CommandDispatcher,
        account_id: i64,
        want: Wanted<'_>,
    ) -> Result<Option<String>, GoogleError> {
        if !self.fetched.contains_key(&account_id) {
            let client = gmail(dispatcher, account_id)?;
            let drafts = client.drafts_list_all(&dispatcher.user_id, None).await?;
            let mut listing = DraftListing::default();
            for draft in drafts {
                if draft.id.is_empty() {
                    continue;
                }
                if !draft.message.id.is_empty() {
                    listing
                        .by_message
                        .insert(draft.message.id, draft.id.clone());
                }
                if !draft.message.thread_id.is_empty() {
                    listing.by_thread.insert(draft.message.thread_id, draft.id);
                }
            }
            self.fetched.insert(account_id, listing);
        }
        let listing = &self.fetched[&account_id];
        Ok(match want {
            Wanted::ByMessage(id) => listing.by_message.get(id).cloned(),
            Wanted::ByThread(id) => listing.by_thread.get(id).cloned(),
        })
    }
}

fn compose_error(error: ComposeError) -> CommandError {
    match error {
        ComposeError::Db(inner) => CommandError::Db(inner),
        ComposeError::Command(inner) => inner,
        other => CommandError::Invalid {
            message: other.to_string(),
        },
    }
}
