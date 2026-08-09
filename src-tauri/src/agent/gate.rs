//! The gate: the one place a tool call becomes an action.
//!
//! Before there were several brains there was one loop, and the loop checked the
//! policy itself. That was fine while the model lived at the other end of an
//! HTTPS request Mach had written by hand. It stops being fine the moment the
//! thinking happens in *someone else's process* — the Claude Code CLI has its
//! own permission system, its own `--permission-mode`, and a
//! `--dangerously-skip-permissions` flag, and none of those are Mach's to trust.
//!
//! So the rule is: **the CLI does not decide anything.** A tool call from any
//! backend arrives here, and here is where it meets
//!
//! 1. the tool list — a name that is not in the surface this session was given
//!    is refused before it is looked at, so no backend can widen the surface by
//!    asking nicely;
//! 2. the policy — [`ToolPolicy::Approve`] parks on the owner, in Mach's own
//!    window, and nothing runs until a human clicks;
//! 3. the command layer — the same [`CommandDispatcher`] the keyboard uses.
//!
//! [`CommandDispatcher`]: crate::commands::CommandDispatcher
//!
//! A backend that skipped the gate would not "bypass approval"; it would have no
//! way to reach the mailbox at all, because the gate *is* the path. The MCP
//! server the CLI talks to (see [`super::mcp`]) has no other entry point, and
//! `tests/agent_backends.rs` pins that: a backend handed a raw tool call still
//! gets a parked session and an empty outbox.
//!
//! # One tool at a time
//!
//! [`ToolGate::run`] takes a lock for the whole call, including the wait for a
//! human. That is not a performance decision — a session shows **one**
//! [`PendingApproval`], and two tools parked at once would mean the second
//! silently overwrote the first's prompt. Serialising here keeps "what you are
//! looking at is what you are approving" true no matter how many calls a model
//! fires in parallel.

use std::sync::Arc;

use serde_json::Value;

use crate::plugins::InstalledPlugin;

use super::error::AgentError;
use super::session::{ApprovalOutcome, ApprovalDesk, PendingApproval, SessionUi, ToolState};
use super::tools::{self, Tool, ToolContext, ToolOutcome, ToolPolicy};
use super::wire::ToolDefinition;

/// What one gated call came to.
pub enum GateResult {
    Ok(ToolOutcome),
    /// The call did not run, and the reason is safe to hand back to the model —
    /// a denial, a bad argument, a thread that no longer exists. The session
    /// continues; the model corrects itself or says what it would have done.
    Refused(String),
    /// The owner closed the session while it was parked. Nothing ran.
    Closed,
    /// The session cannot continue: a dead credential, an unreachable store.
    Fatal(AgentError),
}

impl GateResult {
    /// The text a backend hands back as the tool result, and whether it is an
    /// error. `Closed` has no text because nobody is listening any more.
    pub fn as_tool_result(&self) -> Option<(String, bool)> {
        match self {
            GateResult::Ok(outcome) => Some((outcome.payload.to_string(), false)),
            GateResult::Refused(message) => Some((message.clone(), true)),
            GateResult::Closed | GateResult::Fatal(_) => None,
        }
    }
}

pub struct ToolGate {
    ctx: ToolContext,
    /// Read once when the session starts. A plugin installed mid-session must
    /// not change the rules a running session is being judged by — the list the
    /// model was given has to be the list its calls are checked against.
    plugins: Vec<InstalledPlugin>,
    tools: Vec<Tool>,
    ui: Arc<SessionUi>,
    desk: Arc<ApprovalDesk>,
    turn: tokio::sync::Mutex<()>,
}

impl ToolGate {
    pub fn new(
        ctx: ToolContext,
        plugins: Vec<InstalledPlugin>,
        ui: Arc<SessionUi>,
        desk: Arc<ApprovalDesk>,
    ) -> ToolGate {
        let tools = tools::tools_with(&plugins);
        ToolGate {
            ctx,
            plugins,
            tools,
            ui,
            desk,
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// The whole surface, which is the command catalogue plus the local reads
    /// plus the composer plus whatever plugins are installed — and nothing else.
    /// Every backend advertises exactly this.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|t| t.definition.clone()).collect()
    }

    pub fn plugins(&self) -> &[InstalledPlugin] {
        &self.plugins
    }

    /// The policy for a name, or `None` when the name is not in the surface.
    pub fn policy(&self, name: &str) -> Option<ToolPolicy> {
        self.tools
            .iter()
            .find(|t| t.definition.name == name)
            .map(|t| t.policy)
    }

    /// Run one tool call, with everything Mach owes the owner around it.
    ///
    /// `call_id` identifies this call in the drawer and in an approval decision.
    /// The Anthropic backend passes the model's `tool_use` id; a backend that
    /// has no such thing mints one — the id is opaque to everything except the
    /// round trip through the UI.
    pub async fn run(&self, call_id: &str, name: &str, input: &Value) -> GateResult {
        let _turn = self.turn.lock().await;

        let Some(policy) = self.policy(name) else {
            // Not "unknown tool, ignoring". A backend asking for something
            // outside the surface is either confused or trying, and both get the
            // same flat no.
            let message = format!(
                "{name} is not one of Mach's tools. The agent can only use the tools it was given."
            );
            self.ui
                .tool_finished(call_id, name, ToolState::Error, &message);
            return GateResult::Refused(message);
        };

        self.ui.tool_running(call_id, name, &self.running_summary(name));

        if policy == ToolPolicy::Approve {
            let pending = PendingApproval {
                tool_use_id: call_id.to_string(),
                name: name.to_string(),
                summary: self.approval_summary(name, input),
                input: input.clone(),
            };
            match self.desk.ask(pending).await {
                ApprovalOutcome::Approved => {}
                ApprovalOutcome::Denied(reason) => {
                    self.ui
                        .tool_finished(call_id, name, ToolState::Denied, &reason);
                    return GateResult::Refused(format!(
                        "The owner declined this action. {reason}"
                    ));
                }
                ApprovalOutcome::Closed => return GateResult::Closed,
            }
        }

        match tools::execute(&self.ctx, name, input).await {
            Ok(outcome) => {
                // The artifact rides the completed line: whatever the call
                // made, the drawer now has a way to open it.
                self.ui.tool_produced(
                    call_id,
                    name,
                    ToolState::Ok,
                    &outcome.summary,
                    outcome.artifact.clone(),
                );
                if outcome.mutated {
                    self.ui.threads_changed();
                }
                GateResult::Ok(outcome)
            }
            Err(error) if error.is_recoverable_by_model() => {
                let message = error.to_string();
                self.ui
                    .tool_finished(call_id, name, ToolState::Error, &message);
                GateResult::Refused(message)
            }
            // A missing credential or a dead transport is not something the
            // model can work around.
            Err(error) => GateResult::Fatal(error),
        }
    }

    /// What a tool call says while it is running.
    ///
    /// By name only: on the streaming backend the arguments have not finished
    /// arriving when this is written, and a line that changes shape depending on
    /// which backend produced it would be a tell.
    pub fn running_summary(&self, name: &str) -> String {
        super::plugin_tools::running_summary(&self.plugins, name)
            .unwrap_or_else(|| running_summary(name))
    }

    /// The sentence the owner approves. It has to name the consequence — "Send"
    /// and "to whom" and "when" — because that is the whole point of asking.
    pub fn approval_summary(&self, name: &str, input: &Value) -> String {
        if let Some(summary) = super::plugin_tools::approval_summary(&self.plugins, name) {
            return summary;
        }
        if name != tools::SEND_TOOL {
            return format!("Run {name}");
        }

        let draft_id = input.get("draftId").and_then(Value::as_str).unwrap_or_default();
        let draft = crate::ipc::compose::engine::draft::load_draft(&self.ctx.db, draft_id)
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
                super::context::human_time(at)
            ),
            None => format!("Send \u{201c}{subject}\u{201d} to {to} now"),
        }
    }
}

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
