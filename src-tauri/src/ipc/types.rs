//! The payload shapes React sees.
//!
//! Every type here is `camelCase` on the wire and every timestamp is unix
//! milliseconds as a number, because these land in TypeScript with no adapter
//! in between. `tests/ipc.rs` asserts the serialized key names directly — a
//! typo here would silently break the whole frontend and compile fine.
//!
//! Most of the surface is [`crate::db::models`] re-exported: those rows are
//! already the right shape and re-wrapping them would only add a place for the
//! two to drift. What is defined here is the handful of things the store has no
//! type for — a page, a calendar, the app's own health.

use serde::{Deserialize, Serialize};

use crate::db::models::{ThreadCursor, ThreadSummary, ThreadWithMessages};
use crate::sync::AccountStatus;

pub use crate::db::models::{Account, Attachment, Event, Label, Message, Participant, RsvpStatus};

/// What `get_thread` returns: the summary row plus its whole conversation.
pub type ThreadDetail = ThreadWithMessages;

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

/// What the list view asks for.
///
/// `cursor` is the keyset resume point from the previous page, never an offset —
/// see [`ThreadCursor`] for why. `after` is accepted as an alias because that is
/// what the store's own query type calls the same field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadQuery {
    /// `None` = the unified stream across every account.
    pub account_id: Option<i64>,
    /// A Gmail label id, e.g. `INBOX` or `Label_12`.
    pub label_id: Option<String>,
    pub unread_only: bool,
    /// `None` or `0` means the default page size.
    pub limit: Option<u32>,
    #[serde(alias = "after")]
    pub cursor: Option<ThreadCursor>,
}

impl ThreadQuery {
    /// The page size this query actually resolves to, clamped the same way the
    /// store clamps it. Needed here because whether a next cursor exists is
    /// decided by comparing the row count against it.
    pub fn effective_limit(&self) -> u32 {
        match self.limit.unwrap_or(0) {
            0 => crate::db::queries::DEFAULT_PAGE_SIZE,
            n => n.min(crate::db::queries::MAX_PAGE_SIZE),
        }
    }

    /// The store's query type.
    pub fn to_store_query(&self) -> crate::db::models::ThreadQuery {
        crate::db::models::ThreadQuery {
            account_id: self.account_id,
            label_id: self.label_id.clone(),
            unread_only: self.unread_only,
            limit: self.limit.unwrap_or(0),
            after: self.cursor,
        }
    }
}

/// One page of the stream.
///
/// `nextCursor` is `null` at the end of the list. It is derived from the last
/// row rather than counted, so a page that arrives while the sync loop is
/// inserting above it still resumes in exactly the right place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPage {
    pub items: Vec<ThreadSummary>,
    pub next_cursor: Option<ThreadCursor>,
}

impl ThreadPage {
    /// A page whose cursor is present only when the page came back full — a
    /// short page is proof there is nothing after it.
    pub fn paged(items: Vec<ThreadSummary>, limit: u32) -> Self {
        let next_cursor = (items.len() as u32 >= limit)
            .then(|| items.last().map(ThreadSummary::cursor))
            .flatten();
        ThreadPage { items, next_cursor }
    }

    /// A page that is the whole answer — search results, which are ranked
    /// rather than ordered and therefore have no keyset resume point.
    pub fn complete(items: Vec<ThreadSummary>) -> Self {
        ThreadPage {
            items,
            next_cursor: None,
        }
    }
}

// ---------------------------------------------------------------------------
// calendars
// ---------------------------------------------------------------------------

/// A calendar the local store has events from.
///
/// There is no `calendars` table: the sync engine writes events keyed by
/// `(account_id, calendar_id)` and never persists the calendar list itself. So
/// this is derived from the events on hand, which has the useful property that
/// the UI is only ever offered calendars it can actually draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    /// Google's calendar id — usually an email address.
    pub id: String,
    pub account_id: i64,
    pub account_email: String,
    /// Display name. Google's own name is not stored, so this is the id.
    pub name: String,
    /// The owning account's palette index, so calendars colour with their
    /// account.
    pub colour_index: i64,
    pub event_count: i64,
}

// ---------------------------------------------------------------------------
// health
// ---------------------------------------------------------------------------

/// What `sync_status()` returns and what the `sync-status` event carries.
///
/// The sync engine's own [`crate::sync::SyncStatus`] answers "what is the loop
/// doing"; this adds the two questions the loop cannot answer — whether the app
/// is configured at all, and which accounts have lost their Keychain entry and
/// need the user to sign in again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatusPayload {
    pub running: bool,
    pub accounts: Vec<AccountStatus>,
    pub last_pass_started_at: Option<i64>,
    pub last_pass_finished_at: Option<i64>,
    /// False when the OAuth client is missing. Everything local still works.
    pub configured: bool,
    /// Set exactly when `configured` is false — a sentence to render.
    pub configuration_error: Option<String>,
    /// Emails of accounts whose refresh token is gone from the Keychain.
    pub needs_reauthorization: Vec<String>,
}

/// What `begin_add_account()` hands back: a URL for the frontend to open and an
/// opaque handle to pass to `complete_add_account`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAuthorizationHandle {
    pub url: String,
    pub pending_id: String,
}
