//! The one error shape the agent produces.
//!
//! Same two-field contract as [`IpcError`](crate::ipc::IpcError) and
//! [`CommandError`](crate::commands::CommandError) — `{ kind, message }` — so
//! the frontend keeps one branch. `kind` is the stable tag; `message` is the
//! sentence a human reads.
//!
//! [`AgentError::MissingApiKey`] is the one that earns its own variant. A
//! missing `ANTHROPIC_API_KEY` is a *state*, exactly like missing Google
//! credentials: the app boots, mail works, and the agent says what to set. It
//! must never surface as "internal error" or as a 401 from Anthropic.

use crate::commands::CommandError;
use crate::db::DbError;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// No `ANTHROPIC_API_KEY`. Carries the whole sentence to render.
    #[error("{0}")]
    MissingApiKey(String),

    /// The request never got an answer — DNS, TLS, connect, timeout.
    #[error("could not reach the Anthropic API: {message}")]
    Transport { message: String },

    /// A non-2xx from the API.
    #[error("the Anthropic API returned {status}: {message}")]
    Api { status: u16, message: String },

    /// A 2xx whose stream was not the shape we expected.
    #[error("the model stream could not be read: {0}")]
    Protocol(String),

    /// A session id the engine has never heard of (or has already closed).
    #[error("no agent session with id {0}")]
    UnknownSession(String),

    /// A tool call the agent made that could not be turned into an action.
    #[error("{0}")]
    Invalid(String),

    #[error("local store: {0}")]
    Db(#[from] DbError),

    #[error(transparent)]
    Command(#[from] CommandError),
}

impl AgentError {
    pub fn kind(&self) -> &'static str {
        match self {
            AgentError::MissingApiKey(_) => "agentNotConfigured",
            AgentError::Transport { .. } => "agentTransport",
            AgentError::Api { .. } => "agentApi",
            AgentError::Protocol(_) => "agentProtocol",
            AgentError::UnknownSession(_) => "unknownSession",
            AgentError::Invalid(_) => "invalid",
            AgentError::Db(_) => "db",
            AgentError::Command(inner) => inner.kind(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        AgentError::Invalid(message.into())
    }

    pub fn transport(message: impl Into<String>) -> Self {
        AgentError::Transport {
            message: message.into(),
        }
    }

    /// True when a tool failure should be reported *to the model* rather than
    /// killing the session. The model can pick a different thread id or a
    /// different approach; it cannot fix a missing API key.
    pub fn is_recoverable_by_model(&self) -> bool {
        matches!(
            self,
            AgentError::Invalid(_) | AgentError::Command(_) | AgentError::Db(_)
        )
    }
}

impl From<crate::ipc::compose::engine::ComposeError> for AgentError {
    fn from(error: crate::ipc::compose::engine::ComposeError) -> Self {
        use crate::ipc::compose::engine::ComposeError;
        match error {
            ComposeError::Db(inner) => AgentError::Db(inner),
            ComposeError::Command(inner) => AgentError::Command(inner),
            other => AgentError::Invalid(other.to_string()),
        }
    }
}
