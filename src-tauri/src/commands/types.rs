//! The command vocabulary itself.
//!
//! [`Command`] is the app's entire write surface. The UI dispatches these; the
//! agent's tools will *be* these. That dual role is why the enum is internally
//! tagged with `kind` and camelCased: the same JSON that crosses Tauri IPC from
//! `src/lib/data.ts` is also a serviceable tool-call schema, and
//! [`Command::catalogue`] describes it without anyone reading this file.

use serde::{Deserialize, Serialize};

use crate::db::models::{Participant, RsvpStatus};

/// A thread's label set and unread flag at a point in time.
///
/// This is what makes undo exact. Restoring a thread means putting *these*
/// labels back — not adding INBOX and hoping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLabelState {
    pub thread_id: i64,
    pub label_ids: Vec<String>,
    pub is_unread: bool,
}

/// Which occurrences of a recurring series an edit addresses.
///
/// Google expands series into instances (`singleEvents=true`), so a local row
/// is always *one occurrence*. Patching that row's own id changes only it;
/// patching its `recurringEventId` changes the whole series. Those are the two
/// things Google's API can express directly, and they are the two this enum
/// offers.
///
/// **`thisAndFollowing` is deliberately absent.** Google has no endpoint for it:
/// doing it properly means reading the master's RRULE, rewriting it with an
/// `UNTIL`, and inserting a second series — three calls whose failure modes
/// leave a split series behind. The UI says so rather than silently applying
/// one of the two that do exist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventScope {
    /// Just this occurrence — and the only meaning for a non-recurring event.
    #[default]
    This,
    /// The whole series, via the instance's `recurringEventId`.
    All,
}

impl EventScope {
    pub fn as_str(self) -> &'static str {
        match self {
            EventScope::This => "this",
            EventScope::All => "all",
        }
    }
}

/// Everything needed to bring an event into being.
///
/// `recurrence` and `reminderMinutes` used to be write-only — sendable but with
/// nowhere local to land, so an event created weekly came back a one-off.
/// Migration 5 gave both a column and they now round trip; an [`EventPatch`]
/// inverse can carry a recurrence rule.
///
/// One asymmetry survives, and the modal has to say so out loud: Google has
/// *three* reminder states — the calendar's default, an explicit set of alerts,
/// and none at all — while `reminderMinutes` is an `Option<i64>` that can only
/// name two of them. There is no way to spell "go back to the default" here,
/// which is why a reminder edit only inverts when the prior state was
/// expressible. Widening this to Google's `{useDefault, overrides}` shape is
/// the fix, and it belongs with whoever next owns this file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventDraft {
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// Unix millis. All-day events are pinned to UTC midnight, matching what
    /// the sync layer writes for a `date`-only start.
    pub start_ts: i64,
    pub end_ts: i64,
    pub is_all_day: bool,
    pub attendees: Vec<Participant>,
    /// RRULE lines, verbatim: `["RRULE:FREQ=WEEKLY;BYDAY=TU"]`. Empty = one-off.
    pub recurrence: Vec<String>,
    /// Popup reminder offsets in minutes. `None` leaves Google's defaults on.
    pub reminder_minutes: Option<Vec<i64>>,
}

/// A partial edit: every field is optional, and only the named ones change.
///
/// An empty string clears a text field rather than setting it to `""` — there
/// is no third state to represent, and a double `Option` across the IPC
/// boundary would be a worse trade than that one convention.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub is_all_day: Option<bool>,
    pub attendees: Option<Vec<Participant>>,
    pub recurrence: Option<Vec<String>>,
    pub reminder_minutes: Option<Vec<i64>>,
}

impl EventPatch {
    /// True when the patch names nothing — a command that would be a no-op.
    pub fn is_empty(&self) -> bool {
        self == &EventPatch::default()
    }

    /// True when the patch would move the event in time.
    pub fn touches_time(&self) -> bool {
        self.start_ts.is_some() || self.end_ts.is_some() || self.is_all_day.is_some()
    }

    /// True when the patch names something that cannot appear in an inverse.
    ///
    /// Since migration 5 both fields *do* read back, so `calendar.rs` no longer
    /// consults this before building an inverse — it inverts recurrence
    /// directly and decides about reminders from whether the prior state can be
    /// spoken (see [`EventDraft`]). Kept because the question it answers is
    /// still a real one and callers outside the calendar may need it.
    pub fn touches_write_only(&self) -> bool {
        self.recurrence.is_some() || self.reminder_minutes.is_some()
    }
}

/// Every action Mach can take.
///
/// Shapes match the TypeScript `Command` union in `src/lib/data.ts`: same
/// `kind` strings, same field names. `trash`, `untrash` and `unsnooze` are the
/// three that the TypeScript side has yet to gain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Command {
    /// Remove INBOX. Nothing else about the thread's labels is touched, which
    /// is exactly why plain [`Command::Unarchive`] is a correct undo.
    #[serde(rename_all = "camelCase")]
    Archive { thread_ids: Vec<i64> },

    /// Add INBOX — or, when `restore` is non-empty, put those threads back to
    /// exactly the state it names. The UI dispatches the first form; undo
    /// produces the second when a simple "add INBOX" would not be faithful.
    #[serde(rename_all = "camelCase")]
    Unarchive {
        thread_ids: Vec<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restore: Vec<ThreadLabelState>,
    },

    /// `read: true` removes UNREAD, `false` adds it.
    #[serde(rename_all = "camelCase")]
    MarkRead { thread_ids: Vec<i64>, read: bool },

    #[serde(rename_all = "camelCase")]
    Star { thread_ids: Vec<i64>, starred: bool },

    /// Add or remove one Gmail label id.
    #[serde(rename_all = "camelCase")]
    Label {
        thread_ids: Vec<i64>,
        label_id: String,
        add: bool,
    },

    /// Report as spam: add SPAM, drop INBOX. Gmail's `!`.
    ///
    /// A command of its own rather than a [`Command::Label`] naming `SPAM`,
    /// for three reasons that are all about the undo:
    ///
    ///  * `Label` moves **one** label. Reporting spam moves two — SPAM on,
    ///    INBOX off — so the composition would have to be two commands, two
    ///    remote calls, and two entries on the undo stack for one keystroke.
    ///  * Between those two calls a thread can sit in the inbox *and* in spam,
    ///    and a failure of the second leaves it there.
    ///  * The inverse of `Label { SPAM, add: true }` is `Label { SPAM, add:
    ///    false }`, which takes the thread out of spam and does **not** put it
    ///    back in the inbox. That is the wrong answer for the common case and
    ///    the right answer for none.
    #[serde(rename_all = "camelCase")]
    ReportSpam { thread_ids: Vec<i64> },

    /// Gmail's "Not spam": drop SPAM, and put the thread back where it was.
    ///
    /// The pair is named for Gmail's own two buttons — Report spam, Not spam —
    /// rather than being forced into the `un-` shape `Unarchive` and `Untrash`
    /// have, because "unreport spam" is not a thing anybody says.
    ///
    /// **It carries `restore`, and this is the trap [`Command::Archive`]'s doc
    /// comment is about.** Archive removes INBOX and touches nothing else, so a
    /// bare "add INBOX" reverses it exactly. Reporting spam does two things,
    /// and one of them is conditional: a thread that was already archived, or
    /// sitting in a label and out of the inbox, did not *lose* an INBOX it
    /// never had. Reversing that with "remove SPAM, add INBOX" would deposit it
    /// in an inbox it was never in — undo as a second unrequested move. So the
    /// inverse names the exact prior label set, the way [`Command::Untrash`]
    /// does, and a thread that was starred, labelled, unread or already
    /// archived comes back as all of those.
    ///
    /// The plain form (no `restore`) is what a user dispatches from the Spam
    /// mailbox, and it means the obvious thing: out of spam, into the inbox.
    #[serde(rename_all = "camelCase")]
    NotSpam {
        thread_ids: Vec<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restore: Vec<ThreadLabelState>,
    },

    /// Move to Gmail's trash: add TRASH, drop INBOX.
    #[serde(rename_all = "camelCase")]
    Trash { thread_ids: Vec<i64> },

    /// Take a thread back out of the trash, restoring the labels it had.
    #[serde(rename_all = "camelCase")]
    Untrash {
        thread_ids: Vec<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restore: Vec<ThreadLabelState>,
    },

    /// Hide until `until` (unix millis). See `commands::mail` for how snooze is
    /// represented, given that Gmail's API has no such primitive.
    #[serde(rename_all = "camelCase")]
    Snooze { thread_ids: Vec<i64>, until: i64 },

    /// Wake a snoozed thread now, restoring the labels it was snoozed from.
    /// Dispatched both by the user and by whoever runs the wake clock.
    #[serde(rename_all = "camelCase")]
    Unsnooze { thread_ids: Vec<i64> },

    /// Respond to a calendar invitation.
    #[serde(rename_all = "camelCase")]
    Rsvp {
        event_id: i64,
        response: RsvpStatus,
    },

    /// Put a new event on a calendar. Inverse: [`Command::DeleteEvent`] naming
    /// the row that was created.
    #[serde(rename_all = "camelCase")]
    CreateEvent {
        account_id: i64,
        calendar_id: String,
        draft: EventDraft,
    },

    /// Change an existing event. Inverse: the same command carrying the prior
    /// values of exactly the fields this one named.
    #[serde(rename_all = "camelCase")]
    UpdateEvent {
        event_id: i64,
        #[serde(default)]
        patch: EventPatch,
        #[serde(default)]
        scope: EventScope,
    },

    /// Remove an event. Inverse: [`Command::CreateEvent`] rebuilt from the row
    /// as it stood — and `None` for a recurring occurrence, which cannot be put
    /// back into its series by any endpoint Google offers.
    #[serde(rename_all = "camelCase")]
    DeleteEvent {
        event_id: i64,
        #[serde(default)]
        scope: EventScope,
    },

    /// Move an event to another calendar, possibly on another account.
    /// Inverse: the same command pointing back at where it came from.
    #[serde(rename_all = "camelCase")]
    MoveEvent {
        event_id: i64,
        account_id: i64,
        calendar_id: String,
    },
}

impl Command {
    /// The `kind` discriminant, matching the serialized form.
    pub fn kind(&self) -> &'static str {
        match self {
            Command::Archive { .. } => "archive",
            Command::Unarchive { .. } => "unarchive",
            Command::MarkRead { .. } => "markRead",
            Command::Star { .. } => "star",
            Command::Label { .. } => "label",
            Command::ReportSpam { .. } => "reportSpam",
            Command::NotSpam { .. } => "notSpam",
            Command::Trash { .. } => "trash",
            Command::Untrash { .. } => "untrash",
            Command::Snooze { .. } => "snooze",
            Command::Unsnooze { .. } => "unsnooze",
            Command::Rsvp { .. } => "rsvp",
            Command::CreateEvent { .. } => "createEvent",
            Command::UpdateEvent { .. } => "updateEvent",
            Command::DeleteEvent { .. } => "deleteEvent",
            Command::MoveEvent { .. } => "moveEvent",
        }
    }

    /// The ids this command addresses — thread ids for mail, the event id for
    /// an RSVP.
    pub fn target_ids(&self) -> Vec<i64> {
        match self {
            Command::Archive { thread_ids }
            | Command::Unarchive { thread_ids, .. }
            | Command::MarkRead { thread_ids, .. }
            | Command::Star { thread_ids, .. }
            | Command::Label { thread_ids, .. }
            | Command::ReportSpam { thread_ids }
            | Command::NotSpam { thread_ids, .. }
            | Command::Trash { thread_ids }
            | Command::Untrash { thread_ids, .. }
            | Command::Snooze { thread_ids, .. }
            | Command::Unsnooze { thread_ids } => thread_ids.clone(),
            Command::Rsvp { event_id, .. }
            | Command::UpdateEvent { event_id, .. }
            | Command::DeleteEvent { event_id, .. }
            | Command::MoveEvent { event_id, .. } => vec![*event_id],
            // A create has no id until it has run; `CommandResult::applied`
            // carries the row it made.
            Command::CreateEvent { .. } => Vec::new(),
        }
    }

    /// True for the mail half of the vocabulary.
    pub fn is_mail(&self) -> bool {
        !matches!(
            self,
            Command::Rsvp { .. }
                | Command::CreateEvent { .. }
                | Command::UpdateEvent { .. }
                | Command::DeleteEvent { .. }
                | Command::MoveEvent { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// results
// ---------------------------------------------------------------------------

use super::error::CommandFailure;

/// What a dispatched command reports back.
///
/// `undo` is the point of the type. Every command that changed something hands
/// back the command that reverses it, narrowed to the ids that *actually*
/// changed, so undo is a first-class value rather than a special case in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    /// True when nothing failed. A no-op is still `ok`.
    pub ok: bool,
    /// A sentence for the status bar: "Archived 3 conversations".
    pub message: String,
    /// The command that reverses this one. `None` when nothing changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<Command>,
    /// What the ⌘Z entry should say, when that is **less** than [`Self::message`].
    ///
    /// Normally the two are the same sentence and this is `None`. It is filled
    /// in when a command did something its inverse cannot take back, which today
    /// means exactly one thing: trashing a selection that held drafts. Gmail's
    /// `drafts.delete` is permanent, so "Trashed 3 conversations · discarded 1
    /// draft" is the truth about what happened and "Trashed 3 conversations" is
    /// the truth about what ⌘Z would do. A button that offers the first is
    /// lying, so the two strings are kept apart rather than one being made to
    /// serve both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_label: Option<String>,
    /// Ids whose local state now reflects the command and whose remote call
    /// succeeded (or was unnecessary).
    pub applied: Vec<i64>,
    /// Ids that were rolled back, grouped by the failure that hit them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<CommandFailure>,
}

impl CommandResult {
    pub(crate) fn noop(message: impl Into<String>) -> Self {
        CommandResult {
            ok: true,
            message: message.into(),
            undo: None,
            undo_label: None,
            applied: Vec::new(),
            failed: Vec::new(),
        }
    }
}

/// "1 conversation" / "3 conversations".
pub(crate) fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}
