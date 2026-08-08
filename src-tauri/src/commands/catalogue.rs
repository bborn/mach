//! The command vocabulary, described to something that cannot read this
//! source.
//!
//! [`Command::catalogue`](super::Command::catalogue) is the whole reason the
//! command layer is worth calling a layer. The UI already knows these commands
//! because a human typed them into `src/lib/data.ts`. The agent will not: it
//! needs to *enumerate* what it can do and what each action takes, at runtime,
//! as data. This is that data, and it is `Serialize`, so it drops into a tool
//! schema with a small adapter and no reflection.
//!
//! It is kept beside the enum rather than derived from it because the
//! interesting half — what a parameter *means*, whether a command is undoable,
//! whether it batches — is not recoverable from Rust types.

use serde::Serialize;

use super::types::Command;

/// The shape of one parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ParamType {
    /// An array of local thread row ids.
    ThreadIds,
    /// A local event row id.
    EventId,
    Bool,
    /// Unix milliseconds.
    Timestamp,
    /// A Gmail label id, e.g. `INBOX` or `Label_12`.
    LabelId,
    /// One of `accepted`, `declined`, `tentative`, `needsAction`.
    RsvpResponse,
    /// An array of `{ threadId, labelIds, isUnread }` — an exact prior state.
    ThreadLabelStates,
    /// A local account row id.
    AccountId,
    /// A Google calendar id — usually an address, e.g. `alex@example.com`.
    CalendarId,
    /// A whole event: `{ title, startTs, endTs, isAllDay, location,
    /// description, attendees, recurrence, reminderMinutes }`.
    EventDraft,
    /// A partial event edit: the same fields, all optional.
    EventPatch,
    /// `this` or `all` — which occurrences of a series an edit addresses.
    EventScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamSpec {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub ty: ParamType,
    pub required: bool,
    pub description: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    /// Matches the serialized `kind` discriminant exactly.
    pub kind: &'static str,
    pub summary: &'static str,
    pub params: &'static [ParamSpec],
    /// Whether a successful run returns an inverse.
    pub undoable: bool,
    /// Whether the command accepts many ids and batches the remote call.
    pub batch: bool,
}

const THREAD_IDS: ParamSpec = ParamSpec {
    name: "threadIds",
    ty: ParamType::ThreadIds,
    required: true,
    description: "Local thread row ids to act on. May span accounts.",
};

const EVENT_ID: ParamSpec = ParamSpec {
    name: "eventId",
    ty: ParamType::EventId,
    required: true,
    description: "Local event row id.",
};

/// `thisAndFollowing` is absent on purpose: Google has no endpoint for it. See
/// `commands::calendar`.
const EVENT_SCOPE: ParamSpec = ParamSpec {
    name: "scope",
    ty: ParamType::EventScope,
    required: false,
    description: "this (default) for one occurrence, all for the whole series.",
};

const RESTORE: ParamSpec = ParamSpec {
    name: "restore",
    ty: ParamType::ThreadLabelStates,
    required: false,
    description: "Exact prior label sets to put back. Omit for the plain form.",
};

pub(crate) const CATALOGUE: &[CommandSpec] = &[
    CommandSpec {
        kind: "archive",
        summary: "Remove the conversation from the inbox, leaving its other labels alone.",
        params: &[THREAD_IDS],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "unarchive",
        summary: "Put the conversation back in the inbox, or restore an exact prior label set.",
        params: &[THREAD_IDS, RESTORE],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "markRead",
        summary: "Mark conversations read or unread.",
        params: &[
            THREAD_IDS,
            ParamSpec {
                name: "read",
                ty: ParamType::Bool,
                required: true,
                description: "true marks read, false marks unread.",
            },
        ],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "star",
        summary: "Star or unstar conversations.",
        params: &[
            THREAD_IDS,
            ParamSpec {
                name: "starred",
                ty: ParamType::Bool,
                required: true,
                description: "true stars, false unstars.",
            },
        ],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "label",
        summary: "Add or remove one Gmail label.",
        params: &[
            THREAD_IDS,
            ParamSpec {
                name: "labelId",
                ty: ParamType::LabelId,
                required: true,
                description: "The Gmail label id to add or remove.",
            },
            ParamSpec {
                name: "add",
                ty: ParamType::Bool,
                required: true,
                description: "true adds the label, false removes it.",
            },
        ],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "trash",
        summary: "Move conversations to the Gmail trash.",
        params: &[THREAD_IDS],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "untrash",
        summary: "Take conversations back out of the trash, restoring their labels.",
        params: &[THREAD_IDS, RESTORE],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "snooze",
        summary: "Hide conversations until a wake time, then return them to the inbox.",
        params: &[
            THREAD_IDS,
            ParamSpec {
                name: "until",
                ty: ParamType::Timestamp,
                required: true,
                description: "Wake time, unix milliseconds.",
            },
        ],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "unsnooze",
        summary: "Wake snoozed conversations now, restoring the labels they were snoozed from.",
        params: &[THREAD_IDS],
        undoable: true,
        batch: true,
    },
    CommandSpec {
        kind: "rsvp",
        summary: "Respond to a calendar invitation.",
        params: &[
            ParamSpec {
                name: "eventId",
                ty: ParamType::EventId,
                required: true,
                description: "Local event row id.",
            },
            ParamSpec {
                name: "response",
                ty: ParamType::RsvpResponse,
                required: true,
                description: "accepted, declined, tentative or needsAction.",
            },
        ],
        undoable: true,
        batch: false,
    },
    CommandSpec {
        kind: "createEvent",
        summary: "Put a new event on a calendar.",
        params: &[
            ParamSpec {
                name: "accountId",
                ty: ParamType::AccountId,
                required: true,
                description: "Which account's calendar to create on.",
            },
            ParamSpec {
                name: "calendarId",
                ty: ParamType::CalendarId,
                required: true,
                description: "The calendar id, as returned by list_calendars.",
            },
            ParamSpec {
                name: "draft",
                ty: ParamType::EventDraft,
                required: true,
                description: "Title, start and end in unix millis, and anything else to set.",
            },
        ],
        undoable: true,
        batch: false,
    },
    CommandSpec {
        kind: "updateEvent",
        summary: "Change an event's title, time, place, description or guests.",
        params: &[
            EVENT_ID,
            ParamSpec {
                name: "patch",
                ty: ParamType::EventPatch,
                required: true,
                description: "Only the fields to change. An empty string clears a text field.",
            },
            EVENT_SCOPE,
        ],
        undoable: true,
        batch: false,
    },
    CommandSpec {
        kind: "deleteEvent",
        summary: "Remove an event, or every occurrence of its series.",
        params: &[EVENT_ID, EVENT_SCOPE],
        undoable: true,
        batch: false,
    },
    CommandSpec {
        kind: "moveEvent",
        summary: "Move an event to a different calendar, possibly on another account.",
        params: &[
            EVENT_ID,
            ParamSpec {
                name: "accountId",
                ty: ParamType::AccountId,
                required: true,
                description: "The destination account.",
            },
            ParamSpec {
                name: "calendarId",
                ty: ParamType::CalendarId,
                required: true,
                description: "The destination calendar id.",
            },
        ],
        undoable: true,
        batch: false,
    },
];

impl Command {
    /// Every command the app can execute, with its parameters. Stable enough to
    /// hand to an agent as a tool list.
    pub fn catalogue() -> &'static [CommandSpec] {
        CATALOGUE
    }

    /// The spec for this command's kind.
    pub fn spec(&self) -> &'static CommandSpec {
        let kind = self.kind();
        CATALOGUE
            .iter()
            .find(|spec| spec.kind == kind)
            .expect("every Command variant has a catalogue entry")
    }
}
