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
//! # The loop
//!
//! ```text
//!   stream a turn ──► text? emit deltas
//!         │
//!         ├─ no tool calls ──► idle: wait for the next message (or close)
//!         │
//!         └─ tool calls ──► for each:
//!                              Auto     → run it now
//!                              Approve  → park, tell the UI, wait for a decision
//!                           results go back in ONE user message
//! ```
//!
//! Parking on approval is a real state, not a modal: the session sits in
//! [`SessionStatus::AwaitingApproval`] and the task is blocked on its input
//! channel. Nothing has been queued, nothing has left. A denial comes back to
//! the model as a tool error, so it can say what it would have done instead of
//! dying.
//!
//! # Why the transcript is rebuilt rather than derived
//!
//! `entries` is what the drawer renders; `messages` is what the API sees. They
//! are different shapes — the drawer wants "Archived 3 conversations", the API
//! wants a `tool_result` block — and keeping both is cheaper than deriving one
//! from the other on every render.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::commands::CommandDispatcher;
use crate::db::Db;
use crate::ipc::compose::engine::outbox::Outbox;

use super::config::AgentConfig;
use super::context::{self, ContextItem};
use super::error::AgentError;
use super::tools::{self, ToolContext, ToolPolicy};
use super::wire::{
    self, AssistantTurn, ModelTransport, SseDecoder, StreamSignal, ToolDefinition, TurnAccumulator,
    TurnRequest,
};

/// How many model turns one session may take before it is stopped.
///
/// A session that has called forty tools without answering is stuck, and the
/// owner is paying for it. Generous enough that "search, read three threads,
/// draft, schedule" never comes close.
const MAX_TURNS: usize = 24;

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
    /// The model's `tool_use` id — what a decision quotes back.
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
        }
    }

    pub fn with_clock(mut self, now: fn() -> i64) -> Self {
        self.now = now;
        self
    }

    pub fn with_config(mut self, config: AgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Open a session and start work. Returns as soon as it is registered — the
    /// answer arrives as events.
    ///
    /// The credential is resolved here rather than at boot, so a missing
    /// `ANTHROPIC_API_KEY` is a typed error on the one action that needs it and
    /// never a failed launch.
    pub fn start(
        self: &Arc<Self>,
        prompt: String,
        context: Vec<ContextItem>,
    ) -> Result<SessionSnapshot, AgentError> {
        let config = match &self.config {
            Some(pinned) => pinned.clone(),
            None => AgentConfig::load()?,
        };
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
            config,
            db: self.db.clone(),
            tools: ToolContext {
                db: self.db.clone(),
                dispatcher: Arc::clone(&self.dispatcher),
                outbox: Arc::clone(&self.outbox),
                plugins: Arc::clone(&self.plugins),
            },
            transport: Arc::clone(&self.transport),
            emitter: Arc::clone(&self.emitter),
            snapshot: shared,
            rx,
            cancelled,
            now: self.now,
        };

        tokio::spawn(task.run(prompt, context));
        Ok(snapshot)
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
    config: AgentConfig,
    db: Db,
    tools: ToolContext,
    transport: Arc<dyn ModelTransport>,
    emitter: Arc<dyn SessionEmitter>,
    snapshot: Arc<Mutex<SessionSnapshot>>,
    rx: mpsc::UnboundedReceiver<Input>,
    cancelled: Arc<AtomicBool>,
    now: fn() -> i64,
}

impl SessionTask {
    async fn run(mut self, prompt: String, context: Vec<ContextItem>) {
        if let Err(error) = self.drive(prompt, context).await {
            self.fail(error);
        }
    }

    async fn drive(
        &mut self,
        prompt: String,
        context: Vec<ContextItem>,
    ) -> Result<(), AgentError> {
        // Read once per session rather than per turn: the tool list the model
        // was given has to be the list its calls are checked against, and a
        // plugin installed mid-session must not change the rules underneath it.
        let plugins = self.tools.plugin_list();
        let system = context::system_prompt(&self.db, (self.now)(), !plugins.is_empty());
        let tool_defs: Vec<ToolDefinition> = tools::tools_with(&plugins)
            .into_iter()
            .map(|t| t.definition)
            .collect();

        let block = context::render(&self.db, &context)?;
        let mut messages = vec![wire::user_text(format!("{block}{prompt}"))];

        for _ in 0..MAX_TURNS {
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(());
            }
            self.set_status(SessionStatus::Running);

            let request = TurnRequest {
                system: system.clone(),
                messages: messages.clone(),
                tools: tool_defs.clone(),
            };
            let turn = self.stream_turn(&request, &plugins).await?;

            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(());
            }

            if turn.is_refusal() {
                return Err(AgentError::Api {
                    status: 200,
                    message: "the model declined this request".to_string(),
                });
            }

            messages.push(wire::assistant_message(&turn.content));

            let text = turn.text();
            if !text.trim().is_empty() {
                self.push_entry(Entry::Agent { text: text.trim().to_string() });
            }

            if !turn.wants_tools() {
                // Idle, not finished: the drawer stays open and another message
                // resumes the same conversation.
                self.set_status(SessionStatus::Done);
                match self.rx.recv().await {
                    Some(Input::Message(next)) => {
                        self.push_entry(Entry::User { text: next.clone() });
                        messages.push(wire::user_text(next));
                        continue;
                    }
                    // A decision with nothing pending, or a close: we are done.
                    _ => return Ok(()),
                }
            }

            let results = self.run_tools(&turn, &plugins).await?;
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(());
            }
            messages.push(wire::tool_results_message(results));
        }

        Err(AgentError::invalid(
            "the agent kept working without reaching an answer, so it was stopped",
        ))
    }

    /// One model turn, streamed. Emits deltas as they arrive.
    async fn stream_turn(
        &mut self,
        request: &TurnRequest,
        plugins: &[crate::plugins::InstalledPlugin],
    ) -> Result<AssistantTurn, AgentError> {
        let mut rx = match self
            .transport
            .send(wire::build_call(&self.config, request, self.config.fallbacks))
            .await
        {
            Ok(rx) => rx,
            // An account without the fallback beta must still get an agent.
            Err(error) if self.config.fallbacks && wire::is_fallback_rejection(&error) => {
                self.transport
                    .send(wire::build_call(&self.config, request, false))
                    .await?
            }
            Err(error) => return Err(error),
        };

        let mut decoder = SseDecoder::new();
        let mut accumulator = TurnAccumulator::new();

        while let Some(chunk) = rx.recv().await {
            if self.cancelled.load(Ordering::SeqCst) {
                break;
            }
            for payload in decoder.push(&chunk?) {
                for signal in accumulator.apply(&payload)? {
                    match signal {
                        StreamSignal::TextDelta(text) => {
                            self.emit(SessionEvent::Delta {
                                session_id: self.id.clone(),
                                text,
                            });
                        }
                        StreamSignal::ToolStarted { id, name } => {
                            // Attributed the moment it starts: the owner has to
                            // be able to see *which third party* is touching
                            // their mailbox, not just that something is.
                            let summary = super::plugin_tools::running_summary(&plugins, &name)
                                .unwrap_or_else(|| running_summary(&name));
                            self.push_entry(Entry::Tool {
                                id,
                                summary,
                                name,
                                state: ToolState::Running,
                            });
                        }
                        StreamSignal::Done => {}
                    }
                }
            }
        }

        Ok(accumulator.finish())
    }

    /// Execute every tool the turn asked for, gating the outbound ones.
    async fn run_tools(
        &mut self,
        turn: &AssistantTurn,
        plugins: &[crate::plugins::InstalledPlugin],
    ) -> Result<Vec<Value>, AgentError> {
        let mut results = Vec::new();

        for call in turn.tool_uses() {
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(results);
            }

            if tools::policy_for_with(&call.name, plugins) == ToolPolicy::Approve {
                match self.await_approval(&call, plugins).await {
                    Approval::Approved => {}
                    Approval::Denied(reason) => {
                        self.update_tool(&call.id, ToolState::Denied, &reason);
                        results.push(wire::tool_result(
                            &call.id,
                            &format!("The owner declined this action. {reason}"),
                            true,
                        ));
                        continue;
                    }
                    Approval::Closed => return Ok(results),
                }
            }

            match tools::execute(&self.tools, &call.name, &call.input).await {
                Ok(outcome) => {
                    self.update_tool(&call.id, ToolState::Ok, &outcome.summary);
                    if outcome.mutated {
                        self.emitter.threads_changed();
                    }
                    results.push(wire::tool_result(
                        &call.id,
                        &outcome.payload.to_string(),
                        false,
                    ));
                }
                Err(error) if error.is_recoverable_by_model() => {
                    self.update_tool(&call.id, ToolState::Error, &error.to_string());
                    results.push(wire::tool_result(&call.id, &error.to_string(), true));
                }
                // A missing credential or a dead transport is not something the
                // model can work around.
                Err(error) => return Err(error),
            }
        }

        Ok(results)
    }

    /// Park until the owner decides. Nothing has run when this is entered and
    /// nothing runs if it returns anything but [`Approval::Approved`].
    async fn await_approval(
        &mut self,
        call: &wire::ToolUse,
        plugins: &[crate::plugins::InstalledPlugin],
    ) -> Approval {
        let pending = PendingApproval {
            tool_use_id: call.id.clone(),
            name: call.name.clone(),
            summary: approval_summary(&self.tools, plugins, &call.name, &call.input),
            input: call.input.clone(),
        };

        {
            let mut snapshot = lock(&self.snapshot);
            snapshot.pending = Some(pending.clone());
            snapshot.status = SessionStatus::AwaitingApproval;
        }
        self.emit(SessionEvent::Approval {
            session_id: self.id.clone(),
            pending,
        });
        self.emit(SessionEvent::Status {
            session_id: self.id.clone(),
            status: SessionStatus::AwaitingApproval,
        });

        let decision = loop {
            match self.rx.recv().await {
                Some(Input::Approve { tool_use_id }) if tool_use_id == call.id => {
                    break Approval::Approved
                }
                Some(Input::Deny { tool_use_id, reason }) if tool_use_id == call.id => {
                    break Approval::Denied(
                        reason.unwrap_or_else(|| "No reason given.".to_string()),
                    )
                }
                // A decision about some other tool call, or a message typed
                // while the drawer was open: keep waiting rather than treating
                // silence as consent.
                Some(_) => continue,
                None => break Approval::Closed,
            }
        };

        {
            let mut snapshot = lock(&self.snapshot);
            snapshot.pending = None;
            snapshot.status = SessionStatus::Running;
        }
        self.emit(SessionEvent::Status {
            session_id: self.id.clone(),
            status: SessionStatus::Running,
        });
        decision
    }

    // ------------------------------------------------------------------ state

    fn emit(&self, event: SessionEvent) {
        self.emitter.session_event(&event);
    }

    fn set_status(&self, status: SessionStatus) {
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

    fn update_tool(&self, id: &str, state: ToolState, summary: &str) {
        let existing = {
            let snapshot = lock(&self.snapshot);
            snapshot.entries.iter().find_map(|entry| match entry {
                Entry::Tool { id: eid, name, .. } if eid == id => Some(name.clone()),
                _ => None,
            })
        };
        self.push_entry(Entry::Tool {
            id: id.to_string(),
            name: existing.unwrap_or_else(|| "tool".to_string()),
            summary: summary.to_string(),
            state,
        });
    }

    fn fail(&self, error: AgentError) {
        let message = error.to_string();
        {
            let mut snapshot = lock(&self.snapshot);
            snapshot.status = SessionStatus::Failed;
            snapshot.pending = None;
            snapshot.error = Some(message.clone());
        }
        self.emit(SessionEvent::Failed {
            session_id: self.id.clone(),
            message,
        });
    }
}

enum Approval {
    Approved,
    Denied(String),
    Closed,
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

/// What a tool call says while it is running. The model's own arguments have
/// not finished streaming at this point, so this is by name only.
fn running_summary(name: &str) -> String {
    match name {
        "search_threads" => "Searching mail…".to_string(),
        "get_thread" => "Reading the conversation…".to_string(),
        "list_threads" => "Listing conversations…".to_string(),
        "list_events" => "Checking the calendar…".to_string(),
        "list_labels" | "list_accounts" => "Looking things up…".to_string(),
        tools::DRAFT_TOOL => "Writing a reply…".to_string(),
        tools::SEND_TOOL => "Ready to send…".to_string(),
        other => format!("{other}…"),
    }
}

/// The sentence the owner approves. It has to name the consequence — "Send" and
/// "to whom" and "when" — because that is the whole point of asking.
fn approval_summary(
    ctx: &ToolContext,
    plugins: &[crate::plugins::InstalledPlugin],
    name: &str,
    input: &Value,
) -> String {
    if let Some(summary) = super::plugin_tools::approval_summary(plugins, name) {
        return summary;
    }
    if name != tools::SEND_TOOL {
        return format!("Run {name}");
    }

    let draft_id = input.get("draftId").and_then(Value::as_str).unwrap_or_default();
    let draft = crate::ipc::compose::engine::draft::load_draft(&ctx.db, draft_id)
        .ok()
        .flatten();

    let (subject, to) = match &draft {
        Some(draft) => (
            draft.subject.clone(),
            draft
                .to
                .iter()
                .map(|m| m.email.clone())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        None => (String::from("(draft not found)"), String::new()),
    };

    match input.get("sendAt").and_then(Value::as_i64) {
        Some(at) => format!(
            "Send \u{201c}{subject}\u{201d} to {to} on {}",
            context::human_time(at)
        ),
        None => format!("Send \u{201c}{subject}\u{201d} to {to} now"),
    }
}

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
