//! Pushing a draft to Gmail, so it exists somewhere other than this Mac.
//!
//! # Why push at all
//!
//! Mach could have shown local drafts in the Drafts mailbox and stopped there.
//! [`mirror`](super::mirror) does exactly that, and it is what makes the draft
//! appear instantly. But the owner reads mail on his phone, and a draft that
//! only exists inside one laptop is a draft he cannot finish anywhere else —
//! which is most of the reason to have written it. So the mirror is the fast
//! half and this is the true half: `drafts.create` the first time,
//! `drafts.update` after that, `drafts.delete` when it is sent or discarded.
//!
//! # The order, which is the whole invariant
//!
//! Local write, then network, never the other way round. Nothing here is
//! awaited by a UI path: [`spawn_push`] hands the call to the runtime and
//! returns, so `saveDraft` costs a SQLite write and no more. A failure is
//! recorded on the row as [`RemoteState::Failed`] and the composer says so —
//! silence would leave the owner believing a draft was on his phone when it was
//! not, which is the specific failure this project has paid most for.
//!
//! # One draft, not two
//!
//! `drafts.update` addresses the draft by the id Gmail gave it, so editing is
//! an update rather than a second draft, and the id lives in a column the
//! editor cannot overwrite (see [`draft::save_draft`]). The reply Gmail sends
//! back carries the message and thread ids, and [`mirror::adopt`] puts them on
//! the local rows — so when the sync pass eventually sees this draft coming the
//! other way, it lands on the row Mach already wrote.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use crate::commands::GoogleClients;
use crate::db::Db;

use super::draft::{self, Draft, DraftRemote, RemoteState};
use super::mime::build_rfc822;
use super::{mirror, Result};

/// The Gmail side of the composer's drafts.
pub struct DraftRemoteSync {
    db: Db,
    clients: Arc<dyn GoogleClients>,
    user_id: String,
}

impl DraftRemoteSync {
    pub fn new(db: Db, clients: Arc<dyn GoogleClients>) -> Self {
        DraftRemoteSync {
            db,
            clients,
            user_id: "me".to_string(),
        }
    }

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = user_id.into();
        self
    }

    /// Push one draft, creating it on Google or updating what is already there.
    ///
    /// Returns the state the row now holds, so a caller that *does* want to
    /// wait — a test, or a flush — can assert on it.
    pub async fn push(&self, draft_id: &str, now_ms: i64) -> Result<RemoteState> {
        // One push per draft at a time. Two overlapping pushes of a draft that
        // has never been created would each `drafts.create` it, and the owner
        // would find two of one reply on his phone. The row stays `pending`, so
        // the newer text goes out on the next save or the next launch sweep.
        let Some(_claim) = InFlight::claim(draft_id) else {
            return Ok(RemoteState::Pending);
        };
        let Some(draft) = draft::load_draft(&self.db, draft_id)? else {
            return Ok(RemoteState::Pending);
        };
        // An empty draft is not worth a round trip, and creating one on Google
        // would put a blank row in his Drafts on every device.
        if draft.is_empty() {
            return Ok(RemoteState::Pending);
        }

        let built = draft::build(&self.db, &draft, now_ms, entropy(now_ms))?;
        let rfc822 = build_rfc822(&built.outgoing)?;
        // A reply belongs in its conversation on Google too, or it shows up on
        // the phone as a stray message with a `Re:` subject.
        let thread_id = built
            .gmail_thread_id
            .clone()
            .or_else(|| draft.remote.thread_id.clone());

        let client = match self.clients.gmail(draft.account_id) {
            Ok(client) => client,
            Err(error) => return self.record_failure(&draft, &error.to_string(), now_ms),
        };

        let result = match draft.remote.draft_id.as_deref() {
            Some(remote_id) => {
                client
                    .drafts_update(&self.user_id, remote_id, &rfc822, thread_id.as_deref())
                    .await
            }
            None => {
                client
                    .drafts_create(&self.user_id, &rfc822, thread_id.as_deref())
                    .await
            }
        };

        match result {
            Ok(remote) => {
                let state = DraftRemote {
                    state: RemoteState::Synced,
                    draft_id: Some(remote.id.clone()),
                    message_id: non_empty(&remote.message.id),
                    thread_id: non_empty(&remote.message.thread_id),
                    error: None,
                    synced_at: now_ms,
                };
                draft::set_remote(&self.db, &draft.id, &state)?;
                mirror::adopt(
                    &self.db,
                    &draft.id,
                    draft.remote.message_id.as_deref(),
                    &remote.message.id,
                    &remote.message.thread_id,
                )?;
                Ok(RemoteState::Synced)
            }
            Err(error) => self.record_failure(&draft, &error.to_string(), now_ms),
        }
    }

    /// Push everything Gmail has not been told about. Runs at launch, so a
    /// draft written while the network was down is not stranded for ever.
    pub async fn push_pending(&self, now_ms: i64) -> Result<usize> {
        let pending = draft::drafts_needing_push(&self.db)?;
        let mut pushed = 0;
        for draft in pending {
            if self.push(&draft.id, now_ms).await? == RemoteState::Synced {
                pushed += 1;
            }
        }
        Ok(pushed)
    }

    /// Remove the Gmail draft. Called when the draft is sent or discarded —
    /// otherwise the copy on his phone outlives the reply he actually sent.
    ///
    /// A draft that was never pushed has nothing to delete, and a draft Google
    /// has already lost is not an error worth surfacing: the goal state is "it
    /// is not there", and it is not there.
    pub async fn delete(&self, remote_draft_id: &str, account_id: i64) -> Result<()> {
        if remote_draft_id.is_empty() {
            return Ok(());
        }
        let client = self.clients.gmail(account_id)?;
        match client.drafts_delete(&self.user_id, remote_draft_id).await {
            Ok(()) => Ok(()),
            Err(crate::google::GoogleError::NotFound { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn record_failure(&self, draft: &Draft, message: &str, now_ms: i64) -> Result<RemoteState> {
        let state = DraftRemote {
            state: RemoteState::Failed,
            error: Some(message.to_string()),
            synced_at: now_ms,
            ..draft.remote.clone()
        };
        draft::set_remote(&self.db, &draft.id, &state)?;
        Ok(RemoteState::Failed)
    }
}

/// Hand a push to the runtime and get out of the way. The overlap rule lives in
/// [`DraftRemoteSync::push`], so it holds for the flush sweep too.
pub fn spawn_push(db: Db, clients: Arc<dyn GoogleClients>, draft_id: String, now_ms: i64) {
    tokio::spawn(async move {
        let sync = DraftRemoteSync::new(db, clients);
        let _ = sync.push(&draft_id, now_ms).await;
    });
}

/// Delete a Gmail draft in the background. Nothing waits on it: the local rows
/// are already gone, and the worst case is a stale draft on the phone that the
/// next launch's sweep does not resurrect.
pub fn spawn_delete(
    db: Db,
    clients: Arc<dyn GoogleClients>,
    remote_draft_id: String,
    account_id: i64,
) {
    let _ = db;
    tokio::spawn(async move {
        let sync = DraftRemoteSync::new(db, clients);
        let _ = sync.delete(&remote_draft_id, account_id).await;
    });
}

/// A claim on one draft's push slot, released when it is dropped.
///
/// Dropped rather than released by hand because a task that is cancelled — the
/// runtime shutting down mid-push — would otherwise leave the slot held and
/// that draft would never be pushed again.
struct InFlight(String);

impl InFlight {
    fn claim(draft_id: &str) -> Option<InFlight> {
        let mut set = in_flight()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set.insert(draft_id.to_string())
            .then(|| InFlight(draft_id.to_string()))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        in_flight()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

fn in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// Enough variation for a Message-ID on a draft nobody has sent. The bytes are
/// rebuilt on every push, so this is never the id that leaves.
fn entropy(now: i64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (now as u64).rotate_left(23) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
