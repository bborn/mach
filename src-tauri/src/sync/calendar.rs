//! Calendar sync for one account.
//!
//! `events.list` with `singleEvents=true` over a bounded window, so Google
//! expands recurring series into concrete instances and Mach never interprets an
//! RRULE. The first sweep of a calendar is that window; every sweep after it
//! rides the `syncToken` Google returned on the sweep's **final page** — which
//! is why the token is only ever taken from `EventsSweep`, never from an
//! intermediate page.
//!
//! An expired token (410) is a normal path, not a failure: drop the token,
//! re-list the window, carry on.

use std::sync::Arc;

use crate::db::models::{Calendar, NewCalendar};
use crate::db::{queries, sync_queries, Db};
use crate::google::calendar::{CalendarClient, EventsListQuery};
use crate::google::types::{CalendarListEntry, EventsSweep};

use super::cancel::CancelToken;
use super::convert;
use super::status::{AccountReporter, SyncPhase};
use super::{SyncConfig, SyncError};

pub struct CalendarSync {
    pub db: Db,
    pub calendar: CalendarClient,
    pub account_id: i64,
    pub config: Arc<SyncConfig>,
    pub cancel: CancelToken,
    pub report: AccountReporter,
}

impl CalendarSync {
    /// One pass over every calendar this account can see. Returns the number of
    /// event rows written.
    pub async fn run(&self) -> Result<u64, SyncError> {
        self.cancel.check()?;
        self.report.phase(SyncPhase::Calendar);

        let calendars = self.calendars().await?;
        let mut written = 0u64;
        for (calendar_id, is_primary) in calendars {
            self.cancel.check()?;
            written += self.sync_calendar(&calendar_id, is_primary).await?;
        }
        Ok(written)
    }

    /// Which calendars to sync, and their metadata along the way.
    ///
    /// An explicit list in the config still wins — that is the test seam and the
    /// escape hatch — and it deliberately does not touch the `calendars` table,
    /// because a hand-written list is a statement about what to sync and not a
    /// claim about what the account has.
    ///
    /// Otherwise the answer comes from the store when the store is fresh, and
    /// from `calendarList.list` when it is not. This used to be a request on
    /// *every* pass: a dozen rows that change a handful of times a year, fetched
    /// once a minute forever. See [`CALENDAR_LIST_MAX_AGE_MS`] for why six hours.
    ///
    /// A failed fetch falls back to whatever is stored rather than failing the
    /// pass. Metadata is a nicety; events are the point, and a transient 500 on
    /// the list should not blank the week.
    async fn calendars(&self) -> Result<Vec<(String, bool)>, SyncError> {
        if !self.config.calendar_ids.is_empty() {
            return Ok(self
                .config
                .calendar_ids
                .iter()
                .map(|id| (id.clone(), id == "primary"))
                .collect());
        }

        let account_id = self.account_id;
        let stored = self
            .db
            .read(move |conn| queries::list_calendars(conn, Some(account_id)))?;
        if let Some(fresh) = syncable_from_store(&stored, now_ms()) {
            return Ok(fresh);
        }

        let entries = match self.calendar.calendar_list().await {
            Ok(entries) => entries,
            Err(e) => return fall_back_to_store(&stored, e),
        };
        self.store_calendar_list(&entries)?;
        Ok(syncable_from_list(&entries))
    }

    /// Persist a fresh `calendarList.list`, tombstoning whatever it no longer
    /// mentions, in one transaction so a half-written list cannot be read.
    fn store_calendar_list(&self, entries: &[CalendarListEntry]) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let now = now_ms();
        let rows: Vec<NewCalendar> = entries
            .iter()
            .filter(|entry| !entry.id.is_empty())
            .map(|entry| new_calendar(account_id, entry, now))
            .collect();
        // Tombstone against everything Google returned, including the entries it
        // flagged deleted — an id that came back marked deleted is exactly as
        // gone as one that did not come back at all, and both keep their row.
        let present: Vec<String> = rows
            .iter()
            .filter(|row| !row.deleted)
            .map(|row| row.calendar_id.clone())
            .collect();
        self.db.write(move |conn| {
            for row in &rows {
                queries::upsert_calendar(conn, row)?;
            }
            queries::tombstone_missing_calendars(conn, account_id, &present)?;
            Ok(())
        })?;
        Ok(())
    }

    async fn sync_calendar(&self, calendar_id: &str, is_primary: bool) -> Result<u64, SyncError> {
        let account_id = self.account_id;
        let stored = self
            .db
            .read(|conn| sync_queries::calendar_sync_token(conn, account_id, calendar_id))?;

        let sweep = match stored {
            Some(token) => {
                let query = EventsListQuery::new()
                    .sync_token(token)
                    .single_events(true)
                    .show_deleted(true)
                    .max_results(self.config.calendar_page_size);
                match self.calendar.events_list_all(calendar_id, &query).await {
                    Ok(sweep) => sweep,
                    Err(e) if e.requires_full_resync() => {
                        // Expected. Forget the token and re-list the window.
                        self.db.write(|conn| {
                            sync_queries::set_calendar_sync_token(
                                conn,
                                account_id,
                                calendar_id,
                                None,
                                is_primary,
                            )
                        })?;
                        self.full_window(calendar_id).await?
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            None => self.full_window(calendar_id).await?,
        };

        self.cancel.check()?;
        self.apply(calendar_id, is_primary, sweep)
    }

    async fn full_window(&self, calendar_id: &str) -> Result<EventsSweep, SyncError> {
        let now = now_ms();
        let day = 24 * 60 * 60 * 1000;
        let query = EventsListQuery::new()
            .single_events(true)
            .show_deleted(true)
            .time_min(convert::ms_to_rfc3339(
                now - self.config.calendar_past_days * day,
            ))
            .time_max(convert::ms_to_rfc3339(
                now + self.config.calendar_future_days * day,
            ))
            .order_by("startTime")
            .max_results(self.config.calendar_page_size);
        Ok(self.calendar.events_list_all(calendar_id, &query).await?)
    }

    /// Write a sweep, then the token — in that order, in one transaction. A
    /// token persisted before the events it accounts for would silently skip
    /// them on the next run.
    fn apply(
        &self,
        calendar_id: &str,
        is_primary: bool,
        sweep: EventsSweep,
    ) -> Result<u64, SyncError> {
        let account_id = self.account_id;
        let written = self.db.write(|conn| {
            let mut written = 0u64;
            for event in &sweep.events {
                let Some(id) = event.id.as_deref().filter(|s| !s.is_empty()) else {
                    continue;
                };
                if event.is_cancelled() {
                    sync_queries::delete_event_by_google_id(conn, account_id, calendar_id, id)?;
                    continue;
                }
                let Some(row) = convert::prepare_event(account_id, calendar_id, event) else {
                    continue;
                };
                queries::upsert_event(conn, &row)?;
                written += 1;
            }
            sync_queries::set_calendar_sync_token(
                conn,
                account_id,
                calendar_id,
                sweep.next_sync_token.as_deref(),
                is_primary,
            )?;
            Ok(written)
        })?;
        self.report.add_events(written as i64);
        Ok(written)
    }
}

/// How long stored calendar metadata is trusted before the list is fetched
/// again.
///
/// The list is a dozen rows that move when a person subscribes to something,
/// renames a calendar, or changes its colour — a few times a year, not a few
/// times an hour — so the old behaviour of fetching it on every pass was a
/// request per account per minute buying an answer that had not changed since
/// the last release.
///
/// Six hours rather than a day, because the failure mode of being slow here is
/// visible and irritating: you rename a calendar in Google, come back to Mach,
/// and it is still called the old thing. Six hours means a session that starts
/// in the morning picks up an afternoon change, while the request count falls
/// from ~1,440 a day per account to four.
pub const CALENDAR_LIST_MAX_AGE_MS: i64 = 6 * 60 * 60 * 1000;

/// The calendars to sweep, taken from the store — or `None` when the store is
/// empty or stale and Google must be asked.
///
/// Tombstoned rows are dropped: an unsubscribed calendar keeps its row so its
/// leftover events keep a name, but there is nothing left to sync from it.
fn syncable_from_store(stored: &[Calendar], now: i64) -> Option<Vec<(String, bool)>> {
    let oldest = stored.iter().map(|c| c.synced_at).min()?;
    if now.saturating_sub(oldest) > CALENDAR_LIST_MAX_AGE_MS {
        return None;
    }
    Some(
        stored
            .iter()
            .filter(|c| !c.deleted && !c.calendar_id.is_empty())
            .map(|c| (c.calendar_id.clone(), c.is_primary))
            .collect(),
    )
}

fn syncable_from_list(entries: &[CalendarListEntry]) -> Vec<(String, bool)> {
    entries
        .iter()
        .filter(|c| !c.deleted && !c.id.is_empty())
        .map(|c| (c.id.clone(), c.primary))
        .collect()
}

/// What to do when the metadata fetch failed: carry on with what is stored, or
/// surface the error when there is nothing stored to carry on with.
fn fall_back_to_store<E: Into<SyncError>>(
    stored: &[Calendar],
    error: E,
) -> Result<Vec<(String, bool)>, SyncError> {
    let usable: Vec<(String, bool)> = stored
        .iter()
        .filter(|c| !c.deleted && !c.calendar_id.is_empty())
        .map(|c| (c.calendar_id.clone(), c.is_primary))
        .collect();
    if usable.is_empty() {
        return Err(error.into());
    }
    Ok(usable)
}

/// A `calendarList.list` entry as a row.
///
/// Nothing is interpreted here — not the name, not the colour. `summary` and
/// `summary_override` are stored apart because they answer different questions,
/// and resolving them into one label is a read-time decision that needs the
/// `accounts` row to handle the primary calendar. Storing the resolved string
/// would bake today's answer into the database and lose the ability to change
/// its mind when the account's display name arrives.
fn new_calendar(account_id: i64, entry: &CalendarListEntry, now: i64) -> NewCalendar {
    NewCalendar {
        account_id,
        calendar_id: entry.id.clone(),
        summary: entry.summary.clone(),
        summary_override: entry.summary_override.clone(),
        description: entry.description.clone(),
        time_zone: entry.time_zone.clone(),
        color_id: entry.color_id.clone(),
        background_color: entry.background_color.clone(),
        foreground_color: entry.foreground_color.clone(),
        access_role: entry.access_role.clone(),
        is_primary: entry.primary,
        selected: entry.selected,
        deleted: entry.deleted,
        synced_at: now,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(calendar_id: &str, synced_at: i64, deleted: bool) -> Calendar {
        Calendar {
            id: 1,
            account_id: 1,
            calendar_id: calendar_id.to_string(),
            summary: None,
            summary_override: None,
            description: None,
            time_zone: None,
            color_id: None,
            background_color: None,
            foreground_color: None,
            access_role: None,
            is_primary: calendar_id == "primary",
            selected: true,
            deleted,
            synced_at,
        }
    }

    #[test]
    fn a_fresh_store_answers_without_a_request() {
        let rows = vec![stored("primary", 1_000, false), stored("team", 1_000, false)];
        let fresh = syncable_from_store(&rows, 1_000 + CALENDAR_LIST_MAX_AGE_MS)
            .expect("still inside the window");
        assert_eq!(
            fresh,
            vec![("primary".to_string(), true), ("team".to_string(), false)]
        );
    }

    #[test]
    fn a_stale_store_forces_a_refetch() {
        let rows = vec![stored("primary", 0, false)];
        assert!(syncable_from_store(&rows, CALENDAR_LIST_MAX_AGE_MS + 1).is_none());
    }

    #[test]
    fn an_empty_store_forces_a_fetch_rather_than_syncing_nothing() {
        assert!(syncable_from_store(&[], 0).is_none());
    }

    /// The oldest row decides, so one calendar added by hand at time zero cannot
    /// make the whole account look fresh forever.
    #[test]
    fn staleness_is_measured_from_the_oldest_row() {
        let rows = vec![stored("primary", 0, false), stored("team", 10_000, false)];
        assert!(syncable_from_store(&rows, CALENDAR_LIST_MAX_AGE_MS + 1).is_none());
    }

    #[test]
    fn a_tombstoned_calendar_is_never_swept_again() {
        let rows = vec![stored("primary", 5, false), stored("gone", 5, true)];
        let fresh = syncable_from_store(&rows, 5).expect("fresh");
        assert_eq!(fresh, vec![("primary".to_string(), true)]);
    }

    #[test]
    fn a_failed_fetch_keeps_syncing_what_is_already_known() {
        let rows = vec![stored("primary", 0, false)];
        let carried = fall_back_to_store(&rows, SyncError::Config("boom".into()))
            .expect("stored calendars carry the pass");
        assert_eq!(carried, vec![("primary".to_string(), true)]);
    }

    #[test]
    fn a_failed_fetch_with_nothing_stored_is_a_real_failure() {
        let error = fall_back_to_store(&[], SyncError::Config("boom".into()));
        assert!(error.is_err());
    }
}
