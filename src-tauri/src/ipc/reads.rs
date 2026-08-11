//! The read half of the IPC surface, as plain functions.
//!
//! Everything here takes `&Db` and returns a serializable value. Nothing takes
//! `tauri::State`, nothing touches an `AppHandle`, nothing is `#[tauri::command]`.
//! That is deliberate: a Tauri command handler can only really be exercised by
//! standing up an application, so the handlers in [`super::commands`] are thin
//! wrappers and *this* is what `tests/ipc.rs` drives.
//!
//! None of these functions reimplement a query. Keyset pagination and FTS
//! ranking already live in [`crate::db::queries`]; this module's whole job is to
//! shape the result into a payload and turn "not there" into a typed error.

use rusqlite::Connection;

use crate::db::models::{Account, Calendar as StoredCalendar, Contact, Event, Label};
use crate::db::{queries, Db};

use super::error::IpcError;
use super::types::{Calendar, ThreadDetail, ThreadPage, ThreadQuery};

/// Every authorized account, in rail order.
pub fn list_accounts(db: &Db) -> Result<Vec<Account>, IpcError> {
    Ok(db.read(queries::list_accounts)?)
}

/// The address book: every address the store has ever seen, best first.
///
/// A scan over every message, so it is a read the UI starts and then forgets
/// about — see `useContacts` on the frontend. It is never on the path of a
/// keystroke.
pub fn list_contacts(db: &Db) -> Result<Vec<Contact>, IpcError> {
    Ok(db.read(|conn| queries::address_book(conn, queries::MAX_CONTACTS))?)
}

/// One account by row id, as a typed error when it is not there.
pub fn account(db: &Db, account_id: i64) -> Result<Account, IpcError> {
    let found = db.read(|conn| find_account(conn, account_id))?;
    found.ok_or_else(|| IpcError::not_found("account", account_id))
}

/// One account by address, or `None`. Absence is an answer here, not an error.
pub fn account_by_email(db: &Db, email: &str) -> Result<Option<Account>, IpcError> {
    Ok(db.read(|conn| queries::account_by_email(conn, email))?)
}

fn find_account(conn: &Connection, account_id: i64) -> crate::db::Result<Option<Account>> {
    Ok(queries::list_accounts(conn)?
        .into_iter()
        .find(|a| a.id == account_id))
}

/// Labels for one account, or every account's labels when `account_id` is
/// `None`.
///
/// An unknown account id is an error rather than an empty list: an empty list
/// is a real answer (a freshly added account before its first sync) and
/// conflating the two would hide a frontend bug behind an empty rail.
pub fn list_labels(db: &Db, account_id: Option<i64>) -> Result<Vec<Label>, IpcError> {
    if let Some(id) = account_id {
        account(db, id)?;
    }
    Ok(db.read(|conn| queries::list_labels(conn, account_id))?)
}

/// One page of the unified stream.
pub fn list_threads(db: &Db, query: &ThreadQuery) -> Result<ThreadPage, IpcError> {
    let limit = query.effective_limit();
    let store_query = query.to_store_query();
    let items = db.read(|conn| queries::list_threads(conn, &store_query))?;
    Ok(ThreadPage::paged(items, limit))
}

/// A thread and its whole conversation.
pub fn get_thread(db: &Db, thread_id: i64) -> Result<ThreadDetail, IpcError> {
    db.read(|conn| queries::thread_with_messages(conn, thread_id))?
        .ok_or_else(|| IpcError::not_found("thread", thread_id))
}

/// Local full-text search, already collapsed to threads and ranked.
///
/// A blank query is not an error — it is an empty result, which is what the
/// palette wants while the user is still deciding what to type.
pub fn search_threads(db: &Db, query: &str, limit: Option<u32>) -> Result<ThreadPage, IpcError> {
    let limit = match limit.unwrap_or(0) {
        0 => queries::DEFAULT_PAGE_SIZE,
        n => n.min(queries::MAX_PAGE_SIZE),
    };
    let items = db.read(|conn| queries::search_thread_summaries(conn, query, limit))?;
    Ok(ThreadPage::complete(items))
}

/// The same box, once it has operators in it.
///
/// `search_threads` above is the ⌘K path: a bag of words, ranked by bm25, six
/// rows deep. This is the search *view*: a parsed query compiled to SQL, in the
/// same newest-first order as the mailbox next to it, and paginated with the
/// same keyset cursor — which is why it returns a `paged` page rather than a
/// `complete` one. The parse arrives from the frontend; see
/// `db::queries::compile_search` for why the AST rather than the text crosses
/// the seam.
pub fn search_threads_filtered(
    db: &Db,
    filter: &queries::SearchNode,
    request: &queries::SearchRequest,
) -> Result<ThreadPage, IpcError> {
    let limit = match request.limit {
        0 => queries::DEFAULT_PAGE_SIZE,
        n => n.min(queries::MAX_PAGE_SIZE),
    };
    let request = queries::SearchRequest {
        limit,
        ..request.clone()
    };
    let items = db.read(|conn| queries::search_threads_filtered(conn, filter, &request))?;
    Ok(ThreadPage::paged(items, limit))
}

/// Every calendar worth showing, named.
///
/// Three things are being reconciled here, and each one is a case that used to
/// be wrong:
///
///  1. **Metadata leads.** Stored `calendarList.list` rows are the answer when
///     there are any, which is what turns `en.usa#holiday@group.v.calendar.
///     google.com` into "Holidays in United States".
///  2. **Events are the fallback.** A calendar with events but no metadata row —
///     any database written before migration 6, or one whose first metadata
///     sweep has not landed — still appears, still named by its id. The list
///     never shrinks on upgrade; it only gets better names.
///  3. **A tombstone survives its calendar.** An unsubscribed or deleted
///     calendar is dropped once its events are gone, and kept while they are
///     still on the grid. Dropping it the moment Google stopped listing it
///     would put its blocks back to being drawn under a raw group address.
pub fn list_calendars(db: &Db) -> Result<Vec<Calendar>, IpcError> {
    Ok(db.read(|conn| {
        let accounts = queries::list_accounts(conn)?;
        let stored = queries::list_calendars(conn, None)?;
        let counts = queries::event_counts_by_calendar(conn)?;
        Ok(merge_calendars(&accounts, &stored, &counts))
    })?)
}

/// The pure half of [`list_calendars`], so the reconciliation above can be
/// tested without three tables and a sync loop.
fn merge_calendars(
    accounts: &[Account],
    stored: &[StoredCalendar],
    counts: &[(i64, String, i64)],
) -> Vec<Calendar> {
    let mut out: Vec<Calendar> = Vec::new();
    let count_of = |account_id: i64, calendar_id: &str| -> i64 {
        counts
            .iter()
            .find(|(a, c, _)| *a == account_id && c == calendar_id)
            .map(|(_, _, n)| *n)
            .unwrap_or(0)
    };

    for account in accounts {
        for row in stored.iter().filter(|c| c.account_id == account.id) {
            let event_count = count_of(account.id, &row.calendar_id);
            // A tombstone with nothing left to name has done its job.
            if row.deleted && event_count == 0 {
                continue;
            }
            out.push(Calendar {
                name: display_name(row, account),
                id: row.calendar_id.clone(),
                account_id: account.id,
                account_email: account.email.clone(),
                colour_index: account.colour_index,
                event_count,
                description: row.description.clone(),
                background_color: row.background_color.clone(),
                foreground_color: row.foreground_color.clone(),
                color_id: row.color_id.clone(),
                access_role: row.access_role.clone(),
                time_zone: row.time_zone.clone(),
                primary: row.is_primary,
                selected: row.selected,
                deleted: row.deleted,
            });
        }

        // Then whatever the events know about and the metadata does not.
        let mut derived: Vec<&(i64, String, i64)> = counts
            .iter()
            .filter(|(a, c, _)| {
                *a == account.id
                    && !stored
                        .iter()
                        .any(|row| row.account_id == account.id && &row.calendar_id == c)
            })
            .collect();
        derived.sort_by(|a, b| a.1.cmp(&b.1));
        for (_, calendar_id, event_count) in derived {
            out.push(Calendar {
                name: calendar_id.clone(),
                id: calendar_id.clone(),
                account_id: account.id,
                account_email: account.email.clone(),
                colour_index: account.colour_index,
                event_count: *event_count,
                description: None,
                background_color: None,
                foreground_color: None,
                color_id: None,
                // Silence, not a denial: these rows predate the metadata sweep.
                access_role: None,
                time_zone: None,
                primary: false,
                selected: true,
                deleted: false,
            });
        }
    }
    out
}

/// The label for one calendar.
///
/// `summaryOverride ?? summary` is the general rule, and the primary calendar is
/// the exception that makes it look wrong: Google sends the account's own email
/// address as the `summary` of the calendar it labels "Bruno Bornsztein" in its
/// own UI, substituting the account holder's name at render time. Doing anything
/// else here means a sidebar that lists five email addresses and calls them
/// calendars — which is what it did.
///
/// The substitution is deliberately *not* applied when the user has set an
/// override: renaming your own primary calendar is a thing Google lets you do,
/// and honouring the rename is the whole reason the override column exists.
fn display_name(row: &StoredCalendar, account: &Account) -> String {
    if let Some(override_name) = row
        .summary_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return override_name.to_string();
    }
    if row.is_primary {
        return account
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&account.email)
            .to_string();
    }
    row.title().unwrap_or(&row.calendar_id).to_string()
}

/// Every event overlapping `[start_ms, end_ms)`, across every account.
pub fn list_events(db: &Db, start_ms: i64, end_ms: i64) -> Result<Vec<Event>, IpcError> {
    Ok(db.read(|conn| queries::events_in_range(conn, start_ms, end_ms, None))?)
}
