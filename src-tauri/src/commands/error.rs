//! The two error shapes the command layer speaks.
//!
//! They are deliberately different things:
//!
//!  * [`CommandError`] — *the command did not run.* No local write happened, no
//!    request went out. Unknown thread, unknown account, a missing snooze
//!    label, a broken database. The caller has to fix something.
//!  * [`CommandFailure`] — *the command ran and Google refused part or all of
//!    it.* The local store has already been put back; this is a report, not an
//!    exception, and it names exactly which ids it covers.
//!
//! Both serialize with a `kind` discriminant so the UI can branch on the shape
//! and a future agent can reason about the failure without parsing prose.

use serde::{Deserialize, Serialize};

use crate::db::DbError;
use crate::google::GoogleError;

/// A command that could not be attempted.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("no thread with id {thread_id} in the local store")]
    UnknownThread { thread_id: i64 },

    #[error("no event with id {event_id} in the local store")]
    UnknownEvent { event_id: i64 },

    #[error("no google client is configured for account {account_id}")]
    UnknownAccount { account_id: i64 },

    /// Snooze needs a real Gmail label to hang on. Creating one is
    /// `users.labels.create`, which the Gmail client does not expose yet, so a
    /// missing label is surfaced rather than silently worked around.
    #[error("account {account_id} has no label named {label_name:?}; create it before snoozing")]
    MissingLabel { account_id: i64, label_name: String },

    #[error("{message}")]
    Invalid { message: String },

    #[error("local store: {0}")]
    Db(#[from] DbError),
}

impl CommandError {
    /// A stable machine-readable tag. The `Display` text is for humans; this is
    /// what code (and the agent) should branch on.
    pub fn kind(&self) -> &'static str {
        match self {
            CommandError::UnknownThread { .. } => "unknownThread",
            CommandError::UnknownEvent { .. } => "unknownEvent",
            CommandError::UnknownAccount { .. } => "unknownAccount",
            CommandError::MissingLabel { .. } => "missingLabel",
            CommandError::Invalid { .. } => "invalid",
            CommandError::Db(_) => "db",
        }
    }
}

/// `{ "kind": "...", "message": "..." }`, so it can be returned straight out of
/// a Tauri command.
impl Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("CommandError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}

/// Why a remote call failed, collapsed to the categories a caller can act on.
///
/// This mirrors the split in [`GoogleError`] without leaking its variants
/// across the IPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    /// 401 — the grant is dead. Re-authorize the account.
    Auth,
    /// 429, or a 403 whose reason is a quota. Worth trying again later.
    RateLimited,
    /// 403 that is not a quota — missing scope, or policy. Retrying will not
    /// help.
    Forbidden,
    /// 404 — the thread, message or event is gone on Google's side. Sync will
    /// notice; the command will not.
    NotFound,
    /// 5xx. Google's problem, and retriable.
    Server,
    /// Never got an answer. The change may or may not have landed.
    Network,
    /// The command layer would not send the request — e.g. a thread with no
    /// known Gmail message ids.
    Invalid,
    /// A 2xx that did not parse, or anything else unexpected.
    Unexpected,
}

impl FailureKind {
    pub fn from_google(error: &GoogleError) -> Self {
        match error {
            GoogleError::Auth { .. } => FailureKind::Auth,
            GoogleError::RateLimited { .. } => FailureKind::RateLimited,
            GoogleError::Forbidden { .. } => FailureKind::Forbidden,
            GoogleError::NotFound { .. }
            | GoogleError::HistoryExpired { .. }
            | GoogleError::SyncTokenExpired { .. } => FailureKind::NotFound,
            GoogleError::Network { .. } => FailureKind::Network,
            GoogleError::InvalidRequest { .. } => FailureKind::Invalid,
            GoogleError::Api { status, .. } if (500..600).contains(status) => FailureKind::Server,
            GoogleError::Api { .. } | GoogleError::Deserialize { .. } => FailureKind::Unexpected,
        }
    }
}

/// One remote failure, covering the ids it actually affected.
///
/// # Why `rolled_back` is always true today
///
/// The command layer has no durable outbox. Keeping a local write that Google
/// never accepted would leave the store disagreeing with Gmail in a way
/// incremental sync cannot repair — `users.history.list` only reports changes
/// that *happened*, so a change that did not happen is invisible to it and the
/// wrong local state would survive until a full resync. Reverting is therefore
/// the only honest option, including for retriable failures: the retry budget
/// in [`crate::google::RetryPolicy`] has already been spent by the time this
/// type is constructed, and `retriable` tells the caller it is worth dispatching
/// the same command again rather than that something is still in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandFailure {
    /// Thread ids, or the single event id for an RSVP.
    pub ids: Vec<i64>,
    pub kind: FailureKind,
    pub message: String,
    /// Whether dispatching the same command again could plausibly succeed.
    pub retriable: bool,
    /// Whether the local store was reverted for these ids.
    pub rolled_back: bool,
}

impl CommandFailure {
    pub fn from_google(ids: Vec<i64>, error: &GoogleError) -> Self {
        CommandFailure {
            ids,
            kind: FailureKind::from_google(error),
            message: error.to_string(),
            retriable: error.is_retriable(),
            rolled_back: true,
        }
    }

    pub fn invalid(ids: Vec<i64>, message: impl Into<String>) -> Self {
        CommandFailure {
            ids,
            kind: FailureKind::Invalid,
            message: message.into(),
            retriable: false,
            rolled_back: true,
        }
    }
}
