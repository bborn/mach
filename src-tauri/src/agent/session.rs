//! Sessions: the loop, the state machine, and the registry that lets several
//! run at once.
//!
//! # Not modal
//!
//! Asking is not a dialog. [`AgentEngine::start`] registers a session, spawns a
//! task, and returns immediately with the snapshot the bottom bar draws as a
//! pill. Everything after that arrives as events, so the owner keeps reading
//! mail while it works, and several sessions can be in flight at once.
//!
//! # The shape, since the brain became swappable
//!
//! ```text
//!   start ──► resolve a backend ──► build the gate ──► hand a BrainIo to a brain
//!               (agent::backend)      (agent::gate)         (anthropic | cli | command)
//!                                          │
//!   agent_send ──► input pump ─────────────┘
//!                    │  Approve/Deny ──► ApprovalDesk (unparks the gate)
//!                    │  Message ───────► the brain's follow-up channel
//!                    └  Close ─────────► cancelled, and every wait ends
//! ```
//!
//! The pump exists because approval stopped being something the loop could do
//! inline. When the brain lives in another process, its tool calls arrive on the
//! MCP server's threads while this task is blocked on a child — so a decision
//! has to be routable to whatever is waiting for it, from anywhere. The desk is
//! that routing table, and it is the reason "the owner said yes" means the same
//! thing on all three backends.
//!
//! # Why the transcript is rebuilt rather than derived
//!
//! `entries` is what the drawer renders; each brain keeps whatever shape its own
//! wire needs. They are different — the drawer wants "Archived 3 conversations",
//! the Messages API wants a `tool_result` block — and keeping both is cheaper
//! than deriving one from the other on every render.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::commands::CommandDispatcher;
use crate::db::Db;
use crate::ipc::compose::engine::outbox::Outbox;

use super::backend::{self, Availability, Backend, BackendPrefs};
use super::brain::{brain_for, BrainIo};
use super::config::AgentConfig;
use super::context::{self, ContextItem};
use super::error::AgentError;
use super::gate::ToolGate;
use super::tools::{Artifact, ToolContext};
use super::wire::ModelTransport;

// ===========================================================================
// State
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    /// Thinking, streaming, or running a tool.
    Running,
    /// Parked on the owner: an outbound action is waiting for confirmation.
    AwaitingApproval,
    /// Finished its turn. Still open — another message resumes it.
    Done,
    Failed,
}

/// One line in the drawer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Entry {
    User {
        text: String,
    },
    Agent {
        text: String,
    },
    /// Upserted by `id` as it runs, so "Searching…" becomes "7 matches".
    Tool {
        id: String,
        name: String,
        summary: String,
        state: ToolState,
        /// What the call made, when it made something the owner can be put in
        /// front of. The drawer renders it as a button; see [`Artifact`].
        ///
        /// Absent on every read tool and on anything that only moved labels
        /// around, which is why it is skipped rather than sent as null.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<Artifact>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolState {
    Running,
    Ok,
    Error,
    /// The owner said no.
    Denied,
}

/// An outbound action waiting on the owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    /// The id a decision quotes back. The Anthropic backend uses the model's
    /// `tool_use` id; a backend without one is given a minted id.
    pub tool_use_id: String,
    pub name: String,
    /// One line: "Send \u{201c}Re: data room\u{201d} to tawny@\u{2026} on Tue 12 Aug, 09:00".
    pub summary: String,
    /// The full arguments, so the drawer can show exactly what would happen.
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub id: String,
    pub title: String,
    pub status: SessionStatus,
    pub created_at: i64,
    pub context: Vec<ContextItem>,
    pub entries: Vec<Entry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Which brain is answering — "Claude Code", "Anthropic API (claude-opus-5)".
    ///
    /// On the snapshot rather than in a preference read by the UI because it is
    /// a fact about *this session*: changing the preference mid-conversation
    /// must not relabel a session that is still being answered by the old one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

/// What the frontend hears. `sessionId` is on every one so a listener can route
/// without a wrapper.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SessionEvent {
    Created {
        session_id: String,
        session: SessionSnapshot,
    },
    Status {
        session_id: String,
        status: SessionStatus,
    },
    /// Tokens, as they arrive.
    Delta {
        session_id: String,
        text: String,
    },
    /// A completed line. Tool entries are upserted by id.
    Entry {
        session_id: String,
        entry: Entry,
    },
    Approval {
        session_id: String,
        pending: PendingApproval,
    },
    Context {
        session_id: String,
        context: Vec<ContextItem>,
    },
    Failed {
        session_id: String,
        message: String,
    },
    Closed {
        session_id: String,
    },
}

impl SessionEvent {
    pub fn session_id(&self) -> &str {
        match self {
            SessionEvent::Created { session_id, .. }
            | SessionEvent::Status { session_id, .. }
            | SessionEvent::Delta { session_id, .. }
            | SessionEvent::Entry { session_id, .. }
            | SessionEvent::Approval { session_id, .. }
            | SessionEvent::Context { session_id, .. }
            | SessionEvent::Failed { session_id, .. }
            | SessionEvent::Closed { session_id } => session_id,
        }
    }
}

/// Where events go. Tauri in production, a `Vec` in the tests.
pub trait SessionEmitter: Send + Sync {
    fn session_event(&self, event: &SessionEvent);
    /// The agent changed the mailbox; the UI's lists are stale.
    fn threads_changed(&self);
}

/// An emitter that drops everything — for a session run headless.
pub struct NullEmitter;

impl SessionEmitter for NullEmitter {
    fn session_event(&self, _event: &SessionEvent) {}
    fn threads_changed(&self) {}
}

/// What the owner sends into a running session.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Message(String),
    Approve { tool_use_id: String },
    Deny { tool_use_id: String, reason: Option<String> },
    Close,
}

// ===========================================================================
// The drawer, as something a brain can write to
// ===========================================================================

/// Everything a brain is allowed to put on screen.
///
/// One object rather than a handful of channels because every one of these
/// writes has to do the same two things — update the snapshot a reload would
/// read, and emit the event a live drawer is listening to — and a brain that
/// remembered one and forgot the other would produce a session that looked
/// right until the window was reopened.
pub struct SessionUi {
    id: String,
    snapshot: Arc<Mutex<SessionSnapshot>>,
    emitter: Arc<dyn SessionEmitter>,
}

impl SessionUi {
    /// Build one directly. The engine does this per session; a test does it to
    /// drive a gate without a whole engine around it.
    pub fn new(
        id: impl Into<String>,
        snapshot: Arc<Mutex<SessionSnapshot>>,
        emitter: Arc<dyn SessionEmitter>,
    ) -> SessionUi {
        SessionUi {
            id: id.into(),
            snapshot,
            emitter,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Tokens, as they arrive. Not stored: the completed entry supersedes them.
    pub fn delta(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.emit(SessionEvent::Delta {
            session_id: self.id.clone(),
            text: text.to_string(),
        });
    }

    pub fn agent_text(&self, text: &str) {
        self.push_entry(Entry::Agent {
            text: text.to_string(),
        });
    }

    pub fn user_text(&self, text: &str) {
        self.push_entry(Entry::User {
            text: text.to_string(),
        });
    }

    pub fn tool_running(&self, id: &str, name: &str, summary: &str) {
        self.push_entry(Entry::Tool {
            id: id.to_string(),
            name: name.to_string(),
            summary: summary.to_string(),
            state: ToolState::Running,
            artifact: None,
        });
    }

    pub fn tool_finished(&self, id: &str, name: &str, state: ToolState, summary: &str) {
        self.tool_produced(id, name, state, summary, None);
    }

    /// The same line, carrying what the call made.
    ///
    /// Separate from [`Self::tool_finished`] rather than a fifth argument on it
    /// because every failure path passes `None` and reads better without one;
    /// the gate is the only caller that has an artifact to hand over.
    pub fn tool_produced(
        &self,
        id: &str,
        name: &str,
        state: ToolState,
        summary: &str,
        artifact: Option<Artifact>,
    ) {
        self.push_entry(Entry::Tool {
            id: id.to_string(),
            name: name.to_string(),
            summary: summary.to_string(),
            state,
            artifact,
        });
    }

    pub fn threads_changed(&self) {
        self.emitter.threads_changed();
    }

    pub fn set_status(&self, status: SessionStatus) {
        {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status == status {
                return;
            }
            snapshot.status = status;
        }
        self.emit(SessionEvent::Status {
            session_id: self.id.clone(),
            status,
        });
    }

    /// The conversation so far, as plain text. Only what a person said and what
    /// the agent answered — tool lines are Mach's bookkeeping, not conversation.
    pub fn transcript(&self) -> String {
        let snapshot = lock(&self.snapshot);
        snapshot
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::User { text } => Some(format!("owner: {text}")),
                Entry::Agent { text } => Some(format!("agent: {text}")),
                Entry::Tool { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn push_entry(&self, entry: Entry) {
        {
            let mut snapshot = lock(&self.snapshot);
            upsert(&mut snapshot.entries, entry.clone());
        }
        self.emit(SessionEvent::Entry {
            session_id: self.id.clone(),
            entry,
        });
    }

    fn emit(&self, event: SessionEvent) {
        self.emitter.session_event(&event);
    }

    fn fail(&self, message: &str) {
        {
            let mut snapshot = lock(&self.snapshot);
            snapshot.status = SessionStatus::Failed;
            snapshot.pending = None;
            snapshot.error = Some(message.to_string());
        }
        self.emit(SessionEvent::Failed {
            session_id: self.id.clone(),
            message: message.to_string(),
        });
    }
}

// ===========================================================================
// Approval
// ===========================================================================

/// How a parked action ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Denied(String),
    /// The session was closed while it was parked. Nothing ran.
    Closed,
}

/// Where a tool call waits for a human, and how the answer finds it.
///
/// The desk holds one sender per parked call, keyed by the id the drawer shows.
/// [`ToolGate`](super::gate::ToolGate) serialises calls, so in practice there is
/// at most one — but keying by id rather than assuming that is what makes a
/// decision about a *different* call harmless instead of a mis-delivered yes.
pub struct ApprovalDesk {
    ui: Arc<SessionUi>,
    waiting: Mutex<HashMap<String, oneshot::Sender<ApprovalOutcome>>>,
    closed: AtomicBool,
}

impl ApprovalDesk {
    pub fn new(ui: Arc<SessionUi>) -> ApprovalDesk {
        ApprovalDesk {
            ui,
            waiting: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }
    }

    /// Park until the owner decides.
    ///
    /// Nothing has run when this is entered and nothing runs unless it returns
    /// [`ApprovalOutcome::Approved`]. This is the only function in the codebase
    /// that can say yes to sending mail.
    pub async fn ask(&self, pending: PendingApproval) -> ApprovalOutcome {
        if self.closed.load(Ordering::SeqCst) {
            return ApprovalOutcome::Closed;
        }

        let (tx, rx) = oneshot::channel();
        {
            let mut waiting = lock(&self.waiting);
            waiting.insert(pending.tool_use_id.clone(), tx);
        }

        {
            let mut snapshot = lock(&self.ui.snapshot);
            snapshot.pending = Some(pending.clone());
            snapshot.status = SessionStatus::AwaitingApproval;
        }
        let id = self.ui.id.clone();
        self.ui.emit(SessionEvent::Approval {
            session_id: id.clone(),
            pending: pending.clone(),
        });
        self.ui.emit(SessionEvent::Status {
            session_id: id.clone(),
            status: SessionStatus::AwaitingApproval,
        });

        // A dropped sender means the session was closed: silence is never
        // consent, so that path is `Closed`, not `Approved`.
        let outcome = rx.await.unwrap_or(ApprovalOutcome::Closed);

        lock(&self.waiting).remove(&pending.tool_use_id);
        {
            let mut snapshot = lock(&self.ui.snapshot);
            snapshot.pending = None;
            if snapshot.status == SessionStatus::AwaitingApproval {
                snapshot.status = SessionStatus::Running;
            }
        }
        if outcome != ApprovalOutcome::Closed {
            self.ui.emit(SessionEvent::Status {
                session_id: id,
                status: SessionStatus::Running,
            });
        }
        outcome
    }

    /// Deliver a decision. A decision for something that is not parked is
    /// dropped — a stale click on a prompt that has already been answered.
    pub fn decide(&self, tool_use_id: &str, outcome: ApprovalOutcome) {
        let sender = lock(&self.waiting).remove(tool_use_id);
        if let Some(sender) = sender {
            let _ = sender.send(outcome);
        }
    }

    /// Refuse everything, now and in future. Called when the session closes.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let waiting: Vec<_> = lock(&self.waiting).drain().collect();
        for (_, sender) in waiting {
            let _ = sender.send(ApprovalOutcome::Closed);
        }
    }
}

// ===========================================================================
// The engine
// ===========================================================================

struct Live {
    id: String,
    snapshot: Arc<Mutex<SessionSnapshot>>,
    tx: mpsc::UnboundedSender<Input>,
    cancelled: Arc<AtomicBool>,
}

/// Holds every session and the things they all share.
pub struct AgentEngine {
    db: Db,
    dispatcher: Arc<CommandDispatcher>,
    outbox: Arc<Outbox>,
    plugins: Arc<crate::plugins::PluginRuntime>,
    transport: Arc<dyn ModelTransport>,
    emitter: Arc<dyn SessionEmitter>,
    sessions: Mutex<Vec<Live>>,
    /// Injectable so tests are not at the mercy of the wall clock.
    now: fn() -> i64,
    /// Pins the model configuration instead of reading the environment. Set by
    /// the tests, and by anything that wants to run a session against a
    /// specific model without exporting a variable.
    config: Option<AgentConfig>,
    /// Pins the backend instead of detecting one.
    backend: Option<Backend>,
    /// Where a child process runs and where its tool-server config is written.
    workspace: PathBuf,
}

impl AgentEngine {
    pub fn new(
        db: Db,
        dispatcher: Arc<CommandDispatcher>,
        outbox: Arc<Outbox>,
        plugins: Arc<crate::plugins::PluginRuntime>,
        transport: Arc<dyn ModelTransport>,
        emitter: Arc<dyn SessionEmitter>,
    ) -> Self {
        AgentEngine {
            db,
            dispatcher,
            outbox,
            plugins,
            transport,
            emitter,
            sessions: Mutex::new(Vec::new()),
            now: crate::ipc::compose::now_ms,
            config: None,
            backend: None,
            workspace: std::env::temp_dir().join(format!("mach-agent-{}", std::process::id())),
        }
    }

    pub fn with_clock(mut self, now: fn() -> i64) -> Self {
        self.now = now;
        self
    }

    /// Pin the Anthropic configuration, and with it the backend.
    ///
    /// Naming a model, a base URL and a credential is only meaningful for the
    /// Messages API, so setting one *is* choosing that backend — a test that
    /// scripts SSE bytes must not find itself talking to whichever CLI happens
    /// to be installed on the machine running it.
    pub fn with_config(mut self, config: AgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Pin the backend, skipping detection entirely.
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Where child-process backends run. The app puts this next to the database;
    /// it defaults to a temporary directory so a test never has to.
    pub fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = workspace;
        self
    }

    /// Which brain would answer right now, and what else is available.
    ///
    /// Used by the preferences dialog, and by [`Self::start`] itself, so what
    /// the dialog reports and what actually runs cannot drift.
    pub fn resolve_backend(&self) -> (Result<Backend, AgentError>, Availability) {
        let available = Availability::probe();
        if let Some(pinned) = &self.backend {
            return (Ok(pinned.clone()), available);
        }
        if let Some(config) = &self.config {
            return (
                Ok(Backend::AnthropicApi(Box::new(config.clone()))),
                available,
            );
        }
        let prefs = BackendPrefs::load(&self.db);
        let resolved = backend::resolve(&prefs, &available, None);
        (resolved, available)
    }

    /// Open a session and start work. Returns as soon as it is registered — the
    /// answer arrives as events.
    ///
    /// The backend is resolved here rather than at boot, so installing Claude
    /// Code (or setting a key) costs a ⌘K rather than a relaunch, and having
    /// neither is a typed error on the one action that needs it.
    pub fn start(
        self: &Arc<Self>,
        prompt: String,
        context: Vec<ContextItem>,
    ) -> Result<SessionSnapshot, AgentError> {
        let backend = self.resolve_backend().0?;
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return Err(AgentError::invalid("ask the agent something first"));
        }

        let id = new_session_id((self.now)());
        let snapshot = SessionSnapshot {
            id: id.clone(),
            title: derive_title(&prompt),
            status: SessionStatus::Running,
            created_at: (self.now)(),
            context: context.clone(),
            entries: vec![Entry::User { text: prompt.clone() }],
            pending: None,
            error: None,
            backend: Some(backend.label()),
        };

        let shared = Arc::new(Mutex::new(snapshot.clone()));
        let (tx, rx) = mpsc::unbounded_channel();
        let cancelled = Arc::new(AtomicBool::new(false));

        lock(&self.sessions).push(Live {
            id: id.clone(),
            snapshot: Arc::clone(&shared),
            tx,
            cancelled: Arc::clone(&cancelled),
        });

        self.emitter.session_event(&SessionEvent::Created {
            session_id: id.clone(),
            session: snapshot.clone(),
        });

        let task = SessionTask {
            id: id.clone(),
            backend,
            db: self.db.clone(),
            tools: ToolContext {
                db: self.db.clone(),
                dispatcher: Arc::clone(&self.dispatcher),
                outbox: Arc::clone(&self.outbox),
                plugins: Arc::clone(&self.plugins),
            },
            transport: Arc::clone(&self.transport),
            ui: Arc::new(SessionUi {
                id: id.clone(),
                snapshot: shared,
                emitter: Arc::clone(&self.emitter),
            }),
            cancelled,
            now: self.now,
            workspace: self.workspace.clone(),
        };

        tokio::spawn(task.run(prompt, context, rx));
        Ok(snapshot)
    }

    /// Register a session whose thinking happens somewhere Mach does not drive.
    ///
    /// The handoff pane runs `claude` on a pty. That process is not a
    /// [`Brain`](super::brain::Brain) — nobody here feeds it messages or reads
    /// its output; the owner does, by typing at it. What it *does* share with
    /// every other session is the way its tool calls come back: over MCP, into
    /// [`ToolGate`], where `send_draft` parks on a human.
    ///
    /// Something has to render that prompt and route the answer, and the drawer
    /// already does both. So an attached session is a real session in this
    /// registry — a pill, a transcript of the tools it ran, an approval with
    /// Approve and Deny — with no task spawned to drive it. `agent_send` reaches
    /// it exactly as it reaches the others, because the pump is the same pump.
    ///
    /// The gate comes back so the caller can hand it to a server. Closing is the
    /// caller's job and [`AgentEngine::close`] is how, which is what the pane's
    /// reaping does.
    pub fn attach(&self, title: String, backend: String) -> Attached {
        let id = new_session_id((self.now)());
        let snapshot = SessionSnapshot {
            id: id.clone(),
            title,
            status: SessionStatus::Running,
            created_at: (self.now)(),
            context: Vec::new(),
            // Empty on purpose: the conversation is happening in the pane, and
            // an opening line here would be Mach putting words in its mouth.
            entries: Vec::new(),
            pending: None,
            error: None,
            backend: Some(backend),
        };

        let shared = Arc::new(Mutex::new(snapshot.clone()));
        let (tx, rx) = mpsc::unbounded_channel();
        let cancelled = Arc::new(AtomicBool::new(false));

        lock(&self.sessions).push(Live {
            id: id.clone(),
            snapshot: Arc::clone(&shared),
            tx,
            cancelled: Arc::clone(&cancelled),
        });

        self.emitter.session_event(&SessionEvent::Created {
            session_id: id.clone(),
            session: snapshot.clone(),
        });

        let ui = Arc::new(SessionUi::new(
            id.clone(),
            shared,
            Arc::clone(&self.emitter),
        ));
        let desk = Arc::new(ApprovalDesk::new(Arc::clone(&ui)));
        let ctx = ToolContext {
            db: self.db.clone(),
            dispatcher: Arc::clone(&self.dispatcher),
            outbox: Arc::clone(&self.outbox),
            plugins: Arc::clone(&self.plugins),
        };
        let plugins = ctx.plugin_list();
        let gate = Arc::new(ToolGate::new(
            ctx,
            plugins,
            Arc::clone(&ui),
            Arc::clone(&desk),
        ));

        let (messages_tx, messages_rx) = mpsc::unbounded_channel();
        tokio::spawn(pump(rx, desk, messages_tx, Arc::clone(&cancelled)));
        tokio::spawn(redirect(messages_rx, ui));

        Attached { snapshot, gate }
    }

    /// Every session, oldest first — the order the pills sit in.
    pub fn sessions(&self) -> Vec<SessionSnapshot> {
        lock(&self.sessions)
            .iter()
            .map(|live| lock(&live.snapshot).clone())
            .collect()
    }

    pub fn session(&self, id: &str) -> Option<SessionSnapshot> {
        lock(&self.sessions)
            .iter()
            .find(|live| live.id == id)
            .map(|live| lock(&live.snapshot).clone())
    }

    /// Hand something to a running session.
    pub fn send(&self, id: &str, input: Input) -> Result<(), AgentError> {
        let sessions = lock(&self.sessions);
        let live = sessions
            .iter()
            .find(|live| live.id == id)
            .ok_or_else(|| AgentError::UnknownSession(id.to_string()))?;
        live.tx
            .send(input)
            // The task ends when the model is done and the owner closes the
            // drawer; a send after that is a stale click, not a failure worth
            // a red banner.
            .map_err(|_| AgentError::UnknownSession(id.to_string()))
    }

    /// Close a session and forget it. Idempotent.
    pub fn close(&self, id: &str) -> Result<(), AgentError> {
        let mut sessions = lock(&self.sessions);
        let Some(index) = sessions.iter().position(|live| live.id == id) else {
            return Ok(());
        };
        let live = sessions.remove(index);
        live.cancelled.store(true, Ordering::SeqCst);
        let _ = live.tx.send(Input::Close);
        drop(sessions);
        self.emitter.session_event(&SessionEvent::Closed {
            session_id: id.to_string(),
        });
        Ok(())
    }

    /// Drop one attached item — the removable line in the session header.
    pub fn remove_context(&self, id: &str, item_id: &str) -> Result<Vec<ContextItem>, AgentError> {
        let sessions = lock(&self.sessions);
        let live = sessions
            .iter()
            .find(|live| live.id == id)
            .ok_or_else(|| AgentError::UnknownSession(id.to_string()))?;
        let context = {
            let mut snapshot = lock(&live.snapshot);
            snapshot.context.retain(|item| item.id != item_id);
            snapshot.context.clone()
        };
        drop(sessions);
        self.emitter.session_event(&SessionEvent::Context {
            session_id: id.to_string(),
            context: context.clone(),
        });
        Ok(context)
    }
}

// ===========================================================================
// The task
// ===========================================================================

struct SessionTask {
    id: String,
    backend: Backend,
    db: Db,
    tools: ToolContext,
    transport: Arc<dyn ModelTransport>,
    ui: Arc<SessionUi>,
    cancelled: Arc<AtomicBool>,
    now: fn() -> i64,
    workspace: PathBuf,
}

impl SessionTask {
    async fn run(
        self,
        prompt: String,
        context: Vec<ContextItem>,
        rx: mpsc::UnboundedReceiver<Input>,
    ) {
        let ui = Arc::clone(&self.ui);
        if let Err(error) = self.drive(prompt, context, rx).await {
            ui.fail(&error.to_string());
        }
    }

    async fn drive(
        self,
        prompt: String,
        context: Vec<ContextItem>,
        rx: mpsc::UnboundedReceiver<Input>,
    ) -> Result<(), AgentError> {
        // Read once per session rather than per turn: the tool list the brain
        // was given has to be the list its calls are checked against, and a
        // plugin installed mid-session must not change the rules underneath it.
        let plugins = self.tools.plugin_list();
        let system = context::system_prompt(&self.db, (self.now)(), !plugins.is_empty());
        let block = context::render(&self.db, &context)?;

        let desk = Arc::new(ApprovalDesk::new(Arc::clone(&self.ui)));
        let gate = Arc::new(ToolGate::new(
            self.tools,
            plugins,
            Arc::clone(&self.ui),
            Arc::clone(&desk),
        ));

        let (messages_tx, messages_rx) = mpsc::unbounded_channel();
        tokio::spawn(pump(
            rx,
            Arc::clone(&desk),
            messages_tx,
            Arc::clone(&self.cancelled),
        ));

        let io = BrainIo {
            session_id: self.id.clone(),
            system,
            first_message: format!("{block}{prompt}"),
            gate,
            ui: Arc::clone(&self.ui),
            messages: messages_rx,
            cancelled: Arc::clone(&self.cancelled),
            workspace: self.workspace,
        };

        let brain = brain_for(self.backend, self.transport);
        let result = brain.drive(io).await;

        // Whatever happened, nothing may stay parked: a session that ended with
        // a prompt still on screen would be an approval nobody can answer.
        desk.close();
        result
    }
}

/// Route what the owner sends to whoever is waiting for it.
///
/// One task per session, and it is the only reader of the input channel. It ends
/// when the channel closes or a [`Input::Close`] arrives, and its ending is what
/// tells a brain waiting on [`BrainIo::idle`] that the session is over.
async fn pump(
    mut rx: mpsc::UnboundedReceiver<Input>,
    desk: Arc<ApprovalDesk>,
    messages: mpsc::UnboundedSender<String>,
    cancelled: Arc<AtomicBool>,
) {
    while let Some(input) = rx.recv().await {
        match input {
            Input::Approve { tool_use_id } => {
                desk.decide(&tool_use_id, ApprovalOutcome::Approved);
            }
            Input::Deny { tool_use_id, reason } => {
                let reason = reason.unwrap_or_else(|| "No reason given.".to_string());
                desk.decide(&tool_use_id, ApprovalOutcome::Denied(reason));
            }
            Input::Message(text) => {
                if messages.send(text).is_err() {
                    return;
                }
            }
            Input::Close => break,
        }
    }
    cancelled.store(true, Ordering::SeqCst);
    desk.close();
}

/// A session whose brain is somewhere else, and the door its tools come in by.
pub struct Attached {
    pub snapshot: SessionSnapshot,
    pub gate: Arc<ToolGate>,
}

/// Say where a message typed at an attached session should have gone.
///
/// The drawer offers a text box for every session because every session until
/// now could take one. This one cannot: the process is on a pty and the only
/// thing it reads is the pane. Answering is better than dropping the sentence —
/// it is the one fact he could not otherwise work out from what is on screen.
async fn redirect(mut rx: mpsc::UnboundedReceiver<String>, ui: Arc<SessionUi>) {
    while let Some(text) = rx.recv().await {
        ui.user_text(&text);
        ui.agent_text("This session is running in the pane. Type there.");
    }
}

/// Tool entries replace themselves by id; everything else appends.
fn upsert(entries: &mut Vec<Entry>, entry: Entry) {
    if let Entry::Tool { id, .. } = &entry {
        if let Some(slot) = entries.iter_mut().find(
            |existing| matches!(existing, Entry::Tool { id: existing_id, .. } if existing_id == id),
        ) {
            *slot = entry;
            return;
        }
    }
    entries.push(entry);
}

// ===========================================================================
// Wording
// ===========================================================================

/// The pill's label: the ask, trimmed to something that fits.
pub fn derive_title(prompt: &str) -> String {
    const MAX: usize = 48;
    let first = prompt
        .trim()
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Agent")
        .trim();
    // One sentence keeps its punctuation — "what is in my inbox?" is a
    // question, and a pill that drops the question mark reads like a claim.
    // Several sentences are cut after the first.
    let source = match first.find(['.', '?', '!']) {
        Some(end) if first[end + 1..].trim().is_empty() => first,
        Some(end) => first[..=end].trim(),
        None => first,
    };
    let source = if source.is_empty() { first } else { source };

    let mut title: String = if source.chars().count() <= MAX {
        source.to_string()
    } else {
        let head: String = source.chars().take(MAX).collect();
        // Cut on a word boundary when there is one nearby, so the pill does not
        // read "Reply to Tawny about the data ro…".
        match head.rfind(' ') {
            Some(space) if space > MAX / 2 => format!("{}…", &head[..space]),
            _ => format!("{head}…"),
        }
    };

    if let Some(first) = title.chars().next() {
        if first.is_lowercase() {
            title = first.to_uppercase().collect::<String>() + &title[first.len_utf8()..];
        }
    }
    title
}

/// Unique within a run, meaningless outside it.
fn new_session_id(now: i64) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("agent-{now:x}-{n:x}")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
