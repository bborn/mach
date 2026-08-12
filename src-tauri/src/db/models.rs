//! Row types for the local store.
//!
//! These cross the Tauri IPC boundary into React, so everything here is
//! `Serialize`/`Deserialize` with camelCase field names — TypeScript reads them
//! directly with no adapter layer.
//!
//! Time is always **unix milliseconds as `i64`**. Not RFC3339 strings, not
//! seconds: millis sort correctly as an integer index key, survive the JSON
//! boundary as a JS `number`, and match what the Gmail API hands back in
//! `internalDate`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// shared value types
// ---------------------------------------------------------------------------

/// A name/address pair. Stored as JSON in `TEXT` columns rather than in a
/// people table: participants are display data, we never need to query across
/// them, and a join per inbox row would cost more than it buys.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub email: String,
}

impl Participant {
    pub fn new(email: impl Into<String>) -> Self {
        Participant {
            name: None,
            email: email.into(),
        }
    }
}

/// One address in the address book, folded across every message that mentions
/// it.
///
/// There is no contacts table and there should not be one. Every address worth
/// completing is already in `messages` — in `from_email` for everyone who has
/// written to you, and in `to_json`/`cc_json`/`bcc_json` for everyone you have
/// written to — so the address book is a query, not a copy that can go stale.
/// See [`crate::db::queries::address_book`].
///
/// The field names match `Contact` in `src/lib/contacts.ts`, because the
/// frontend merges these rows straight into the index it builds from whatever
/// is on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// Lowercased. This is the identity of a contact.
    pub email: String,
    /// The most recent non-empty display name seen for this address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How many messages *you* addressed to them: they appear in to/cc/bcc on
    /// a message whose sender is one of your accounts.
    pub sends: i64,
    /// The most recent moment this address appeared anywhere, unix millis.
    pub last_seen: i64,
    /// One of your own accounts. Kept, never dropped, and sorted last.
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// Gmail's two label flavours. Anything unrecognised is treated as a user
/// label, which is the harmless default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelType {
    System,
    User,
}

impl LabelType {
    pub fn as_str(self) -> &'static str {
        match self {
            LabelType::System => "system",
            LabelType::User => "user",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "system" => LabelType::System,
            _ => LabelType::User,
        }
    }
}

/// Calendar RSVP state for *our* account on an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RsvpStatus {
    NeedsAction,
    Declined,
    Tentative,
    Accepted,
}

impl RsvpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RsvpStatus::NeedsAction => "needsAction",
            RsvpStatus::Declined => "declined",
            RsvpStatus::Tentative => "tentative",
            RsvpStatus::Accepted => "accepted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "needsAction" => Some(RsvpStatus::NeedsAction),
            "declined" => Some(RsvpStatus::Declined),
            "tentative" => Some(RsvpStatus::Tentative),
            "accepted" => Some(RsvpStatus::Accepted),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// accounts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: i64,
    pub email: String,
    pub display_name: Option<String>,
    /// Keychain item name. The OAuth tokens themselves never touch SQLite.
    pub token_ref: String,
    /// Gmail `historyId` watermark. Stored as TEXT: Google documents it as an
    /// unsigned 64-bit value serialised as a string, we only ever echo it back,
    /// and TEXT avoids an i64 overflow question we would otherwise have to
    /// think about.
    pub history_id: Option<String>,
    /// Calendar incremental `syncToken`.
    pub calendar_sync_token: Option<String>,
    /// Index into the UI's fixed palette — drives the per-account colour bar.
    pub colour_index: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAccount {
    pub email: String,
    pub display_name: Option<String>,
    pub token_ref: String,
    pub colour_index: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCount {
    pub account_id: i64,
    pub unread: i64,
}

// ---------------------------------------------------------------------------
// labels
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub id: i64,
    pub account_id: i64,
    pub gmail_label_id: String,
    pub name: String,
    pub label_type: LabelType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLabel {
    pub account_id: i64,
    pub gmail_label_id: String,
    pub name: String,
    pub label_type: LabelType,
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

/// One row of the unified stream. Carries the account's email and colour index
/// so the list renders without a second lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: i64,
    pub account_id: i64,
    pub account_email: String,
    pub account_colour_index: i64,
    pub gmail_thread_id: String,
    pub participants: Vec<Participant>,
    pub subject: String,
    pub snippet: String,
    pub last_message_at: i64,
    pub is_unread: bool,
    pub message_count: i64,
    pub has_attachments: bool,
    pub label_ids: Vec<String>,
}

impl ThreadSummary {
    /// The keyset cursor that resumes the list immediately after this row.
    pub fn cursor(&self) -> ThreadCursor {
        ThreadCursor {
            last_message_at: self.last_message_at,
            id: self.id,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewThread {
    pub account_id: i64,
    pub gmail_thread_id: String,
    pub participants: Vec<Participant>,
    pub subject: String,
    pub snippet: String,
    pub last_message_at: i64,
    pub is_unread: bool,
    pub message_count: i64,
    pub has_attachments: bool,
    pub label_ids: Vec<String>,
}

/// Keyset cursor over `(last_message_at DESC, id DESC)`.
///
/// Offset pagination was rejected: the sync loop is inserting into the top of
/// this list continuously, and `LIMIT/OFFSET` would duplicate or skip rows
/// whenever a page arrives between two scroll fetches. A keyset cursor is also
/// the only form that stays O(log n) at any depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCursor {
    pub last_message_at: i64,
    pub id: i64,
}

/// Parameters for the unified stream. `Default` is "everything, newest first,
/// first page" — the inbox as it opens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadQuery {
    /// `None` = the unified stream across every account.
    pub account_id: Option<i64>,
    /// Gmail label id, e.g. `INBOX` or `Label_12`.
    pub label_id: Option<String>,
    pub unread_only: bool,
    /// `0` means "use the default page size" (`DEFAULT_PAGE_SIZE`).
    pub limit: u32,
    /// Resume point; rows strictly older than this are returned.
    pub after: Option<ThreadCursor>,
}

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

/// The namespace Mach mints its own message ids in.
///
/// A row Google has not been told about still needs a `gmail_message_id`: that
/// column is the key `messages` is upserted on, and it is what lets the same
/// row be found again when Google finally answers. So the composer writes one
/// of its own — `mach-draft:<draft id>` for an unsent draft,
/// `mach-outbox:<entry id>` for a message sitting out its send delay — and
/// swaps in Google's the moment it arrives.
///
/// Gmail's message ids are lowercase hex, so nothing real can start with this.
/// The two halves of the app can therefore ask one question, "is this id ours
/// or Google's", and get one answer: [`is_local_message_id`].
pub const LOCAL_ID_PREFIX: &str = "mach-";

/// An unsent draft's placeholder namespace. See [`LOCAL_ID_PREFIX`].
pub const DRAFT_ID_PREFIX: &str = "mach-draft:";

/// A queued outgoing message's placeholder namespace. See [`LOCAL_ID_PREFIX`].
pub const OUTBOX_ID_PREFIX: &str = "mach-outbox:";

/// Whether this id is Mach's own placeholder rather than one Google minted.
///
/// The empty string counts, because a row with no id is unaddressable for the
/// same reason and in the same place.
pub fn is_local_message_id(gmail_message_id: &str) -> bool {
    gmail_message_id.is_empty() || gmail_message_id.starts_with(LOCAL_ID_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub thread_id: i64,
    pub account_id: i64,
    pub gmail_message_id: String,
    /// RFC822 `Message-ID` header — the identity used for threading replies.
    pub rfc822_message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub from: Participant,
    /// `Reply-To`, when the sender set one. Mailing lists rely on it, so the
    /// composer prefers it over `From` when choosing where a reply goes.
    pub reply_to: Vec<Participant>,
    pub to: Vec<Participant>,
    pub cc: Vec<Participant>,
    pub bcc: Vec<Participant>,
    pub subject: String,
    pub body_html: Option<String>,
    pub body_text: Option<String>,
    /// `body_text` declared `format=flowed` (RFC 3676). False also means "we
    /// were never told", which is the same thing for every reader: the breaks
    /// stay exactly as they arrived. See migration 11.
    pub body_text_flowed: bool,
    /// `delsp=yes` alongside it. Meaningless without `body_text_flowed`.
    pub body_text_delsp: bool,
    /// `body_html` was dropped to reclaim disk and Gmail still has it.
    ///
    /// The difference between "this message has no HTML part" — ordinary, and
    /// true of every plain-text mail — and "we let go of the HTML we had". Only
    /// the second is worth a request, and only the second renders as text now
    /// and upgrades when the request lands. See [`crate::evict`].
    pub html_evicted: bool,
    pub snippet: String,
    pub internal_date: i64,
    pub is_unread: bool,
    pub is_draft: bool,
    /// The composer draft this row is the mirror of, when it is one.
    ///
    /// The frontend's half of the same identity `compose::mirror` writes: the
    /// composer knows which draft it has just sent, and this is what lets the
    /// open conversation drop that row in the same frame rather than waiting for
    /// the write and the refetch behind it. See `useMach`'s `draftSent`.
    pub mach_draft_id: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMessage {
    pub thread_id: i64,
    pub account_id: i64,
    pub gmail_message_id: String,
    pub rfc822_message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub from: Participant,
    /// `Reply-To`, when the sender set one. Mailing lists rely on it, so the
    /// composer prefers it over `From` when choosing where a reply goes.
    pub reply_to: Vec<Participant>,
    pub to: Vec<Participant>,
    pub cc: Vec<Participant>,
    pub bcc: Vec<Participant>,
    pub subject: String,
    pub body_html: Option<String>,
    pub body_text: Option<String>,
    /// See [`Message::body_text_flowed`]. Defaults to `false`, so every writer
    /// that does not know about `format=flowed` — the composer's own mirror
    /// rows, the outbox — stores "not flowed", which is what they are.
    pub body_text_flowed: bool,
    pub body_text_delsp: bool,
    pub snippet: String,
    pub internal_date: i64,
    pub is_unread: bool,
    pub is_draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadWithMessages {
    pub thread: ThreadSummary,
    /// Oldest first — the order a conversation is read in.
    pub messages: Vec<Message>,
}

// ---------------------------------------------------------------------------
// attachments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: i64,
    pub message_id: i64,
    pub gmail_attachment_id: Option<String>,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    /// Set once the bytes have been fetched to disk; `None` means metadata only.
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAttachment {
    pub message_id: i64,
    pub gmail_attachment_id: Option<String>,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub local_path: Option<String>,
}

// ---------------------------------------------------------------------------
// calendar
// ---------------------------------------------------------------------------

/// One alert, as an offset in minutes before the event starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventReminder {
    /// Google's `popup`, `email` or `sms`. Kept verbatim rather than as an enum:
    /// we only ever create `popup`, but we must not silently rewrite an alert
    /// someone set to something else on the web.
    pub method: String,
    pub minutes: i64,
}

/// An event's alerts, in the shape Google models them.
///
/// `use_default` is not decoration and not derivable from the list. Google has
/// three states, and all three are reachable from the UI: follow the calendar's
/// default (`use_default: true`), no alert at all (`use_default: false` with an
/// empty `overrides`), and these specific alerts. A bare `Vec<i64>` collapses
/// the first two into "empty", which is how an event that should have popped up
/// silently stops popping up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventReminders {
    pub use_default: bool,
    #[serde(default)]
    pub overrides: Vec<EventReminder>,
}

impl EventReminders {
    /// The offsets an [`crate::commands::EventPatch`] can name — i.e. the
    /// explicit ones. `None` when the event is on the calendar's default, which
    /// the patch vocabulary has no way to express and must not pretend to.
    pub fn explicit_minutes(&self) -> Option<Vec<i64>> {
        if self.use_default {
            return None;
        }
        Some(self.overrides.iter().map(|r| r.minutes).collect())
    }
}

/// One guest, with the answer they gave.
///
/// [`Participant`] cannot carry this and should not learn to: it is the shape of
/// a name on a mail header, used in six places that have no notion of an RSVP.
/// A guest on an invitation is a different thing — the same address plus the
/// four facts that make a guest list readable, which is why `attendees` (the
/// editable list of addresses) and `guests` (Google's answer sheet) are two
/// columns and not one.
///
/// `comment` is worth the column on its own. "Declined because I am out of
/// office" is frequently the entire content of the notification a decline
/// generates, and dropping it means the user learns that someone said no but
/// not why — which is the half that would have changed what they did next.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventGuest {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `None` where Google sent no `responseStatus` at all — which is not the
    /// same as `needsAction`, and is what every row written before this column
    /// existed reads as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<RsvpStatus>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub organizer: bool,
    /// Google's `self` — the signed-in account's own row.
    #[serde(default)]
    pub is_self: bool,
    /// A meeting room or other bookable resource rather than a person.
    #[serde(default)]
    pub resource: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl EventGuest {
    /// The plain address-book form of this guest, for the editable list.
    pub fn participant(&self) -> Participant {
        Participant {
            name: self.name.clone().filter(|n| !n.is_empty()),
            email: self.email.clone(),
        }
    }
}

/// One way into a conference — a video link, a dial-in, a SIP address, or the
/// page listing the other twenty phone numbers.
///
/// `uri` is a string an attacker chose: an invitation is an unauthenticated
/// write into this store from anyone who knows the user's address. It is stored
/// verbatim and rendered as text; the only place it is ever *followed* is the
/// join affordance, which validates the scheme and host shape first, and
/// `ipc::render::open_external`, which validates the scheme again on the far
/// side of the IPC boundary because the webview is not a trust boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceEntry {
    /// `video`, `phone`, `sip` or `more`.
    pub kind: String,
    pub uri: String,
    /// The readable form — `meet.google.com/abc-defg-hij`, or a phone number
    /// spaced the way its country spaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_code: Option<String>,
}

/// The conference on an event, flattened out of Google's `conferenceData`.
///
/// Deliberately not a verbatim copy of the wire shape. `conferenceData` carries
/// a create-request block, a signature and a solution key that exist for
/// round-tripping a conference Mach never creates; what a person needs is the
/// name of the thing, the code to read out loud, and the ways in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventConference {
    /// The meeting code — `abc-defg-hij`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// "Google Meet", or what a third-party add-on calls itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_points: Vec<ConferenceEntry>,
    /// Google's free-text note ("This meeting is being recorded"), when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl EventConference {
    /// The video link, which is what a "Join" button means.
    pub fn video(&self) -> Option<&ConferenceEntry> {
        self.entry_points.iter().find(|e| e.kind == "video")
    }
}

/// A file attached to an event — always a Drive file today, since that is the
/// only kind Google's API accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttachment {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: i64,
    pub account_id: i64,
    pub calendar_id: String,
    pub google_event_id: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start_ts: i64,
    pub end_ts: i64,
    pub is_all_day: bool,
    pub attendees: Vec<Participant>,
    pub rsvp_status: Option<RsvpStatus>,
    /// Set on instances expanded from a recurring series (`singleEvents=true`).
    pub recurring_event_id: Option<String>,
    /// The series' RRULE/EXDATE lines, verbatim.
    ///
    /// Google never puts these on an expanded instance — the rule lives on the
    /// master, and `singleEvents=true` returns instances — so this is filled
    /// from the only two places that do know it: an event Mach itself created
    /// recurring, and any sibling occurrence of the same series that already
    /// carries it. Empty therefore means "no rule is known here", which is not
    /// the same as "does not repeat"; `recurring_event_id` is what answers that.
    #[serde(default)]
    pub recurrence: Vec<String>,
    /// Alerts, or `None` when Google did not say (a row written before the
    /// column existed, or an event created offline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<EventReminders>,
    /// The identity that survives a meeting being copied onto another account.
    ///
    /// Serialized as `iCalUID` because that is what Google calls it, what the
    /// merge code in `calendar-merge.ts` already reads for, and because a
    /// camelCased `iCalUid` would be a third spelling of the same thing.
    #[serde(default, rename = "iCalUID", skip_serializing_if = "Option::is_none")]
    pub ical_uid: Option<String>,
    /// Who is coming, and what each of them said.
    ///
    /// A superset of `attendees`, and never a replacement for it: `attendees` is
    /// the list the editor round-trips, and a save writes exactly what is in it.
    /// Empty on a row the store has never been told about, in which case
    /// [`crate::db::queries`] projects `attendees` into this shape so a reader
    /// never has to ask which of the two columns to look at.
    #[serde(default)]
    pub guests: Vec<EventGuest>,
    /// The video call, its code and its dial-ins. `None` for an event with no
    /// conference — the common case, and the reason this is not a struct of six
    /// nullable columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference: Option<EventConference>,
    /// Who made the event, which is not always who owns it.
    ///
    /// An assistant creating a meeting on a director's calendar, a room-booking
    /// system, a Zapier integration: in all three the creator and the organizer
    /// are different people, and Google shows both. Stored separately for that
    /// reason and shown only when the two disagree — repeating one name under
    /// two labels is noise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<Participant>,
    #[serde(default)]
    pub attachments: Vec<EventAttachment>,
    /// `default`, `public`, `private` or `confidential`. `None` means Google
    /// said nothing, which means `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// `opaque` (busy) or `transparent` (free). The thing that decides whether
    /// this event defends the time or merely records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Participant>,
    /// Google's `organizer.self`: whether the organizer is the calendar this
    /// copy of the event appears on. The authoritative "do I own this".
    ///
    /// `None` means the store has never been told — an old row, or one Mach
    /// wrote itself before this column existed. Readers treat that as permissive
    /// rather than as a denial: taking an edit affordance away on a guess is
    /// worse than offering one Google might refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer_self: Option<bool>,
    /// The one thing that lets a guest write to an event they do not own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_modify: Option<bool>,
    pub status: String,
    pub html_link: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEvent {
    pub account_id: i64,
    pub calendar_id: String,
    pub google_event_id: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start_ts: i64,
    pub end_ts: i64,
    pub is_all_day: bool,
    pub attendees: Vec<Participant>,
    pub rsvp_status: Option<RsvpStatus>,
    pub recurring_event_id: Option<String>,
    #[serde(default)]
    pub recurrence: Vec<String>,
    #[serde(default)]
    pub reminders: Option<EventReminders>,
    #[serde(default, rename = "iCalUID")]
    pub ical_uid: Option<String>,
    #[serde(default)]
    pub guests: Vec<EventGuest>,
    #[serde(default)]
    pub conference: Option<EventConference>,
    #[serde(default)]
    pub creator: Option<Participant>,
    #[serde(default)]
    pub attachments: Vec<EventAttachment>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub transparency: Option<String>,
    #[serde(default)]
    pub organizer: Option<Participant>,
    #[serde(default)]
    pub organizer_self: Option<bool>,
    #[serde(default)]
    pub guests_can_modify: Option<bool>,
    pub status: String,
    pub html_link: Option<String>,
    pub updated_at: i64,
}

/// A row of `calendars` — everything `calendarList.list` says about one
/// calendar as seen from one account.
///
/// Two fields deserve their names read carefully. `summary` is the calendar's
/// own title, set by whoever owns it; `summary_override` is the title *this*
/// account gave its subscription, and it is what the user actually recognises.
/// [`Calendar::title`] resolves the pair; nothing else should.
///
/// `access_role` is `Option` for the same reason `Event::organizer_self` is: a
/// row can exist before a metadata sweep has ever run, and silence there means
/// "not told", never "denied".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: i64,
    pub account_id: i64,
    /// Google's calendar id — usually an address, and never a label.
    pub calendar_id: String,
    pub summary: Option<String>,
    pub summary_override: Option<String>,
    pub description: Option<String>,
    pub time_zone: Option<String>,
    pub color_id: Option<String>,
    /// The colour the user chose, as `#rrggbb`.
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
    /// `owner`, `writer`, `reader`, `freeBusyReader` — or `None` for "not told".
    pub access_role: Option<String>,
    pub is_primary: bool,
    /// Google's own "is this calendar shown".
    pub selected: bool,
    /// Unsubscribed or removed. The row survives so its events keep a name.
    pub deleted: bool,
    pub synced_at: i64,
}

impl Calendar {
    /// The name to show, or `None` when Google supplied neither title.
    ///
    /// The override is checked first because it is the more specific answer:
    /// "Dad/Ben Schedule" is what this account renamed somebody else's
    /// "Ben — school" to, and showing the owner's title instead would be showing
    /// a name the user has explicitly replaced.
    ///
    /// The primary calendar is the one case this cannot answer alone: Google
    /// sends the account's own email address as `summary` there and substitutes
    /// the account holder's display name in its own UI. `None` here lets the
    /// caller — which has the `accounts` row and therefore the display name — do
    /// the same. See `ipc::reads::list_calendars`.
    pub fn title(&self) -> Option<&str> {
        self.summary_override
            .as_deref()
            .or(self.summary.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Whether Google would accept a write to events on this calendar.
    ///
    /// `reader` and `freeBusyReader` are read-only; `owner` and `writer` are
    /// not. Anything else — including `None` — is permissive, because an
    /// unrecognised role is far more likely to be a role we have not heard of
    /// than a denial, and the cost of guessing wrong in that direction is one
    /// refused request rather than an app that will not let you edit anything.
    pub fn writable(&self) -> bool {
        !matches!(self.access_role.as_deref(), Some("reader") | Some("freeBusyReader"))
    }
}

/// The upsert shape: the same row without the identity SQLite assigns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCalendar {
    pub account_id: i64,
    pub calendar_id: String,
    pub summary: Option<String>,
    pub summary_override: Option<String>,
    pub description: Option<String>,
    pub time_zone: Option<String>,
    pub color_id: Option<String>,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
    pub access_role: Option<String>,
    pub is_primary: bool,
    pub selected: bool,
    pub deleted: bool,
    pub synced_at: i64,
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// A ranked FTS5 hit. `score` is the best (lowest) bm25 value among the
/// thread's matching messages; more negative is a better match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadHit {
    pub thread_id: i64,
    pub account_id: i64,
    pub score: f64,
}
