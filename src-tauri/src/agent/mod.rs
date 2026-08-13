//! Agent sessions — ⌘K, a sentence, and the command layer as tools.
//!
//! The spec's claim is that the agent needs no new access: *"its tools are the
//! command layer"*. This module is that claim built. It adds no path to Google,
//! no second write surface, and no privileged query — it composes what already
//! exists:
//!
//! ```text
//!   ⌘K sentence ──► session ──► a brain (Claude Code · Anthropic API · your own)
//!        + context      │              │
//!    (what's on screen) │              └── tool calls
//!                       │                     │
//!                       │                 ToolGate  ← the approval rule lives here
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
//! | [`backend`] | which brain, and how Mach detects one without being told |
//! | [`brain`] | the seam: what a brain is handed and what it may touch |
//! | [`cli`] | Claude Code, spawned headless — the default |
//! | [`anthropic`] | the Messages API, for people who prefer a key |
//! | [`command`] | any other agent, against a written contract |
//! | [`mcp`] | the command layer served as MCP, so an out-of-process brain can call it |
//! | [`gate`] | the tool surface, the approval rule, the one door to the mailbox |
//! | [`config`] | `ANTHROPIC_API_KEY`, model, effort |
//! | [`wire`] | the Messages API: request, SSE decoding, turn accumulation |
//! | [`complete`] | one-shot, tool-less, unstreamed completions — the ghost text |
//! | [`price`] | list prices, for the one path that reports tokens and not money |
//! | [`tools`] | [`Command::catalogue`](crate::commands::Command::catalogue) → tool definitions, and execution |
//! | [`plugin_tools`] | plugin actions → tools, attributed, with the approval policy they inherit |
//! | [`context`] | what "this" means, and the system prompt |
//! | [`session`] | the loop, the registry, the approval desk |
//! | [`error`] | one `{ kind, message }`, as everywhere else |
//!
//! # Three decisions worth knowing
//!
//! **The approval line is "does this touch another human", not "is this
//! risky".** Archiving fifty threads runs unattended because the command layer
//! hands back its own inverse and one keystroke undoes it. Sending mail and
//! RSVPing do not run unattended, because undo does not unsend. The command
//! layer's undo makes mistakes survivable; an unsent email is better than an
//! undone one.
//!
//! **The brain is swappable; the rules are not.** Whichever backend is thinking,
//! its tool calls arrive at [`gate::ToolGate`] in this process, which checks the
//! surface, applies the policy, and parks on the owner. A backend cannot widen
//! the surface, skip the gate, or approve on the owner's behalf — not with its
//! own permission system, not with `--dangerously-skip-permissions`, not by
//! being a program somebody else wrote.
//!
//! **Nothing here is registered at boot.** The backend is resolved on first use,
//! so installing Claude Code — or setting a key — costs a ⌘K rather than a
//! relaunch, and having neither is a typed error on the one action that needs
//! it, with a sentence saying what to do about it.

pub mod anthropic;
pub mod backend;
pub mod brain;
pub mod cli;
pub mod command;
pub mod complete;
pub mod config;
pub mod context;
pub mod error;
pub mod gate;
pub mod mcp;
pub mod plugin_tools;
pub mod price;
pub mod session;
pub mod tools;
pub mod wire;

pub use backend::{Availability, Backend, BackendChoice, BackendPrefs};
pub use brain::{Brain, BrainIo};
pub use config::{AgentConfig, Credential};
pub use context::ContextItem;
pub use error::AgentError;
pub use gate::{GateResult, ToolGate};
pub use session::{
    AgentEngine, ApprovalDesk, ApprovalOutcome, Attached, Entry, Input, PendingApproval,
    SessionEmitter, SessionEvent, SessionSnapshot, SessionStatus, SessionUi, ToolState,
};
pub use tools::{Tool, ToolPolicy};
pub use wire::{ModelCall, ModelTransport, ToolDefinition};
