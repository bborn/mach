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
//! `list_threads`, `search_threads`, `get_thread`, `list_events`, `get_event`,
//! `list_calendars`, `list_labels` and `list_accounts` are the local store,
//! through [`crate::ipc::reads`]. They are the reason a session can answer
//! "what did Tawny say about the data room?" rather than only archiving things.
//! They touch nothing and never wait for approval.
//!
//! A read that surfaces one object carries an [`Artifact`], the same as a write
//! does, so the drawer can draw it as a card instead of the model retyping the
//! row as a bulleted list. `get_event` exists for that: it is the calendar's
//! `get_thread`, and it is how the model says *this one* out of a list.
//!
//! `list_calendars` is also the half of `createEvent` that was missing. Every
//! calendar write takes a `calendarId`, a user names a calendar rather than
//! identifying it, and nothing turned one into the other — so "add these to
//! Dad/Ben Schedule" was unanswerable. See [`LIST_CALENDARS_TOOL`].
//!
//! # Compose
//!
//! `draft_reply`, `draft_message` and `send_draft` go through the composer's own
//! router ([`crate::ipc::compose::dispatch`]) rather than reimplementing
//! threading and MIME. Drafting is local and free; sending is the one thing that
//! reaches another human, so it is [`ToolPolicy::Approve`].
//!
//! `draft_reply` answers a conversation and takes its recipients, its subject
//! and its threading headers from that conversation. `draft_message` is the
//! other half, and it exists because the agent had only the first: asked to
//! write to somebody, it replied to an unrelated month-old thread and reported
//! that the draft had inherited the subject "Re: Molly Swenson requests
//! $288.00". `draft_message` writes the subject it was given, on no thread.
//!
//! # Filters
//!
//! `list_filters`, `create_filter` and `delete_filter` reach Gmail through the
//! same [`CommandDispatcher`] everything else does — see
//! [`crate::commands::filters`]. Creating and deleting are
//! [`ToolPolicy::Approve`] for a reason that is not "it is a write": see
//! [`filter_tools`].

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::commands::{Command, CommandDispatcher, CommandSpec, ParamSpec, ParamType};
use crate::db::Db;
use crate::google::types::{Filter, FilterAction, FilterCriteria};
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

/// The commands the agent may run without asking — and the list is this way
/// round on purpose.
///
/// Every one of these is a label move inside his own mailbox: undoable by the
/// command layer's own inverse, invisible to everybody else, and reversible
/// with ⌘Z. Nothing else in the catalogue qualifies, and **anything the
/// catalogue grows from now on is [`ToolPolicy::Approve`] until somebody puts
/// it here on purpose.**
///
/// It used to be the other way — a list of commands that needed approval, with
/// `Auto` as the default. That default is the bug: a command added to
/// [`Command::catalogue`] became a thing the agent could do unattended, on a
/// model's say-so, without anybody making a decision about it. With the list
/// inverted, a new command has to be argued *into* the auto set rather than
/// silently falling into it.
///
/// What is deliberately not here:
///
/// * `unsubscribe` — the strongest case of the lot, and the one that shows why
///   the default had to change. It is not undoable at all, the request reaches
///   a stranger's server rather than Google's, and what it tells them is that
///   this address is live and read. `crate::unsub::rule` is re-run in the
///   command layer whoever asks, so an agent cannot talk the app into
///   unsubscribing from something it would refuse a keystroke — but "the rule
///   would have allowed it" is a different claim from "he wanted it", and this
///   is the one command where getting that wrong cannot be taken back. It
///   arrived in the catalogue looking exactly like a label move.
/// * `reportSpam` — locally a label move, but a spam report is a signal to
///   Google about a sender, it feeds a classifier, and `notSpam` puts the labels
///   back without retracting it. The undo is exact about the mailbox and cannot
///   be about Google's opinion of the sender.
/// * `rsvp` — mails the organiser the moment it lands, and un-declining does not
///   unsend that.
/// * every calendar write that can carry guests — creating, moving, editing or
///   cancelling an event with attendees sends them mail, and the inverse command
///   sends them a second one.
///
/// Names rather than a property on [`CommandSpec`] because "does this reach
/// someone else" is a judgement about consequences, and the catalogue is owned
/// by the command layer, not by this module.
pub const AUTO_COMMANDS: &[&str] = &[
    "archive",
    "unarchive",
    "markRead",
    "star",
    "label",
    "moveToInbox",
    "notSpam",
    "trash",
    "untrash",
    "snooze",
    "unsnooze",
];

/// The policy for one catalogue command, by name.
///
/// Takes a name rather than a [`CommandSpec`] so the claim "a command nobody has
/// judged asks first" can be made about a command that does not exist yet.
pub fn command_policy(kind: &str) -> ToolPolicy {
    match AUTO_COMMANDS.contains(&kind) {
        true => ToolPolicy::Auto,
        false => ToolPolicy::Approve,
    }
}

/// The tool that actually puts a message in the outbox.
pub const SEND_TOOL: &str = "send_draft";
pub const DRAFT_TOOL: &str = "draft_reply";
/// A message to somebody, from nothing — the agent's `c`.
pub const NEW_DRAFT_TOOL: &str = "draft_message";

/// The calendars, by name, with the id every calendar write demands.
///
/// `createEvent` has always taken a `calendarId` and its own parameter
/// description has always said "as returned by list_calendars" — and until this
/// existed, nothing returned one. Asked for eleven recurring events on
/// "Dad/Ben Schedule", the agent had no way to turn that name into an id, no
/// way to see which account held it, and no way to know whether it could be
/// written to at all. It wrote the owner a list and asked him to make them
/// himself.
pub const LIST_CALENDARS_TOOL: &str = "list_calendars";

/// One event, by id. The calendar's `get_thread`, and the way the model shows
/// the owner a single event rather than describing one out of a list.
pub const GET_EVENT_TOOL: &str = "get_event";

/// Gmail filters. Listing is a read; the other two are standing rules.
pub const LIST_FILTERS_TOOL: &str = "list_filters";
pub const CREATE_FILTER_TOOL: &str = "create_filter";
pub const DELETE_FILTER_TOOL: &str = "delete_filter";

// ===========================================================================
// Definitions
// ===========================================================================

/// Everything the agent can do, reads first. Core only.
pub fn tools() -> Vec<Tool> {
    let mut all = read_tools();
    all.extend(command_tools());
    all.extend(compose_tools());
    all.extend(filter_tools());
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

/// The policy for a tool name. A name nobody recognises asks first — the gate
/// refuses it anyway, but no caller of this should be told "unknown" and hear
/// "safe".
pub fn policy_for(name: &str) -> ToolPolicy {
    find(name).map(|t| t.policy).unwrap_or(ToolPolicy::Approve)
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
            policy: command_policy(spec.kind),
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
        ParamType::MessageId => json!({ "type": "integer" }),
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
        ParamType::Notify => json!({
            "type": "string",
            "enum": ["guests", "externalGuests", "nobody"],
        }),
        ParamType::Text => json!({ "type": "string" }),
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
///
/// # Three of these used to describe a shape the command layer refuses
///
/// `attendees` was advertised as an array of strings against an
/// [`EventDraft::attendees`] of `Vec<Participant>`; `recurrence` as a single
/// string against a `Vec<String>` of RRULE lines; `reminderMinutes` as one
/// integer against a `Vec<i64>`. Serde rejects all three, so a model that read
/// the schema and did exactly what it said got
/// *"invalid type: string, expected struct Participant"* back — and a request
/// for recurring events with a guest on it trips two of them at once, on every
/// call, with nothing in the error naming the schema as the thing that lied.
///
/// So the types here are now the types the command layer holds, and
/// [`normalize_event`] accepts the singular form of each anyway. Describing the
/// real shape is what stops the mistake; taking the friendly one is what stops
/// it costing a turn when the model writes `"molly@example.com"` regardless.
///
/// [`EventDraft::attendees`]: crate::commands::EventDraft
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
                "items": {
                    "type": "object",
                    "properties": {
                        "email": { "type": "string" },
                        "name": { "type": "string" },
                    },
                    "required": ["email"],
                },
                "description": "Guests, as { email } objects — a bare address string is \
                                accepted too. Adding one invites that person.",
            },
            "recurrence": {
                "type": "array",
                "items": { "type": "string" },
                "description": "RRULE lines, e.g. [\"RRULE:FREQ=WEEKLY;BYDAY=TU\"]. One \
                                recurring event beats eleven copies. Omit for a one-off.",
            },
            "reminderMinutes": {
                "type": "array",
                "items": { "type": "integer" },
                "description": "Popup reminders, minutes before the start. Omit to leave \
                                the calendar's own defaults on.",
            },
            "conferencing": {
                "type": "string",
                "enum": ["meet", "none"],
                "description": "meet adds a Google Meet link; none removes the call.",
            },
            "transparency": {
                "type": "string",
                "enum": ["opaque", "transparent"],
                "description": "opaque is busy (the default); transparent is free.",
            },
            "notify": {
                "type": "string",
                "enum": ["guests", "externalGuests", "nobody"],
                "description": "Who Google emails. Omit to tell the guests.",
            },
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
/// The catalogue's `kind` is the enum's serde tag, so this is very nearly the
/// whole adapter: put the tag back and let serde do the rest. A malformed call
/// is an [`AgentError::Invalid`], which the session reports to the model as a
/// tool error so it can correct itself.
///
/// The one thing it does beyond that is [`normalize_event`], which widens the
/// three event fields that are lists in Rust and singular in the sentence a
/// model is working from. It is a deliberate exception and not a precedent:
/// everywhere else, a shape the command layer refuses should be a shape the
/// schema never offered.
pub fn command_from_call(name: &str, input: &Value) -> Result<Command, AgentError> {
    let mut object = match input {
        Value::Object(map) => map.clone(),
        Value::Null => Map::new(),
        _ => return Err(AgentError::invalid(format!("{name} takes an object"))),
    };
    for key in ["draft", "patch"] {
        if let Some(event) = object.get_mut(key) {
            normalize_event(event);
        }
    }
    object.insert("kind".to_string(), json!(name));
    serde_json::from_value(Value::Object(object))
        .map_err(|e| AgentError::invalid(format!("{name} was called with bad arguments: {e}")))
}

/// The singular form of a list-valued event field, made plural in place.
///
/// `attendees: ["molly@example.com"]` is what a model writes when it is thinking
/// about who to invite rather than about `Vec<Participant>`, and one RRULE or
/// one reminder is the overwhelmingly common case for the other two. Each
/// becomes the shape [`crate::commands::EventDraft`] holds. Anything already in
/// that shape is untouched, and anything in neither is left exactly as it came
/// so that serde produces the error rather than this silently dropping a field.
fn normalize_event(event: &mut Value) {
    let Some(object) = event.as_object_mut() else {
        return;
    };

    if let Some(attendees) = object.get_mut("attendees") {
        // A bare address becomes a Participant. A name-and-address string is
        // not parsed here: `Participant` has somewhere to put a display name,
        // and guessing where "Molly <molly@…>" divides is the composer's job,
        // not this one's.
        if let Some(list) = attendees.as_array_mut() {
            for entry in list.iter_mut() {
                if let Some(email) = entry.as_str() {
                    *entry = json!({ "email": email });
                }
            }
        } else if let Some(email) = attendees.as_str() {
            *attendees = json!([{ "email": email }]);
        }
    }

    for key in ["recurrence", "reminderMinutes"] {
        if let Some(value) = object.get_mut(key) {
            if value.is_string() || value.is_i64() || value.is_u64() {
                *value = Value::Array(vec![value.clone()]);
            }
        }
    }
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
            GET_EVENT_TOOL,
            "One event by its eventId, with its guests, their answers and its video \
             link. Call this for the event your answer is about: it is what shows the \
             event to the user as a card they can open in the calendar.",
            json!({
                "type": "object",
                "properties": { "eventId": { "type": "integer" } },
                "required": ["eventId"],
                "additionalProperties": false,
            }),
        ),
        Tool::auto(
            LIST_CALENDARS_TOOL,
            "Every calendar, across every account, with the calendarId createEvent and \
             moveEvent need. A calendar the user names — \u{201c}Dad/Ben Schedule\u{201d}, \
             \u{201c}Family\u{201d} — is a name, not an id: resolve it here first, and take \
             its accountId from the same row. writable is false on a subscription you can \
             only read, and Google refuses every write to one, so pick another calendar or \
             say so rather than trying.",
            json!({
                "type": "object",
                "properties": {
                    "accountId": { "type": "integer", "description": "Restrict to one account. Omit for every account." },
                },
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
        // Auto, exactly like `draft_reply`. Writing a draft tells nobody: it is
        // a row in the Drafts mailbox, editable and discardable, and
        // `send_draft` is still the approval that stands between it and another
        // human. Asking twice for one message would teach the owner to click
        // yes without reading, which costs the approval that matters.
        Tool::auto(
            NEW_DRAFT_TOOL,
            "Write a new message and save it as a draft. Use this whenever the message \
             is not an answer to an existing conversation: it starts a conversation of \
             its own, and the subject is the one you give here. draft_reply is for \
             answering a thread, and inherits that thread's subject. Nothing is sent. \
             Returns a draftId for send_draft.",
            json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Email addresses. \u{201c}Molly <molly@example.com>\u{201d} is accepted too.",
                    },
                    "cc": { "type": "array", "items": { "type": "string" } },
                    "bcc": { "type": "array", "items": { "type": "string" } },
                    "subject": { "type": "string", "description": "Used exactly as written." },
                    "body": { "type": "string", "description": "The message text. Markdown-ish: *bold*, _italic_, - bullets." },
                    "accountId": {
                        "type": "integer",
                        "description": "Send from this account. Omit to use the default account.",
                    },
                },
                "required": ["to", "subject", "body"],
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

/// Gmail filters — the one thing the agent can make that outlives the session.
///
/// # Why creating one has to ask
///
/// Every other write the agent performs is an act on mail that already exists,
/// bounded by the ids it names and undoable by the command layer's own inverse.
/// A filter is neither. It is a standing rule that acts on mail nobody has
/// written yet, silently, for as long as it exists, and the two most useful
/// things it can express are the two least visible: `removeLabelIds: ["INBOX"]`
/// means the mail never appears, and `addLabelIds: ["TRASH"]` means it is
/// deleted on arrival. Nothing in the mailbox moves when the rule is made, so
/// there is no moment at which the owner could notice it and take it back.
///
/// That is the exact shape of thing that must not happen because a model
/// inferred it from a sentence, so both writes are [`ToolPolicy::Approve`] and
/// the prompt is the sentence in [`crate::commands::filters::describe`], not
/// the JSON.
///
/// Listing is a read of the account's own settings and asks nothing.
fn filter_tools() -> Vec<Tool> {
    vec![
        Tool::auto(
            LIST_FILTERS_TOOL,
            "The Gmail filters on an account: what each one matches, what it does, and \
             the filterId delete_filter needs. Read this before creating one — the rule \
             the user is asking for may already exist.",
            json!({
                "type": "object",
                "properties": {
                    "accountId": { "type": "integer", "description": "Omit for every account." },
                },
                "additionalProperties": false,
            }),
        ),
        Tool {
            definition: ToolDefinition {
                name: CREATE_FILTER_TOOL.to_string(),
                description:
                    "Create a Gmail filter: a standing rule Google applies to mail as it \
                     arrives. Requires the owner's confirmation before it runs. It does \
                     nothing to mail already in the mailbox — to deal with that, search \
                     and use the archive, trash or label commands, which are undoable. \
                     Removing the INBOX label is how \u{201c}skip the inbox\u{201d} is \
                     expressed; adding TRASH is how \u{201c}delete it\u{201d} is. There \
                     is no way to edit a filter: delete it and make another."
                        .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "accountId": {
                            "type": "integer",
                            "description": "Required unless exactly one account is connected.",
                        },
                        "from": { "type": "string", "description": "Sender. Matched as a substring, so a bare domain catches every address at it." },
                        "to": { "type": "string" },
                        "subject": { "type": "string", "description": "Matched anywhere in the subject." },
                        "query": { "type": "string", "description": "A Gmail search expression, the same language the search box takes." },
                        "negatedQuery": { "type": "string", "description": "A Gmail search expression that must not match." },
                        "hasAttachment": { "type": "boolean" },
                        "addLabelIds": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Label ids from list_labels, or TRASH, SPAM, STARRED, IMPORTANT.",
                        },
                        "removeLabelIds": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "INBOX to skip the inbox, UNREAD to mark it read.",
                        },
                        "forward": {
                            "type": "string",
                            "description": "An address Gmail already has verified for this account. Anything else is refused by Google.",
                        },
                    },
                    "additionalProperties": false,
                }),
            },
            policy: ToolPolicy::Approve,
        },
        Tool {
            definition: ToolDefinition {
                name: DELETE_FILTER_TOOL.to_string(),
                description:
                    "Delete a Gmail filter by its filterId. Requires the owner's \
                     confirmation. Mail the filter has already acted on stays where it \
                     was put; only the rule goes."
                        .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "accountId": { "type": "integer", "description": "Required unless exactly one account is connected." },
                        "filterId": { "type": "string" },
                    },
                    "required": ["filterId"],
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

/// One object a tool put in front of the owner, with enough of it to draw.
///
/// # Why this is a shape and not a sentence
///
/// The agent drafted a reply, the drawer printed *"Drafted a reply to the
/// bookkeeper…"*, and the draft was unreachable from anywhere in the app. Prose
/// is not an affordance. A tool that surfaces something — a draft it wrote, an
/// event it read, the one conversation a search landed on — hands back one of
/// these instead, the drawer draws it as a card, and the object stops being
/// orphaned.
///
/// # Reads carry one too
///
/// This started as "what a *write* made", and that restriction was the bug. Ask
/// for an event and the model would print the row it had just read out of
/// SQLite as a bulleted list: the title, the time, the guests, the Meet link,
/// none of it clickable. A read tool that surfaces a specific object has put the
/// owner in front of something exactly as a write has, so it carries an artifact
/// on the same terms.
///
/// **One object, or none.** `get_thread` and `get_event` name one thing and
/// always carry it. A tool that returns a list carries an artifact only when the
/// list has exactly one row in it, because that is the only case where the
/// answer is unambiguously *about* one object — seventeen cards for a week of
/// calendar is worse than the prose it replaced. To show one specific thing, the
/// model calls the singular tool for it; the system prompt says so.
///
/// It is deliberately a small closed enum rather than a free-form link. Each
/// variant is a thing the shell already knows how to open, so adding an
/// affordance to a new tool is filling this in rather than teaching the UI a
/// new kind of navigation. Everything past the ids is what the card *says* —
/// carried on the wire rather than looked up in the frontend, because the object
/// is very often outside the window the shell has loaded, which is the same
/// reason `start_ms` is here.
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
        /// Who it is addressed to, as the card shows them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        to: Vec<String>,
    },
    /// A conversation worth landing on.
    Thread {
        thread_id: i64,
        /// The subject.
        label: String,
        /// Who it is from — a display name where there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        /// When it last moved.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_email: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        unread: bool,
    },
    /// A calendar entry. `start_ms` is carried because the grid shows a window,
    /// and an event you cannot scroll to is as orphaned as a draft you cannot
    /// open.
    Event {
        event_id: i64,
        start_ms: i64,
        /// The title.
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "is_false")]
        all_day: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        /// The video call, which is the one field on an event people click.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conference_url: Option<String>,
        /// Who is coming, names where there are names. Capped: the card shows a
        /// few and counts the rest, and a fifty-guest all-hands must not put
        /// fifty addresses on the wire to be truncated in the frontend.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        guests: Vec<String>,
        /// How many there are in total, when more were dropped than sent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guest_count: Option<usize>,
        /// The owner's own answer: `accepted`, `declined`, `tentative`, `needsAction`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rsvp: Option<String>,
    },
}

/// How many guests ride along on an event artifact. Six is more than a card
/// shows and enough for the count line to be exact on an ordinary meeting.
const CARD_GUESTS: usize = 6;

fn is_false(value: &bool) -> bool {
    !*value
}

impl Artifact {
    /// One stored event, as much of it as a card draws.
    pub fn event(event: &crate::db::models::Event) -> Artifact {
        // `guests` is the richer list and `attendees` is what an event Mach
        // wrote itself carries; a reader takes whichever is populated.
        let names: Vec<String> = if event.guests.is_empty() {
            event.attendees.iter().map(short_name).collect()
        } else {
            event.guests.iter().map(guest_name).collect()
        };
        let total = names.len();
        Artifact::Event {
            event_id: event.id,
            start_ms: event.start_ts,
            label: event.title.clone(),
            end_ms: Some(event.end_ts),
            all_day: event.is_all_day,
            location: non_empty(event.location.clone()),
            conference_url: event
                .conference
                .as_ref()
                .and_then(|c| c.video())
                .map(|entry| entry.uri.clone()),
            guests: names.into_iter().take(CARD_GUESTS).collect(),
            guest_count: (total > CARD_GUESTS).then_some(total),
            rsvp: event.rsvp_status.map(|status| status.as_str().to_string()),
        }
    }

    /// One conversation, as much of it as a card draws.
    pub fn thread(summary: &crate::db::models::ThreadSummary) -> Artifact {
        Artifact::Thread {
            thread_id: summary.id,
            label: summary.subject.clone(),
            from: summary.participants.first().map(short_name),
            at_ms: Some(summary.last_message_at),
            account_email: non_empty(Some(summary.account_email.clone())),
            unread: summary.is_unread,
        }
    }
}

/// A person, as a card names them: the display name, or the address.
fn short_name(p: &crate::db::models::Participant) -> String {
    named(p.name.as_deref(), &p.email)
}

fn guest_name(g: &crate::db::models::EventGuest) -> String {
    named(g.name.as_deref(), &g.email)
}

fn named(name: Option<&str>, email: &str) -> String {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => email.to_string(),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
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
        GET_EVENT_TOOL => get_event(ctx, input),
        LIST_CALENDARS_TOOL => list_calendars(ctx, input),
        "list_labels" => list_labels(ctx, input),
        "list_accounts" => list_accounts(ctx),
        DRAFT_TOOL => draft_reply(ctx, input).await,
        NEW_DRAFT_TOOL => draft_message(ctx, input).await,
        SEND_TOOL => send_draft(ctx, input).await,
        LIST_FILTERS_TOOL => list_filters(ctx, input).await,
        CREATE_FILTER_TOOL => create_filter(ctx, input).await,
        DELETE_FILTER_TOOL => delete_filter(ctx, input).await,
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
    // The stored row, so the card says what the event *is* — its guests, its
    // video link, the end it was given — rather than only what was typed at it.
    // The draft is the fallback for a row that cannot be read back.
    let artifact = created.and_then(|(title, start_ms)| {
        let event_id = *result.applied.first()?;
        let stored = ctx
            .db
            .read(|conn| crate::db::command_queries::event_by_id(conn, event_id))
            .ok()
            .flatten();
        Some(match stored {
            Some(event) => Artifact::event(&event),
            None => Artifact::Event {
                event_id,
                start_ms,
                label: title,
                end_ms: None,
                all_day: false,
                location: None,
                conference_url: None,
                guests: Vec::new(),
                guest_count: None,
                rsvp: None,
            },
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
    let artifact = only_thread(&page.items);
    let items: Vec<Value> = page.items.iter().map(thread_row).collect();
    Ok(ToolOutcome {
        summary: format!("{} conversations", items.len()),
        payload: json!({ "threads": items }),
        mutated: false,
        artifact,
    })
}

/// The card a list earns: one row, or nothing.
///
/// A mailbox with seventeen conversations in it is a list, and the drawer has
/// no business drawing seventeen cards under one tool line. One row is the case
/// where the list *is* the object.
fn only_thread(items: &[crate::db::models::ThreadSummary]) -> Option<Artifact> {
    match items {
        [one] => Some(Artifact::thread(one)),
        _ => None,
    }
}

fn search_threads(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::invalid("search_threads needs a query"))?;
    let page = reads::search_threads(&ctx.db, query, Some(limit_of(input, 20)))
        .map_err(ipc_to_agent)?;
    let artifact = only_thread(&page.items);
    let items: Vec<Value> = page.items.iter().map(thread_row).collect();
    Ok(ToolOutcome {
        summary: format!("{} matches for \u{201c}{query}\u{201d}", items.len()),
        payload: json!({ "threads": items }),
        mutated: false,
        artifact,
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
        // Naming one thread is putting the owner in front of it.
        artifact: Some(Artifact::thread(&detail.thread)),
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
    // One event in the range is the range answering with an object. Seventeen
    // is a list — see [`Artifact`], and `get_event` for how the model shows one.
    let artifact = match events.as_slice() {
        [one] => Some(Artifact::event(one)),
        _ => None,
    };
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
        artifact,
    })
}

/// One event, by id — the calendar's `get_thread`.
///
/// It exists for the card. `list_events` answers "what is on Thursday" and
/// cannot know which of the seventeen rows the reply is about; this is how the
/// model says *this one*, and the owner gets something to click instead of a
/// bulleted transcription of the row.
fn get_event(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let event_id = required_i64(input, "eventId")?;
    let event = ctx
        .db
        .read(|conn| crate::db::command_queries::event_by_id(conn, event_id))
        .map_err(|e| AgentError::invalid(e.to_string()))?
        .ok_or_else(|| AgentError::invalid(format!("no event {event_id} in the local store")))?;

    let guests: Vec<Value> = if event.guests.is_empty() {
        event.attendees.iter().map(participant).collect()
    } else {
        event
            .guests
            .iter()
            .map(|g| {
                json!({
                    "who": super::context::who(&crate::db::models::Participant {
                        name: g.name.clone(),
                        email: g.email.clone(),
                    }),
                    "response": g.response.map(|r| r.as_str()),
                    "optional": g.optional,
                    "organizer": g.organizer,
                })
            })
            .collect()
    };

    Ok(ToolOutcome {
        summary: format!("Read \u{201c}{}\u{201d}", event.title),
        payload: json!({
            "eventId": event.id,
            "accountId": event.account_id,
            "calendarId": event.calendar_id,
            "title": event.title,
            "startMs": event.start_ts,
            "endMs": event.end_ts,
            "when": super::context::human_time(event.start_ts),
            "isAllDay": event.is_all_day,
            "location": event.location,
            "description": event.description,
            "guests": guests,
            "rsvp": event.rsvp_status.map(|r| r.as_str()),
            "conference": event
                .conference
                .as_ref()
                .and_then(|c| c.video())
                .map(|entry| entry.uri.clone()),
        }),
        mutated: false,
        artifact: Some(Artifact::event(&event)),
    })
}

/// The calendars, as the four facts a write actually needs.
///
/// `id` and `accountId` are what `createEvent` takes, `name` is what the user
/// said, and `writable` is whether trying is worth a round trip. Everything
/// else `ipc::reads::list_calendars` carries — the colours, the palette index,
/// Google's `selected` — is for drawing the grid, and it is dropped here rather
/// than spent on context the model cannot act on.
///
/// `writable` is computed by [`role_writable`], the same rule
/// `db::models::Calendar::writable` uses and the same two role names
/// `canEditEvent` opens with in the frontend. A read-only calendar refuses
/// every write regardless of who is asking, so the answer must not depend on
/// which of the three asks.
///
/// A tombstoned calendar — unsubscribed in Google, still holding events here —
/// is listed with `writable: false`, because it is: Google will not take an
/// insert for a calendar this account no longer subscribes to. It is listed at
/// all so that "move the Molly events off the old calendar" can name it.
///
/// [`role_writable`]: crate::db::models::role_writable
fn list_calendars(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let account_id = input.get("accountId").and_then(Value::as_i64);
    let calendars = reads::list_calendars(&ctx.db).map_err(ipc_to_agent)?;
    let items: Vec<Value> = calendars
        .iter()
        .filter(|c| account_id.is_none_or(|id| c.account_id == id))
        .map(|c| {
            json!({
                "calendarId": c.id,
                "name": c.name,
                "accountId": c.account_id,
                "accountEmail": c.account_email,
                "primary": c.primary,
                "writable": !c.deleted
                    && crate::db::models::role_writable(c.access_role.as_deref()),
                "timeZone": c.time_zone,
                "unsubscribed": c.deleted,
            })
        })
        .collect();
    Ok(ToolOutcome {
        summary: format!("{} calendars", items.len()),
        payload: json!({ "calendars": items }),
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
    Ok(drafted(saved))
}

/// A message to somebody, on no conversation at all.
///
/// The composer's `c` builds a blank draft in the window and lets the first
/// autosave write the row; this builds the same object and hands it straight to
/// `saveDraft`, which is the same write — the row, the mirror that puts it in
/// the Drafts mailbox, and the push to Gmail. There is no second way to make a
/// draft, and in particular no `prepare`: `prepare` exists to read a *thread*,
/// and a message being started has none.
///
/// The subject goes in as written. Nothing here derives one, and `kind: new`
/// is what keeps [`crate::ipc::compose::engine::draft::build`] from threading
/// the message onto anything at send.
async fn draft_message(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let to = mailboxes(input, "to")?;
    if to.is_empty() {
        return Err(AgentError::invalid(
            "draft_message needs at least one address in to",
        ));
    }
    let cc = mailboxes(input, "cc")?;
    let bcc = mailboxes(input, "bcc")?;
    let subject = input
        .get("subject")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::invalid("draft_message needs a subject"))?;
    let body = input
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::invalid("draft_message needs a body"))?;
    let account_id = compose_account(ctx, input)?;

    // Only the fields the model owns. Everything else — `bodyFormat`, the
    // Gmail half, the attachments — takes the `Draft` struct's own default,
    // which is the same thing that happens to a draft arriving from the editor
    // without them.
    let draft = json!({
        "id": crate::ipc::compose::new_draft_id(now_ms()),
        "accountId": account_id,
        "threadId": Value::Null,
        "replyToId": Value::Null,
        "kind": "new",
        "to": to,
        "cc": cc,
        "bcc": bcc,
        "subject": subject,
        "body": body,
    });

    let saved = compose_dispatch(&ctx.db, &ctx.outbox, json!({ "op": "saveDraft", "draft": draft }))
        .await?;
    Ok(drafted(saved))
}

/// What `saveDraft` came back with, as a tool result the drawer can open.
fn drafted(saved: Value) -> ToolOutcome {
    let draft = saved.get("draft").cloned().unwrap_or(Value::Null);

    let subject = draft
        .get("subject")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
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
            to: recipients(&draft),
        });

    ToolOutcome {
        summary: format!("Drafted \u{201c}{subject}\u{201d}"),
        payload: json!({ "draft": draft }),
        // The draft is now a row in the Drafts mailbox, so the list is stale —
        // this is what makes it appear without a relaunch.
        mutated: true,
        artifact,
    }
}

/// Who a saved draft is addressed to, as the card names them.
fn recipients(draft: &Value) -> Vec<String> {
    draft
        .get("to")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|who| {
                    named(
                        who.get("name").and_then(Value::as_str),
                        who.get("email").and_then(Value::as_str).unwrap_or_default(),
                    )
                })
                .filter(|who| !who.is_empty())
                .take(CARD_GUESTS)
                .collect()
        })
        .unwrap_or_default()
}

/// Which account a message with nothing behind it is sent from.
///
/// The same three-step rule the composer follows in `composeAccountId`
/// (`src/lib/prefs.ts`), with the tool's own argument standing in for "the
/// mailbox the list is filtered to": what the caller named, then the stored
/// default if it still names an account that exists, then the first account,
/// which is the order the sidebar shows them in.
///
/// A named account that does not exist is an error rather than a fall-through.
/// The model asked to send from a particular address; quietly sending from a
/// different one is the class of mistake this whole tool exists to stop.
fn compose_account(ctx: &ToolContext, input: &Value) -> Result<i64, AgentError> {
    let accounts = ctx.db.read(crate::db::queries::list_accounts)?;
    if accounts.is_empty() {
        return Err(AgentError::invalid(
            "there is no account to send from — add one in Mach first",
        ));
    }

    if let Some(named) = input.get("accountId").and_then(Value::as_i64) {
        return accounts
            .iter()
            .find(|a| a.id == named)
            .map(|a| a.id)
            .ok_or_else(|| {
                let known: Vec<String> = accounts
                    .iter()
                    .map(|a| format!("{} (accountId {})", a.email, a.id))
                    .collect();
                AgentError::invalid(format!(
                    "there is no account with id {named}. The accounts are: {}",
                    known.join(", ")
                ))
            });
    }

    let preferred = ctx.db.read(crate::ipc::prefs::default_account_id)?;
    if let Some(id) = preferred.filter(|id| accounts.iter().any(|a| a.id == *id)) {
        return Ok(id);
    }
    Ok(accounts[0].id)
}

/// The addresses in one field, parsed and then actually checked.
///
/// [`parse_list`] is the composer's own grammar, so `"Molly <molly@x.com>"` and
/// a bare address both work, and it is deliberately forgiving — a typed field
/// must not refuse a keystroke. Nothing here is typed, though, and a draft
/// addressed to `molly` would sit in the Drafts mailbox looking finished and
/// fail at send. So a malformed address fails the tool call instead: the drawer
/// shows it in red and the model is told what was wrong with it.
///
/// [`parse_list`]: crate::ipc::compose::engine::address::parse_list
fn mailboxes(input: &Value, field: &str) -> Result<Vec<Value>, AgentError> {
    use crate::ipc::compose::engine::address;

    let entries: Vec<String> = match input.get(field) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(list)) => list
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    AgentError::invalid(format!("{field} takes email addresses, as strings"))
                })
            })
            .collect::<Result<_, _>>()?,
        Some(_) => {
            return Err(AgentError::invalid(format!(
                "{field} takes an array of email addresses"
            )))
        }
    };

    let mut parsed = Vec::new();
    for entry in &entries {
        parsed.extend(address::parse_list(entry));
    }
    let parsed = address::dedupe(parsed);

    let bad: Vec<String> = parsed
        .iter()
        .map(|mailbox| mailbox.email.clone())
        .filter(|email| !is_address(email))
        .collect();
    if !bad.is_empty() {
        return Err(AgentError::invalid(format!(
            "{} in {field} {} not an email address. Use the address itself, \
             e.g. molly@example.com — search the mail for it rather than guessing.",
            bad.join(", "),
            if bad.len() == 1 { "is" } else { "are" },
        )));
    }

    Ok(parsed
        .iter()
        .map(|mailbox| serde_json::to_value(mailbox).unwrap_or(Value::Null))
        .collect())
}

/// Whether a string is shaped like an email address.
///
/// A shape check and nothing more — no mailbox on the other end is being
/// claimed. It rejects what the model actually gets wrong: a bare name, a
/// display name that lost its address, two addresses run together, a domain
/// with no dot in it.
fn is_address(email: &str) -> bool {
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && !email
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>,;\"".contains(c))
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
// filters
// ---------------------------------------------------------------------------

/// The account a filter tool call is about.
///
/// Optional in the schema and resolved here, because with one account "make a
/// filter for the login codes" should not fail on a field the model had no way
/// to know. With more than one, guessing would put a rule on the wrong mailbox,
/// so it asks — through an error the model reports back rather than a silent
/// pick.
pub fn filter_account(ctx: &ToolContext, input: &Value) -> Result<i64, AgentError> {
    if let Some(id) = input.get("accountId").and_then(Value::as_i64) {
        return Ok(id);
    }
    let accounts = reads::list_accounts(&ctx.db).map_err(ipc_to_agent)?;
    match accounts.as_slice() {
        [only] => Ok(only.id),
        [] => Err(AgentError::invalid("no account is connected")),
        many => Err(AgentError::invalid(format!(
            "accountId is required — {} accounts are connected: {}",
            many.len(),
            many.iter()
                .map(|a| format!("{} ({})", a.email, a.id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// A `create_filter` call as the two halves Gmail wants.
///
/// Shared with the gate: the sentence the owner approves has to be built from
/// the same arguments the call will use, or it is describing something else.
pub fn filter_from_call(input: &Value) -> Filter {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let ids = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    Filter {
        id: String::new(),
        criteria: FilterCriteria {
            from: text("from"),
            to: text("to"),
            subject: text("subject"),
            query: text("query"),
            negated_query: text("negatedQuery"),
            has_attachment: input.get("hasAttachment").and_then(Value::as_bool),
            exclude_chats: None,
        },
        action: FilterAction {
            add_label_ids: ids("addLabelIds"),
            remove_label_ids: ids("removeLabelIds"),
            forward: text("forward"),
        },
    }
}

async fn list_filters(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let account_id = input.get("accountId").and_then(Value::as_i64);
    let filters = ctx.dispatcher.list_filters(account_id).await?;
    let items: Vec<Value> = filters
        .iter()
        .map(|f| {
            json!({
                "filterId": f.id,
                "accountId": f.account_id,
                "accountEmail": f.account_email,
                "description": f.description,
                "criteria": f.criteria,
                "action": f.action,
            })
        })
        .collect();
    Ok(ToolOutcome {
        summary: format!("{} filters", items.len()),
        payload: json!({ "filters": items }),
        mutated: false,
        artifact: None,
    })
}

async fn create_filter(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let account_id = filter_account(ctx, input)?;
    let created = ctx
        .dispatcher
        .create_filter(account_id, filter_from_call(input))
        .await?;
    Ok(ToolOutcome {
        summary: created.description.clone(),
        payload: json!({ "filterId": created.id, "description": created.description }),
        // Nothing in the mailbox moved: a filter acts on what arrives next.
        // Saying otherwise would make the window re-read for no reason.
        mutated: false,
        artifact: None,
    })
}

async fn delete_filter(ctx: &ToolContext, input: &Value) -> Result<ToolOutcome, AgentError> {
    let account_id = filter_account(ctx, input)?;
    let filter_id = input
        .get("filterId")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::invalid("delete_filter needs a filterId"))?;
    ctx.dispatcher.delete_filter(account_id, filter_id).await?;
    Ok(ToolOutcome {
        summary: "Filter deleted".to_string(),
        payload: json!({ "filterId": filter_id, "deleted": true }),
        mutated: false,
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

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn a_command_nobody_has_judged_asks_first() {
        // The gap this list was inverted to close. `unsubscribe` looks like a
        // label move, reaches a stranger's server, and is not undoable — and
        // under the old default it would have arrived in the catalogue as an
        // auto tool, silently, with nobody making a decision about it.
        assert_eq!(command_policy("unsubscribe"), ToolPolicy::Approve);
        assert_eq!(command_policy("forwardThread"), ToolPolicy::Approve);
        assert_eq!(command_policy(""), ToolPolicy::Approve);
        // And a tool name that is not in the surface at all.
        assert_eq!(policy_for("exfiltrate"), ToolPolicy::Approve);
    }

    #[test]
    fn the_auto_set_is_exactly_the_undoable_label_moves() {
        // Pinned by name so widening it is an edit to this test as well, made
        // by somebody who had to type out what they were adding.
        assert_eq!(
            AUTO_COMMANDS,
            &[
                "archive", "unarchive", "markRead", "star", "label", "moveToInbox", "notSpam",
                "trash", "untrash", "snooze", "unsnooze"
            ]
        );
    }

    #[test]
    fn everything_in_the_catalogue_outside_the_auto_set_asks() {
        for spec in Command::catalogue() {
            let expected = match AUTO_COMMANDS.contains(&spec.kind) {
                true => ToolPolicy::Auto,
                false => ToolPolicy::Approve,
            };
            assert_eq!(policy_for(spec.kind), expected, "{}", spec.kind);
        }
        // The ones that reach another human, by name, so a refactor that
        // quietly dropped one from the catalogue fails here.
        for kind in ["reportSpam", "unsubscribe", "rsvp", "createEvent", "deleteEvent"] {
            assert_eq!(policy_for(kind), ToolPolicy::Approve, "{kind}");
        }
    }

    #[test]
    fn sending_and_the_standing_rules_ask_and_the_reads_do_not() {
        assert_eq!(policy_for(SEND_TOOL), ToolPolicy::Approve);
        assert_eq!(policy_for(CREATE_FILTER_TOOL), ToolPolicy::Approve);
        assert_eq!(policy_for(DELETE_FILTER_TOOL), ToolPolicy::Approve);
        for name in ["get_thread", "search_threads", "list_labels", LIST_FILTERS_TOOL] {
            assert_eq!(policy_for(name), ToolPolicy::Auto, "{name}");
        }
        // Drafting is local and unsent; it is the send that asks.
        assert_eq!(policy_for(DRAFT_TOOL), ToolPolicy::Auto);
        assert_eq!(policy_for(NEW_DRAFT_TOOL), ToolPolicy::Auto);
    }
}
