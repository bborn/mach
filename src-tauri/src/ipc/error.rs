//! The one error shape that crosses the IPC boundary.
//!
//! A Tauri command must never panic: an unwind inside the invoke handler takes
//! the webview's promise with it and the UI is left with a rejected value it
//! cannot render. So every fallible path in `ipc` returns [`IpcError`], which
//! serializes as `{ "kind": "...", "message": "..." }` — the same two-field
//! shape [`CommandError`](crate::commands::CommandError) already uses, so the
//! frontend has one branch, not two.
//!
//! `kind` is the stable tag code branches on; `message` is the sentence a human
//! reads. Where an underlying error already has a good tag (the command layer's
//! `unknownThread`, `unknownAccount`, …) it is passed through unchanged rather
//! than flattened into "command failed".

use serde::Serialize;

use crate::auth::AuthError;
use crate::commands::CommandError;
use crate::db::DbError;
use crate::sync::SyncError;

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// No Google OAuth client. The app runs; sign-in and sync do not.
    #[error("{0}")]
    NotConfigured(String),

    /// A row the UI asked for is not in the local store.
    #[error("no {entity} with id {id} in the local store")]
    NotFound { entity: &'static str, id: i64 },

    /// An add-account handshake that is not in flight (already completed,
    /// cancelled, or from a previous run of the app).
    #[error("no pending sign-in with id {0}")]
    UnknownPending(String),

    /// A sign-in started for one address came back holding another. Nothing was
    /// written; the row that asked to be repaired is still broken.
    #[error("signed in as {got}, not {expected}")]
    WrongAccount { expected: String, got: String },

    #[error("{0}")]
    Auth(#[from] AuthError),

    #[error(transparent)]
    Command(#[from] CommandError),

    #[error("local store: {0}")]
    Db(#[from] DbError),

    #[error("sync: {0}")]
    Sync(#[from] SyncError),

    /// A lock we could not take, a thread we could not spawn — a bug, but one
    /// the UI still has to render instead of hanging.
    #[error("{0}")]
    Internal(String),
}

impl IpcError {
    /// A stable machine-readable tag. `Display` is for humans; this is what the
    /// frontend (and later the agent) branches on.
    pub fn kind(&self) -> &'static str {
        match self {
            IpcError::NotConfigured(_) => "notConfigured",
            IpcError::NotFound { .. } => "notFound",
            IpcError::UnknownPending(_) => "unknownPending",
            IpcError::WrongAccount { .. } => "wrongAccount",
            IpcError::Auth(_) => "auth",
            // The command layer's own tag is more specific than anything this
            // enum could invent, so it wins.
            IpcError::Command(inner) => inner.kind(),
            IpcError::Db(_) => "db",
            IpcError::Sync(_) => "sync",
            IpcError::Internal(_) => "internal",
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        IpcError::Internal(message.into())
    }

    pub fn not_found(entity: &'static str, id: i64) -> Self {
        IpcError::NotFound { entity, id }
    }
}

/// `{ "kind": "...", "message": "..." }`.
impl Serialize for IpcError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("IpcError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}
