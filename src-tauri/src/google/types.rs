//! Wire types for the Gmail and Google Calendar REST APIs, plus the MIME
//! walking that turns Gmail's recursive part tree into a flat body + attachment
//! list.
//!
//! Everything here is pure data. No HTTP, no auth, no clock — so it is all
//! directly testable against fixture JSON.

use base64::engine::general_purpose::{URL_SAFE_NO_PAD, URL_SAFE_NO_PAD_INDIFFERENT};
use base64::Engine as _;
use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::{Deserialize, Serialize};

// =============================================================== base64url

/// Gmail encodes every body and attachment as base64url. Padding is usually
/// stripped but not always, and some senders' data round-trips through standard
/// base64 alphabets, so decoding is deliberately permissive.
pub fn decode_base64url(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let cleaned: String = input.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    match URL_SAFE_NO_PAD_INDIFFERENT.decode(&cleaned) {
        Ok(v) => Ok(v),
        Err(first) => {
            // Fall back to the standard alphabet before giving up.
            let translated: String = cleaned
                .chars()
                .map(|c| match c {
                    '+' => '-',
                    '/' => '_',
                    other => other,
                })
                .collect();
            URL_SAFE_NO_PAD_INDIFFERENT
                .decode(&translated)
                .map_err(|_| first)
        }
    }
}

/// Encode bytes the way Gmail's `raw` field wants them: base64url, no padding.
pub fn encode_base64url(input: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(input)
}

// =================================================================== Gmail

/// A `{ id, threadId }` pair — what the list and history endpoints return.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub thread_id: String,
}

/// `users.drafts` — an id, and the message it holds.
///
/// The two ids are different things and both matter: `id` addresses the draft
/// (update it, delete it, send it), while `message.id` is the ordinary Gmail
/// message id the sync engine will later see carrying the `DRAFT` label. Mach
/// stores both, and it is `message.id` that lets a synced draft land on the
/// local row Mach already wrote rather than beside it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub message: Message,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePartBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: i64,
    /// base64url-encoded payload. Absent for parts that must be fetched via
    /// `users.messages.attachments.get`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// One node of Gmail's recursive MIME tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Header>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<MessagePartBody>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<MessagePart>,
}

impl MessagePart {
    /// Case-insensitive header lookup, the only kind that is ever correct.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    pub fn mime(&self) -> &str {
        self.mime_type.as_deref().unwrap_or("text/plain")
    }

    fn is_multipart(&self) -> bool {
        self.mime().to_ascii_lowercase().starts_with("multipart/")
    }

    fn disposition(&self) -> Option<String> {
        self.header("content-disposition")
            .map(|d| d.trim().to_ascii_lowercase())
    }

    fn content_id(&self) -> Option<String> {
        self.header("content-id")
            .map(|v| v.trim().trim_start_matches('<').trim_end_matches('>').to_string())
            .filter(|v| !v.is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_id: Option<String>,
    /// Milliseconds since the epoch, as a decimal string. Google's int64 wire
    /// convention — parse with [`Message::internal_date_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_estimate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<MessagePart>,
    /// Only present with `format=raw`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl Message {
    pub fn header(&self, name: &str) -> Option<String> {
        self.payload
            .as_ref()
            .and_then(|p| p.header(name))
            .map(str::to_string)
    }

    pub fn internal_date_ms(&self) -> Option<i64> {
        self.internal_date.as_ref()?.parse().ok()
    }

    /// Walk the MIME tree and pull out the display bodies and attachments.
    pub fn extract_body(&self) -> ExtractedBody {
        match &self.payload {
            Some(p) => extract_body(p),
            None => ExtractedBody::default(),
        }
    }
}

/// A leaf part that is a file rather than a display body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentMeta {
    pub part_id: Option<String>,
    /// The handle for `users.messages.attachments.get`. Absent when the bytes
    /// came inline in `data`.
    pub attachment_id: Option<String>,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    /// `Content-ID` with the angle brackets stripped, for resolving `cid:` URLs
    /// in the HTML body.
    pub content_id: Option<String>,
    /// This part belongs in the rendered body rather than in the attachment
    /// row. See [`walk`] for how the two are told apart, and why a Content-ID
    /// alone is not enough to decide it.
    pub inline: bool,
    /// Decoded bytes, when Gmail inlined them instead of handing out an
    /// attachment id.
    pub data: Option<Vec<u8>>,
}

/// The flattened result of walking a message's MIME tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedBody {
    pub text: Option<String>,
    /// The `text/plain` part declared `format=flowed` (RFC 3676), so its soft
    /// line breaks are the generator's and can be rejoined. Only ever set from
    /// the part's own `Content-Type`; there is no inference from the body, and
    /// a part that says nothing leaves this `false`.
    pub text_flowed: bool,
    /// `delsp=yes` on the same part.
    pub text_delsp: bool,
    pub html: Option<String>,
    pub attachments: Vec<AttachmentMeta>,
}

impl ExtractedBody {
    /// Attachments a user would think of as attachments (excludes inline
    /// images referenced by the HTML).
    pub fn files(&self) -> impl Iterator<Item = &AttachmentMeta> {
        self.attachments.iter().filter(|a| !a.inline)
    }

    pub fn inline_parts(&self) -> impl Iterator<Item = &AttachmentMeta> {
        self.attachments.iter().filter(|a| a.inline)
    }
}

/// Walk a part tree depth-first.
///
/// Rules, in the order they matter:
/// * `multipart/*` nodes are containers — recurse, never emit.
/// * A `text/plain` or `text/html` leaf with no filename and no
///   `Content-Disposition: attachment` is a display body. First one wins, which
///   is what makes `multipart/alternative` (plain then html) come out right.
/// * Everything else with bytes behind it is an attachment. `message/rfc822`
///   is treated as an opaque attachment rather than descended into, so a
///   forwarded mail's body never masquerades as this message's body.
pub fn extract_body(root: &MessagePart) -> ExtractedBody {
    let mut out = ExtractedBody::default();
    walk(root, &mut out);
    out
}

fn walk(part: &MessagePart, out: &mut ExtractedBody) {
    if part.is_multipart() {
        for child in &part.parts {
            walk(child, out);
        }
        return;
    }

    let mime = part.mime().split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let filename = part.filename.clone().unwrap_or_default();
    let disposition = part.disposition().unwrap_or_default();
    let is_attached = disposition.starts_with("attachment") || !filename.is_empty();

    if !is_attached && (mime == "text/plain" || mime == "text/html") {
        let data = part.body.as_ref().and_then(|b| b.data.as_deref());
        if let Some(encoded) = data {
            let decoded = decode_base64url(encoded)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            let is_html = mime == "text/html";
            let slot = if is_html { &mut out.html } else { &mut out.text };
            if slot.is_none() && !decoded.is_empty() {
                *slot = Some(decoded);
                if !is_html {
                    /*
                     * The Content-Type parameters, which live on the *part* and
                     * are dropped everywhere else in this walk.
                     *
                     * `mimeType` is Gmail's normalized type and carries no
                     * parameters, so the raw header is the only place
                     * `format=flowed` appears. It is read from the part that
                     * actually won the body slot, not from the message, because
                     * a multipart/alternative can put a flowed plain part next
                     * to an HTML one and only the plain part's declaration
                     * means anything.
                     */
                    let content_type = part.header("content-type").unwrap_or_else(|| part.mime());
                    let (flowed, delsp) = crate::render::flowed_params(content_type);
                    out.text_flowed = flowed;
                    out.text_delsp = delsp;
                }
            }
        }
        return;
    }

    let body = part.body.clone().unwrap_or_default();
    let has_bytes = body.attachment_id.is_some() || body.data.is_some();
    if !has_bytes && filename.is_empty() {
        // A container we do not understand and that carries nothing. Recurse
        // in case Gmail nested something under a non-multipart mime type.
        for child in &part.parts {
            walk(child, out);
        }
        return;
    }

    let content_id = part.content_id();
    out.attachments.push(AttachmentMeta {
        part_id: part.part_id.clone().filter(|s| !s.is_empty()),
        attachment_id: body.attachment_id.clone(),
        filename,
        mime_type: mime,
        size: body.size,
        inline: is_inline(&disposition, content_id.is_some()),
        content_id,
        data: body.data.as_deref().and_then(|d| decode_base64url(d).ok()),
    });
}

/// Is this part part of the body, or a file hanging off the message?
///
/// The distinction is not cosmetic: only a non-inline part becomes a row in
/// `attachments`, which is the only thing the reading pane offers to open or
/// download. A part that lands on the wrong side of this is not merely styled
/// differently — it is invisible and unreachable.
///
/// The rule this replaced was `disposition is inline OR there is a Content-ID`,
/// and the second half of it was wrong. **Gmail's own web composer stamps a
/// `Content-ID: <f_…>` on every file it attaches**, alongside
/// `Content-Disposition: attachment`. So a PDF sent from Gmail arrived carrying
/// both, was classified as part of the body, was written to no `attachments`
/// row, and — since a Gmail-composed message with an empty body has no `cid:`
/// reference to it either — appeared nowhere at all. The owner's report was
/// exactly that: "this email has an attachment but I can't see it at all or
/// download it".
///
/// So an explicit `Content-Disposition: attachment` wins. RFC 2183 makes it a
/// statement of intent by the sender, and it is a stronger signal than a
/// Content-ID, which is only an *address* — a name the body may or may not use.
/// A Content-ID still decides the case where the sender said nothing, which is
/// the mailer that inlines an image without a disposition header.
fn is_inline(disposition: &str, has_content_id: bool) -> bool {
    if disposition.starts_with("attachment") {
        return false;
    }
    disposition.starts_with("inline") || has_content_id
}

// ---------------------------------------------------------- Gmail responses

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagesListResponse {
    #[serde(default)]
    pub messages: Vec<MessageRef>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub result_size_estimate: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadsListResponse {
    #[serde(default)]
    pub threads: Vec<MessageRef>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub result_size_estimate: Option<i64>,
}

/// `users.drafts.list`.
///
/// Each entry is a `{ id, message: { id, threadId } }` triple and nothing else —
/// no headers, no bodies, however the request is formatted. That is the whole
/// reason this endpoint is worth calling on every pass: it is the only place the
/// draft id can be learned, and it costs one small request to learn all of them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftsListResponse {
    #[serde(default)]
    pub drafts: Vec<Draft>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub result_size_estimate: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryMessage {
    #[serde(default)]
    pub message: Message,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryLabelChange {
    #[serde(default)]
    pub message: Message,
    #[serde(default)]
    pub label_ids: Vec<String>,
}

/// One entry in `users.history.list`. Each kind of change is its own list, and
/// a single record can carry several kinds at once.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub messages: Vec<MessageRef>,
    #[serde(default)]
    pub messages_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub messages_deleted: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_added: Vec<HistoryLabelChange>,
    #[serde(default)]
    pub labels_removed: Vec<HistoryLabelChange>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryListResponse {
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// The watermark to store once this sweep has been applied.
    #[serde(default)]
    pub history_id: Option<String>,
}

/// Every page of a history sweep, plus the watermark to persist afterwards.
#[derive(Debug, Clone, Default)]
pub struct HistorySweep {
    pub records: Vec<HistoryRecord>,
    pub history_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelColor {
    #[serde(default)]
    pub text_color: Option<String>,
    #[serde(default)]
    pub background_color: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `system` or `user`. Named around the Rust keyword.
    #[serde(default, rename = "type")]
    pub label_type: Option<String>,
    #[serde(default)]
    pub message_list_visibility: Option<String>,
    #[serde(default)]
    pub label_list_visibility: Option<String>,
    #[serde(default)]
    pub color: Option<LabelColor>,
    #[serde(default)]
    pub messages_total: Option<i64>,
    #[serde(default)]
    pub messages_unread: Option<i64>,
    #[serde(default)]
    pub threads_total: Option<i64>,
    #[serde(default)]
    pub threads_unread: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelsListResponse {
    #[serde(default)]
    pub labels: Vec<Label>,
}

// ------------------------------------------------------------------ filters

/// `users.settings.filters` — one standing rule, server-side.
///
/// A filter is two halves and nothing else: what it matches, and what happens
/// to what it matched. Gmail evaluates it as mail *arrives*; it has no bearing
/// on anything already in the mailbox, which is why Gmail's own web UI offers
/// "also apply to matching conversations" as a separate checkbox rather than as
/// part of the filter. See `commands::filters` for what Mach does about that.
///
/// `id` is assigned by Google on create and is the only handle a delete has.
/// It is skipped when empty so the same struct serializes as a create body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default)]
    pub criteria: FilterCriteria,
    #[serde(default)]
    pub action: FilterAction,
}

impl Filter {
    /// True when nothing at all is being matched on.
    ///
    /// Gmail's API accepts this and it means *every message, forever*, which is
    /// never what anybody typed on purpose. Callers refuse it.
    pub fn matches_everything(&self) -> bool {
        self.criteria == FilterCriteria::default()
    }

    /// True when the rule would do nothing to what it matched.
    pub fn does_nothing(&self) -> bool {
        self.action == FilterAction::default()
    }
}

/// What a filter matches. Every field is optional; the ones that are set are
/// combined with AND, which is Gmail's rule, not ours.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterCriteria {
    /// A sender. Gmail matches this as a substring of the From header, so
    /// `stripe.com` catches every address at the domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// A Gmail search expression, the same language the search box takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// A Gmail search expression that must *not* match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negated_query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_chats: Option<bool>,
}

/// What happens to a message the criteria matched.
///
/// Gmail expresses all of it as label movement, including the two things that
/// do not sound like labels: removing `INBOX` is "skip the inbox", and adding
/// `TRASH` is "delete it". `SPAM`, `UNREAD`, `STARRED` and `IMPORTANT` are the
/// same trick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterAction {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_label_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_label_ids: Vec<String>,
    /// An address Gmail forwards the message to. Only an address the account
    /// has already verified in Gmail's own settings is accepted — registering a
    /// new one needs `gmail.settings.sharing`, which Mach does not request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward: Option<String>,
}

/// `users.settings.filters.list`. The field really is singular.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FiltersListResponse {
    #[serde(default)]
    pub filter: Vec<Filter>,
}

/// `users.getProfile` — the account's own address and its current watermark.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    pub email_address: String,
    #[serde(default)]
    pub messages_total: Option<i64>,
    #[serde(default)]
    pub threads_total: Option<i64>,
    #[serde(default)]
    pub history_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentBody {
    #[serde(default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub data: Option<String>,
}

// ================================================================ Calendar

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarListEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub summary_override: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub color_id: Option<String>,
    #[serde(default)]
    pub background_color: Option<String>,
    #[serde(default)]
    pub foreground_color: Option<String>,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub access_role: Option<String>,
}

impl CalendarListEntry {
    /// What the sidebar should show.
    pub fn display_name(&self) -> &str {
        self.summary_override
            .as_deref()
            .or(self.summary.as_deref())
            .unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarListResponse {
    #[serde(default)]
    pub items: Vec<CalendarListEntry>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub next_sync_token: Option<String>,
}

/// Calendar's tagged union of "all-day" (`date`) and "timed" (`dateTime`).
///
/// Keeping both fields on one struct mirrors the wire format exactly; the
/// helpers below are how callers should read it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDateTime {
    /// `YYYY-MM-DD` — set only for all-day events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// RFC3339 with an offset — set only for timed events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_time: Option<String>,
    /// IANA zone name. Google sends this alongside `dateTime`; it is *not*
    /// redundant with the offset (it survives DST changes when patching).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

impl EventDateTime {
    pub fn date_time(value: impl Into<String>, time_zone: Option<&str>) -> Self {
        Self {
            date: None,
            date_time: Some(value.into()),
            time_zone: time_zone.map(str::to_string),
        }
    }

    pub fn all_day(date: impl Into<String>) -> Self {
        Self {
            date: Some(date.into()),
            date_time: None,
            time_zone: None,
        }
    }

    pub fn is_all_day(&self) -> bool {
        self.date_time.is_none() && self.date.is_some()
    }

    /// The instant, with the sender's UTC offset preserved rather than
    /// normalised away.
    pub fn as_datetime(&self) -> Option<DateTime<FixedOffset>> {
        DateTime::parse_from_rfc3339(self.date_time.as_deref()?).ok()
    }

    /// The calendar day, for all-day events.
    pub fn as_date(&self) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(self.date.as_deref()?, "%Y-%m-%d").ok()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPerson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, rename = "self", skip_serializing_if = "is_false")]
    pub is_self: bool,
}

/// Who Google emails about a write to an event.
///
/// **This is the parameter that decides whether an invitation is ever sent.**
/// Google's calendar API does not notify anybody by default: an `events.insert`
/// carrying three attendees and no `sendUpdates` puts the event on the
/// organizer's calendar, records the three names on it, and tells none of them.
/// The event exists, the guest list is right, and the guests do not know — which
/// is worse than no event at all, because the organizer believes it is on their
/// calendar too.
///
/// So every write in this file takes one of these explicitly. There is no
/// "leave it out and let Google decide", because what Google decides is silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendUpdates {
    /// Tell everyone on the invitation.
    All,
    /// Tell only guests outside this Google Workspace domain — Google's
    /// concession to organisations whose people all see the calendar anyway.
    ExternalOnly,
    /// Tell nobody. A silent write, which is a legitimate thing to want (a typo
    /// in the description of a thirty-person meeting) and never a default.
    None,
}

impl SendUpdates {
    pub fn as_str(self) -> &'static str {
        match self {
            SendUpdates::All => "all",
            SendUpdates::ExternalOnly => "externalOnly",
            SendUpdates::None => "none",
        }
    }
}

impl std::fmt::Display for SendUpdates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `conferenceData` block that asks Google to mint a new Meet link.
///
/// Google will not create a conference from a URL you hand it — the only way to
/// get a Meet link on an event is to ask for one with a `createRequest` and read
/// the answer back off the response. `request_id` is the idempotency key: repeat
/// the same one and Google returns the conference it already made rather than a
/// second one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceCreateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_solution_key: Option<ConferenceSolutionKey>,
}

/// The four values Google accepts for an attendee's `responseStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStatus {
    NeedsAction,
    Declined,
    Tentative,
    Accepted,
}

impl ResponseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseStatus::NeedsAction => "needsAction",
            ResponseStatus::Declined => "declined",
            ResponseStatus::Tentative => "tentative",
            ResponseStatus::Accepted => "accepted",
        }
    }
}

impl std::fmt::Display for ResponseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttendee {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub organizer: bool,
    /// Google's `self` flag — this is the signed-in account's own row, which is
    /// the one an RSVP has to touch.
    #[serde(default, rename = "self", skip_serializing_if = "is_false")]
    pub is_self: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub resource: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_guests: Option<i64>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One way into a conference: a video URL, a dial-in number, a SIP address, or
/// the "more phone numbers" page.
///
/// Every field is optional because Google fills in a different subset per
/// `entryPointType`, and because the shape is versioned by addition — a
/// `passcode` appeared on Meet entry points years after `pin` did. Deserializing
/// permissively is what stops a new field on a video entry point from costing us
/// the whole conference block.
///
/// `uri` is the only field that is load-bearing, and it is attacker-controlled:
/// anyone who can send an invitation can put a string here. Nothing in this file
/// validates it — this is the wire, verbatim — and the two places that act on it
/// (the join affordance in the modal, `ipc::render::open_external` behind it)
/// each check the scheme themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceEntryPoint {
    /// `video`, `phone`, `sip` or `more`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_point_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The human form of `uri` — `meet.google.com/abc-defg-hij`, or a phone
    /// number written the way its country writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The dial-in PIN. Useless without the number, and the number is useless
    /// without it, which is why they travel together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passcode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// ISO 3166-1 alpha-2 for a phone entry point — which country's number this
    /// is, and the only thing that distinguishes six identical-looking rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_code: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceSolutionKey {
    /// `hangoutsMeet`, `addOn`, and the two deprecated Hangouts values.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub solution_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceSolution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<ConferenceSolutionKey>,
    /// "Google Meet", or whatever a third-party add-on calls itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_uri: Option<String>,
}

/// The conference attached to an event.
///
/// This was an `Option<serde_json::Value>` — parsed off the wire, held as an
/// opaque blob, and dropped on the floor before it reached the store. Naming it
/// is what lets the sync layer keep the join link, the dial-in number and its
/// PIN, which between them are the difference between a calendar entry and a
/// meeting you can attend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceData {
    /// The meeting code — `abc-defg-hij` for Meet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_solution: Option<ConferenceSolution>,
    /// Only ever *sent*, never received: this is how a client asks for a Meet
    /// link. See [`ConferenceCreateRequest`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_request: Option<ConferenceCreateRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_points: Vec<ConferenceEntryPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Google's own round-trip token. Meaningless to us, and preserved for
    /// exactly that reason: a patch that dropped it would be a patch that
    /// re-created the conference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// A Drive file attached to an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAttachment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_link: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventReminderOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventReminders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<EventReminderOverride>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<EventPerson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<EventPerson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<EventDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<EventDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time_unspecified: Option<bool>,
    /// RRULE/EXDATE lines. Empty on the concrete instances that
    /// `singleEvents=true` expands a series into.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recurrence: Vec<String>,
    /// Set on an expanded instance, pointing at the series it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_start_time: Option<EventDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(default, rename = "iCalUID", skip_serializing_if = "Option::is_none")]
    pub ical_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<EventAttendee>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees_omitted: Option<bool>,
    /// The pre-`conferenceData` Meet URL. Deprecated for a decade and still
    /// populated on every Meet event Google sends, so it is the fallback when
    /// `conferenceData` is absent — which it is on events created by clients
    /// that only ever knew the old field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hangout_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_data: Option<ConferenceData>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<EventAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<EventReminders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_modify: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_invite_others: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_see_other_guests: Option<bool>,
}

impl Event {
    /// True for a concrete occurrence expanded out of a recurring series.
    pub fn is_instance(&self) -> bool {
        self.recurring_event_id.is_some()
    }

    /// True for a series master (only ever returned with `singleEvents=false`).
    pub fn is_recurring_master(&self) -> bool {
        !self.recurrence.is_empty()
    }

    /// Deleted, or an instance cancelled out of a series. With
    /// `showDeleted=true` these arrive as skeletons carrying only ids.
    pub fn is_cancelled(&self) -> bool {
        self.status.as_deref() == Some("cancelled")
    }

    /// The signed-in account's own attendee row, if this account is invited.
    pub fn self_attendee(&self) -> Option<&EventAttendee> {
        self.attendees.iter().find(|a| a.is_self)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsListResponse {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub access_role: Option<String>,
    #[serde(default)]
    pub default_reminders: Vec<EventReminderOverride>,
    #[serde(default)]
    pub items: Vec<Event>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// Present only on the final page. Store it; pass it back next sync.
    #[serde(default)]
    pub next_sync_token: Option<String>,
}

/// Every page of an `events.list` sweep, plus the token to persist afterwards.
#[derive(Debug, Clone, Default)]
pub struct EventsSweep {
    pub events: Vec<Event>,
    pub next_sync_token: Option<String>,
    /// The calendar's default timezone, from the last page seen.
    pub time_zone: Option<String>,
}
