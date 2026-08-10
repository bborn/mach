//! The invoke surface, and nothing else.
//!
//! Every handler here is a wrapper: it unpacks `tauri::State`, calls a plain
//! function, and emits an event if something changed. The reason is testability
//! — a `#[tauri::command]` can only really be driven by standing up an
//! application, so no decision is allowed to live inside one. The logic is in
//! [`super::reads`] and [`super::state`], where `tests/ipc.rs` can reach it.
//!
//! Argument names are snake_case here and camelCase on the wire; Tauri does that
//! conversion, which is why `threadId` from TypeScript lands in `thread_id`.

use tauri::{AppHandle, State};

use crate::commands::{Command, CommandResult, CommandSpec};
use crate::db::models::{Account, Event, Label, ThreadCursor};
use crate::db::queries::{SearchNode, SearchRequest};

use super::error::IpcError;
use super::events;
use super::reads;
use super::state::{self, AppState};
use super::types::{
    Calendar, PendingAuthorizationHandle, SyncStatusPayload, ThreadDetail, ThreadPage, ThreadQuery,
};

// ---------------------------------------------------------------------------
// reads
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_accounts(state: State<'_, AppState>) -> Result<Vec<Account>, IpcError> {
    reads::list_accounts(&state.db)
}

#[tauri::command]
pub fn list_labels(
    state: State<'_, AppState>,
    account_id: Option<i64>,
) -> Result<Vec<Label>, IpcError> {
    reads::list_labels(&state.db, account_id)
}

#[tauri::command]
pub fn list_threads(
    state: State<'_, AppState>,
    query: ThreadQuery,
) -> Result<ThreadPage, IpcError> {
    reads::list_threads(&state.db, &query)
}

#[tauri::command]
pub fn get_thread(state: State<'_, AppState>, thread_id: i64) -> Result<ThreadDetail, IpcError> {
    reads::get_thread(&state.db, thread_id)
}

/// Search.
///
/// Two shapes through one command, because they are one feature. With no
/// `filter` this is what it always was: the raw text, ranked by bm25, which is
/// what ⌘K wants. With a `filter` — the AST the frontend parsed out of the same
/// text — it is the operator search behind the search view, ordered newest
/// first and keyset-paginated like the mailbox.
///
/// Additive on purpose: the extra arguments are all optional, so an older
/// caller (and every existing test) still compiles and still gets the ranked
/// answer it asked for.
#[tauri::command]
pub fn search_threads(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
    filter: Option<SearchNode>,
    account_id: Option<i64>,
    cursor: Option<ThreadCursor>,
) -> Result<ThreadPage, IpcError> {
    match filter {
        Some(filter) => reads::search_threads_filtered(
            &state.db,
            &filter,
            &SearchRequest {
                account_id,
                limit: limit.unwrap_or(0),
                after: cursor,
            },
        ),
        None => reads::search_threads(&state.db, &query, limit),
    }
}

#[tauri::command]
pub fn list_calendars(state: State<'_, AppState>) -> Result<Vec<Calendar>, IpcError> {
    reads::list_calendars(&state.db)
}

#[tauri::command]
pub fn list_events(
    state: State<'_, AppState>,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Event>, IpcError> {
    reads::list_events(&state.db, start_ms, end_ms)
}

// ---------------------------------------------------------------------------
// writes
// ---------------------------------------------------------------------------

/// Run a command.
///
/// `source` is `user` (the default), `agent`, or `plugin:<id>`, and it is not
/// decoration: a plugin source is **checked against that plugin's declared
/// capabilities and rate limit before the command runs**. The frontend's sandbox
/// host refuses an undeclared command first, and with a better message — but the
/// frontend is not the trust boundary. The command layer is, so the grant is
/// enforced here too. It costs one map lookup on a path that is about to make a
/// network round trip.
#[tauri::command]
pub async fn execute_command(
    app: AppHandle,
    state: State<'_, AppState>,
    command: Command,
    source: Option<String>,
) -> Result<CommandResult, IpcError> {
    if let Some(plugin_id) = source.as_deref().and_then(|s| s.strip_prefix("plugin:")) {
        state
            .plugins
            .authorize_command(plugin_id, command.kind(), crate::ipc::compose::now_ms())
            .map_err(|message| {
                IpcError::Command(crate::commands::CommandError::Invalid { message })
            })?;
    }
    let result = state.dispatcher.execute(command).await?;
    // Even a partial failure has already written and rolled back rows, so the
    // list is stale either way.
    events::emit_threads_changed(&app);
    Ok(result)
}

#[tauri::command]
pub fn command_catalogue() -> Vec<CommandSpec> {
    Command::catalogue().to_vec()
}

// ---------------------------------------------------------------------------
// sync
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn sync_status(state: State<'_, AppState>) -> SyncStatusPayload {
    state.status_payload()
}

#[tauri::command]
pub async fn sync_now(state: State<'_, AppState>) -> Result<(), IpcError> {
    state.client_config()?;
    // `start` is idempotent; calling it here is what makes "Sync now" work
    // immediately after the very first account is added, when the loop was
    // never started at boot.
    state.sync.start();
    state.sync.sync_now();
    Ok(())
}

// ---------------------------------------------------------------------------
// accounts
// ---------------------------------------------------------------------------

/// Start a sign-in. `email` names the account being repaired, if any.
///
/// With an address it becomes Google's `login_hint`, so the consent page opens
/// on the account the user pointed at rather than on whichever one the browser
/// happens to be signed into — and `complete_add_account` holds the same
/// address against what comes back.
#[tauri::command]
pub fn begin_add_account(
    state: State<'_, AppState>,
    email: Option<String>,
) -> Result<PendingAuthorizationHandle, IpcError> {
    let config = state.client_config()?;
    let pending = crate::auth::flow::begin_authorization(config, email.as_deref())?;
    let url = pending.url.clone();
    let pending_id = state.store_pending(state::Handshake {
        authorization: pending,
        email,
    })?;
    Ok(PendingAuthorizationHandle { url, pending_id })
}

#[tauri::command]
pub async fn complete_add_account(
    app: AppHandle,
    state: State<'_, AppState>,
    pending_id: String,
) -> Result<Account, IpcError> {
    let config = state.client_config()?.clone();
    let handshake = state.take_pending(&pending_id)?;
    let expected = handshake.email.clone();

    let authorized = state::await_authorization(handshake.authorization, config).await?;

    // A sign-in started from one row must not connect a different account. The
    // `login_hint` is a suggestion Google is free to ignore, and the browser may
    // already be holding a different session, so the identity is checked rather
    // than assumed. Nothing has been written to the store yet; the refresh token
    // the exchange saved is the only trace, and it goes too unless it belongs to
    // an account Mach already has.
    if let Some(expected) = expected {
        if !expected.eq_ignore_ascii_case(&authorized.email) {
            let known = reads::account_by_email(&state.db, &authorized.email)?.is_some();
            if !known {
                if let Some(tokens) = state.tokens() {
                    let _ = tokens.sign_out(&authorized.email);
                }
            }
            return Err(IpcError::WrongAccount {
                expected,
                got: authorized.email,
            });
        }
    }

    let account = state::persist_account(&state.db, &authorized.email)?;

    // Seed the access token so the first sync does not have to spend a refresh
    // round trip on a credential we are already holding.
    if let Some(tokens) = state.tokens() {
        tokens.insert_tokens(&authorized.email, authorized.tokens);
    }
    state.mark_reauthorized(&authorized.email);

    state.sync.start();
    state.sync.sync_now();

    events::emit_threads_changed(&app);
    events::emit_sync_status(&app, &state.status_payload());
    Ok(account)
}

#[tauri::command]
pub fn remove_account(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: i64,
) -> Result<(), IpcError> {
    let account = reads::account(&state.db, account_id)?;
    // The row goes first: cascades take its threads, messages and events with
    // it, so the UI has nothing left to render for the account even if the
    // Keychain refuses below.
    state.db.write(|conn| {
        crate::db::queries::delete_account(conn, account_id)
    })?;

    if let Some(tokens) = state.tokens() {
        // A Keychain entry we cannot delete is worth neither failing the
        // removal nor leaving the account visible; the credential is now
        // unreferenced either way.
        let _ = tokens.sign_out(&account.email);
    }
    state.mark_reauthorized(&account.email);

    events::emit_threads_changed(&app);
    events::emit_sync_status(&app, &state.status_payload());
    Ok(())
}
