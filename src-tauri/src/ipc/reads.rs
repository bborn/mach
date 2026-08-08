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

use crate::db::models::{Account, Event, Label};
use crate::db::{queries, Db};

use super::error::IpcError;
use super::types::{Calendar, ThreadDetail, ThreadPage, ThreadQuery};

/// Every authorized account, in rail order.
pub fn list_accounts(db: &Db) -> Result<Vec<Account>, IpcError> {
    Ok(db.read(queries::list_accounts)?)
}

/// One account by row id, as a typed error when it is not there.
pub fn account(db: &Db, account_id: i64) -> Result<Account, IpcError> {
    let found = db.read(|conn| find_account(conn, account_id))?;
    found.ok_or_else(|| IpcError::not_found("account", account_id))
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

/// Every calendar the store holds events for. See [`Calendar`] for why this is
/// derived rather than stored.
pub fn list_calendars(db: &Db) -> Result<Vec<Calendar>, IpcError> {
    Ok(db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT e.calendar_id, e.account_id, a.email, a.colour_index, count(*)
             FROM events e JOIN accounts a ON a.id = e.account_id
             GROUP BY e.account_id, e.calendar_id
             ORDER BY a.colour_index, a.id, e.calendar_id",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            Ok(Calendar {
                name: id.clone(),
                id,
                account_id: row.get(1)?,
                account_email: row.get(2)?,
                colour_index: row.get(3)?,
                event_count: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?)
}

/// Every event overlapping `[start_ms, end_ms)`, across every account.
pub fn list_events(db: &Db, start_ms: i64, end_ms: i64) -> Result<Vec<Event>, IpcError> {
    Ok(db.read(|conn| queries::events_in_range(conn, start_ms, end_ms, None))?)
}
