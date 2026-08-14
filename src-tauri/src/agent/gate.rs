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
//! # One session, a bounded amount of damage
//!
//! Every auto command is undoable, and for a long time that was the whole
//! answer. It is not, for two reasons that only show up in the aggregate: ⌘Z
//! undoes *one* command, and a hundred archives is a hundred commands; and a
//! mailbox that quietly reorganised itself while he was reading one thread is a
//! mess whether or not each step had an inverse. A message that talks the model
//! into "file everything from this sender, and everything that looks like an
//! invoice, and…" costs nothing to write.
//!
//! So [`ToolGate`] counts conversations. [`WRITE_BUDGET`] of them may move
//! without asking; the call that would take a session past it parks on the owner
//! instead, naming the number. He is not refused anything — "archive these two
//! hundred newsletters" is a real request and it costs one prompt — but nothing
//! reaches two hundred without him seeing the figure.
//!
//! # One tool at a time
//!
//! [`ToolGate::run`] takes a lock for the whole call, including the wait for a
//! human. That is not a performance decision — a session shows **one**
//! [`PendingApproval`], and two tools parked at once would mean the second
//! silently overwrote the first's prompt. Serialising here keeps "what you are
//! looking at is what you are approving" true no matter how many calls a model
//! fires in parallel.

use std::sync::atomic::{AtomicUsize, Ordering};
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

/// How many conversations one session may move before it starts asking.
///
/// Twenty-five is "more than any sentence he would type by hand implies, and
/// far short of a mailbox". A number is arbitrary; having one is not.
pub const WRITE_BUDGET: usize = 25;

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
    /// Conversations this session has moved. Only ever grows: once a session has
    /// spent its budget every further write asks, which is the point.
    spent: AtomicUsize,
    /// Addresses he has actually seen: the ones he typed into ⌘K, and the people
    /// already in the conversations he attached. See [`Self::unexpected`].
    expected: Vec<String>,
}

/// What one call costs against [`WRITE_BUDGET`].
///
/// Reads cost nothing. A batch costs the number of conversations it names,
/// because that is the number of things that moved — one call with two hundred
/// ids is not one unit of surprise. Anything else that mutates costs one.
///
/// Free-standing and taking only the call, so the rule can be read and tested
/// without a database behind it.
pub fn write_cost(name: &str, policy: ToolPolicy, input: &Value) -> usize {
    if policy == ToolPolicy::Approve {
        // It is going to ask anyway; charging for it would make a session that
        // asked politely twenty-five times start asking twice.
        return 0;
    }
    match name {
        "list_threads" | "search_threads" | "get_thread" | "list_events" | "list_labels"
        | "list_accounts" | tools::LIST_CALENDARS_TOOL | tools::GET_EVENT_TOOL
        | tools::LIST_FILTERS_TOOL => 0,
        // A draft is local, unsent and one object. It is a write, but it is not
        // a conversation moving.
        tools::DRAFT_TOOL | tools::NEW_DRAFT_TOOL => 1,
        _ => input
            .get("threadIds")
            .and_then(Value::as_array)
            .map(|ids| ids.len())
            .unwrap_or(1)
            .max(1),
    }
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
            spent: AtomicUsize::new(0),
            expected: Vec::new(),
        }
    }

    /// The addresses this session should not be surprised by.
    ///
    /// A builder rather than a sixth argument to [`ToolGate::new`]: everything
    /// that constructs a gate does so to run tools, and only the session knows
    /// what he typed.
    pub fn expecting(mut self, addresses: Vec<String>) -> ToolGate {
        self.expected = addresses
            .into_iter()
            .map(|a| a.trim().to_lowercase())
            .filter(|a| !a.is_empty())
            .collect();
        self
    }

    /// Which of these recipients he has never seen.
    ///
    /// The distinction that matters is not "is this address in the prompt
    /// somewhere" — an exfiltration address usually *is*, because it was in the
    /// body of the message that asked for it. It is: did **he** name it, or is
    /// this person already in the conversation? An address that appears only in
    /// the text of somebody's email is the one to say out loud.
    ///
    /// This is a fact computed from the session, not an opinion from the model,
    /// and it is still only a line on a sheet he has to read.
    fn unexpected(&self, recipients: &[String]) -> Vec<String> {
        recipients
            .iter()
            .map(|r| r.trim().to_lowercase())
            .filter(|r| !r.is_empty() && !self.expected.contains(r))
            .collect()
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

        // What this call costs, and whether the session can still afford it.
        // Charged before the call runs: a batch that parks and is denied has
        // still told us how big the model's appetite was.
        let cost = write_cost(name, policy, input);
        let spent = self.spent.fetch_add(cost, Ordering::Relaxed) + cost;
        let over_budget = cost > 0 && spent > WRITE_BUDGET;

        if policy == ToolPolicy::Approve || over_budget {
            let summary = match over_budget {
                true => self.budget_summary(name, cost),
                false => self.approval_summary(name, input).await,
            };
            let pending = PendingApproval {
                tool_use_id: call_id.to_string(),
                name: name.to_string(),
                summary,
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

    /// The sentence for a call that is only being asked about because of the
    /// budget. It has to say *why* — the owner is being asked about `archive`,
    /// which normally asks nothing, and an unexplained prompt teaches him to
    /// click through prompts.
    fn budget_summary(&self, name: &str, cost: usize) -> String {
        let how_many = match cost {
            1 => String::from("1 conversation"),
            n => format!("{n} conversations"),
        };
        format!(
            "Run {name} on {how_many}. This session has already changed {WRITE_BUDGET} \
             without asking, so it is asking about the rest."
        )
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
    ///
    /// Async because one of these has to be looked up before it can be written:
    /// see [`delete_filter_summary`](Self::delete_filter_summary). Nothing has
    /// run at this point and nothing will until the owner answers, so the wait
    /// costs a prompt that appears a beat later rather than an action that
    /// happens early.
    pub async fn approval_summary(&self, name: &str, input: &Value) -> String {
        if let Some(summary) = super::plugin_tools::approval_summary(&self.plugins, name) {
            return summary;
        }
        match name {
            tools::CREATE_FILTER_TOOL => return self.create_filter_summary(input),
            tools::DELETE_FILTER_TOOL => return self.delete_filter_summary(input).await,
            _ => {}
        }
        if name != tools::SEND_TOOL {
            return format!("Run {name}");
        }

        let draft_id = input.get("draftId").and_then(Value::as_str).unwrap_or_default();
        let draft = crate::ipc::compose::engine::draft::load_draft(&self.ctx.db, draft_id)
            .ok()
            .flatten();

        let (subject, recipients) = match &draft {
            Some(draft) => (
                draft.subject.clone(),
                draft
                    .to
                    .iter()
                    .chain(draft.cc.iter())
                    .chain(draft.bcc.iter())
                    .map(|m| m.email.clone())
                    .collect::<Vec<_>>(),
            ),
            None => (String::from("(draft not found)"), Vec::new()),
        };
        let to = recipients.join(", ");

        // A reply's own conversation counts as "seen" whether or not he
        // attached it: the people on a thread the draft answers are, by
        // definition, people in the conversation.
        // A *reply's* thread only: a new message gets a thread row of its own,
        // whose participants are the recipients the model just chose — asking
        // that row whether it recognises them would be asking the model.
        let mut recipients_to_check = recipients.clone();
        let answers_a_conversation = draft
            .as_ref()
            .map(|d| !matches!(d.kind, crate::ipc::compose::engine::draft::DraftKind::New))
            .unwrap_or(false);
        if let Some(thread_id) = draft
            .as_ref()
            .filter(|_| answers_a_conversation)
            .and_then(|d| d.thread_id)
        {
            if let Ok(Some(detail)) = self
                .ctx
                .db
                .read(|conn| crate::db::queries::thread_with_messages(conn, thread_id))
            {
                let known: Vec<String> = detail
                    .messages
                    .iter()
                    .filter(|m| !m.is_draft)
                    .flat_map(|m| {
                        std::iter::once(m.from.email.clone())
                            .chain(m.to.iter().map(|p| p.email.clone()))
                            .chain(m.cc.iter().map(|p| p.email.clone()))
                    })
                    .map(|e| e.trim().to_lowercase())
                    .collect();
                recipients_to_check.retain(|r| !known.contains(&r.trim().to_lowercase()));
            }
        }

        // The line that would have caught it. A recipient he never named, who
        // is not in the conversation either, came from somewhere — and the only
        // other thing in the prompt is mail somebody else wrote.
        // One sentence, not a second line: the approval bar draws its summary in
        // a single span, so a newline here would arrive as a space.
        let strangers = match self.unexpected(&recipients_to_check).as_slice() {
            [] => String::new(),
            [one] => format!(". {one} is not in this conversation and you did not name it"),
            many => format!(
                ". {} are not in this conversation and you did not name them",
                many.join(", ")
            ),
        };

        match input.get("sendAt").and_then(Value::as_i64) {
            Some(at) => format!(
                "Send \u{201c}{subject}\u{201d} to {to} on {}{strangers}",
                super::context::human_time(at)
            ),
            None => format!("Send \u{201c}{subject}\u{201d} to {to} now{strangers}"),
        }
    }

    /// What a new filter will actually do, in a sentence.
    ///
    /// > Create a filter on alex@lumen.example. Mail from no-reply@okta.com
    /// > with “code” in the subject. It skips the inbox and is labelled Codes.
    /// > It applies to mail that arrives from now on.
    ///
    /// The last line is there because it is the question the owner would
    /// otherwise have to ask, and getting it wrong in the optimistic direction
    /// — believing the inbox is about to be cleared — is how somebody approves
    /// a rule and then goes looking for the mail it did not touch.
    ///
    /// Label ids are resolved to names through the same store the mail list
    /// uses, so this says "Codes" rather than "Label_18".
    fn create_filter_summary(&self, input: &Value) -> String {
        let filter = tools::filter_from_call(input);
        let account = tools::filter_account(&self.ctx, input);
        let (where_, description) = match account {
            Ok(account_id) => (
                account_email(&self.ctx, account_id)
                    .map(|email| format!(" on {email}"))
                    .unwrap_or_default(),
                self.ctx.dispatcher.describe_filter(account_id, &filter),
            ),
            // The call is going to fail on the same resolution a moment from
            // now. Describe the rule anyway rather than showing nothing.
            Err(_) => (
                String::new(),
                crate::commands::filters::describe(&filter, &Default::default()),
            ),
        };
        format!(
            "Create a filter{where_}. {description} It applies to mail that arrives from now on."
        )
    }

    /// Which rule is about to stop existing, said the same way it was created.
    ///
    /// The filter is fetched to write this. A filter id is an opaque string of
    /// twenty characters and approving its deletion on sight is not consent to
    /// anything — the owner has to be told which rule, and the only place that
    /// knows is Google. One GET, on a path that is already about to make a
    /// request, and the id is the fallback if it fails.
    async fn delete_filter_summary(&self, input: &Value) -> String {
        let filter_id = input
            .get("filterId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let account_id = tools::filter_account(&self.ctx, input).ok();

        let described = match account_id {
            Some(account_id) => self
                .ctx
                .dispatcher
                .list_filters(Some(account_id))
                .await
                .ok()
                .and_then(|filters| filters.into_iter().find(|f| f.id == filter_id))
                .map(|f| format!("this filter: {}", f.description)),
            None => None,
        }
        .unwrap_or_else(|| format!("the filter with id {filter_id}."));

        format!("Delete {described} Mail it has already moved stays where it is; only the rule goes.")
    }
}

/// The address of an account, for a sentence that has to name a mailbox.
fn account_email(ctx: &ToolContext, account_id: i64) -> Option<String> {
    ctx.db
        .read(|conn| crate::db::command_queries::account_by_id(conn, account_id))
        .ok()
        .flatten()
        .map(|account| account.email)
}

fn running_summary(name: &str) -> String {
    match name {
        "search_threads" => "Searching mail…".to_string(),
        "get_thread" => "Reading the conversation…".to_string(),
        "list_threads" => "Listing conversations…".to_string(),
        "list_events" => "Checking the calendar…".to_string(),
        "list_labels" | "list_accounts" => "Looking things up…".to_string(),
        tools::DRAFT_TOOL => "Writing a reply…".to_string(),
        tools::NEW_DRAFT_TOOL => "Writing a message…".to_string(),
        tools::SEND_TOOL => "Ready to send…".to_string(),
        tools::LIST_FILTERS_TOOL => "Reading the filters…".to_string(),
        tools::CREATE_FILTER_TOOL => "Ready to make a filter…".to_string(),
        tools::DELETE_FILTER_TOOL => "Ready to delete a filter…".to_string(),
        other => format!("{other}…"),
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_read_costs_nothing_and_a_batch_costs_what_it_moves() {
        assert_eq!(
            write_cost("search_threads", ToolPolicy::Auto, &json!({ "query": "invoice" })),
            0
        );
        assert_eq!(
            write_cost("archive", ToolPolicy::Auto, &json!({ "threadIds": [1, 2, 3] })),
            3
        );
        assert_eq!(
            write_cost("markRead", ToolPolicy::Auto, &json!({ "threadIds": [] })),
            1
        );
        assert_eq!(write_cost("unsnooze", ToolPolicy::Auto, &json!({})), 1);
    }

    #[test]
    fn a_call_that_is_already_going_to_ask_is_not_charged_twice() {
        // Otherwise a session that behaved perfectly — twenty-six sends, each
        // approved — would start asking a second time for the same act.
        assert_eq!(
            write_cost(tools::SEND_TOOL, ToolPolicy::Approve, &json!({ "draftId": "d" })),
            0
        );
        // And the same is true of everything the inverted list now gates,
        // `unsubscribe` included: it parks on its own account, not on the
        // budget's.
        assert_eq!(
            write_cost("unsubscribe", ToolPolicy::Approve, &json!({ "messageId": 1 })),
            0
        );
    }

    #[test]
    fn one_call_can_exceed_the_budget_on_its_own() {
        // "Archive everything from this sender" is one call with two hundred
        // ids, and the point of counting conversations rather than calls is
        // that this parks.
        let ids: Vec<i64> = (1..=200).collect();
        let cost = write_cost("archive", ToolPolicy::Auto, &json!({ "threadIds": ids }));
        assert!(cost > WRITE_BUDGET, "{cost}");
    }
}
