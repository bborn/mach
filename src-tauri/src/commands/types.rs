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

/// Whether Google should tell the event's guests about this write.
///
/// # Why the default is "tell them"
///
/// Google's calendar API notifies nobody unless the request says `sendUpdates`.
/// Mach never said it, so for as long as this vocabulary has existed, an event
/// created here with three guests on it went onto one calendar and no others,
/// and nobody was told. The organizer's own view is indistinguishable from the
/// working case — the names are all there, in the right order, awaiting a reply
/// that was never asked for.
///
/// That failure is silent and one-directional, so the default is the loud
/// direction. Over-notifying costs an email nobody needed; under-notifying costs
/// a meeting nobody attends.
///
/// # And why it is not undoable
///
/// An invitation cannot be recalled. A command that notified guests still has an
/// exact *calendar* inverse — the event goes back to what it was, and the guests
/// are told about that too — but the emails already sent stay sent, so the
/// commands below report what they did in `message` and what ⌘Z can honour in
/// `undo_label`. See [`CommandResult::undo_label`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Notify {
    /// Everyone on the invitation hears about it — Google's `sendUpdates=all`.
    #[default]
    Guests,
    /// Guests outside this Workspace domain only.
    ExternalGuests,
    /// A silent write. Real, and never a default: fixing a typo in the notes of
    /// a thirty-person meeting should not email thirty people.
    Nobody,
}

impl Notify {
    pub fn as_send_updates(self) -> crate::google::types::SendUpdates {
        use crate::google::types::SendUpdates;
        match self {
            Notify::Guests => SendUpdates::All,
            Notify::ExternalGuests => SendUpdates::ExternalOnly,
            Notify::Nobody => SendUpdates::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Notify::Guests => "guests",
            Notify::ExternalGuests => "externalGuests",
            Notify::Nobody => "nobody",
        }
    }
}

/// What to do about an event's video call.
///
/// Google will not take a Meet URL you hand it. The only way onto an event is to
/// ask for a conference with a `createRequest` and read the minted link back off
/// the response, which is why this is a verb ("make me one", "take it off")
/// rather than a field holding a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Conferencing {
    /// Ask Google for a Meet link.
    Meet,
    /// Remove whatever call is on the event.
    None,
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
    /// Ask Google to mint a Meet link for this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conferencing: Option<Conferencing>,
    /// Who hears about it. Absent means [`Notify::Guests`] — see [`Notify`].
    ///
    /// It rides on the draft rather than on [`Command::CreateEvent`] because the
    /// draft is the whole payload the editor produces, and the answer to "should
    /// I invite these people" is made in the same breath as the guest list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<Notify>,
}

impl EventDraft {
    /// Who to tell, resolved. See [`Notify`] for why silence is not the default.
    pub fn notify(&self) -> Notify {
        self.notify.unwrap_or_default()
    }
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
    /// Add or remove the video call. See [`Conferencing`].
    pub conferencing: Option<Conferencing>,
    /// Who hears about it. Absent means [`Notify::Guests`] — see [`Notify`].
    pub notify: Option<Notify>,
}

impl EventPatch {
    /// True when the patch names nothing — a command that would be a no-op.
    ///
    /// `notify` is excluded on purpose: it is a directive about the *request*,
    /// not a field of the event, so a patch carrying only an answer to "tell the
    /// guests?" still changes nothing and still deserves no round trip.
    pub fn is_empty(&self) -> bool {
        EventPatch {
            notify: None,
            ..self.clone()
        } == EventPatch::default()
    }

    /// Who to tell, resolved. See [`Notify`] for why silence is not the default.
    pub fn notify(&self) -> Notify {
        self.notify.unwrap_or_default()
    }

    /// True when the change is one a guest would want to hear about.
    ///
    /// The time, the place, the name, the call and the guest list are all things
    /// a person acts on. A reminder offset is between the organizer and their own
    /// phone, and Google itself does not treat it as an update worth mailing.
    pub fn guests_would_care(&self) -> bool {
        self.title.is_some()
            || self.description.is_some()
            || self.location.is_some()
            || self.attendees.is_some()
            || self.recurrence.is_some()
            || self.conferencing.is_some()
            || self.touches_time()
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
/// `kind` strings, same field names.
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

    /// Respond to a calendar invitation, optionally with a note.
    ///
    /// `comment` is Google Calendar's line next to the three buttons — "Yes,
    /// I'll be ten minutes late". It replaces whatever note is on the account's
    /// own attendee row; `None` leaves it alone.
    ///
    /// `notify` defaults to telling the organizer, which is what pressing the
    /// same button in Google Calendar does. A response nobody is told about is
    /// visible in the organizer's calendar and in no mailbox.
    #[serde(rename_all = "camelCase")]
    Rsvp {
        event_id: i64,
        response: RsvpStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify: Option<Notify>,
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
    ///
    /// `notify` carries the cancellation. It lives on the command rather than in
    /// a payload because a delete has no payload; absent means guests are told,
    /// for the reason in [`Notify`].
    #[serde(rename_all = "camelCase")]
    DeleteEvent {
        event_id: i64,
        #[serde(default)]
        scope: EventScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify: Option<Notify>,
    },

    /// Move an event to another calendar, possibly on another account.
    /// Inverse: the same command pointing back at where it came from.
    ///
    /// A move is an insert plus a delete, so `notify` decides both halves — and
    /// with `Notify::Guests` the guests get one cancellation and one fresh
    /// invitation, which is the honest description of what happened to them.
    #[serde(rename_all = "camelCase")]
    MoveEvent {
        event_id: i64,
        account_id: i64,
        calendar_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify: Option<Notify>,
    },

    /// Leave the mailing list a message came from. **No inverse.**
    ///
    /// It is the one command in this vocabulary that breaks the contract in the
    /// module doc, and it breaks it in both directions: it writes nothing
    /// locally, and it cannot be undone. Both follow from what it is — a
    /// message to a stranger's server saying "stop", which no later message can
    /// take back.
    ///
    /// It is a command anyway, for two reasons that are worth the exception.
    /// The catalogue is the agent's tool list, and "unsubscribe me from this"
    /// is a thing to be able to ask for. And [`CommandResult`] already carries
    /// the truthful-failure shape this needs — `ok`, a sentence, and a
    /// [`CommandFailure`](super::CommandFailure) naming what refused — so
    /// routing it here means a sender that returns `500` surfaces through
    /// exactly the machinery a Gmail refusal does, rather than through a second
    /// one built for this feature.
    ///
    /// Addressed by message rather than by thread because the headers are
    /// per-message: a thread can hold two newsletters, and a digest from March
    /// can name an endpoint that has since moved.
    ///
    /// Whether the message may be unsubscribed from at all is not the caller's
    /// decision. [`crate::unsub::rule`] is re-run here from the store, so a
    /// stale UI — or an agent asking for the wrong thing — cannot turn this
    /// into a confirmation that his address is live.
    #[serde(rename_all = "camelCase")]
    Unsubscribe { message_id: i64 },
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
            Command::Unsubscribe { .. } => "unsubscribe",
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
            // Deliberately empty rather than the message id. Every caller of
            // this reads it as *thread* ids — the selection to re-select, the
            // rows to roll back — and a message id in that position would be a
            // silent type confusion against a table that has both.
            Command::Unsubscribe { .. } => Vec::new(),
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
    /// in when a command did something its inverse cannot take back, of which
    /// there are two cases.
    ///
    /// The second one is **telling the guests**. An invitation, an update or a
    /// cancellation that has gone out cannot be recalled — ⌘Z on a created event
    /// deletes it and sends a cancellation, which is the correct thing to do to
    /// the calendar and does nothing about the mail already in three inboxes. So
    /// "Created “Board call” · invited 3 guests" is the truth about what
    /// happened, and "Created “Board call”" is the truth about what ⌘Z takes
    /// back. The first is the older case: trashing a selection that held drafts.
    /// Gmail's
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
