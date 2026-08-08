//! Google wire types → local store rows.
//!
//! Pure functions, no I/O and no clock, so every mapping decision here is
//! directly testable. The sync engine calls these; nothing else should need to.

use crate::db::models::{
    EventReminder, EventReminders, NewAttachment, NewEvent, NewLabel, NewMessage, Participant,
    RsvpStatus,
};
use crate::google::types as g;

/// A message ready to be written, with the pieces that need row ids filled in
/// by the caller.
#[derive(Debug, Clone)]
pub struct PreparedMessage {
    pub gmail_thread_id: String,
    /// `thread_id` is left at 0; the caller sets it once the thread row exists.
    pub message: NewMessage,
    /// `message_id` is left at 0 for the same reason.
    pub attachments: Vec<NewAttachment>,
    /// The full Gmail label set for this message.
    pub label_ids: Vec<String>,
}

/// Turn a `users.messages.get` response into the rows it becomes.
///
/// Read state comes from the `UNREAD` label rather than a dedicated field —
/// that is how Gmail models it, and it is why `labelsAdded`/`labelsRemoved` is
/// all the incremental path needs to keep the inbox's unread count honest.
pub fn prepare_message(account_id: i64, msg: &g::Message) -> PreparedMessage {
    let body = msg.extract_body();
    let subject = msg.header("Subject").unwrap_or_default();
    let from = msg
        .header("From")
        .and_then(|raw| parse_address(&raw))
        .unwrap_or_default();

    // Gmail's `snippet` is HTML-encoded even though it is plain text, so an
    // apostrophe arrives as `&#39;` and the inbox row reads "Sure I&#39;m free".
    // Decoded here, at the point the wire shape becomes ours, rather than in
    // each of the places that display it.
    let snippet = crate::render::entities::decode(
        &msg.snippet
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| snippet_from_body(&body)),
    );

    let label_ids = {
        let mut l = msg.label_ids.clone();
        l.sort();
        l.dedup();
        l
    };

    let attachments = body
        .files()
        .map(|a| NewAttachment {
            message_id: 0,
            gmail_attachment_id: a.attachment_id.clone(),
            filename: a.filename.clone(),
            mime_type: a.mime_type.clone(),
            size_bytes: a.size,
            local_path: None,
        })
        .collect();

    PreparedMessage {
        gmail_thread_id: msg.thread_id.clone(),
        message: NewMessage {
            thread_id: 0,
            account_id,
            gmail_message_id: msg.id.clone(),
            rfc822_message_id: msg.header("Message-ID").filter(|s| !s.is_empty()),
            in_reply_to: msg.header("In-Reply-To").filter(|s| !s.is_empty()),
            // A sender who set Reply-To asked for the answer to go elsewhere.
            // Mailing lists depend on it; without this a reply goes to whoever
            // happened to post rather than back to the list.
            reply_to: msg
                .header("Reply-To")
                .filter(|s| !s.is_empty())
                .map(|h| parse_address_list(&h))
                .unwrap_or_default(),
            references: msg.header("References").filter(|s| !s.is_empty()),
            from,
            to: parse_address_list(&msg.header("To").unwrap_or_default()),
            cc: parse_address_list(&msg.header("Cc").unwrap_or_default()),
            bcc: parse_address_list(&msg.header("Bcc").unwrap_or_default()),
            subject,
            body_html: body.html.clone(),
            body_text: body.text.clone(),
            snippet,
            internal_date: msg.internal_date_ms().unwrap_or(0),
            is_unread: label_ids.iter().any(|l| l == "UNREAD"),
            is_draft: label_ids.iter().any(|l| l == "DRAFT"),
        },
        attachments,
        label_ids,
    }
}

/// Gmail normally sends a `snippet`; when it does not (some `format=full`
/// responses on very small messages), fall back to the first line of the body
/// so the list row is not blank.
fn snippet_from_body(body: &g::ExtractedBody) -> String {
    const MAX: usize = 160;
    let source = body.text.as_deref().unwrap_or("");
    let flattened: String = source
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let trimmed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    trimmed.chars().take(MAX).collect()
}

pub fn prepare_label(account_id: i64, label: &g::Label) -> NewLabel {
    NewLabel {
        account_id,
        gmail_label_id: label.id.clone(),
        name: if label.name.is_empty() {
            label.id.clone()
        } else {
            label.name.clone()
        },
        label_type: crate::db::models::LabelType::from_str_lossy(
            label.label_type.as_deref().unwrap_or("user"),
        ),
    }
}

// ---------------------------------------------------------------------------
// address headers
// ---------------------------------------------------------------------------

/// Split an address header on commas that are not inside a quoted display name
/// or an angle-bracketed address, then parse each part.
///
/// `"Rivera, Alex" <b@x.com>, tawny@y.com` is one address plus one
/// address, not three — which is why this cannot be `split(',')`.
pub fn parse_address_list(raw: &str) -> Vec<Participant> {
    split_addresses(raw)
        .iter()
        .filter_map(|part| parse_address(part))
        .collect()
}

fn split_addresses(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut in_angle = false;
    let mut escaped = false;

    for c in raw.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => {
                escaped = true;
                current.push(c);
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            '<' if !in_quotes => {
                in_angle = true;
                current.push(c);
            }
            '>' if !in_quotes => {
                in_angle = false;
                current.push(c);
            }
            ',' if !in_quotes && !in_angle => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// `Name <addr@host>` or a bare `addr@host`. Returns `None` for anything with
/// no address in it at all.
pub fn parse_address(raw: &str) -> Option<Participant> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if let (Some(open), Some(close)) = (raw.rfind('<'), raw.rfind('>')) {
        if open < close {
            let email = raw[open + 1..close].trim().to_string();
            let name = unquote(raw[..open].trim());
            if email.is_empty() {
                return None;
            }
            return Some(Participant {
                name: (!name.is_empty()).then_some(name),
                email,
            });
        }
    }

    let email = unquote(raw);
    if email.is_empty() {
        return None;
    }
    Some(Participant { name: None, email })
}

fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed);
    stripped.replace("\\\"", "\"").trim().to_string()
}

// ---------------------------------------------------------------------------
// calendar
// ---------------------------------------------------------------------------

/// Turn one `events.list` item into an event row.
///
/// Returns `None` when the item carries no id or no usable start — Google sends
/// id-only skeletons for cancelled instances, and those are handled as deletes
/// rather than as rows.
pub fn prepare_event(account_id: i64, calendar_id: &str, event: &g::Event) -> Option<NewEvent> {
    let google_event_id = event.id.clone().filter(|s| !s.is_empty())?;
    let start = event.start.as_ref().and_then(datetime_ms)?;
    // An event with no end is a point in time; Google also allows
    // `endTimeUnspecified`, which we render as zero-length.
    let end = event.end.as_ref().and_then(datetime_ms).unwrap_or(start);

    let is_all_day = event.start.as_ref().map(|s| s.is_all_day()).unwrap_or(false);

    let attendees = event
        .attendees
        .iter()
        .filter_map(|a| {
            a.email.clone().map(|email| Participant {
                name: a.display_name.clone().filter(|n| !n.is_empty()),
                email,
            })
        })
        .collect();

    let rsvp_status = event
        .self_attendee()
        .and_then(|a| a.response_status.as_deref())
        .and_then(RsvpStatus::parse);

    Some(NewEvent {
        recurrence: event.recurrence.clone(),
        reminders: event.reminders.as_ref().map(reminders_of),
        ical_uid: event.ical_uid.clone().filter(|s| !s.is_empty()),
        organizer: event.organizer.as_ref().and_then(|person| {
            person.email.clone().map(|email| Participant {
                name: person.display_name.clone().filter(|n| !n.is_empty()),
                email,
            })
        }),
        // `organizer.self` is Google's own answer to "is this event mine": it is
        // true when the organizer is the calendar this copy appears on. Absent
        // is not false — an event Google described without an organizer block
        // tells us nothing, and the UI must not read silence as a refusal.
        organizer_self: event.organizer.as_ref().map(|person| person.is_self),
        guests_can_modify: event.guests_can_modify,
        account_id,
        calendar_id: calendar_id.to_string(),
        google_event_id,
        title: event.summary.clone().unwrap_or_default(),
        description: event.description.clone(),
        location: event.location.clone(),
        start_ts: start,
        end_ts: end,
        is_all_day,
        attendees,
        rsvp_status,
        recurring_event_id: event.recurring_event_id.clone(),
        status: event
            .status
            .clone()
            .unwrap_or_else(|| "confirmed".to_string()),
        html_link: event.html_link.clone(),
        updated_at: event.updated.as_deref().and_then(rfc3339_ms).unwrap_or(0),
    })
}

/// Google's reminder block, in the store's own shape.
///
/// The method is carried across verbatim rather than normalised to `popup`.
/// Mach only ever *creates* popups, but an alert someone set to email on the
/// web is theirs, and rewriting it on the next sync would be a silent edit to
/// an event the user never opened.
fn reminders_of(reminders: &g::EventReminders) -> EventReminders {
    EventReminders {
        // Google omits `useDefault` on an event that has explicit overrides;
        // the presence of the block at all with no flag means "not the default".
        use_default: reminders.use_default.unwrap_or(false),
        overrides: reminders
            .overrides
            .iter()
            .filter_map(|o| {
                o.minutes.map(|minutes| EventReminder {
                    method: o.method.clone().unwrap_or_else(|| "popup".to_string()),
                    minutes,
                })
            })
            .collect(),
    }
}

/// Milliseconds for either flavour of Calendar's start/end union. All-day dates
/// are pinned to UTC midnight: the store is a grid position, not an instant, and
/// re-anchoring per viewer timezone is the renderer's problem.
pub fn datetime_ms(value: &g::EventDateTime) -> Option<i64> {
    if let Some(dt) = value.as_datetime() {
        return Some(dt.timestamp_millis());
    }
    let date = value.as_date()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}

pub fn rfc3339_ms(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Format milliseconds as the RFC3339 string `timeMin`/`timeMax` want.
pub fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp_millis(0).expect("epoch"))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
