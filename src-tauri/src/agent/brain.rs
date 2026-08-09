//! The seam a brain plugs into.
//!
//! A *brain* is the thing that decides what to do about a sentence. It is
//! deliberately the only part of the agent that is swappable: the tools, the
//! approval gate, the context block, the transcript and the events are all
//! Mach's, identical whichever brain is thinking, and a brain reaches the
//! mailbox through exactly one door ([`ToolGate::run`]).
//!
//! That is what makes "use whatever agent you like" a safe offer rather than a
//! frightening one. Swapping the brain changes who decides; it cannot change
//! what is permitted, what is undoable, or what has to be confirmed.
//!
//! # The contract
//!
//! A brain is handed a [`BrainIo`] and must:
//!
//! - answer the first message, emitting text through [`SessionUi::delta`] as it
//!   arrives (or in one lump, for a backend that cannot stream);
//! - call tools *only* through [`BrainIo::gate`];
//! - when it has nothing more to say, park on [`BrainIo::idle`] — which marks
//!   the session done and waits for the owner's next message — and answer that
//!   the same way;
//! - return when [`BrainIo::idle`] returns `None`, which means the session was
//!   closed;
//! - check [`BrainIo::cancelled`] around anything slow, so ⌘W is not a lie.
//!
//! Returning `Err` fails the session visibly, with the sentence the drawer shows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::google::BoxFuture;

use super::backend::Backend;
use super::error::AgentError;
use super::gate::ToolGate;
use super::session::{SessionStatus, SessionUi};
use super::wire::ModelTransport;

/// Everything a brain gets, and everything it is allowed to touch.
pub struct BrainIo {
    pub session_id: String,
    /// Mach's system prompt — who you are, what time it is, whose mail this is.
    pub system: String,
    /// The `<context>` block and the owner's sentence, already joined.
    pub first_message: String,
    /// The only path to the mailbox.
    pub gate: Arc<ToolGate>,
    /// Text out, tool lines, status.
    pub ui: Arc<SessionUi>,
    /// Follow-up messages typed into an open session.
    pub messages: mpsc::UnboundedReceiver<String>,
    pub cancelled: Arc<AtomicBool>,
    /// Where a backend that spawns a process should run it, and where its
    /// tool-server configuration is written. Owned by Mach, next to the
    /// database — never the user's home directory or a repository.
    pub workspace: std::path::PathBuf,
}

impl BrainIo {
    /// Mark the session finished-for-now and wait for the owner.
    ///
    /// `None` means the session is over. "Done" is not "closed": the drawer
    /// stays open and another message resumes the same conversation, which is
    /// why this is a wait rather than a return.
    pub async fn idle(&mut self) -> Option<String> {
        self.ui.set_status(SessionStatus::Done);
        let next = self.messages.recv().await?;
        if self.is_cancelled() {
            return None;
        }
        self.ui.set_status(SessionStatus::Running);
        Some(next)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// One brain.
///
/// Object-safe and future-boxing for the same reason [`ModelTransport`] is:
/// there are three implementations with wildly different shapes — an HTTPS
/// stream, a child process, someone else's program — and the session loop must
/// not know which it has.
pub trait Brain: Send + Sync {
    fn drive<'a>(&'a self, io: BrainIo) -> BoxFuture<'a, Result<(), AgentError>>;
}

/// Build the brain a resolved backend names.
///
/// The transport is only meaningful to the Anthropic backend; passing it here
/// rather than storing it in the backend keeps [`Backend`] a plain value that a
/// test can construct and compare.
pub fn brain_for(backend: Backend, transport: Arc<dyn ModelTransport>) -> Box<dyn Brain> {
    match backend {
        Backend::AnthropicApi(config) => {
            Box::new(super::anthropic::AnthropicBrain::new(*config, transport))
        }
        Backend::ClaudeCli { exe, model } => {
            Box::new(super::cli::ClaudeCliBrain::new(exe, model))
        }
        Backend::Command { program, args } => {
            Box::new(super::command::CommandBrain::new(program, args))
        }
    }
}
