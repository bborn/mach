//! Recognising a calendar invitation inside a message.
//!
//! A Google invitation is an ordinary email that happens to carry an iCalendar
//! document — a `text/calendar` part inside the `multipart/alternative`, and
//! usually an `invite.ics` beside it. That document is the only thing in the
//! message that says, unambiguously, *which meeting this is*: its `UID` is the
//! same string Google puts on the event's `iCalUID`, which is what
//! `events.ical_uid` holds for 1,323 of the owner's rows. Everything else in
//! the message — the subject, the sender, the "Yes / No / Maybe" links — is
//! either ambiguous or a URL back to google.com.
//!
//! So this module answers one question: *is this message an invitation, and to
//! what?* It does not decide anything about the UI, and it never talks to
//! Google. It is called once per message at sync time, and what it finds is
//! stored on the message row (`invite_uid`, `invite_method`) so that opening a
//! conversation is still a local read.
//!
//! # `METHOD` is the whole of the recognition
//!
//! iCalendar's `METHOD` says what the document *is for* (RFC 5546), and Google
//! sets it on every calendar part it sends:
//!
//! | `METHOD` | what it is | what we do |
//! |---|---|---|
//! | `REQUEST` | an organiser inviting you | offer the RSVP |
//! | `REPLY` | someone answering *your* invitation | nothing |
//! | `CANCEL` | the meeting is off | nothing |
//! | `COUNTER`, `REFRESH`, `PUBLISH`, … | everything else | nothing |
//!
//! Only `REQUEST` produces an [`Invitation`]. A reply landing in the inbox
//! ("Sam has accepted") carries the same `UID` as the meeting, and treating it
//! as an invitation would put three answer buttons on a message that is not
//! asking a question. A cancellation is worse: it would offer to accept a
//! meeting that no longer exists.
//!
//! # What stops a forward from looking like an invitation
//!
//! Three things, and none of them is a heuristic about the subject line.
//!
//!  * A message that merely *quotes* an invitation has no calendar part at all,
//!    so it never reaches the parser.
//!  * A mail forwarded as `message/rfc822` is not descended into —
//!    `google::types::walk` treats it as an opaque attachment — so the nested
//!    invitation's parts are not this message's parts.
//!  * A forward that re-attaches `invite.ics` as a file gets an attachment id
//!    from Gmail rather than inline bytes, and this module only reads bytes
//!    that arrived with the message. Fetching them would be a network call on
//!    a code path that must not make one.
//!
//! The last guard is the one that matters, and it is not in this file: an
//! invitation only produces a *control* when its `UID` matches an event on the
//! same account's calendar (`db::queries::invitation_event`). Google puts an
//! event there when you are invited to it and not otherwise, so "is this an
//! invitation to me" is answered by the calendar rather than by parsing
//! `ATTENDEE` lines and guessing which address is the owner's.

/// The identity of the meeting a message is inviting someone to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// The iCalendar `UID`, verbatim. Joins to `events.ical_uid`.
    pub uid: String,
    /// The `METHOD`, uppercased. Only `REQUEST` gets here today; the field is
    /// stored anyway so a future cancellation notice has something to read.
    pub method: String,
}

/// What was in a calendar part, whatever it was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub method: Option<String>,
    pub uid: Option<String>,
}

pub const METHOD_REQUEST: &str = "REQUEST";

/// Largest calendar document this will look at.
///
/// A Google invitation is one to three kilobytes. Anything past this is either
/// a mailing-list digest of a year of meetings or an attempt to make the sync
/// pass allocate, and neither is an invitation. Truncating rather than refusing
/// would be worse than both: a half-read document can yield a `UID` from one
/// event and a `METHOD` from another.
pub const MAX_ICS_BYTES: usize = 256 * 1024;

/// The invitation in an iCalendar document, if that is what it is.
///
/// `None` for every document that is not an organiser asking a question: a
/// reply, a cancellation, a published calendar, a `VEVENT` with no `UID`, and
/// anything that is not iCalendar at all.
pub fn invitation(ics: &str) -> Option<Invitation> {
    let calendar = parse(ics)?;
    let method = calendar.method?;
    if method != METHOD_REQUEST {
        return None;
    }
    Some(Invitation {
        uid: calendar.uid?,
        method,
    })
}

/// Read `METHOD` and the first `VEVENT`'s `UID` out of an iCalendar document.
///
/// Deliberately not a full parser. Every property this app will ever need from
/// an invitation email is already in the event row the `UID` points at — the
/// title, the time, the guest list and the response are all synced from the
/// Calendar API, where they are *current* rather than as they stood when the
/// mail was sent. So this reads the two fields that identify the document and
/// stops.
///
/// Total, in the sense that matters: every input returns, and a malformed one
/// returns `None` or a partial [`Calendar`] rather than panicking. There is no
/// indexing, no slicing at a byte offset, and no `unwrap`.
pub fn parse(ics: &str) -> Option<Calendar> {
    if ics.len() > MAX_ICS_BYTES {
        return None;
    }
    // A document that does not open a calendar is not one. Cheap, and it is
    // what keeps an HTML part that happens to be typed `text/calendar` out.
    if !ics.to_ascii_uppercase().contains("BEGIN:VCALENDAR") {
        return None;
    }

    let mut method: Option<String> = None;
    let mut uid: Option<String> = None;
    // `UID` is a property of the *event*, and a document can hold several —
    // Google sends one, but a series with an exception sends two. The first
    // one wins, which is the one the mail is about.
    let mut depth_vevent = false;

    for line in unfold(ics) {
        let Some((name, value)) = property(&line) else {
            continue;
        };
        match name.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VEVENT") => depth_vevent = true,
            "END" if value.eq_ignore_ascii_case("VEVENT") => depth_vevent = false,
            // METHOD is a calendar-level property. Reading it anywhere in the
            // document is a small looseness that costs nothing: no generator
            // puts one inside a component, and a stricter reading would drop
            // the field for a document with an unusual order.
            "METHOD" if method.is_none() && !value.is_empty() => {
                method = Some(value.to_ascii_uppercase());
            }
            "UID" if depth_vevent && uid.is_none() && !value.is_empty() => {
                uid = Some(value);
            }
            _ => {}
        }
    }

    Some(Calendar { method, uid })
}

/// True for a part that could be an iCalendar document.
///
/// Both spellings, because a Google invitation uses both: the alternative part
/// is `text/calendar`, and the file beside it arrives as `application/ics`
/// named `invite.ics`. A `.ics` filename alone is enough for the same reason a
/// mail client shows an attachment by its name — the sender's type is often
/// `application/octet-stream` and the name is the only true thing about it.
pub fn is_calendar_part(mime_type: &str, filename: &str) -> bool {
    let mime = mime_type.split(';').next().unwrap_or("").trim();
    mime.eq_ignore_ascii_case("text/calendar")
        || mime.eq_ignore_ascii_case("application/ics")
        || mime.eq_ignore_ascii_case("text/x-vcalendar")
        || filename.to_ascii_lowercase().ends_with(".ics")
}

/// The invitation in a message's parts, if one of them is an invitation.
///
/// `parts` is `(mime type, filename, the bytes that arrived inline)`. A part
/// with no bytes is skipped rather than fetched: sync is the caller, and a
/// request per message is not a trade this app makes.
pub fn from_parts<'a, I>(parts: I) -> Option<Invitation>
where
    I: IntoIterator<Item = (&'a str, &'a str, Option<&'a [u8]>)>,
{
    for (mime_type, filename, data) in parts {
        if !is_calendar_part(mime_type, filename) {
            continue;
        }
        let Some(bytes) = data else { continue };
        if bytes.len() > MAX_ICS_BYTES {
            continue;
        }
        // Lossy on purpose. iCalendar is UTF-8 in practice and Latin-1 in
        // 1998; a stray byte should cost one replacement character in a title
        // nobody reads from here, not the whole invitation.
        let text = String::from_utf8_lossy(bytes);
        if let Some(found) = invitation(&text) {
            return Some(found);
        }
    }
    None
}

/// iCalendar's line folding, undone (RFC 5545 §3.1).
///
/// A continuation line begins with one space or tab, and that first character
/// is *not* part of the value. Google folds `UID` at 75 octets routinely, so
/// without this every uid over that length comes out truncated — and it would
/// truncate silently, matching nothing, on exactly the long uids Google mints.
fn unfold(ics: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match line.strip_prefix([' ', '\t']) {
            Some(rest) => match out.last_mut() {
                // A fold with nothing to fold onto is malformed; keep it as its
                // own line rather than dropping it.
                None => out.push(rest.to_string()),
                Some(previous) => previous.push_str(rest),
            },
            None => out.push(line.to_string()),
        }
    }
    out
}

/// `NAME;PARAM=VALUE:the value` → `("NAME", "the value")`.
///
/// Parameters are skipped rather than parsed, and a quoted parameter value may
/// itself contain a colon (`CN="Rivera, Alex: PM"`), so the split is on the
/// first colon *outside* quotes. Getting that wrong reads the tail of a
/// parameter as the property value, which for `ORGANIZER;CN="…"` looks like a
/// perfectly plausible answer.
fn property(line: &str) -> Option<(String, String)> {
    let mut quoted = false;
    let mut name_end: Option<usize> = None;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ';' if !quoted && name_end.is_none() => name_end = Some(index),
            ':' if !quoted => {
                let name = line.get(..name_end.unwrap_or(index))?.trim();
                let value = line.get(index + 1..)?.trim();
                if name.is_empty() {
                    return None;
                }
                return Some((name.to_ascii_uppercase(), value.to_string()));
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape Google actually sends, down to the folded uid.
    fn google_request() -> String {
        "BEGIN:VCALENDAR\r\n\
         PRODID:-//Google Inc//Google Calendar 70.9054//EN\r\n\
         VERSION:2.0\r\n\
         CALSCALE:GREGORIAN\r\n\
         METHOD:REQUEST\r\n\
         BEGIN:VEVENT\r\n\
         DTSTART:20260812T150000Z\r\n\
         DTEND:20260812T160000Z\r\n\
         DTSTAMP:20260807T120000Z\r\n\
         ORGANIZER;CN=\"Rivera, Alex: PM\":mailto:alex@example.com\r\n\
         UID:6r2h1c9k6ss30b9k6ss30b9k6s_20260812T150000Z@goo\r\n \
         gle.com\r\n\
         ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;\r\n \
         RSVP=TRUE;CN=bruno@example.com;X-NUM-GUESTS=0:mailto:bruno@example.com\r\n\
         SUMMARY:Quarterly review\r\n\
         END:VEVENT\r\n\
         END:VCALENDAR\r\n"
            .to_string()
    }

    #[test]
    fn reads_a_google_request() {
        let found = invitation(&google_request()).expect("an invitation");
        assert_eq!(
            found.uid,
            "6r2h1c9k6ss30b9k6ss30b9k6s_20260812T150000Z@google.com",
            "the folded uid has to be rejoined without the fold's leading space"
        );
        assert_eq!(found.method, METHOD_REQUEST);
    }

    #[test]
    fn a_cancellation_is_not_an_invitation() {
        let ics = google_request().replace("METHOD:REQUEST", "METHOD:CANCEL");
        assert_eq!(invitation(&ics), None);
        // The method was still read: the refusal is a decision, not a failure
        // to parse.
        assert_eq!(parse(&ics).and_then(|c| c.method), Some("CANCEL".into()));
    }

    #[test]
    fn a_reply_is_not_an_invitation() {
        let ics = google_request().replace("METHOD:REQUEST", "METHOD:REPLY");
        assert_eq!(invitation(&ics), None);
        assert!(parse(&ics).and_then(|c| c.uid).is_some());
    }

    #[test]
    fn a_published_calendar_is_not_an_invitation() {
        let ics = google_request().replace("METHOD:REQUEST", "METHOD:PUBLISH");
        assert_eq!(invitation(&ics), None);
    }

    #[test]
    fn a_document_with_no_method_is_not_an_invitation() {
        let ics = google_request().replace("METHOD:REQUEST\r\n", "");
        assert_eq!(invitation(&ics), None);
    }

    #[test]
    fn a_request_with_no_uid_is_not_an_invitation() {
        let ics = google_request()
            .replace("UID:6r2h1c9k6ss30b9k6ss30b9k6s_20260812T150000Z@goo\r\n gle.com\r\n", "");
        assert_eq!(invitation(&ics), None);
    }

    #[test]
    fn a_uid_outside_an_event_is_not_the_events_uid() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nUID:not-an-event@example.com\r\n\
                   END:VCALENDAR\r\n";
        assert_eq!(invitation(ics), None);
    }

    #[test]
    fn malformed_input_does_not_panic() {
        for ics in [
            "",
            "\r\n\r\n",
            ":",
            ":::::",
            "BEGIN:VCALENDAR",
            "BEGIN:VCALENDAR\r\nMETHOD",
            "BEGIN:VCALENDAR\r\nMETHOD:\r\nBEGIN:VEVENT\r\nUID:\r\n",
            "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\n UID:folded-with-no-parent\n",
            "BEGIN:VCALENDAR\r\n;:;:;\r\nBEGIN:VEVENT\r\nUID;;;:x\r\nEND:VEVENT\r\n",
            "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:\u{1F4C5}\u{FFFD}\r\n",
            "not a calendar at all",
            "BEGIN:VCALENDAR\r\nSUMMARY:caf\u{e9} \u{2014} \u{4f1a}\u{8b70}\r\n",
        ] {
            // The claim is that it returns at all, and that it never claims an
            // invitation it did not read a uid for.
            if let Some(found) = invitation(ics) {
                assert!(!found.uid.is_empty());
            }
            let _ = parse(ics);
        }
    }

    #[test]
    fn a_document_past_the_cap_is_refused() {
        let mut ics = google_request();
        ics.push_str(&"X-PADDING:".repeat(MAX_ICS_BYTES / 4));
        assert!(ics.len() > MAX_ICS_BYTES);
        assert_eq!(parse(&ics), None);
        assert_eq!(invitation(&ics), None);
    }

    #[test]
    fn lf_only_line_endings_parse() {
        let ics = google_request().replace("\r\n", "\n");
        assert!(invitation(&ics).is_some());
    }

    #[test]
    fn a_quoted_colon_in_a_parameter_is_not_the_value_separator() {
        let line = "ORGANIZER;CN=\"Rivera, Alex: PM\":mailto:alex@example.com";
        assert_eq!(
            property(line),
            Some(("ORGANIZER".into(), "mailto:alex@example.com".into()))
        );
    }

    #[test]
    fn calendar_parts_are_recognised_by_type_or_by_name() {
        assert!(is_calendar_part("text/calendar; method=REQUEST", ""));
        assert!(is_calendar_part("TEXT/CALENDAR", ""));
        assert!(is_calendar_part("application/ics", "invite.ics"));
        assert!(is_calendar_part("application/octet-stream", "invite.ICS"));
        assert!(!is_calendar_part("text/html", ""));
        assert!(!is_calendar_part("application/pdf", "deck.pdf"));
        // "calendar.pdf" is not a calendar part, and neither is a name that
        // merely contains the extension.
        assert!(!is_calendar_part("application/pdf", "invite.ics.pdf"));
    }

    #[test]
    fn the_first_calendar_part_with_bytes_wins() {
        let ics = google_request();
        let found = from_parts([
            ("text/html", "", Some(b"<p>not this one</p>".as_slice())),
            // No bytes: Gmail handed out an attachment id instead, and this
            // module does not make requests.
            ("application/ics", "invite.ics", None),
            ("text/calendar", "", Some(ics.as_bytes())),
        ])
        .expect("the part that arrived inline");
        assert!(found.uid.ends_with("@google.com"));
    }

    #[test]
    fn a_message_with_no_calendar_part_has_no_invitation() {
        assert_eq!(
            from_parts([
                ("text/plain", "", Some(b"lunch?".as_slice())),
                ("application/pdf", "deck.pdf", Some(b"%PDF".as_slice())),
            ]),
            None
        );
    }
}
