//! Google Calendar REST client.
//!
//! The load-bearing call is `events.list` with `singleEvents=true`: Google
//! expands recurring series into concrete instances for the requested window,
//! so Mach never has to interpret an RRULE. Incremental sync then rides on
//! `syncToken`, whose expiry (410) is surfaced as a distinct error the way
//! Gmail's expired `historyId` is.

use std::sync::Arc;

use serde_json::json;

use super::types::{
    CalendarListEntry, CalendarListResponse, Event, EventsListResponse, EventsSweep,
    ResponseStatus,
};
use super::{
    GoogleError, HttpMethod, HttpTransport, RestClient, RetryPolicy, Sleeper, TokenProvider,
    CALENDAR_BASE_URL,
};

#[derive(Debug, Clone, Default)]
pub struct EventsListQuery {
    pub single_events: Option<bool>,
    pub time_min: Option<String>,
    pub time_max: Option<String>,
    pub updated_min: Option<String>,
    pub order_by: Option<String>,
    pub max_results: Option<u32>,
    pub show_deleted: Option<bool>,
    pub q: Option<String>,
    pub time_zone: Option<String>,
    /// When set, Google returns only what changed since the token was issued.
    /// Mutually exclusive with the time window — see [`EventsListQuery::apply`].
    pub sync_token: Option<String>,
}

impl EventsListQuery {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` makes Google expand recurring events into individual instances
    /// covering `time_min..time_max`. This is what Mach wants everywhere.
    pub fn single_events(mut self, yes: bool) -> Self {
        self.single_events = Some(yes);
        self
    }

    /// Inclusive lower bound, RFC3339 with an offset.
    pub fn time_min(mut self, value: impl Into<String>) -> Self {
        self.time_min = Some(value.into());
        self
    }

    /// Exclusive upper bound, RFC3339 with an offset.
    pub fn time_max(mut self, value: impl Into<String>) -> Self {
        self.time_max = Some(value.into());
        self
    }

    pub fn updated_min(mut self, value: impl Into<String>) -> Self {
        self.updated_min = Some(value.into());
        self
    }

    /// `startTime` (only legal with `singleEvents=true`) or `updated`.
    pub fn order_by(mut self, value: impl Into<String>) -> Self {
        self.order_by = Some(value.into());
        self
    }

    pub fn max_results(mut self, n: u32) -> Self {
        self.max_results = Some(n);
        self
    }

    /// Include cancelled instances — required for incremental sync, otherwise
    /// deletions are invisible.
    pub fn show_deleted(mut self, yes: bool) -> Self {
        self.show_deleted = Some(yes);
        self
    }

    pub fn q(mut self, value: impl Into<String>) -> Self {
        self.q = Some(value.into());
        self
    }

    pub fn time_zone(mut self, value: impl Into<String>) -> Self {
        self.time_zone = Some(value.into());
        self
    }

    pub fn sync_token(mut self, value: impl Into<String>) -> Self {
        self.sync_token = Some(value.into());
        self
    }

    /// Google rejects a request that combines `syncToken` with any filter that
    /// would change the result set, so an incremental call drops the window,
    /// the ordering and the text query rather than 400-ing.
    fn apply(&self, url: &mut url::Url) {
        let incremental = self.sync_token.is_some();
        let mut pairs = url.query_pairs_mut();
        if let Some(token) = &self.sync_token {
            pairs.append_pair("syncToken", token);
        }
        if let Some(b) = self.single_events {
            pairs.append_pair("singleEvents", if b { "true" } else { "false" });
        }
        if !incremental {
            if let Some(v) = &self.time_min {
                pairs.append_pair("timeMin", v);
            }
            if let Some(v) = &self.time_max {
                pairs.append_pair("timeMax", v);
            }
            if let Some(v) = &self.updated_min {
                pairs.append_pair("updatedMin", v);
            }
            if let Some(v) = &self.order_by {
                pairs.append_pair("orderBy", v);
            }
            if let Some(v) = &self.q {
                pairs.append_pair("q", v);
            }
        }
        if let Some(n) = self.max_results {
            pairs.append_pair("maxResults", &n.to_string());
        }
        if let Some(b) = self.show_deleted {
            pairs.append_pair("showDeleted", if b { "true" } else { "false" });
        }
        if let Some(v) = &self.time_zone {
            pairs.append_pair("timeZone", v);
        }
    }
}

/// Google Calendar API client for one account.
#[derive(Clone)]
pub struct CalendarClient {
    rest: RestClient,
}

impl CalendarClient {
    pub fn new(transport: Arc<dyn HttpTransport>, tokens: Arc<dyn TokenProvider>) -> Self {
        Self {
            rest: RestClient::new(transport, tokens, CALENDAR_BASE_URL),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.rest = self.rest.with_base_url(base_url);
        self
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.rest = self.rest.with_retry_policy(retry);
        self
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.rest = self.rest.with_sleeper(sleeper);
        self
    }

    pub fn base_url(&self) -> &str {
        self.rest.base_url()
    }

    // -------------------------------------------------------- calendar list

    /// `calendarList.list`, all pages.
    pub async fn calendar_list(&self) -> Result<Vec<CalendarListEntry>, GoogleError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut url = self.rest.endpoint(&["users", "me", "calendarList"])?;
            if let Some(t) = &token {
                url.query_pairs_mut().append_pair("pageToken", t);
            }
            let page: CalendarListResponse = self.rest.send_json(HttpMethod::Get, url, None).await?;
            out.extend(page.items);
            match page.next_page_token.filter(|t| !t.is_empty()) {
                Some(next) => token = Some(next),
                None => return Ok(out),
            }
        }
    }

    // --------------------------------------------------------------- events

    /// `events.list`, one page.
    ///
    /// A 410 means the `syncToken` has expired: drop it and re-list the whole
    /// window. That surfaces as [`GoogleError::SyncTokenExpired`], never a
    /// generic HTTP error.
    pub async fn events_list_page(
        &self,
        calendar_id: &str,
        query: &EventsListQuery,
        page_token: Option<&str>,
    ) -> Result<EventsListResponse, GoogleError> {
        let mut url = self
            .rest
            .endpoint(&["calendars", calendar_id, "events"])?;
        query.apply(&mut url);
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        self.rest
            .send_json(HttpMethod::Get, url, None)
            .await
            .map_err(gone_means_sync_token_expired)
    }

    /// `events.list`, following `nextPageToken` to the end. The returned
    /// `next_sync_token` is what to store for the next incremental sync — it
    /// only appears on the final page.
    pub async fn events_list_all(
        &self,
        calendar_id: &str,
        query: &EventsListQuery,
    ) -> Result<EventsSweep, GoogleError> {
        let mut sweep = EventsSweep::default();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .events_list_page(calendar_id, query, token.as_deref())
                .await?;
            if page.next_sync_token.is_some() {
                sweep.next_sync_token = page.next_sync_token.clone();
            }
            if page.time_zone.is_some() {
                sweep.time_zone = page.time_zone.clone();
            }
            sweep.events.extend(page.items);
            match page.next_page_token.filter(|t| !t.is_empty()) {
                Some(next) => token = Some(next),
                None => return Ok(sweep),
            }
        }
    }

    /// `events.get`.
    pub async fn events_get(
        &self,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<Event, GoogleError> {
        let url = self
            .rest
            .endpoint(&["calendars", calendar_id, "events", event_id])?;
        self.rest.send_json(HttpMethod::Get, url, None).await
    }

    /// `events.insert`. Unset fields are omitted rather than sent as null.
    pub async fn events_insert(
        &self,
        calendar_id: &str,
        event: &Event,
    ) -> Result<Event, GoogleError> {
        let url = self
            .rest
            .endpoint(&["calendars", calendar_id, "events"])?;
        let body = serde_json::to_vec(event).map_err(|e| GoogleError::InvalidRequest {
            message: format!("could not serialize event: {e}"),
        })?;
        self.rest.send_json(HttpMethod::Post, url, Some(body)).await
    }

    /// `events.patch` — a genuine partial update, so callers pass only the
    /// fields they mean to change.
    pub async fn events_patch(
        &self,
        calendar_id: &str,
        event_id: &str,
        patch: &serde_json::Value,
    ) -> Result<Event, GoogleError> {
        let url = self
            .rest
            .endpoint(&["calendars", calendar_id, "events", event_id])?;
        let body = serde_json::to_vec(patch).map_err(|e| GoogleError::InvalidRequest {
            message: format!("could not serialize patch: {e}"),
        })?;
        self.rest
            .send_json(HttpMethod::Patch, url, Some(body))
            .await
    }

    /// `events.delete`. Answers 204 with an empty body, which is why this does
    /// not try to deserialize anything.
    pub async fn events_delete(
        &self,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<(), GoogleError> {
        let url = self
            .rest
            .endpoint(&["calendars", calendar_id, "events", event_id])?;
        self.rest.send_empty(HttpMethod::Delete, url, None).await
    }

    /// RSVP on behalf of a named attendee.
    ///
    /// Google has no "set my response" endpoint: patching `attendees` replaces
    /// the whole array, so this reads the event, changes exactly one row, and
    /// writes the full list back. Nothing else about the event is touched.
    ///
    /// An address that is not on the invitation is an error rather than a
    /// silent no-op — RSVPing to the wrong event should be loud.
    pub async fn events_rsvp(
        &self,
        calendar_id: &str,
        event_id: &str,
        attendee_email: &str,
        response: ResponseStatus,
    ) -> Result<Event, GoogleError> {
        self.rsvp_matching(calendar_id, event_id, response, |a| {
            a.email
                .as_deref()
                .map(|e| e.eq_ignore_ascii_case(attendee_email))
                .unwrap_or(false)
        })
        .await
        .map_err(|e| match e {
            GoogleError::InvalidRequest { .. } => GoogleError::InvalidRequest {
                message: format!(
                    "{attendee_email} is not an attendee of event {event_id}; nothing to RSVP"
                ),
            },
            other => other,
        })
    }

    /// RSVP as whichever attendee Google flagged `self` — for when the caller
    /// has the event but not the account's address.
    pub async fn events_rsvp_as_self(
        &self,
        calendar_id: &str,
        event_id: &str,
        response: ResponseStatus,
    ) -> Result<Event, GoogleError> {
        self.rsvp_matching(calendar_id, event_id, response, |a| a.is_self)
            .await
    }

    async fn rsvp_matching(
        &self,
        calendar_id: &str,
        event_id: &str,
        response: ResponseStatus,
        matches: impl Fn(&super::types::EventAttendee) -> bool,
    ) -> Result<Event, GoogleError> {
        let event = self.events_get(calendar_id, event_id).await?;
        let mut attendees = event.attendees.clone();

        match attendees.iter_mut().find(|a| matches(a)) {
            Some(attendee) => attendee.response_status = Some(response.as_str().to_string()),
            None => {
                return Err(GoogleError::InvalidRequest {
                    message: format!("no matching attendee on event {event_id}; nothing to RSVP"),
                })
            }
        }

        self.events_patch(calendar_id, event_id, &json!({ "attendees": attendees }))
            .await
    }
}

/// `events.list` answers 410 when a stored `syncToken` has aged out. Some
/// responses carry the `fullSyncRequired` reason and some do not, so the status
/// alone is treated as authoritative here.
fn gone_means_sync_token_expired(error: GoogleError) -> GoogleError {
    match error {
        GoogleError::Api {
            status: 410,
            message,
            ..
        } => GoogleError::SyncTokenExpired { message },
        other => other,
    }
}
