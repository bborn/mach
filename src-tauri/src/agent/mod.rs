//! Agent sessions — ⌘K, a sentence, and the command layer as tools.
//!
//! The spec's claim is that the agent needs no new access: *"its tools are the
//! command layer"*. This module is that claim built. It adds no path to Google,
//! no second write surface, and no privileged query — it composes what already
//! exists:
//!
//! ```text
//!   ⌘K sentence ──► session ──► Anthropic Messages API (streamed)
//!        + context      │              │
//!    (what's on screen) │              └── tool calls
//!                       │                     │
//!                       │        ┌────────────┴─────────────┐
//!                       │        │                          │
//!                       │   ipc::reads              CommandDispatcher
//!                       │   (local SQLite)          (typed · undoable · logged)
//!                       │                                   │
//!                       └── events ──► the bottom bar   compose::Outbox
//!                                                       (approval-gated)
//! ```
//!
//! # Layout
//!
//! | module | what lives there |
//! |---|---|
//! | [`config`] | `ANTHROPIC_API_KEY`, model, effort — and the sentence when it is missing |
//! | [`wire`] | the Messages API: request, SSE decoding, turn accumulation |
//! | [`tools`] | [`Command::catalogue`](crate::commands::Command::catalogue) → tool definitions, and execution |
//! | [`context`] | what "this" means, and the system prompt |
//! | [`session`] | the loop, the approval gate, the registry |
//! | [`error`] | one `{ kind, message }`, as everywhere else |
//!
//! # Two decisions worth knowing
//!
//! **The approval line is "does this touch another human", not "is this
//! risky".** Archiving fifty threads runs unattended because the command layer
//! hands back its own inverse and one keystroke undoes it. Sending mail and
//! RSVPing do not run unattended, because undo does not unsend. The command
//! layer's undo makes mistakes survivable; an unsent email is better than an
//! undone one.
//!
//! **Nothing here is registered at boot.** The engine is built on first use and
//! the credential is read then, so a missing `ANTHROPIC_API_KEY` is a typed
//! error on ⌘K rather than a failed launch — the same shape as missing Google
//! credentials.

pub mod config;
pub mod context;
pub mod error;
pub mod session;
pub mod tools;
pub mod wire;

pub use config::{AgentConfig, Credential};
pub use context::ContextItem;
pub use error::AgentError;
pub use session::{
    AgentEngine, Entry, Input, PendingApproval, SessionEmitter, SessionEvent, SessionSnapshot,
    SessionStatus, ToolState,
};
pub use tools::{Tool, ToolPolicy};
pub use wire::{ModelCall, ModelTransport, ToolDefinition};
