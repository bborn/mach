//! The agent's tools — which are the command layer, plus enough reads to
//! answer a question instead of only acting on one.
//!
//! # The payoff
//!
//! [`command_tools`] does not hand-write a single mail tool. It walks
//! [`Command::catalogue`] and turns each [`CommandSpec`] into an Anthropic tool
//! definition: the `kind` is the tool name, the summary is the description, and
//! each [`ParamSpec`] becomes a JSON Schema property. Because the catalogue's
//! `kind` strings are exactly the enum's serde tags, turning a tool call back
//! into a [`Command`] is *one* line — inject `kind` and deserialize
//! ([`command_from_call`]). Add a command to the enum and the catalogue and the
//! agent can use it; nothing here changes.
//!
//! The agent therefore has **no privileged path to Google**. Every action it
//! takes goes through [`CommandDispatcher::execute`], which writes locally,
//! calls the API, rolls back on failure and returns its own inverse — the same
//! undoable, logged path the keyboard uses.
//!
//! # Reads
//!
//! `list_threads`, `search_threads`, `get_thread`, `list_events`, `list_labels`
//! and `list_accounts` are the local store, through [`crate::ipc::reads`]. They
//! are the reason a session can answer "what did Tawny say about the data
//! room?" rather than only archiving things. They touch nothing and never wait
//! for approval.
//!
//! # Compose
//!
//! `draft_reply` and `send_draft` go through the composer's own router
//! ([`crate::ipc::compose::dispatch`]) rather than reimplementing threading and
//! MIME. Drafting is local and free; sending is the one thing that reaches
//! another human, so it is [`ToolPolicy::Approve`].

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::commands::{Command, CommandDispatcher, CommandSpec, ParamSpec, ParamType};
use crate::db::Db;
use crate::plugins::{InstalledPlugin, PluginRuntime};
use crate::ipc::compose::engine::outbox::Outbox;
use crate::ipc::compose::{dispatch as compose_dispatch, now_ms};
use crate::ipc::reads;
use crate::ipc::types::ThreadQuery;

use super::error::AgentError;
use super::wire::ToolDefinition;

/// Whether a tool may run on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    /// Reading, searching, and anything the command layer can undo.
    Auto,
    /// Sends mail or notifies another human. Blocks on the owner.
    Approve,
}

/// One tool, with the policy the session enforces around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub definition: ToolDefinition,
    pub policy: ToolPolicy,
}

impl Tool {
    fn auto(name: &str, description: &str, schema: Value) -> Tool {
        Tool {
            definition: ToolDefinition {
                name: name.to_string(),
                description: description.to_string(),
                input_schema: schema,
            },
            policy: ToolPolicy::Auto,
        }
    }
}

/// The commands that touch another human even though they are undoable.
///
/// An RSVP mails the organiser the moment it lands, and un-declining does not
/// unsend that. The same is true of every calendar write that can carry guests:
/// creating, moving, editing or cancelling an event with attendees sends them
/// mail, and the inverse command sends them a second one. Everything else in
/// the catalogue is a label move nobody else can see.
///
/// Names rather than a property on [`CommandSpec`] because "does this reach
/// someone else" is a judgement about consequences, and the catalogue is owned
/// by the command layer, not by this module.
pub const APPROVAL_COMMANDS: &[&str] = &[
    "rsvp",
    "createEvent",
    "updateEvent",
    "deleteEvent",
    "moveEvent",
];

/// The tool that actually puts a message in the outbox.
pub const SEND_TOOL: &str = "send_draft";
pub const DRAFT_TOOL: &str = "draft_reply";

// ===========================================================================
// Definitions
// ===========================================================================

/// Everything the agent can do, reads first. Core only.
pub fn tools() -> Vec<Tool> {
    let mut all = read_tools();
    all.extend(command_tools());
    all.extend(compose_tools());
    all
}

/// The core tools plus whatever the installed plugins contribute.
///
/// Plugins come last so a plugin can never shadow a core tool by name — and it
/// could not anyway, because every plugin tool is prefixed.
pub fn tools_with(plugins: &[InstalledPlugin]) -> Vec<Tool> {
    let mut all = tools();
    all.extend(super::plugin_tools::plugin_tools(plugins));
    all
}

pub fn find(name: &str) -> Option<Tool> {
    tools().into_iter().find(|t| t.definition.name == name)
}

pub fn policy_for(name: &str) -> ToolPolicy {
    find(name).map(|t| t.policy).unwrap_or(ToolPolicy::Auto)
}

/// The policy for a tool that may be a plugin's.
///
/// A plugin tool whose plugin is no longer installed resolves to
/// [`ToolPolicy::Approve`] rather than `Auto`: an unknown tool is not a safe
/// tool, and the call is going to fail anyway — better it fails after asking
/// than before.
pub fn policy_for_with(name: &str, plugins: &[InstalledPlugin]) -> ToolPolicy {
    if super::plugin_tools::is_plugin_tool(name) {
        return super::plugin_tools::plugin_tools(plugins)
            .into_iter()
            .find(|t| t.definition.name == name)
            .map(|t| t.policy)
            .unwrap_or(ToolPolicy::Approve);
    }
    policy_for(name)
}

/// The command layer, described to something that cannot read Rust.
pub fn command_tools() -> Vec<Tool> {
    Command::catalogue()
        .iter()
        .map(|spec| Tool {
            definition: ToolDefinition {
                name: spec.kind.to_string(),
                description: describe(spec),
                input_schema: schema_for(spec),
            },
            policy: if APPROVAL_COMMANDS.contains(&spec.kind) {
                ToolPolicy::Approve
            } else {
                ToolPolicy::Auto
            },
        })
        .collect()
}

/// The description the model reads: the summary, plus the two facts that change
/// how it should behave — that a mistake is undoable, and that many ids in one
/// call cost one round trip rather than many.
fn describe(spec: &CommandSpec) -> String {
    let mut out = spec.summary.to_string();
    if spec.batch {
        out.push_str(" Pass every id in one call rather than calling repeatedly.");
    }
    if spec.undoable {
        out.push_str(" Undoable — the app records the exact inverse.");
    }
    out
}

/// A [`CommandSpec`]'s parameters as JSON Schema.
pub fn schema_for(spec: &CommandSpec) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();

    for param in spec.params {
        properties.insert(param.name.to_string(), property_for(param));
        if param.required {
            required.push(json!(param.name));
        }
    }

    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": required,
        "additionalProperties": false,
    })
}

fn property_for(param: &ParamSpec) -> Value {
    let mut node = match param.ty {
        ParamType::ThreadIds => json!({
            "type": "array",
            "items": { "type": "integer" },
            "minItems": 1,
        }),
        ParamType::EventId => json!({ "type": "integer" }),
        ParamType::Bool => json!({ "type": "boolean" }),
        ParamType::Timestamp => json!({ "type": "integer" }),
        ParamType::LabelId => json!({ "type": "string" }),
        ParamType::RsvpResponse => json!({
            "type": "string",
            "enum": ["accepted", "declined", "tentative", "needsAction"],
        }),
        ParamType::AccountId => json!({ "type": "integer" }),
        ParamType::CalendarId => json!({ "type": "string" }),
        // The event shapes are owned by the calendar-commands unit and are
        // still growing. The documented fields are listed so the model has
        // something concrete to aim at, and extra keys are permitted so a
        // field added there is usable here before this file hears about it.
        ParamType::EventDraft => event_object(true),
        ParamType::EventPatch => event_object(false),
        ParamType::EventScope => json!({
            "type": "string",
            "enum": ["this", "all"],
        }),
        ParamType::ThreadLabelStates => json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "threadId": { "type": "integer" },
                    "labelIds": { "type": "array", "items": { "type": "string" } },
                    "isUnread": { "type": "boolean" },
                },
                "required": ["threadId", "labelIds", "isUnread"],
                "additionalProperties": false,
            },
        }),
    };
    node["description"] = json!(param.description);
    node
}

/// The event fields, as a schema. `required` is only meaningful for a whole
/// draft — a patch is by definition partial.
fn event_object(is_draft: bool) -> Value {
    let mut node = json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "startTs": { "type": "integer", "description": "Start, unix milliseconds." },
            "endTs": { "type": "integer", "description": "End, unix milliseconds." },
            "isAllDay": { "type": "boolean" },
            "location": { "type": "string" },
            "description": { "type": "string" },
            "attendees": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Email addresses. Adding one invites that person.",
            },
            "recurrence": { "type": "string", "description": "An RRULE, e.g. RRULE:FREQ=WEEKLY;BYDAY=TU." },
            "reminderMinutes": { "type": "integer" },
        },
        "additionalProperties": true,
    });
    if is_draft {
        node["required"] = json!(["title", "startTs", "endTs"]);
    }
    node
}

/// A tool call, back into the typed command it names.
///
/// The catalogue's `kind` is the enum's serde tag, so this is the whole
/// adapter: put the tag back and let serde do the rest. A malformed call is an
/// [`AgentError::Invalid`], which the session reports to the model as a tool
/// error so it can correct itself.
pub fn command_from_call(name: &str, input: &Value) -> Result<Command, AgentError> {
    let mut object = match input {
        Value::Object(map) => map.clone(),
        Value::Null => Map::new(),
        _ => return Err(AgentError::invalid(format!("{name} takes an object"))),
    };
    object.insert("kind".to_string(), json!(name));
    serde_json::from_value(Value::Object(object))
        .map_err(|e| AgentError::invalid(format!("{name} was called with bad arguments: {e}")))
}

fn read_tools() -> Vec<Tool> {
    vec![
        Tool::auto(
            "list_threads",
            "List conversations from the local store, newest first. Use this for \
             \"my inbox\", \"unread\", or a specific label.",
            json!({
                "type": "object",
                "properties": {
                    "labelId": { "type": "string", "description": "Gmail label id, e.g. INBOX, STARRED or Label_12. Defaults to INBOX." },
                    "accountId": { "type": "integer", "description": "Restrict to one account. Omit for the unified stream." },
                    "unreadOnly": { "type": "boolean" },
                    "limit": { "type": "integer", "description": "1..50. Defaults to 20." },
                },
                "additionalProperties": false,
            }),
        ),
        Tool::auto(
            "search_threads",
            "Full-text search over every synced message (subject and body). Ranked, \
             not chronological.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "description": "1..50. Defaults to 20." },
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
        ),
        Tool::auto(
            "get_thread",
            "The whole conversation for one thread id: participants, labels, and the \
             text of each message with quoted trails trimmed.",
            json!({
                "type": "object",
                "properties": { "threadId": { "type": "integer" } },
                "required": ["threadId"],
                "additionalProperties": false,
            }),
        ),
        Tool::auto(
            "list_events",
            "Calendar events overlapping a time range, across every account. Use this \
             before proposing a time.",
            json!({
                "type": "object",
                "properties": {
                    "startMs": { "type": "integer", "description": "Unix milliseconds, inclusive." },
                    "endMs": { "type": "integer", "description": "Unix milliseconds, exclusive." },
                },
                "required": ["startMs", "endMs"],
                "additionalProperties": false,
            }),
        ),
        Tool::auto(
            "list_labels",
            "Every Gmail label, with the id the label command needs.",
            json!({
                "type": "object",
                "properties": { "accountId": { "type": "integer" } },
                "additionalProperties": false,
            }),
        ),
        Tool::auto(
            "list_accounts",
            "The connected Google accounts and their row ids.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
    ]
}

fn compose_tools() -> Vec<Tool> {
    vec![
        Tool::auto(
            DRAFT_TOOL,
            "Write a reply to a conversation and save it as a draft. Recipients, \
             subject and threading headers are derived from the thread — supply the \
             body only. Nothing is sent. Returns a draftId for send_draft.",
            json!({
                "type": "object",
                "properties": {
                    "threadId": { "type": "integer" },
                    "body": { "type": "string", "description": "The message text. Markdown-ish: *bold*, _italic_, - bullets." },
                    "kind": {
                        "type": "string",
                        "enum": ["reply", "replyAll", "forward"],
                        "description": "Defaults to reply.",
                    },
                    "to": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Override the recipients. Email addresses. Omit to reply to the thread.",
                    },
                },
                "required": ["threadId", "body"],
                "additionalProperties": false,
            }),
        ),
        Tool {
            definition: ToolDefinition {
                name: SEND_TOOL.to_string(),
                description:
                    "Queue a saved draft for sending, optionally at a future time. \
                     Requires the owner's confirmation before it runs. With sendAt in \
                     the future this is a scheduled send; without it the message goes \
                     out after the usual ten-second undo window."
                        .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "draftId": { "type": "string" },
                        "sendAt": {
                            "type": "integer",
                            "description": "Unix milliseconds. Omit to send now.",
                        },
                    },
                    "required": ["draftId"],
                    "additionalProperties": false,
                }),
            },
            policy: ToolPolicy::Approve,
        },
    ]
}

// ===========================================================================
// Execution
// ===========================================================================

/// What a tool needs to run. Everything is shared with the rest of the app —
/// the same `Db`, the same dispatcher — because the point is that the agent has
/// no side door.
pub struct ToolContext {
    pub db: Db,
    pub dispatcher: Arc<CommandDispatcher>,
    pub outbox: Arc<Outbox>,
    /// What is installed, and the bridge a plugin action runs over. The agent
    /// gets no privileged path to a plugin either: it asks the same host the
    /// keyboard asks, in the window, and the answer comes back over IPC.
    pub plugins: Arc<PluginRuntime>,
}

impl ToolContext {
    /// The plugins whose actions may be offered as tools right now — empty
    /// until the sandbox has been verified in this window.
    pub fn plugin_list(&self) -> Vec<InstalledPlugin> {
        self.plugins.runnable()
    }
}

/// Something a tool made, and enough to put the owner in front of it.
///
/// # Why this is a shape and not a sentence
///
/// The agent drafted a reply, the drawer printed *"Drafted a reply to the
/// bookkeeper…"*, and the draft was unreachable from anywhere in the app. Prose
/// is not an affordance. A tool that brings something into being — a draft, an
/// event, a conversation it moved — hands back one of these instead, the drawer
/// renders it as a button, and the artifact stops being orphaned.
///
/// It is deliberately a small closed enum rather than a free-form link. Each
/// variant is a thing the shell already knows how to open, so adding an
/// affordance to a new tool is filling this in rather than teaching the UI a
/// new kind of navigation. The label is what the artifact is *called*; the verb
/// ("Open draft", "Show event") belongs to the UI, not to Rust.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Artifact {
    /// An unsent message. Opening it resumes it in the composer.
    Draft {
        draft_id: String,
        /// The conversation it belongs to, so opening it navigates there first.
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_id: Option<i64>,
        account_id: i64,
        label: String,
    },
    /// A conversation worth landing on.
    Thread { thread_id: i64, label: String },
    /// A calendar entry the agent created. `start_ms` is carried because the
    /// grid shows a window, and an event you cannot scroll to is as orphaned as
    /// a draft you cannot open.
    Event {
        event_id: i64,
        start_ms: i64,
        label: String,
    },
}

/// What a tool run came to. `summary` is the one line the drawer shows.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub summary: String,
    pub payload: Value,
    /// True when this changed the mailbox, so the UI has to re-read.
    pub mutated: bool,
    /// What it made, when it made something. See [`Artifact`].
    pub artifact: Option<Artifact>,
}

pub async fn execute(ctx: &ToolContext, name: &str, input: &Value) -> Result<ToolOutcome, AgentError> {
    match name {
        "list_threads" => list_threads(ctx, input),
        "search_threads" => search_threads(ctx, input),
        "get_thread" => get_thread(ctx, input),
        "list_events" => list_events(ctx, input),
        "list_labels" => list_labels(ctx, input),
        "list_accounts" => list_accounts(ctx),
        DRAFT_TOOL => draft_reply(ctx, input).await,
        SEND_TOOL => send_draft(ctx, input).await,
        other if super::plugin_tools::is_plugin_tool(other) => run_plugin(ctx, other, input).await,
        _ => run_command(ctx, name, input).await,
    }
}

/// Hand a tool call to a plugin action, in the window, and wait.
///
/// The plugin's answer is returned to the model as data — it is a third party's
/// output, and it is framed as such in the payload so a plugin cannot pretend to
/// be the app talking.
async fn run_plugin(ctx: &ToolContext, name: &str, input: &Value) -> Result<ToolOutcome, AgentError> {
    let (plugin_id, action) = super::plugin_tools::split_tool_name(name)
        .ok_or_else(|| AgentError::invalid(format!("{name} is not a plugin tool")))?;

    let value = ctx
        .plugins
        .invoke(plugin_id, action, input.clone(), "agent")
        .await
        .map_err(|e| AgentError::invalid(e.to_string()))?;

    Ok(ToolOutcome {
        summary: format!("{plugin_id}: {action}"),
        payload: json!({ "plugin": plugin_id, "action": action, "result": value }),
        // A plugin action's whole point is dispatching commands, and the UI has
        // no way to know which ones. Assume the mailbox moved.
        mutated: true,
        artifact: None,
    })
}

async fn run_command(ctx: &ToolContext, name: &str, input: &Value) -> Result<ToolOutcome, AgentError> {
    let command = command_from_call(name, input)?;
    // Read before the move: a create is the one command whose result the owner
    // cannot find by looking where they already were.
    let created = match &command {
        Command::CreateEvent { draft, .. } => Some((draft.title.clone(), draft.start_ts)),
        _ => None,
    };
    let result = ctx.dispatcher.execute(command).await?;
    let artifact = created.and_then(|(title, start_ms)| {
        result.applied.first().map(|event_id| Artifact::Event {
            event_id: *event_id,
            start_ms,
            label: title,
        })
    });
    Ok(ToolOutcome {
        summary: result.message.clone(),
        payload: serde_json::to_value(&result).unwrap_or(Value::Null),
        mutated: true,
        artifact,
    })
}

fn list_threads(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let query = ThreadQuery {
        account_id: input.get("accountId").and_then(Value::as_i64),
        label_id: Some(
            input
                .get("labelId")
                .and_then(Value::as_str)
                .unwrap_or("INBOX")
                .to_string(),
        ),
        unread_only: input
            .get("unreadOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        limit: Some(limit_of(input, 20)),
        cursor: None,
    };
    let page = reads::list_threads(&ctx.db, &query).map_err(ipc_to_agent)?;
    let items: Vec<Value> = page.items.iter().map(thread_row).collect();
    Ok(ToolOutcome {
        summary: format!("{} conversations", items.len()),
        payload: json!({ "threads": items }),
        mutated: false,
        artifact: None,
    })
}

fn search_threads(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::invalid("search_threads needs a query"))?;
    let page = reads::search_threads(&ctx.db, query, Some(limit_of(input, 20)))
        .map_err(ipc_to_agent)?;
    let items: Vec<Value> = page.items.iter().map(thread_row).collect();
    Ok(ToolOutcome {
        summary: format!("{} matches for \u{201c}{query}\u{201d}", items.len()),
        payload: json!({ "threads": items }),
        mutated: false,
        artifact: None,
    })
}

/// How much of one message body to hand the model.
///
/// Enough for a marketing email's actual content, short of pasting a newsletter
/// into the context window. Quoted trails are dropped first, which usually
/// makes the cap irrelevant.
const BODY_CHARS: usize = 4_000;

fn get_thread(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let thread_id = required_i64(input, "threadId")?;
    let detail = reads::get_thread(&ctx.db, thread_id).map_err(ipc_to_agent)?;

    let messages: Vec<Value> = detail
        .messages
        .iter()
        .map(|m| {
            json!({
                "messageId": m.id,
                "from": participant(&m.from),
                "to": m.to.iter().map(participant).collect::<Vec<_>>(),
                "cc": m.cc.iter().map(participant).collect::<Vec<_>>(),
                "at": m.internal_date,
                "subject": m.subject,
                "body": message_text(m),
                "attachments": m.attachments.iter().map(|a| a.filename.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(ToolOutcome {
        summary: format!("Read \u{201c}{}\u{201d}", detail.thread.subject),
        payload: json!({
            "threadId": detail.thread.id,
            "accountId": detail.thread.account_id,
            "accountEmail": detail.thread.account_email,
            "subject": detail.thread.subject,
            "labelIds": detail.thread.label_ids,
            "isUnread": detail.thread.is_unread,
            "participants": detail.thread.participants.iter().map(participant).collect::<Vec<_>>(),
            "messages": messages,
        }),
        mutated: false,
        artifact: None,
    })
}

/// The readable part of a message: plain text where there is any, the quoted
/// trail removed, capped.
fn message_text(message: &crate::db::models::Message) -> String {
    let raw = match (&message.body_text, &message.body_html) {
        (Some(text), _) if !text.trim().is_empty() => crate::render::quotes::split_text(text).new,
        // Sanitize before stripping: the tag-stripper is a formatter, not a
        // security boundary, and the sanitizer has already thrown away scripts,
        // styles and everything else that would otherwise become "text".
        (_, Some(html)) if !html.trim().is_empty() => {
            crate::render::text::from_sanitized(&crate::render::render_html(html).html)
        }
        _ => message.snippet.clone(),
    };
    let trimmed = raw.trim();
    if trimmed.chars().count() <= BODY_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(BODY_CHARS).collect();
    format!("{head}\n… [truncated]")
}

fn list_events(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let start = required_i64(input, "startMs")?;
    let end = required_i64(input, "endMs")?;
    let events = reads::list_events(&ctx.db, start, end).map_err(ipc_to_agent)?;
    let items: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "eventId": e.id,
                "title": e.title,
                "startMs": e.start_ts,
                "endMs": e.end_ts,
                "isAllDay": e.is_all_day,
                "location": e.location,
                "attendees": e.attendees.iter().map(participant).collect::<Vec<_>>(),
                "rsvp": e.rsvp_status,
            })
        })
        .collect();
    Ok(ToolOutcome {
        summary: format!("{} events", items.len()),
        payload: json!({ "events": items }),
        mutated: false,
        artifact: None,
    })
}

fn list_labels(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let labels = reads::list_labels(&ctx.db, input.get("accountId").and_then(Value::as_i64))
        .map_err(ipc_to_agent)?;
    let items: Vec<Value> = labels
        .iter()
        .map(|l| json!({ "labelId": l.gmail_label_id, "name": l.name, "accountId": l.account_id }))
        .collect();
    Ok(ToolOutcome {
        summary: format!("{} labels", items.len()),
        payload: json!({ "labels": items }),
        mutated: false,
        artifact: None,
    })
}

fn list_accounts(ctx: &ToolContext) -> Result<ToolOutcome, AgentError> {
    let accounts = reads::list_accounts(&ctx.db).map_err(ipc_to_agent)?;
    let items: Vec<Value> = accounts
        .iter()
        .map(|a| json!({ "accountId": a.id, "email": a.email }))
        .collect();
    Ok(ToolOutcome {
        summary: format!("{} accounts", items.len()),
        payload: json!({ "accounts": items }),
        mutated: false,
        artifact: None,
    })
}

// ---------------------------------------------------------------------------
// compose
// ---------------------------------------------------------------------------

async fn draft_reply(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let thread_id = required_i64(input, "threadId")?;
    let body = input
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::invalid("draft_reply needs a body"))?;
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("reply");

    // `prepare` is what knows about Reply-To, References and which account the
    // thread arrived on. Reimplementing any of that here would be a second,
    // worse composer.
    let prepared = compose_dispatch(
        &ctx.db,
        &ctx.outbox,
        json!({ "op": "prepare", "threadId": thread_id, "kind": kind }),
    )
    .await?;
    let mut draft = prepared
        .get("draft")
        .cloned()
        .ok_or_else(|| AgentError::invalid("the composer returned no draft"))?;

    draft["body"] = json!(body);
    if let Some(to) = input.get("to").and_then(Value::as_array) {
        let recipients: Vec<Value> = to
            .iter()
            .filter_map(Value::as_str)
            .map(|email| json!({ "email": email }))
            .collect();
        if !recipients.is_empty() {
            draft["to"] = Value::Array(recipients);
        }
    }

    let saved = compose_dispatch(&ctx.db, &ctx.outbox, json!({ "op": "saveDraft", "draft": draft }))
        .await?;
    let draft = saved.get("draft").cloned().unwrap_or(Value::Null);

    let subject = draft
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or("(no subject)")
        .to_string();
    // The one line this whole change exists for: the drawer gets something to
    // open, not just a sentence claiming a draft was written.
    let artifact = draft
        .get("id")
        .and_then(Value::as_str)
        .map(|draft_id| Artifact::Draft {
            draft_id: draft_id.to_string(),
            thread_id: draft.get("threadId").and_then(Value::as_i64),
            account_id: draft.get("accountId").and_then(Value::as_i64).unwrap_or(0),
            label: subject.clone(),
        });

    Ok(ToolOutcome {
        summary: format!("Drafted \u{201c}{subject}\u{201d}"),
        payload: json!({ "draft": draft }),
        // The draft is now a row in the Drafts mailbox, so the list is stale —
        // this is what makes it appear without a relaunch.
        mutated: true,
        artifact,
    })
}

async fn send_draft(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let draft_id = input
        .get("draftId")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::invalid("send_draft needs a draftId"))?;

    let loaded = compose_dispatch(
        &ctx.db,
        &ctx.outbox,
        json!({ "op": "loadDraft", "draftId": draft_id }),
    )
    .await?;
    let draft = loaded
        .get("draft")
        .filter(|d| !d.is_null())
        .cloned()
        .ok_or_else(|| AgentError::invalid(format!("no draft with id {draft_id}")))?;

    let mut payload = json!({ "op": "send", "draft": draft });
    if let Some(at) = input.get("sendAt").and_then(Value::as_i64) {
        payload["scheduleAt"] = json!(at);
    }
    let sent = compose_dispatch(&ctx.db, &ctx.outbox, payload).await?;

    let scheduled = sent.get("scheduled").and_then(Value::as_bool).unwrap_or(false);
    let at = sent.get("undoUntil").and_then(Value::as_i64).unwrap_or_else(now_ms);
    Ok(ToolOutcome {
        summary: if scheduled {
            format!("Scheduled to send at {}", super::context::human_time(at))
        } else {
            "Queued to send".to_string()
        },
        payload: sent,
        mutated: true,
        artifact: None,
    })
}

// ---------------------------------------------------------------------------
// shaping
// ---------------------------------------------------------------------------

fn thread_row(thread: &crate::db::models::ThreadSummary) -> Value {
    json!({
        "threadId": thread.id,
        "accountId": thread.account_id,
        "accountEmail": thread.account_email,
        "subject": thread.subject,
        "snippet": thread.snippet,
        "from": thread.participants.iter().map(participant).collect::<Vec<_>>(),
        "at": thread.last_message_at,
        "isUnread": thread.is_unread,
        "labelIds": thread.label_ids,
        "messageCount": thread.message_count,
    })
}

fn participant(p: &crate::db::models::Participant) -> Value {
    json!(super::context::who(p))
}

fn limit_of(input: &Value, default: u32) -> u32 {
    input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n.clamp(1, 50) as u32)
        .unwrap_or(default)
}

fn required_i64(input: &Value, key: &str) -> Result<i64, AgentError> {
    input
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| AgentError::invalid(format!("{key} is required and must be a number")))
}

/// The read paths speak `IpcError`; the agent speaks `AgentError`. Only three
/// shapes can actually come out of a read, so this stays honest rather than
/// collapsing everything into "internal".
fn ipc_to_agent(error: crate::ipc::IpcError) -> AgentError {
    use crate::ipc::IpcError;
    match error {
        IpcError::Db(inner) => AgentError::Db(inner),
        IpcError::Command(inner) => AgentError::Command(inner),
        other => AgentError::Invalid(other.to_string()),
    }
}
