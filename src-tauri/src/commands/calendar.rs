//! The calendar half of the command vocabulary: RSVP, and the write path for
//! events themselves — create, update, delete, move between calendars.
//!
//! Same contract as the mail commands — local write first, remote call second,
//! revert on failure, hand back the inverse — but the shape is different in
//! three ways worth naming up front.
//!
//! # 1. RSVP costs two requests, and that is Google's design
//!
//! There is no "set my response" endpoint. [`CalendarClient::events_rsvp`]
//! therefore reads the event and patches its `attendees` array with one row
//! changed. Patching `attendees` replaces the array wholesale, so the current
//! list has to be read before it can be written back.
//!
//! # 2. A created event has no id until it exists
//!
//! Every other command addresses rows that are already there. A create writes a
//! row under a placeholder Google id *first* — so the grid can draw the block
//! before the network is involved — and adopts the real id when the insert
//! answers. If it does not, the placeholder row is deleted. The inverse is a
//! [`Command::DeleteEvent`] naming the row that was made, which is only knowable
//! after the fact; that is exactly what `CommandResult::undo` is for.
//!
//! # 3. Recurrence: two scopes, honestly
//!
//! Rows are concrete occurrences (`singleEvents=true`), so an edit has to say
//! which occurrences it means. [`EventScope::This`] addresses the instance's own
//! id; [`EventScope::All`] addresses its `recurringEventId`, the series master.
//! Those are the two things Google's API can express in one call, and they are
//! the two offered.
//!
//! **"This and following" is not implemented**, and is refused rather than
//! quietly downgraded. It has no endpoint: it means reading the master's RRULE,
//! rewriting it with an `UNTIL`, and inserting a second series — three calls
//! whose partial failure leaves a split series behind and a local store that
//! disagrees with both halves. **Re-timing a whole series is refused for the
//! same reason**: patching a master's `start` means recomputing the first
//! occurrence's wall time, which is not derivable from the instance the user
//! actually dragged.
//!
//! # Moving between calendars
//!
//! Google's `events.move` only works inside one account, and Mach has several.
//! So a move is always insert-into-destination then delete-from-source, which
//! covers both cases with one code path. The cost is honest and worth stating:
//! the moved event gets a new Google id, and attendees are re-invited.

use serde_json::{json, Map, Value};

use crate::db::command_queries::{self as cq, EventFields};
use crate::db::models::{Event, EventReminder, EventReminders, NewEvent, Participant, RsvpStatus};
use crate::db::queries;
use crate::google::types::{
    EventAttendee, EventDateTime, EventReminderOverride, EventReminders as GoogleReminders,
    Event as GoogleEvent, ResponseStatus,
};
use crate::google::GoogleError;

use super::error::{CommandError, CommandFailure};
use super::types::{Command, CommandResult, EventDraft, EventPatch, EventScope};
use super::CommandDispatcher;

fn to_google(status: RsvpStatus) -> ResponseStatus {
    match status {
        RsvpStatus::NeedsAction => ResponseStatus::NeedsAction,
        RsvpStatus::Declined => ResponseStatus::Declined,
        RsvpStatus::Tentative => ResponseStatus::Tentative,
        RsvpStatus::Accepted => ResponseStatus::Accepted,
    }
}

fn describe(status: RsvpStatus) -> &'static str {
    match status {
        RsvpStatus::Accepted => "Accepted the invitation",
        RsvpStatus::Declined => "Declined the invitation",
        RsvpStatus::Tentative => "Responded maybe",
        RsvpStatus::NeedsAction => "Cleared the response",
    }
}

pub(crate) async fn execute_rsvp(
    dispatcher: &CommandDispatcher,
    event_id: i64,
    response: RsvpStatus,
) -> Result<CommandResult, CommandError> {
    let event = dispatcher
        .db
        .read(|conn| cq::event_by_id(conn, event_id))?
        .ok_or(CommandError::UnknownEvent { event_id })?;

    // Idempotent: responding the same way twice is not an error and costs no
    // round trip. The inverse stays `None` because nothing moved.
    let prior = event.rsvp_status;
    if prior == Some(response) {
        return Ok(CommandResult {
            ok: true,
            message: describe(response).to_string(),
            undo: None,
            applied: vec![event_id],
            failed: Vec::new(),
        });
    }

    let account = dispatcher
        .db
        .read(|conn| cq::account_by_id(conn, event.account_id))?
        .ok_or(CommandError::UnknownAccount {
            account_id: event.account_id,
        })?;
    let client = dispatcher.clients.calendar(event.account_id)?;

    // Local first — the week grid repaints from this row.
    dispatcher
        .db
        .write(|conn| cq::set_event_rsvp(conn, event_id, Some(response)))?;

    match client
        .events_rsvp(
            &event.calendar_id,
            &event.google_event_id,
            &account.email,
            to_google(response),
        )
        .await
    {
        Ok(_) => Ok(CommandResult {
            ok: true,
            message: describe(response).to_string(),
            // `needsAction` is a real response state, so an event that had no
            // recorded answer still has a faithful inverse.
            undo: Some(Command::Rsvp {
                event_id,
                response: prior.unwrap_or(RsvpStatus::NeedsAction),
            }),
            applied: vec![event_id],
            failed: Vec::new(),
        }),
        Err(error) => {
            dispatcher
                .db
                .write(|conn| cq::set_event_rsvp(conn, event_id, prior))?;
            Ok(CommandResult {
                ok: false,
                message: "Could not send the RSVP".to_string(),
                undo: None,
                applied: Vec::new(),
                failed: vec![CommandFailure::from_google(vec![event_id], &error)],
            })
        }
    }
}

// ===========================================================================
// create
// ===========================================================================

pub(crate) async fn execute_create(
    dispatcher: &CommandDispatcher,
    account_id: i64,
    calendar_id: &str,
    draft: &EventDraft,
) -> Result<CommandResult, CommandError> {
    if draft.end_ts < draft.start_ts {
        return Err(CommandError::Invalid {
            message: "an event cannot end before it starts".into(),
        });
    }
    // Resolve the client before writing anything: an unconfigured account is a
    // command that never ran, not a local row to clean up afterwards.
    let client = dispatcher.clients.calendar(account_id)?;

    // We are creating this on our own calendar, so we are its organizer — and
    // saying so up front is what keeps the block editable between the optimistic
    // write and the first sync that would otherwise be the only source of the
    // fact. A missing account row is not fatal here: it costs a name on the
    // organizer line, not the ability to make the event.
    let organizer = dispatcher
        .db
        .read(|conn| cq::account_by_id(conn, account_id))?
        .map(|account| Participant {
            name: account.display_name.filter(|n| !n.is_empty()),
            email: account.email,
        });

    // The placeholder id is unique enough to satisfy
    // `UNIQUE (account_id, calendar_id, google_event_id)` and obvious enough in
    // a database dump to be recognisable if one ever survives a crash between
    // the insert below and the adoption further down.
    let placeholder = format!("mach-pending-{}-{}", now_ms(), draft.start_ts);
    let row = NewEvent {
        account_id,
        calendar_id: calendar_id.to_string(),
        google_event_id: placeholder,
        title: draft.title.clone(),
        description: draft.description.clone().filter(|s| !s.is_empty()),
        location: draft.location.clone().filter(|s| !s.is_empty()),
        start_ts: draft.start_ts,
        end_ts: draft.end_ts,
        is_all_day: draft.is_all_day,
        attendees: draft.attendees.clone(),
        // We are the organizer of anything we create, so there is nothing to
        // respond to — an RSVP status here would paint the block as an invite.
        rsvp_status: None,
        recurring_event_id: None,
        // The rule is written down at the moment we know it. Google will never
        // tell us again: `singleEvents=true` returns occurrences, and an
        // occurrence carries no RRULE. Sync's upsert is built to preserve this
        // rather than overwrite it with the silence of an expanded instance.
        recurrence: draft.recurrence.clone(),
        reminders: draft.reminder_minutes.as_deref().map(stored_reminders),
        // Minted by Google; adopted below once the insert answers.
        ical_uid: None,
        organizer,
        organizer_self: Some(true),
        guests_can_modify: Some(false),
        status: "confirmed".into(),
        html_link: None,
        updated_at: now_ms(),
    };

    let event_id = dispatcher.db.write(|conn| queries::upsert_event(conn, &row))?;

    match client
        .events_insert(calendar_id, &google_event_for(draft))
        .await
    {
        Ok(created) => {
            let google_id = created.id.clone().unwrap_or_default();
            // A recurring insert answers with the *series master*. The local row
            // stands for one occurrence, so it takes the id Google's own
            // `instances` expansion will use for that occurrence — `{master}_{
            // original start in UTC}` — and sync then updates this row instead
            // of adding a second one beside it.
            let (row_id, parent) = if draft.recurrence.is_empty() {
                (google_id.clone(), None)
            } else {
                (
                    instance_id(&google_id, draft.start_ts, draft.is_all_day),
                    Some(google_id.clone()),
                )
            };
            dispatcher.db.write(|conn| {
                cq::set_event_identity(
                    conn,
                    event_id,
                    &row_id,
                    created.html_link.as_deref(),
                    parent.as_deref(),
                    created.ical_uid.as_deref(),
                )
            })?;

            Ok(CommandResult {
                ok: true,
                message: format!("Created “{}”", title_or_placeholder(&draft.title)),
                undo: Some(Command::DeleteEvent {
                    event_id,
                    scope: if draft.recurrence.is_empty() {
                        EventScope::This
                    } else {
                        EventScope::All
                    },
                }),
                applied: vec![event_id],
                failed: Vec::new(),
            })
        }
        Err(error) => {
            dispatcher
                .db
                .write(|conn| queries::delete_event(conn, event_id))?;
            Ok(CommandResult {
                ok: false,
                message: "Could not create the event".to_string(),
                undo: None,
                applied: Vec::new(),
                failed: vec![CommandFailure::from_google(vec![event_id], &error)],
            })
        }
    }
}

// ===========================================================================
// update
// ===========================================================================

pub(crate) async fn execute_update(
    dispatcher: &CommandDispatcher,
    event_id: i64,
    patch: &EventPatch,
    scope: EventScope,
) -> Result<CommandResult, CommandError> {
    let event = dispatcher
        .db
        .read(|conn| cq::event_by_id(conn, event_id))?
        .ok_or(CommandError::UnknownEvent { event_id })?;

    if patch.is_empty() {
        return Ok(CommandResult::noop("Nothing changed"));
    }
    if let (Some(start), Some(end)) = (patch.start_ts, patch.end_ts) {
        if end < start {
            return Err(CommandError::Invalid {
                message: "an event cannot end before it starts".into(),
            });
        }
    }

    // A series is only a series when Google says so. `scope: All` on a one-off
    // event is just `This`, and treating it as such keeps the caller from
    // having to know which is which before it asks.
    let series_parent = event.recurring_event_id.clone().filter(|s| !s.is_empty());
    let effective = match (&series_parent, scope) {
        (Some(_), EventScope::All) => EventScope::All,
        _ => EventScope::This,
    };

    // How an event repeats is a property of the series, and Google says so:
    // `events.patch` on an expanded instance refuses a `recurrence` key
    // outright. Catching it here turns a 400 the user cannot act on into a
    // sentence that names the button they need. (The modal forces the choice to
    // "all of them" whenever the rule changed, so this should be unreachable
    // from the UI — it is reachable from the agent, and from a plugin.)
    if effective == EventScope::This && series_parent.is_some() && patch.recurrence.is_some() {
        return Err(CommandError::Invalid {
            message: "how an event repeats belongs to the whole series — choose “all of them”, \
                      or change just this occurrence's time instead"
                .into(),
        });
    }

    if effective == EventScope::All && patch.touches_time() {
        return Err(CommandError::Invalid {
            message: "changing the time of a whole series is not something Google's API can do \
                      from one occurrence — edit this occurrence, or re-time the series in \
                      Google Calendar"
                .into(),
        });
    }

    let client = dispatcher.clients.calendar(event.account_id)?;
    let target_id = match effective {
        EventScope::All => series_parent.clone().unwrap_or_else(|| event.google_event_id.clone()),
        EventScope::This => event.google_event_id.clone(),
    };

    // Every row this command is about to touch, exactly as it stands. This is
    // both the rollback and the source of the inverse.
    let before: Vec<Event> = match effective {
        EventScope::This => vec![event.clone()],
        EventScope::All => dispatcher.db.read(|conn| {
            cq::events_in_series(
                conn,
                event.account_id,
                &event.calendar_id,
                series_parent.as_deref().unwrap_or_default(),
            )
        })?,
    };
    let touched: Vec<i64> = before.iter().map(|e| e.id).collect();

    let fields = stored_fields(patch);
    dispatcher.db.write(|conn| {
        for row in &before {
            cq::update_event_fields(conn, row.id, &fields)?;
        }
        Ok(())
    })?;

    match client
        .events_patch(&event.calendar_id, &target_id, &patch_body(patch))
        .await
    {
        Ok(_) => Ok(CommandResult {
            ok: true,
            message: update_message(patch, effective, before.len()),
            undo: inverse_patch(patch, &event).map(|prior| Command::UpdateEvent {
                event_id,
                patch: prior,
                scope: effective,
            }),
            applied: touched,
            failed: Vec::new(),
        }),
        Err(error) => {
            dispatcher.db.write(|conn| {
                for row in &before {
                    cq::restore_event(conn, row)?;
                }
                Ok(())
            })?;
            Ok(CommandResult {
                ok: false,
                message: "Could not save the change".to_string(),
                undo: None,
                applied: Vec::new(),
                failed: vec![CommandFailure::from_google(touched, &error)],
            })
        }
    }
}

// ===========================================================================
// delete
// ===========================================================================

pub(crate) async fn execute_delete(
    dispatcher: &CommandDispatcher,
    event_id: i64,
    scope: EventScope,
) -> Result<CommandResult, CommandError> {
    let event = dispatcher
        .db
        .read(|conn| cq::event_by_id(conn, event_id))?
        .ok_or(CommandError::UnknownEvent { event_id })?;

    let series_parent = event.recurring_event_id.clone().filter(|s| !s.is_empty());
    let effective = match (&series_parent, scope) {
        (Some(_), EventScope::All) => EventScope::All,
        _ => EventScope::This,
    };

    let client = dispatcher.clients.calendar(event.account_id)?;
    let target_id = match effective {
        EventScope::All => series_parent.clone().unwrap_or_else(|| event.google_event_id.clone()),
        EventScope::This => event.google_event_id.clone(),
    };

    let before: Vec<Event> = match effective {
        EventScope::This => vec![event.clone()],
        EventScope::All => dispatcher.db.read(|conn| {
            cq::events_in_series(
                conn,
                event.account_id,
                &event.calendar_id,
                series_parent.as_deref().unwrap_or_default(),
            )
        })?,
    };
    let touched: Vec<i64> = before.iter().map(|e| e.id).collect();

    dispatcher.db.write(|conn| {
        for row in &before {
            queries::delete_event(conn, row.id)?;
        }
        Ok(())
    })?;

    match client.events_delete(&event.calendar_id, &target_id).await {
        Ok(()) => Ok(CommandResult {
            ok: true,
            message: match effective {
                EventScope::All => format!(
                    "Deleted every occurrence of “{}”",
                    title_or_placeholder(&event.title)
                ),
                EventScope::This => format!("Deleted “{}”", title_or_placeholder(&event.title)),
            },
            // A one-off can be put back verbatim. An occurrence cannot: there
            // is no endpoint that returns a cancelled instance to its series,
            // and re-creating it would make a standalone event wearing the same
            // name. Claiming an inverse there would be a lie, so there isn't one.
            undo: if series_parent.is_none() {
                Some(Command::CreateEvent {
                    account_id: event.account_id,
                    calendar_id: event.calendar_id.clone(),
                    draft: draft_from(&event),
                })
            } else {
                None
            },
            applied: touched,
            failed: Vec::new(),
        }),
        Err(error) => {
            // A 404 means Google already lost it. Keeping the row would put a
            // block on screen that nothing can delete, so the local delete
            // stands and the command reports success against a corrected world.
            if matches!(error, GoogleError::NotFound { .. }) {
                return Ok(CommandResult {
                    ok: true,
                    message: "Deleted — it was already gone on Google's side".to_string(),
                    undo: None,
                    applied: touched,
                    failed: Vec::new(),
                });
            }
            dispatcher.db.write(|conn| {
                for row in &before {
                    cq::restore_event(conn, row)?;
                }
                Ok(())
            })?;
            Ok(CommandResult {
                ok: false,
                message: "Could not delete the event".to_string(),
                undo: None,
                applied: Vec::new(),
                failed: vec![CommandFailure::from_google(touched, &error)],
            })
        }
    }
}

// ===========================================================================
// move between calendars
// ===========================================================================

pub(crate) async fn execute_move(
    dispatcher: &CommandDispatcher,
    event_id: i64,
    account_id: i64,
    calendar_id: &str,
) -> Result<CommandResult, CommandError> {
    let event = dispatcher
        .db
        .read(|conn| cq::event_by_id(conn, event_id))?
        .ok_or(CommandError::UnknownEvent { event_id })?;

    if event.account_id == account_id && event.calendar_id == calendar_id {
        return Ok(CommandResult::noop("Already on that calendar"));
    }
    if event.recurring_event_id.is_some() {
        return Err(CommandError::Invalid {
            message: "moving one occurrence of a recurring event to another calendar would \
                      detach it from its series — move the series in Google Calendar instead"
                .into(),
        });
    }

    let source = dispatcher.clients.calendar(event.account_id)?;
    let destination = dispatcher.clients.calendar(account_id)?;

    let prior_account = event.account_id;
    let prior_calendar = event.calendar_id.clone();
    let prior_google_id = event.google_event_id.clone();

    dispatcher
        .db
        .write(|conn| cq::set_event_calendar(conn, event_id, account_id, calendar_id))?;

    let draft = draft_from(&event);
    let created = match destination
        .events_insert(calendar_id, &google_event_for(&draft))
        .await
    {
        Ok(created) => created,
        Err(error) => {
            dispatcher.db.write(|conn| {
                cq::set_event_calendar(conn, event_id, prior_account, &prior_calendar)
            })?;
            return Ok(CommandResult {
                ok: false,
                message: "Could not move the event".to_string(),
                undo: None,
                applied: Vec::new(),
                failed: vec![CommandFailure::from_google(vec![event_id], &error)],
            });
        }
    };

    let new_google_id = created.id.clone().unwrap_or_default();
    dispatcher.db.write(|conn| {
        cq::set_event_identity(
            conn,
            event_id,
            &new_google_id,
            created.html_link.as_deref(),
            None,
            created.ical_uid.as_deref(),
        )
    })?;

    // The copy exists. Removing the original is the half that can leave a
    // duplicate behind, so a failure here undoes the copy rather than leaving
    // the same meeting on two calendars.
    if let Err(error) = source
        .events_delete(&prior_calendar, &prior_google_id)
        .await
    {
        if !matches!(error, GoogleError::NotFound { .. }) {
            let _ = destination.events_delete(calendar_id, &new_google_id).await;
            dispatcher.db.write(|conn| {
                cq::set_event_calendar(conn, event_id, prior_account, &prior_calendar)?;
                cq::set_event_identity(
                    conn,
                    event_id,
                    &prior_google_id,
                    event.html_link.as_deref(),
                    None,
                    event.ical_uid.as_deref(),
                )
            })?;
            return Ok(CommandResult {
                ok: false,
                message: "Could not move the event".to_string(),
                undo: None,
                applied: Vec::new(),
                failed: vec![CommandFailure::from_google(vec![event_id], &error)],
            });
        }
    }

    Ok(CommandResult {
        ok: true,
        message: format!("Moved “{}”", title_or_placeholder(&event.title)),
        undo: Some(Command::MoveEvent {
            event_id,
            account_id: prior_account,
            calendar_id: prior_calendar,
        }),
        applied: vec![event_id],
        failed: Vec::new(),
    })
}

// ===========================================================================
// shaping
// ===========================================================================

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn title_or_placeholder(title: &str) -> &str {
    if title.trim().is_empty() {
        "(no title)"
    } else {
        title
    }
}

/// RFC3339 in the machine's own zone, which is the zone the user typed in.
///
/// Sending UTC would be accepted and would also be wrong the moment the event
/// crosses a DST boundary in Google's UI: the offset is what tells Google which
/// wall clock the user meant.
fn rfc3339_local(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH)
        .with_timezone(&chrono::Local)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// `YYYY-MM-DD`, read in UTC — the convention `sync::convert` already stores
/// all-day events under, so a round trip through Mach does not shift the date.
fn utc_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH)
        .format("%Y-%m-%d")
        .to_string()
}

/// The id Google gives the occurrence of `master` that starts at `start_ts`.
///
/// `{master}_{YYYYMMDD}` for an all-day series, `{master}_{YYYYMMDDTHHMMSSZ}`
/// for a timed one — the form `events.instances` returns. Guessing it lets the
/// row written at create time be the row sync later updates. If Google ever
/// answers with something else, the cost is one stale row until the next full
/// resync, not a wrong event.
fn instance_id(master: &str, start_ts: i64, all_day: bool) -> String {
    let stamp = chrono::DateTime::from_timestamp_millis(start_ts)
        .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH)
        .format(if all_day { "%Y%m%d" } else { "%Y%m%dT%H%M%SZ" })
        .to_string();
    format!("{master}_{stamp}")
}

fn attendee_rows(people: &[Participant]) -> Vec<EventAttendee> {
    people
        .iter()
        .filter(|p| !p.email.trim().is_empty())
        .map(|p| EventAttendee {
            email: Some(p.email.clone()),
            display_name: p.name.clone().filter(|n| !n.is_empty()),
            ..Default::default()
        })
        .collect()
}

fn reminders_for(minutes: &[i64]) -> GoogleReminders {
    GoogleReminders {
        // Naming any override at all means "not the calendar default", so this
        // has to be false even when the list is empty — that is how "no
        // reminder on this one event" is expressed.
        use_default: Some(false),
        overrides: minutes
            .iter()
            .map(|m| EventReminderOverride {
                method: Some("popup".into()),
                minutes: Some(*m),
            })
            .collect(),
    }
}

fn start_end(start_ts: i64, end_ts: i64, all_day: bool) -> (EventDateTime, EventDateTime) {
    if all_day {
        // Google's all-day `end.date` is exclusive, and so is the stored
        // `end_ts`; a one-day event is midnight to midnight.
        (
            EventDateTime::all_day(utc_date(start_ts)),
            EventDateTime::all_day(utc_date(end_ts.max(start_ts + 86_400_000))),
        )
    } else {
        (
            EventDateTime::date_time(rfc3339_local(start_ts), None),
            EventDateTime::date_time(rfc3339_local(end_ts), None),
        )
    }
}

fn google_event_for(draft: &EventDraft) -> GoogleEvent {
    let (start, end) = start_end(draft.start_ts, draft.end_ts, draft.is_all_day);
    GoogleEvent {
        summary: Some(draft.title.clone()).filter(|s| !s.is_empty()),
        description: draft.description.clone().filter(|s| !s.is_empty()),
        location: draft.location.clone().filter(|s| !s.is_empty()),
        start: Some(start),
        end: Some(end),
        attendees: attendee_rows(&draft.attendees),
        recurrence: draft.recurrence.clone(),
        reminders: draft.reminder_minutes.as_deref().map(reminders_for),
        ..Default::default()
    }
}

/// The `events.patch` body: only the keys the patch actually named.
///
/// `events.patch` is a genuine partial update, so an absent key is "leave it
/// alone" and an explicit `null` is "clear it". Both are used here, which is
/// why this is hand-built rather than a serialized struct with skip rules.
fn patch_body(patch: &EventPatch) -> Value {
    let mut body = Map::new();

    if let Some(title) = &patch.title {
        body.insert("summary".into(), json!(title));
    }
    for (key, value) in [
        ("description", &patch.description),
        ("location", &patch.location),
    ] {
        if let Some(text) = value {
            body.insert(
                key.into(),
                if text.is_empty() { Value::Null } else { json!(text) },
            );
        }
    }
    if let Some(attendees) = &patch.attendees {
        body.insert("attendees".into(), json!(attendee_rows(attendees)));
    }
    if let Some(recurrence) = &patch.recurrence {
        body.insert("recurrence".into(), json!(recurrence));
    }
    if let Some(minutes) = &patch.reminder_minutes {
        body.insert("reminders".into(), json!(reminders_for(minutes)));
    }
    // Start and end move together even when only one was named: switching
    // between timed and all-day changes the *shape* of both keys, and Google
    // rejects a body whose start is a `date` and whose end is a `dateTime`.
    if patch.touches_time() {
        // The caller always sends both halves of a time change for exactly this
        // reason; a half-specified move is refused rather than guessed at.
        if let (Some(start), Some(end)) = (patch.start_ts, patch.end_ts) {
            let (s, e) = start_end(start, end, patch.is_all_day.unwrap_or(false));
            body.insert("start".into(), json!(s));
            body.insert("end".into(), json!(e));
        }
    }

    Value::Object(body)
}

/// The stored form of a patch's reminder offsets.
///
/// Naming any override at all means "not the calendar default", which is why
/// `use_default` is false even for an empty list — that is how "no alert on
/// this one event" is expressed, and it mirrors [`reminders_for`], the wire
/// version of the same rule.
fn stored_reminders(minutes: &[i64]) -> EventReminders {
    EventReminders {
        use_default: false,
        overrides: minutes
            .iter()
            .map(|m| EventReminder {
                method: "popup".into(),
                minutes: *m,
            })
            .collect(),
    }
}

/// The stored subset of a patch — what SQLite has columns for.
fn stored_fields(patch: &EventPatch) -> EventFields {
    EventFields {
        title: patch.title.clone(),
        description: patch
            .description
            .clone()
            .map(|text| if text.is_empty() { None } else { Some(text) }),
        location: patch
            .location
            .clone()
            .map(|text| if text.is_empty() { None } else { Some(text) }),
        start_ts: patch.start_ts,
        end_ts: patch.end_ts,
        is_all_day: patch.is_all_day,
        attendees: patch.attendees.clone(),
        // Both of these used to be dropped on the floor here, and everything
        // downstream inherited the hole: the modal reopened on "does not
        // repeat", the inverse could not be built, and an edit that changed a
        // rule looked, on reload, like an edit that had not happened.
        recurrence: patch.recurrence.clone(),
        reminders: patch.reminder_minutes.as_deref().map(stored_reminders),
    }
}

/// The patch that puts `event` back, narrowed to the fields `patch` named.
///
/// # The one field that still cannot be inverted
///
/// Recurrence now round trips: the store keeps the rule, so "make this weekly"
/// inverts to whatever it was, including the empty list that means "does not
/// repeat".
///
/// Reminders only invert *out of* an explicit setting. Google has three states
/// — the calendar's default, no alert, and these specific alerts — and
/// [`EventPatch::reminder_minutes`] can only name the last two. So an event
/// that was on the calendar default before the edit has a prior state this
/// vocabulary cannot express, and the honest answer is no inverse at all rather
/// than an undo that quietly sets "no alert" and calls it the default. The
/// status bar then offers no undo, which is true.
fn inverse_patch(patch: &EventPatch, event: &Event) -> Option<EventPatch> {
    // `Option<Option<_>>`: the outer says whether the patch named reminders at
    // all, the inner whether the prior state can be spoken.
    let prior_reminders = patch.reminder_minutes.as_ref().map(|_| {
        event
            .reminders
            .as_ref()
            .and_then(EventReminders::explicit_minutes)
    });
    if matches!(prior_reminders, Some(None)) {
        return None;
    }

    let prior = EventPatch {
        title: patch.title.as_ref().map(|_| event.title.clone()),
        description: patch
            .description
            .as_ref()
            .map(|_| event.description.clone().unwrap_or_default()),
        location: patch
            .location
            .as_ref()
            .map(|_| event.location.clone().unwrap_or_default()),
        // A time change always names start, end and all-day together, so the
        // inverse does too — see `patch_body`.
        start_ts: patch.touches_time().then_some(event.start_ts),
        end_ts: patch.touches_time().then_some(event.end_ts),
        is_all_day: patch.touches_time().then_some(event.is_all_day),
        attendees: patch.attendees.as_ref().map(|_| event.attendees.clone()),
        recurrence: patch.recurrence.as_ref().map(|_| event.recurrence.clone()),
        reminder_minutes: prior_reminders.flatten(),
    };
    (!prior.is_empty()).then_some(prior)
}

fn draft_from(event: &Event) -> EventDraft {
    EventDraft {
        title: event.title.clone(),
        description: event.description.clone(),
        location: event.location.clone(),
        start_ts: event.start_ts,
        end_ts: event.end_ts,
        is_all_day: event.is_all_day,
        attendees: event.attendees.clone(),
        // Re-creating a deleted event now re-creates the event, not a
        // lookalike: the rule and the alerts come back with it. This is the
        // undo of a delete, and it is also the body of a cross-calendar move —
        // both of which used to quietly drop both.
        recurrence: event.recurrence.clone(),
        reminder_minutes: event
            .reminders
            .as_ref()
            .and_then(EventReminders::explicit_minutes),
    }
}

fn update_message(patch: &EventPatch, scope: EventScope, rows: usize) -> String {
    let what = if patch.touches_time() {
        "Moved the event"
    } else if patch.title.is_some() {
        "Renamed the event"
    } else if patch.attendees.is_some() {
        "Updated the guest list"
    } else {
        "Saved the event"
    };
    match scope {
        EventScope::All => format!("{what} — every occurrence ({rows})"),
        EventScope::This => what.to_string(),
    }
}
