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
    pub snippet: String,
    pub internal_date: i64,
    pub is_unread: bool,
    pub is_draft: bool,
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
