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

pub use crate::db::models::{
    Account, Attachment, ConferenceEntry, Event, EventAttachment, EventConference, EventGuest,
    Label, Message, Participant, RsvpStatus,
};

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

/// What the rail puts a number on.
///
/// Two mailboxes, and only two. Both are totals rather than unread counts,
/// because in neither place does a read state mean anything: a draft is never
/// unread — its existence is the signal — and a snooze is a queue of
/// conversations coming back. Spam and Trash are the counts left out on
/// purpose; 354 of the owner's 384 spam threads are unread, because nobody
/// reads spam, and neither number is something he can act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxCounts {
    pub drafts: i64,
    pub snoozed: i64,
}

// ---------------------------------------------------------------------------
// calendars
// ---------------------------------------------------------------------------

/// A calendar, as the sidebar needs it.
///
/// This is the join of two sources, and it is a join rather than a table read
/// because either side can be missing. `calendars` (migration 6) holds what
/// `calendarList.list` said; `events` holds what has actually been swept. A
/// calendar with metadata and no events is real — an empty calendar is still a
/// calendar — and so is a calendar with events and no metadata, which is every
/// calendar in every database written before migration 6 and every one whose
/// first metadata sweep has not landed yet. `ipc::reads::list_calendars` covers
/// both, so nothing disappears mid-migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    /// Google's calendar id — usually an email address.
    pub id: String,
    pub account_id: i64,
    pub account_email: String,
    /// What to show. `summaryOverride ?? summary`, except on the primary
    /// calendar where Google's `summary` is the account's own email address and
    /// the account's display name is substituted instead — which is what Google
    /// itself does. Falls back to the id when the store knows nothing.
    pub name: String,
    /// The owning account's palette index, so calendars colour with their
    /// account.
    pub colour_index: i64,
    pub event_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The colour the user chose in Google, `#rrggbb`. `None` when unknown —
    /// the UI keeps its own palette for those rather than inventing a colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_id: Option<String>,
    /// `owner`, `writer`, `reader`, `freeBusyReader`.
    ///
    /// Absent means "never fetched", which every reader treats as permissive.
    /// The alternative — defaulting to `reader` — turns every pre-migration row
    /// read-only on the first launch after an upgrade and hands editing back a
    /// minute later, which looks exactly like a bug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    pub primary: bool,
    /// Google's own "is this calendar shown", the initial visibility state.
    pub selected: bool,
    /// Unsubscribed or removed in Google, but still holding events here.
    pub deleted: bool,
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
    /// Emails of every account that has to be authorized again — a refresh
    /// token gone from the Keychain, or a grant too narrow for what was asked.
    pub needs_reauthorization: Vec<String>,
    /// The subset of the above whose credential is alive and whose *grant* is
    /// missing a scope. Mail and calendar keep syncing for these; the account
    /// has to consent again before the scope-gated feature works. Adding a
    /// scope to `oauth::SCOPES` puts every account here at once.
    #[serde(default)]
    pub missing_scope: Vec<String>,
    /// No conversation has ever landed in the store.
    ///
    /// The one fact that separates "this mailbox is empty" from "the first pass
    /// has not filled the store yet". Without it the empty list read a running
    /// sync as proof of the second, and offered a first-sync progress bar to a
    /// store holding sixty-seven thousand messages.
    #[serde(default)]
    pub store_empty: bool,
}

/// What `begin_add_account()` hands back: a URL for the frontend to open and an
/// opaque handle to pass to `complete_add_account`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAuthorizationHandle {
    pub url: String,
    pub pending_id: String,
}
