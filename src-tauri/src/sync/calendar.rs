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

use crate::db::{queries, sync_queries, Db};
use crate::google::calendar::{CalendarClient, EventsListQuery};
use crate::google::types::EventsSweep;

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

    /// Which calendars to sync. An explicit list in the config wins; otherwise
    /// ask Google, skipping the ones it has already deleted.
    async fn calendars(&self) -> Result<Vec<(String, bool)>, SyncError> {
        if !self.config.calendar_ids.is_empty() {
            return Ok(self
                .config
                .calendar_ids
                .iter()
                .map(|id| (id.clone(), id == "primary"))
                .collect());
        }
        let entries = self.calendar.calendar_list().await?;
        Ok(entries
            .into_iter()
            .filter(|c| !c.deleted && !c.id.is_empty())
            .map(|c| (c.id, c.primary))
            .collect())
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
