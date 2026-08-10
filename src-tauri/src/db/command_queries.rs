//! Extra read/write helpers owned by the command layer unit.
//!
//! `db::queries` is the surface the UI and the sync loop share. This file holds
//! the handful of statements only the command layer needs, so that adding a
//! command never means widening a file two other units are also editing.
//!
//! Three things live here that are not in `queries`:
//!
//!  * **Thread snapshots.** Every command is optimistic, so every command needs
//!    the exact prior state it must be able to put back: the thread's label set,
//!    its unread flag, and the Gmail message ids the remote call will name.
//!    One function reads all three.
//!  * **The snooze table.** Gmail has no snooze primitive, so Mach keeps the
//!    wake time locally. See `commands::mail` for the full decision.
//!  * **Point lookups `queries` does not expose** — an account by row id, an
//!    event by row id — because the read paths there are list-shaped (the UI
//!    renders lists) while a command always addresses one row.
//!
//! # The snooze table and migrations
//!
//! `db::schema` is owned by another unit, so `snoozed_threads` is created here
//! by [`ensure_command_schema`], which the dispatcher calls once at
//! construction. It is written as `CREATE TABLE IF NOT EXISTS` so it is safe to
//! call on every start and safe to *move* into `MIGRATIONS` later: the migration
//! would create the same table, and this call would then find it already there.
//! That is the intended end state; this is the seam that keeps the two units
//! from editing the same file today.

use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::models::{self, Account, Event, EventReminders, Participant, RsvpStatus};
use crate::db::{queries, Result};

// ---------------------------------------------------------------------------
// schema owned by the command layer
// ---------------------------------------------------------------------------

/// Local-only state that has no Gmail representation.
///
/// `prior_label_ids` is what makes un-snooze *correct* rather than a guess: a
/// thread snoozed out of the inbox comes back with the labels it left with, and
/// a thread snoozed while already archived does not gain INBOX on wake.
pub const COMMAND_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS snoozed_threads (
    thread_id       INTEGER PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    -- Unix millis, like every other timestamp in this store.
    wake_at         INTEGER NOT NULL,
    snoozed_at      INTEGER NOT NULL DEFAULT 0,
    -- JSON array of gmail label ids the thread carried before it was snoozed.
    prior_label_ids TEXT    NOT NULL DEFAULT '[]',
    prior_is_unread INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_snoozed_threads_wake ON snoozed_threads (wake_at);
"#;

/// Idempotent. Called once when a [`crate::commands::CommandDispatcher`] is
/// built.
pub fn ensure_command_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(COMMAND_SCHEMA)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// thread snapshots
// ---------------------------------------------------------------------------

/// Everything a command needs to act on a thread and to put it back afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSnapshot {
    pub thread_id: i64,
    pub account_id: i64,
    pub gmail_thread_id: String,
    /// Sorted, so two snapshots of the same state compare equal.
    pub label_ids: Vec<String>,
    pub is_unread: bool,
    /// Gmail message ids, oldest first — the ids `messages.modify` and
    /// `messages.batchModify` address. Gmail has no thread-level modify that
    /// takes a label delta, so every mail command is expressed over messages.
    ///
    /// **Only ids Google minted.** A conversation can also hold rows Mach wrote
    /// for itself — an unsent draft, a reply waiting out its send delay — and
    /// those are in [`local_message_ids`](Self::local_message_ids) instead.
    pub message_ids: Vec<String>,
    /// The rows in this conversation Google has not been told about yet, under
    /// the placeholder ids Mach filed them under (`mach-draft:…`,
    /// `mach-outbox:…`).
    ///
    /// They are messages to the reader and to the store, and they are not ids
    /// Gmail will accept: naming one in `batchModify` is a `400 Invalid ids
    /// value` that takes the whole request down with it. Kept as a separate
    /// list rather than dropped so that a command with nothing left to send can
    /// say *why* — "this conversation is only a draft" and "this conversation
    /// has never been synced" are different news.
    pub local_message_ids: Vec<String>,
}

pub fn thread_snapshot(conn: &Connection, thread_id: i64) -> Result<Option<ThreadSnapshot>> {
    let base = conn
        .query_row(
            "SELECT id, account_id, gmail_thread_id, is_unread FROM threads WHERE id = ?1",
            [thread_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((id, account_id, gmail_thread_id, is_unread)) = base else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(
        "SELECT gmail_label_id FROM thread_labels WHERE thread_id = ?1 ORDER BY gmail_label_id",
    )?;
    let label_ids = stmt
        .query_map([thread_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare(
        "SELECT gmail_message_id FROM messages WHERE thread_id = ?1 ORDER BY internal_date, id",
    )?;
    let all_ids = stmt
        .query_map([thread_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // The split is done here, once, rather than at each of the nine mail
    // commands: every one of them reads this struct to build a Gmail request,
    // and every one of them was sending the placeholders.
    let (local_message_ids, message_ids): (Vec<String>, Vec<String>) = all_ids
        .into_iter()
        .partition(|id| models::is_local_message_id(id));

    Ok(Some(ThreadSnapshot {
        thread_id: id,
        account_id,
        gmail_thread_id,
        label_ids,
        is_unread,
        message_ids,
        local_message_ids,
    }))
}

/// Set a thread's label set and unread flag to exactly this state.
///
/// The unread flag is written to `threads` **and** to the thread's `messages`,
/// because the list renders from the former and the reading pane from the
/// latter; leaving them to disagree is how a thread ends up looking read in one
/// pane and unread in the other.
pub fn set_thread_state(
    conn: &Connection,
    thread_id: i64,
    label_ids: &[String],
    is_unread: bool,
) -> Result<()> {
    queries::set_thread_labels(conn, thread_id, label_ids)?;
    queries::set_thread_unread(conn, thread_id, is_unread)?;
    conn.execute(
        "UPDATE messages SET is_unread = ?2 WHERE thread_id = ?1",
        params![thread_id, is_unread],
    )?;
    Ok(())
}

/// The Gmail label id for a label with this display name, if the account has
/// one. Used to resolve Mach's snooze label without hard-coding an id.
pub fn label_id_by_name(
    conn: &Connection,
    account_id: i64,
    name: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT gmail_label_id FROM labels WHERE account_id = ?1 AND name = ?2",
            params![account_id, name],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

// ---------------------------------------------------------------------------
// snooze
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeRow {
    pub thread_id: i64,
    pub wake_at: i64,
    pub snoozed_at: i64,
    pub prior_label_ids: Vec<String>,
    pub prior_is_unread: bool,
}

fn map_snooze(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnoozeRow> {
    let prior: String = row.get(3)?;
    Ok(SnoozeRow {
        thread_id: row.get(0)?,
        wake_at: row.get(1)?,
        snoozed_at: row.get(2)?,
        prior_label_ids: serde_json::from_str(&prior).unwrap_or_default(),
        prior_is_unread: row.get(4)?,
    })
}

const SNOOZE_COLUMNS: &str =
    "thread_id, wake_at, snoozed_at, prior_label_ids, prior_is_unread";

pub fn snooze_row(conn: &Connection, thread_id: i64) -> Result<Option<SnoozeRow>> {
    let sql = format!("SELECT {SNOOZE_COLUMNS} FROM snoozed_threads WHERE thread_id = ?1");
    Ok(conn.query_row(&sql, [thread_id], map_snooze).optional()?)
}

pub fn upsert_snooze(conn: &Connection, row: &SnoozeRow) -> Result<()> {
    let prior = serde_json::to_string(&row.prior_label_ids).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO snoozed_threads (thread_id, wake_at, snoozed_at, prior_label_ids, prior_is_unread)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(thread_id) DO UPDATE SET
             wake_at         = excluded.wake_at,
             snoozed_at      = excluded.snoozed_at,
             prior_label_ids = excluded.prior_label_ids,
             prior_is_unread = excluded.prior_is_unread",
        params![
            row.thread_id,
            row.wake_at,
            row.snoozed_at,
            prior,
            row.prior_is_unread
        ],
    )?;
    Ok(())
}

pub fn delete_snooze(conn: &Connection, thread_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM snoozed_threads WHERE thread_id = ?1",
        [thread_id],
    )?;
    Ok(())
}

/// Threads whose wake time has arrived, oldest wake first.
///
/// The clock that calls this belongs to the sync/scheduler unit; the command
/// layer only supplies the query and the [`crate::commands::Command::Unsnooze`]
/// that acts on the answer. Because the wake time is stored rather than held in
/// a timer, a snooze that comes due while Mach is closed fires on next launch
/// instead of being lost.
pub fn due_snoozes(conn: &Connection, now_ms: i64) -> Result<Vec<SnoozeRow>> {
    let sql = format!(
        "SELECT {SNOOZE_COLUMNS} FROM snoozed_threads WHERE wake_at <= ?1 ORDER BY wake_at, thread_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([now_ms], map_snooze)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------------------------------------------------------------------------
// point lookups
// ---------------------------------------------------------------------------

/// An account by row id. `queries::account_by_email` exists; commands hold
/// numeric ids, and RSVP needs the address to name the right attendee.
pub fn account_by_id(conn: &Connection, account_id: i64) -> Result<Option<Account>> {
    let accounts = queries::list_accounts(conn)?;
    Ok(accounts.into_iter().find(|a| a.id == account_id))
}

// The column list and the row mapper are `queries`'. They used to be copied
// here byte for byte, which held only as long as nobody added a column: a
// `SELECT` widened in one file and not the other reads a `TEXT` out of an
// `INTEGER` slot, and that is a runtime panic rather than a compile error.
use queries::{map_event, EVENT_COLUMNS};

/// One event by row id. `queries::events_in_range` is the window query the week
/// grid uses; RSVP addresses a single row and must not scan a range to find it.
pub fn event_by_id(conn: &Connection, event_id: i64) -> Result<Option<Event>> {
    let sql = format!("SELECT {EVENT_COLUMNS} FROM events WHERE id = ?1");
    Ok(conn.query_row(&sql, [event_id], map_event).optional()?)
}

pub fn set_event_rsvp(
    conn: &Connection,
    event_id: i64,
    status: Option<RsvpStatus>,
) -> Result<()> {
    conn.execute(
        "UPDATE events SET rsvp_status = ?2 WHERE id = ?1",
        params![event_id, status.map(|s| s.as_str())],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// event writes
// ---------------------------------------------------------------------------

/// The stored half of an event edit.
///
/// Deliberately *not* `commands::EventPatch`: `db` must not depend on
/// `commands`, and the two types answer different questions — a patch says what
/// to send Google, this says what to write down. Every field is `None` for
/// "leave alone"; the nested options distinguish "leave alone" from "set to
/// NULL".
///
/// `recurrence` and `reminders` are here now, and that is the point of them:
/// they were the two fields a patch could send but the store could not keep, so
/// an edit that touched either had no honest inverse and no way to be read back
/// into the modal that made it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFields {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub is_all_day: Option<bool>,
    pub attendees: Option<Vec<Participant>>,
    /// `Some(vec![])` clears the rule — unlike the sync path, an edit that says
    /// "does not repeat" genuinely means it, so this one writes SQL `NULL`
    /// rather than leaving what was there.
    pub recurrence: Option<Vec<String>>,
    pub reminders: Option<EventReminders>,
}

impl EventFields {
    pub fn is_empty(&self) -> bool {
        self == &EventFields::default()
    }
}

/// Write exactly the named columns of one event row.
///
/// Built as a dynamic `SET` list rather than a full-row `UPDATE` so that a
/// title edit cannot silently rewrite a time that sync changed underneath it.
pub fn update_event_fields(conn: &Connection, event_id: i64, fields: &EventFields) -> Result<()> {
    if fields.is_empty() {
        return Ok(());
    }
    let mut sets: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();

    let push = |column: &str, value: Value, sets: &mut Vec<String>, args: &mut Vec<Value>| {
        args.push(value);
        sets.push(format!("{column} = ?{}", args.len()));
    };

    if let Some(title) = &fields.title {
        push("title", Value::Text(title.clone()), &mut sets, &mut args);
    }
    if let Some(description) = &fields.description {
        let value = match description {
            Some(text) => Value::Text(text.clone()),
            None => Value::Null,
        };
        push("description", value, &mut sets, &mut args);
    }
    if let Some(location) = &fields.location {
        let value = match location {
            Some(text) => Value::Text(text.clone()),
            None => Value::Null,
        };
        push("location", value, &mut sets, &mut args);
    }
    if let Some(start) = fields.start_ts {
        push("start_ts", Value::Integer(start), &mut sets, &mut args);
    }
    if let Some(end) = fields.end_ts {
        push("end_ts", Value::Integer(end), &mut sets, &mut args);
    }
    if let Some(all_day) = fields.is_all_day {
        push(
            "is_all_day",
            Value::Integer(all_day as i64),
            &mut sets,
            &mut args,
        );
    }
    if let Some(attendees) = &fields.attendees {
        let json = serde_json::to_string(attendees).unwrap_or_else(|_| "[]".into());
        push("attendees", Value::Text(json), &mut sets, &mut args);
        // The answer sheet belongs to Google and this edit has just invalidated
        // it: a guest who was removed would otherwise keep their "yes" on the
        // list, and a guest who was added would be missing from it entirely.
        // `NULL` is the honest state — "we no longer know" — and the store reads
        // it back as the new addresses with no answers against them, which is
        // what is true until the next sync says otherwise.
        push("guests", Value::Null, &mut sets, &mut args);
    }
    if let Some(recurrence) = &fields.recurrence {
        let value = match queries::json_if_present(recurrence) {
            Some(json) => Value::Text(json),
            None => Value::Null,
        };
        push("recurrence", value, &mut sets, &mut args);
    }
    if let Some(reminders) = &fields.reminders {
        let json = serde_json::to_string(reminders).unwrap_or_else(|_| "null".into());
        push("reminders", Value::Text(json), &mut sets, &mut args);
    }

    args.push(Value::Integer(event_id));
    let sql = format!(
        "UPDATE events SET {} WHERE id = ?{}",
        sets.join(", "),
        args.len()
    );
    conn.execute(&sql, params_from_iter(args.iter()))?;
    Ok(())
}

/// Point the local row at the identity Google gave it back.
///
/// A created event is written locally *before* the insert goes out, under a
/// placeholder id, so the grid has something to draw. This is the second half:
/// once Google answers, the row takes on the real id, link, series parent and
/// `iCalUID`.
///
/// The uid is the one of the four Mach cannot guess. The row id can be derived
/// (`{master}_{start}`) and the link is cosmetic, but the uid is minted by
/// Google, and it is the only thing that will let the copy of this meeting that
/// lands on another of the owner's accounts be recognised as the same meeting
/// rather than drawn beside it.
pub fn set_event_identity(
    conn: &Connection,
    event_id: i64,
    google_event_id: &str,
    html_link: Option<&str>,
    recurring_event_id: Option<&str>,
    ical_uid: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE events
            SET google_event_id = ?2,
                html_link       = ?3,
                recurring_event_id = ?4,
                ical_uid        = COALESCE(?5, ical_uid)
          WHERE id = ?1",
        params![
            event_id,
            google_event_id,
            html_link,
            recurring_event_id,
            ical_uid
        ],
    )?;
    Ok(())
}

/// Re-home an event on another calendar, possibly on another account.
pub fn set_event_calendar(
    conn: &Connection,
    event_id: i64,
    account_id: i64,
    calendar_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE events SET account_id = ?2, calendar_id = ?3 WHERE id = ?1",
        params![event_id, account_id, calendar_id],
    )?;
    Ok(())
}

/// Every locally known occurrence of one series, oldest first.
///
/// Scoping by account *and* calendar as well as the series id matters: the same
/// meeting copied onto two accounts has the same `recurringEventId` on both, and
/// a series edit must not reach across the copy the user did not touch.
pub fn events_in_series(
    conn: &Connection,
    account_id: i64,
    calendar_id: &str,
    recurring_event_id: &str,
) -> Result<Vec<Event>> {
    let sql = format!(
        "SELECT {EVENT_COLUMNS} FROM events
          WHERE account_id = ?1 AND calendar_id = ?2 AND recurring_event_id = ?3
          ORDER BY start_ts, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![account_id, calendar_id, recurring_event_id], map_event)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Put a deleted or edited row back exactly as it was, id included.
///
/// `queries::upsert_event` cannot do this, for two reasons. It lets SQLite
/// assign the id, and a rollback that changes the id would strand every
/// reference the UI is holding. And it is deliberately *preserving* — its
/// `COALESCE`s exist so that a lossy sync cannot erase a rule it was never
/// told about — which is the opposite of what a rollback needs. This is a
/// verbatim replacement: every column, including the ones whose prior value was
/// `NULL`, because "the recurrence was cleared and then the save failed" has to
/// come back as cleared and not as whatever it was two edits ago.
///
/// Every column the table has must appear here. A column left out silently
/// reverts to its default on the first rollback that touches the row, which is
/// data loss that only shows up on the unhappy path.
pub fn restore_event(conn: &Connection, event: &Event) -> Result<()> {
    let attendees = serde_json::to_string(&event.attendees).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT OR REPLACE INTO events
            (id, account_id, calendar_id, google_event_id, title, description, location,
             start_ts, end_ts, is_all_day, attendees, rsvp_status, recurring_event_id,
             status, html_link, updated_at, recurrence, reminders, ical_uid, organizer,
             organizer_self, guests_can_modify, conference, guests, creator, attachments,
             visibility, transparency)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
        params![
            event.id,
            event.account_id,
            event.calendar_id,
            event.google_event_id,
            event.title,
            event.description,
            event.location,
            event.start_ts,
            event.end_ts,
            event.is_all_day,
            attendees,
            event.rsvp_status.map(|r| r.as_str()),
            event.recurring_event_id,
            event.status,
            event.html_link,
            event.updated_at,
            queries::json_if_present(&event.recurrence),
            queries::json_of(event.reminders.as_ref()),
            event.ical_uid,
            queries::json_of(event.organizer.as_ref()),
            event.organizer_self,
            event.guests_can_modify,
            queries::json_of(event.conference.as_ref()),
            queries::json_if_present(&event.guests),
            queries::json_of(event.creator.as_ref()),
            queries::json_if_present(&event.attachments),
            event.visibility.as_deref(),
            event.transparency.as_deref(),
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// bulk read
// ---------------------------------------------------------------------------

/// Thread ids that currently carry a label — the query the snooze waker and the
/// agent both want ("what is snoozed?", "what is starred?").
pub fn thread_ids_with_label(
    conn: &Connection,
    account_id: Option<i64>,
    gmail_label_id: &str,
) -> Result<Vec<i64>> {
    let mut sql = String::from(
        "SELECT tl.thread_id FROM thread_labels tl JOIN threads t ON t.id = tl.thread_id \
         WHERE tl.gmail_label_id = ?1",
    );
    let mut args: Vec<Value> = vec![Value::Text(gmail_label_id.to_string())];
    if let Some(id) = account_id {
        args.push(Value::Integer(id));
        sql.push_str(&format!(" AND t.account_id = ?{}", args.len()));
    }
    sql.push_str(" ORDER BY tl.thread_id");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), |row| row.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
