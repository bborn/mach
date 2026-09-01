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

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::commands::{AccountFilter, Command, CommandError, CommandResult, CommandSpec};
use crate::db::models::{Account, Contact, Event, Label, ThreadCursor};
use crate::db::queries::{SearchNode, SearchRequest};
use crate::google::types::{Filter, FilterAction, FilterCriteria};
use crate::sync::{ForcedPass, SyncScope};

use super::error::IpcError;
use super::events;
use super::reads;
use super::state::{self, AppState};
use super::types::{
    Calendar, MailboxCounts, PendingAuthorizationHandle, SyncStatusPayload, ThreadDetail,
    ThreadPage, ThreadQuery,
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

/// Everyone you have corresponded with, for the address fields to complete.
#[tauri::command]
pub fn list_contacts(state: State<'_, AppState>) -> Result<Vec<Contact>, IpcError> {
    reads::list_contacts(&state.db)
}

#[tauri::command]
pub fn list_threads(
    state: State<'_, AppState>,
    query: ThreadQuery,
) -> Result<ThreadPage, IpcError> {
    reads::list_threads(&state.db, &query)
}

/// Drafts and Snoozed, as numbers for the rail.
#[tauri::command]
pub fn mailbox_counts(state: State<'_, AppState>) -> Result<MailboxCounts, IpcError> {
    reads::mailbox_counts(&state.db)
}

#[tauri::command]
pub fn get_thread(state: State<'_, AppState>, thread_id: i64) -> Result<ThreadDetail, IpcError> {
    reads::get_thread(&state.db, thread_id)
}

/// The unsubscribe URL for a message, looked up again from the store.
///
/// Split out of the command below for the reason every other handler in this
/// file is a wrapper: a `#[tauri::command]` can only be driven by standing up
/// an application, so the decision — which verdicts have a page, and which do
/// not — lives here where `tests/unsub.rs` can reach it.
///
/// Every kind with a URL is returned, not only `Link`. A sender who supports
/// one-click also has a page behind the same URL, and this is the fallback
/// offered when the POST fails; refusing it there because the header said
/// one-click would leave him with nowhere to go.
pub fn unsubscribe_page_url(
    db: &crate::db::Db,
    message_id: i64,
) -> Result<String, IpcError> {
    use crate::unsub::{store, Target, Verdict};

    let candidate = db
        .read(move |conn| store::candidate(conn, message_id))?
        .ok_or_else(|| IpcError::not_found("message", message_id))?;

    match crate::unsub::verdict(&candidate) {
        Verdict::Unsubscribe(Target::Link { url } | Target::OneClick { url }) => Ok(url),
        Verdict::Unsubscribe(Target::Mail { .. }) => Err(IpcError::Internal(
            "that unsubscribe is an email address, not a page".into(),
        )),
        _ => Err(IpcError::Internal(
            "that message has no unsubscribe page to open".into(),
        )),
    }
}

/// Open a message's unsubscribe page.
///
/// The one case Mach will not automate — an `https` `List-Unsubscribe` with no
/// RFC 8058 one-click support, which may be a form, a login wall or a plain
/// confirmation. Rather than guessing, it shows him the page.
///
/// # Why the URL is looked up here instead of being passed in
///
/// It never left Rust. The frontend was told the offer is of kind `link` and
/// nothing else, so there is no URL in the webview for a rendered message to
/// reach, and no way for a caller to name a destination of its own. This
/// re-reads the header from the store and re-runs both the rule and the scheme
/// and host validation before anything opens — the parameter is a message id,
/// which is not something an attacker can turn into a URL. It still is not: the
/// in-app window is built from Rust with the URL Rust resolved, and the string
/// never crosses into the app's webview in either direction.
///
/// # Two destinations, and the second one is not a lesser version
///
/// `system: false` — the default — puts the page in [`crate::browser`], a
/// window inside Mach with no capability grant, no shared cookie jar, and the
/// host in its title bar. `system: true` hands it to the default browser
/// through the opener plugin, which is a different process with his real
/// session in it. The page that wants him signed in needs the second, and the
/// command palette is where he asks for it.
#[tauri::command]
pub fn open_unsubscribe_page(
    app: AppHandle,
    state: State<'_, AppState>,
    message_id: i64,
    system: Option<bool>,
) -> Result<(), IpcError> {
    let url = unsubscribe_page_url(&state.db, message_id)?;

    if system.unwrap_or(false) {
        tauri_plugin_opener::open_url(&url, None::<&str>)
            .map_err(|e| IpcError::Internal(format!("the page could not be opened: {e}")))?;
        return Ok(());
    }

    crate::browser::open(&app, &url, None).map_err(IpcError::Internal)
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
// filters
// ---------------------------------------------------------------------------

/// Gmail filters for one account, or for every account when `account_id` is
/// absent.
///
/// Read live from Google rather than from SQLite, which is the one place in the
/// app that does that and is argued in [`crate::commands::filters`]: a filter
/// has never been a local row, there is no incremental feed to keep a copy
/// fresh, and a delete addressing a stale id is worse than a spinner.
#[tauri::command]
pub async fn list_filters(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: Option<i64>,
) -> Result<Vec<AccountFilter>, IpcError> {
    scope_aware(&app, &state, state.dispatcher.list_filters(account_id).await)
}

/// Create one filter.
///
/// Takes the two halves separately rather than a whole `Filter`, because a
/// caller has no id to send and an id in the request would be silently dropped.
#[tauri::command]
pub async fn create_filter(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: i64,
    criteria: FilterCriteria,
    action: FilterAction,
) -> Result<AccountFilter, IpcError> {
    let filter = Filter {
        id: String::new(),
        criteria,
        action,
    };
    scope_aware(
        &app,
        &state,
        state.dispatcher.create_filter(account_id, filter).await,
    )
}

#[tauri::command]
pub async fn delete_filter(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: i64,
    filter_id: String,
) -> Result<(), IpcError> {
    scope_aware(
        &app,
        &state,
        state.dispatcher.delete_filter(account_id, &filter_id).await,
    )
}

/// Pass a filter result through, and push a new sync status if the reason it
/// failed was a grant that is too narrow.
///
/// The command layer has already recorded the account; this is what makes the
/// window notice without waiting for the next sync pass, so "Needs permission"
/// appears in Preferences → Accounts in the same breath as the error.
fn scope_aware<T>(
    app: &AppHandle,
    state: &AppState,
    result: Result<T, CommandError>,
) -> Result<T, IpcError> {
    if let Err(CommandError::MissingScope { .. }) = &result {
        events::emit_sync_status(app, &state.status_payload());
    }
    result.map_err(IpcError::from)
}

// ---------------------------------------------------------------------------
// sync
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn sync_status(state: State<'_, AppState>) -> SyncStatusPayload {
    state.status_payload()
}

/// Go and look at Google now — mail and calendar, every account or one.
///
/// # Why this awaits the pass
///
/// It used to nudge the loop and return, which meant the only way to find out
/// whether anything had happened was to watch the status bar and guess. The
/// standing rule is that the *UI* never waits on Google, not that nothing may:
/// nothing on screen is blocked by this, every pane still renders from SQLite
/// throughout, and the one thing that changes when it resolves is a line of
/// status text. Awaiting is what lets that line say "up to date" or name the
/// account Google refused, which is the whole reason the feature exists.
///
/// The engine holds the pass to the same writer discipline the loop uses —
/// `Db::write_background`, which stands aside for a queued user command at
/// every batch boundary — so a forced pass cannot be the thing that makes
/// archiving a conversation take seconds. That is not a property of this
/// command; it is a property of running the *same* pass rather than a special
/// urgent one.
#[tauri::command]
pub async fn sync_now(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: Option<i64>,
) -> Result<ForcedPass, IpcError> {
    state.client_config()?;
    // `start` is idempotent; calling it here is what makes "Sync now" work
    // immediately after the very first account is added, when the loop was
    // never started at boot.
    state.sync.start();
    let sync = Arc::clone(&state.sync);
    let scope = match account_id {
        Some(id) => SyncScope::Account(id),
        None => SyncScope::All,
    };
    let outcome = sync.force_sync(scope).await;
    // The status the pass just wrote, pushed without waiting for the watch
    // channel's own forwarder — a failure has to be on screen by the time the
    // caller's promise resolves, or the message and the detail panel disagree
    // for a frame.
    events::emit_sync_status(&app, &state.status_payload());
    if outcome.accounts.iter().any(|a| a.messages_written > 0) {
        events::emit_threads_changed(&app);
    }
    Ok(outcome)
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
