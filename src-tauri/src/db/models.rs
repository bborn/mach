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
    pub status: String,
    pub html_link: Option<String>,
    pub updated_at: i64,
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
