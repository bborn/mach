//! Google wire types → local store rows.
//!
//! Pure functions, no I/O and no clock, so every mapping decision here is
//! directly testable. The sync engine calls these; nothing else should need to.

use crate::db::models::{
    ConferenceEntry, EventAttachment, EventConference, EventGuest, EventReminder, EventReminders,
    NewAttachment, NewEvent, NewLabel, NewMessage, Participant, RsvpStatus,
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
            body_text_flowed: body.text_flowed,
            body_text_delsp: body.text_delsp,
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

    // Guests first; the address list is a projection of them. Built in that
    // order so the two columns cannot disagree about who is invited — they are
    // one list read twice, not two lists kept in step.
    let guests: Vec<EventGuest> = event
        .attendees
        .iter()
        .filter_map(|a| {
            let email = a.email.clone().filter(|e| !e.is_empty())?;
            Some(EventGuest {
                email,
                name: a.display_name.clone().filter(|n| !n.is_empty()),
                response: a.response_status.as_deref().and_then(RsvpStatus::parse),
                optional: a.optional,
                organizer: a.organizer,
                is_self: a.is_self,
                resource: a.resource,
                comment: a.comment.clone().filter(|c| !c.trim().is_empty()),
            })
        })
        .collect();
    let attendees = guests.iter().map(EventGuest::participant).collect();

    let rsvp_status = event
        .self_attendee()
        .and_then(|a| a.response_status.as_deref())
        .and_then(RsvpStatus::parse);

    Some(NewEvent {
        recurrence: event.recurrence.clone(),
        reminders: event.reminders.as_ref().map(reminders_of),
        ical_uid: event.ical_uid.clone().filter(|s| !s.is_empty()),
        guests,
        conference: conference_of(event),
        creator: event.creator.as_ref().and_then(person_of),
        attachments: event
            .attachments
            .iter()
            .filter_map(|a| {
                let url = a.file_url.clone().filter(|u| !u.is_empty())?;
                Some(EventAttachment {
                    // A file with no title still has to be nameable; the URL is
                    // the only other thing we have, and it is at least unique.
                    title: a.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| url.clone()),
                    url,
                    mime_type: a.mime_type.clone().filter(|m| !m.is_empty()),
                })
            })
            .collect(),
        visibility: event.visibility.clone().filter(|v| !v.is_empty()),
        transparency: event.transparency.clone().filter(|t| !t.is_empty()),
        organizer: event.organizer.as_ref().and_then(person_of),
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

/// A Google person block as a [`Participant`], or `None` when it names nobody.
fn person_of(person: &g::EventPerson) -> Option<Participant> {
    person
        .email
        .clone()
        .filter(|e| !e.is_empty())
        .map(|email| Participant {
            name: person.display_name.clone().filter(|n| !n.is_empty()),
            email,
        })
}

/// The conference on an event, from either of the two places Google keeps one.
///
/// `conferenceData` is the modern field and carries everything — the meeting
/// code, the video link, the dial-ins with their PINs, the "more phone numbers"
/// page. `hangoutLink` is the field it replaced, it has been deprecated since
/// Hangouts became Meet, and it is still populated on essentially every Meet
/// event Google sends. Reading only the first would lose the call on events
/// created by anything old enough to write only the second, and reading only the
/// second would lose the dial-in on everything else.
///
/// So both are read, `conferenceData` wins, and `hangoutLink` is folded in as
/// the video entry point when the modern block somehow has none. That is also
/// why `hangout_link` gets no column of its own: two spellings of one URL are
/// not two facts, and a UI that had to choose between them would eventually
/// choose wrong.
///
/// An entry point with no `uri` is dropped. There is nothing to show and nothing
/// to dial, and a row that renders as an empty line is worse than no row.
///
/// `pub(crate)` because the write path needs it too: `events.insert` answers
/// with the conference Google just minted, and reading it straight off the
/// response is what puts a Join button on a new meeting now rather than at the
/// next sync.
pub(crate) fn conference_of(event: &g::Event) -> Option<EventConference> {
    let data = event.conference_data.as_ref();

    let mut entry_points: Vec<ConferenceEntry> = data
        .map(|d| d.entry_points.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            let uri = entry.uri.clone().filter(|u| !u.is_empty())?;
            Some(ConferenceEntry {
                // Google has always sent `entryPointType`, but an entry point
                // with a URI and no type is still a way in; `video` is the only
                // guess that makes it reachable rather than merely visible.
                kind: entry
                    .entry_point_type
                    .clone()
                    .filter(|k| !k.is_empty())
                    .unwrap_or_else(|| "video".to_string()),
                label: entry.label.clone().filter(|l| !l.is_empty()),
                pin: entry
                    .pin
                    .clone()
                    .or_else(|| entry.access_code.clone())
                    .or_else(|| entry.passcode.clone())
                    .filter(|p| !p.is_empty()),
                region_code: entry.region_code.clone().filter(|r| !r.is_empty()),
                uri,
            })
        })
        .collect();

    if let Some(link) = event.hangout_link.as_deref().filter(|l| !l.is_empty()) {
        if !entry_points.iter().any(|e| e.kind == "video") {
            entry_points.push(ConferenceEntry {
                kind: "video".to_string(),
                label: link.strip_prefix("https://").map(str::to_string),
                uri: link.to_string(),
                pin: None,
                region_code: None,
            });
        }
    }

    if entry_points.is_empty() {
        return None;
    }

    Some(EventConference {
        id: data.and_then(|d| d.conference_id.clone()).filter(|i| !i.is_empty()),
        name: data
            .and_then(|d| d.conference_solution.as_ref())
            .and_then(|s| s.name.clone())
            .filter(|n| !n.is_empty())
            // Named from the link rather than left blank: "Join" with no noun
            // after it is a button that does not say where it goes.
            .or_else(|| {
                entry_points
                    .iter()
                    .any(|e| e.uri.contains("meet.google.com"))
                    .then(|| "Google Meet".to_string())
            }),
        notes: data.and_then(|d| d.notes.clone()).filter(|n| !n.trim().is_empty()),
        entry_points,
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
