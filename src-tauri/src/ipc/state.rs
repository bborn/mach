//! Boot: open the store, restore the accounts, wire the engine.
//!
//! This is where the seven units that were built independently are finally
//! joined. The joins themselves are small — the adapter that lets `auth`'s
//! `TokenManager` satisfy `google`'s `TokenProvider` is nine lines, exactly as
//! `examples/gmail_smoke.rs` proved — because every seam was designed for this.
//!
//! # Two client factories, one token manager
//!
//! The sync engine wants a [`ClientFactory`] (keyed by `&Account`); the command
//! layer wants [`GoogleClients`] (keyed by account id). Both resolve to the same
//! [`TokenManager`], which owns the refresh loop and the Keychain, so five
//! accounts share one set of credentials and one connection pool no matter which
//! side of the app asks.
//!
//! Neither factory captures a fixed set of accounts. Both resolve the account at
//! call time — the sync engine from the `&Account` it was handed, the command
//! layer from the store — so an account added at runtime works immediately
//! without rebuilding anything.
//!
//! # Restoring accounts
//!
//! On boot every row in `accounts` is checked against the Keychain. A missing
//! entry (the user deleted it, or the app is running on a different machine)
//! makes that account appear in `needsReauthorization` — it is not an error and
//! it does not stop the other four from syncing.
//!
//! An account whose credential is fine and whose *grant* is too narrow lands in
//! the same list, from the other end: a 403 with `insufficientPermissions` is
//! recorded by the command layer (see [`crate::commands::filters::ScopeNotices`])
//! and merged in here. Both mean "consent again"; `missing_scope` is what lets a
//! surface say which of the two it is looking at, because they are not the same
//! failure and the account with the narrow grant is still syncing mail perfectly.
//!
//! A QA instance is the same case for every row at once. It addresses its own
//! Keychain namespace (see [`crate::auth::tokens::keychain_service`]), so a
//! store copied from the owner by `scripts/qa seed` arrives with accounts and
//! without credentials: every address lands in `needsReauthorization`, the
//! mailbox renders from SQLite as it always does, and each sync pass records
//! "no credentials for …" against that account rather than retrying forever.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::flow::{self, AuthorizedAccount, PendingAuthorization};
use crate::auth::http::GoogleTokenHttp;
use crate::auth::oauth;
use crate::auth::tokens::{keychain_service, KeychainTokenStore, TokenManager, TokenStore};
use crate::auth::ClientConfig;
use crate::commands::{CommandDispatcher, CommandError, GoogleClients};
use crate::config::AppConfig;
use crate::db::models::{Account, NewAccount};
use crate::db::{command_queries, queries, Db};
use crate::google::calendar::CalendarClient;
use crate::google::gmail::GmailClient;
use crate::google::{
    BoxFuture, GoogleError, HttpTransport, ReqwestTransport, TokenProvider,
};
use crate::sync::{SyncConfig, SyncEngine, TransportClients};

use super::error::IpcError;
use super::types::SyncStatusPayload;

/// The concrete token manager this app uses: Google's token endpoint over
/// HTTPS, credentials in the macOS Keychain.
pub type Tokens = TokenManager<GoogleTokenHttp, KeychainTokenStore>;

/// What every client factory says when there is no OAuth client at all.
const NOT_CONFIGURED: &str = "Google sign-in is not configured";

// ---------------------------------------------------------------------------
// the auth ↔ google adapter
// ---------------------------------------------------------------------------

/// Bridges `auth`'s [`TokenManager`] to `google`'s [`TokenProvider`].
///
/// Lifted verbatim from `examples/gmail_smoke.rs`, which is the only place the
/// two units had ever been connected before this one.
struct ManagedToken {
    manager: Arc<Tokens>,
    email: String,
}

impl TokenProvider for ManagedToken {
    fn access_token(&self) -> BoxFuture<'_, Result<String, GoogleError>> {
        Box::pin(async move {
            self.manager
                .access_token(&self.email)
                .await
                .map(|s| s.expose().to_string())
                // The one classification this adapter has to preserve. A
                // refresh Google answered `invalid_grant` is the account being
                // logged out, and it arrives here as an ordinary `Err` among
                // timeouts and 503s; flattening it to `Auth` is what left the
                // owner with "Sync failed" and no way to see or fix why.
                .map_err(|e| {
                    let message = e.to_string();
                    if e.is_credential_rejected() {
                        GoogleError::CredentialRejected { message }
                    } else {
                        GoogleError::Auth { message }
                    }
                })
        })
    }
}

/// The provider handed out when there is no OAuth client. Fails at the moment a
/// request would go out, with the reason, instead of making every caller carry
/// an `Option`.
struct UnconfiguredToken;

impl TokenProvider for UnconfiguredToken {
    fn access_token(&self) -> BoxFuture<'_, Result<String, GoogleError>> {
        Box::pin(async {
            Err(GoogleError::Auth {
                message: NOT_CONFIGURED.to_string(),
            })
        })
    }
}

fn provider_for(tokens: &Option<Arc<Tokens>>, email: &str) -> Arc<dyn TokenProvider> {
    match tokens {
        Some(manager) => Arc::new(ManagedToken {
            manager: Arc::clone(manager),
            email: email.to_string(),
        }),
        None => Arc::new(UnconfiguredToken),
    }
}

// ---------------------------------------------------------------------------
// the command layer's client factory
// ---------------------------------------------------------------------------

/// Resolves an account id to a client, through the store and the token manager.
///
/// `commands::AccountClients` exists and does almost this, but it takes its
/// account map at construction — which would mean rebuilding the dispatcher
/// every time an account is added. Looking the address up per call costs one
/// indexed read and removes that whole class of staleness.
pub struct ManagedClients {
    db: Db,
    transport: Arc<dyn HttpTransport>,
    tokens: Option<Arc<Tokens>>,
}

impl ManagedClients {
    pub fn new(db: Db, transport: Arc<dyn HttpTransport>, tokens: Option<Arc<Tokens>>) -> Self {
        ManagedClients {
            db,
            transport,
            tokens,
        }
    }

    fn provider(&self, account_id: i64) -> Result<Arc<dyn TokenProvider>, CommandError> {
        if self.tokens.is_none() {
            return Err(CommandError::Invalid {
                message: NOT_CONFIGURED.to_string(),
            });
        }
        let account = self
            .db
            .read(|conn| command_queries::account_by_id(conn, account_id))?
            .ok_or(CommandError::UnknownAccount { account_id })?;
        Ok(provider_for(&self.tokens, &account.email))
    }
}

impl GoogleClients for ManagedClients {
    fn gmail(&self, account_id: i64) -> Result<GmailClient, CommandError> {
        Ok(GmailClient::new(
            Arc::clone(&self.transport),
            self.provider(account_id)?,
        ))
    }

    fn calendar(&self, account_id: i64) -> Result<CalendarClient, CommandError> {
        Ok(CalendarClient::new(
            Arc::clone(&self.transport),
            self.provider(account_id)?,
        ))
    }
}

// ---------------------------------------------------------------------------
// application state
// ---------------------------------------------------------------------------

/// Everything the IPC handlers share. Lives in `tauri::State`.
pub struct AppState {
    pub db: Db,
    pub dispatcher: Arc<CommandDispatcher>,
    pub sync: Arc<SyncEngine>,
    pub config: AppConfig,
    /// What is installed, whether the sandbox was verified, and the bridge the
    /// agent calls plugin actions through. Built even in safe mode, because the
    /// plugin list has to be able to say *why* nothing is running.
    pub plugins: Arc<crate::plugins::PluginRuntime>,
    tokens: Option<Arc<Tokens>>,
    /// Accounts whose Keychain entry is gone. Guarded rather than immutable
    /// because completing a sign-in clears an entry from it.
    needs_reauthorization: Mutex<Vec<String>>,
    /// Authorization handshakes waiting for their loopback callback, keyed by
    /// the opaque id handed to the frontend.
    pending: Mutex<HashMap<String, Handshake>>,
}

/// A sign-in in flight, plus the address it was started for.
///
/// "Add account" has no address in mind and leaves `email` empty. "Sign in
/// again", next to a row that has lost its Keychain entry, names one — which is
/// both a `login_hint` for Google and the thing `complete_add_account` checks
/// the returned identity against, so that fixing one account cannot quietly
/// connect a different one.
pub struct Handshake {
    pub authorization: PendingAuthorization,
    pub email: Option<String>,
}

impl AppState {
    /// The OAuth client, or the "not configured" error the UI should render.
    pub fn client_config(&self) -> Result<&ClientConfig, IpcError> {
        self.config.client.as_ref().ok_or_else(|| {
            IpcError::NotConfigured(
                self.config
                    .configuration_error
                    .clone()
                    .unwrap_or_else(|| NOT_CONFIGURED.to_string()),
            )
        })
    }

    pub fn tokens(&self) -> Option<&Arc<Tokens>> {
        self.tokens.as_ref()
    }

    /// Whether the background loop should be started at all. No accounts means
    /// nothing to sync; no credentials means every pass would fail on the first
    /// request.
    pub fn should_start_sync(&self) -> bool {
        if !self.config.is_configured() {
            return false;
        }
        self.db
            .read(queries::list_accounts)
            .map(|accounts| !accounts.is_empty())
            .unwrap_or(false)
    }

    /// The engine's picture plus the two questions it cannot answer.
    pub fn status_payload(&self) -> SyncStatusPayload {
        let status = self.sync.status_snapshot();
        let missing_scope = self.dispatcher.scope_notices().emails();
        SyncStatusPayload {
            running: status.running,
            accounts: status.accounts,
            last_pass_started_at: status.last_pass_started_at,
            last_pass_finished_at: status.last_pass_finished_at,
            configured: self.config.is_configured(),
            configuration_error: self.config.configuration_error.clone(),
            needs_reauthorization: self.needs_reauthorization(),
            missing_scope,
        }
    }

    /// Every address that needs a person to sign in again, from all three
    /// places that can know.
    ///
    /// The startup check finds accounts with *no* Keychain entry. It cannot find
    /// the other and more common case — an entry that is present and dead —
    /// because a revoked refresh token is a perfectly ordinary string until
    /// Google is asked about it. That one is found by the sync loop, on the
    /// first pass after launch.
    ///
    /// The third is a grant that is missing a scope, which belongs here and not
    /// only in `missing_scope`: the remedy is identical, and the status bar
    /// counts this list. `missing_scope` is the narrower fact, for the surfaces
    /// that can say *which* of the three it is.
    pub fn needs_reauthorization(&self) -> Vec<String> {
        let mut out = lock(&self.needs_reauthorization).clone();
        let live = self.sync.status_snapshot();
        for email in live.needs_reauthorization() {
            if !out.iter().any(|e| e == email) {
                out.push(email.to_string());
            }
        }
        for email in self.dispatcher.scope_notices().emails() {
            if !out.contains(&email) {
                out.push(email);
            }
        }
        out
    }

    pub fn mark_reauthorized(&self, email: &str) {
        lock(&self.needs_reauthorization).retain(|e| e != email);
        // And the sync loop's own verdict, or a successful sign-in would leave
        // the label up until the next pass proved it wrong.
        self.sync.clear_reauthorization(email);
        // A fresh grant is a fresh set of scopes, so whatever was refused
        // before is worth trying again rather than being remembered forever.
        self.dispatcher.scope_notices().clear(email);
    }

    pub fn mark_needs_reauthorization(&self, email: &str) {
        let mut list = lock(&self.needs_reauthorization);
        if !list.iter().any(|e| e == email) {
            list.push(email.to_string());
        }
    }

    /// Park a handshake and return the opaque id that reclaims it.
    pub fn store_pending(&self, pending: Handshake) -> Result<String, IpcError> {
        // The id is the same kind of value as an OAuth `state`: unguessable and
        // meaningless. It never reaches Google — it exists so the frontend can
        // name one of several in-flight sign-ins.
        let id = oauth::generate_state()?;
        lock(&self.pending).insert(id.clone(), pending);
        Ok(id)
    }

    pub fn take_pending(&self, pending_id: &str) -> Result<Handshake, IpcError> {
        lock(&self.pending)
            .remove(pending_id)
            .ok_or_else(|| IpcError::UnknownPending(pending_id.to_string()))
    }
}

/// `--safe-mode`, or `MACH_SAFE_MODE=1`: boot with every plugin disabled.
///
/// The single most valuable operational feature in `docs/plugins.md`, and it is
/// here in v1 rather than added after the first bad week. It disables; it never
/// uninstalls, so the plugin list still shows what is there and why it is off.
pub fn safe_mode() -> bool {
    std::env::args().any(|arg| arg == "--safe-mode")
        || matches!(std::env::var("MACH_SAFE_MODE").as_deref(), Ok("1") | Ok("true"))
}

/// A poisoned mutex here means some other command panicked. The data behind it
/// is a plain map, so recovering is strictly better than propagating a failure
/// that would make every later command fail too.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// boot
// ---------------------------------------------------------------------------

/// Open the store, restore accounts, and build everything the IPC layer needs.
///
/// Does **not** start the sync loop — that needs a Tokio context and an
/// `AppHandle` to emit from, so it is [`super::events::run`]'s job.
pub fn bootstrap(config: AppConfig) -> Result<AppState, IpcError> {
    // The plugin directory sits beside the database, so an instance launched
    // with its own MACH_DATA_DIR gets its own plugins too — QA can install a
    // plugin without touching the mailbox the owner is reading.
    let data_dir = config
        .database_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let plugins = super::plugins::runtime(&data_dir, safe_mode());
    // `Db::open` creates the directory, applies the pragmas and runs migrations.
    let db = Db::open(&config.database_path)?;

    // Repairs to row *content* live here rather than in `Db::open`, which every
    // test opens a database through: this is a property of starting the app,
    // not of having a database. Each repair records its own completion, so this
    // is a single indexed lookup on every boot after the first.
    match crate::db::backfill::decode_snippets(&mut db.writer()) {
        Ok(0) => {}
        Ok(n) => eprintln!("decoded HTML entities in {n} message snippets"),
        // A failed repair must not stop the app booting: the symptom is some
        // rows still reading `&#39;`, which is cosmetic and will be retried.
        Err(e) => eprintln!("snippet repair failed, will retry next boot: {e}"),
    }

    let tokens = config.client.clone().map(|client| {
        Arc::new(TokenManager::new(
            client,
            GoogleTokenHttp::new(),
            KeychainTokenStore::default(),
        ))
    });

    // One transport for the whole app: five accounts against two Google hosts
    // should share one connection pool.
    let transport: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::new());

    let dispatcher = Arc::new(CommandDispatcher::new(
        db.clone(),
        Arc::new(ManagedClients::new(
            db.clone(),
            Arc::clone(&transport),
            tokens.clone(),
        )),
    )?);

    let sync_tokens = tokens.clone();
    let sync_clients = Arc::new(TransportClients::new(Arc::clone(&transport), move |account| {
        provider_for(&sync_tokens, &account.email)
    }));
    // The sync interval is the one preference this side owns, so it is read here
    // rather than replayed from the frontend after boot — otherwise the first
    // pass of every launch would run on the default gap regardless of what the
    // owner chose. An unreadable or unset row leaves `SyncConfig`'s own default.
    let mut sync_config = SyncConfig::default();
    if let Ok(Some(interval)) = db.read(super::prefs::sync_interval) {
        sync_config.poll_interval = interval;
    }
    let sync = Arc::new(SyncEngine::new(db.clone(), sync_clients, sync_config)?);

    // Deliberately *not* the Keychain read that used to be here. See
    // `restore_accounts_into`: doing it on this thread deadlocks the launch.
    let needs_reauthorization = Vec::new();

    Ok(AppState {
        db,
        dispatcher,
        sync,
        config,
        plugins,
        tokens,
        needs_reauthorization: Mutex::new(needs_reauthorization),
        pending: Mutex::new(HashMap::new()),
    })
}

/// Check stored credentials and record the accounts that need attention.
///
/// # Why this cannot run during `bootstrap`
///
/// It used to, and the app hung on launch with no window at all.
///
/// `bootstrap` is called from Tauri's `setup`, which runs on the main thread
/// inside `applicationDidFinishLaunching`. Reading a Keychain item from there
/// is a deadlock waiting for a reason to happen, and rebuilding the binary is
/// reason enough: the code signature changes, the item's ACL no longer matches,
/// and macOS wants to ask permission. But `SecurityAgent` cannot put its dialog
/// on screen until the application has finished launching — so the app waits
/// for the Keychain, the Keychain waits for an answer, and the answer waits for
/// the app. The process sits there alive, holding no window, forever.
///
/// A sampled stack of exactly that:
///
/// ```text
///   applicationDidFinishLaunching
///     └ mach_lib::run::{{closure}}          <- setup
///        └ bootstrap → restore_accounts
///           └ SecKeychainFindGenericPassword
///              └ CSSM_DecryptDataFinal      <- blocked
/// ```
///
/// So this runs afterwards, off the main thread, once there is a window for the
/// prompt to appear over. Nothing needs it to have finished: the mailbox renders
/// from SQLite, and an account whose credential turns out to be missing is
/// reported through `needs_reauthorization` a moment later — which is the same
/// path used when a token expires while the app is running.
///
/// This is the app's own invariant applied to the login keychain rather than to
/// Google: the UI never waits on anything it does not already have locally.
///
/// # On a QA instance
///
/// The store here is namespaced to the instance, so the lookup is for a service
/// that holds no items at all: `errSecItemNotFound` on every account, returned
/// without a dialog because there is no item whose ACL macOS would need to ask
/// about. Every address is reported as needing reauthorization, which is the
/// honest answer — a seeded database carries mail, not credentials.
pub fn restore_accounts_into(state: &AppState) {
    match restore_accounts(&state.db, &KeychainTokenStore::default()) {
        Ok(emails) => {
            for email in emails {
                state.mark_needs_reauthorization(&email);
            }
        }
        // A failed read is not fatal and never was: every account simply looks
        // as though it needs attention, which is the honest answer.
        Err(error) => eprintln!("could not check stored credentials: {error}"),
    }
}

/// Which stored accounts still have a usable credential.
///
/// Returns the addresses that do not. A Keychain read that fails outright (the
/// user denied access, the entry is corrupt) counts the same as a missing one:
/// the account needs attention, and the app carries on.
pub fn restore_accounts(db: &Db, store: &impl TokenStore) -> Result<Vec<String>, IpcError> {
    let accounts = db.read(queries::list_accounts)?;
    Ok(accounts
        .into_iter()
        .filter(|account| !matches!(store.load_refresh_token(&account.email), Ok(Some(_))))
        .map(|account| account.email)
        .collect())
}

// ---------------------------------------------------------------------------
// adding an account
// ---------------------------------------------------------------------------

/// Wait for the loopback callback and finish the exchange, off the IPC thread.
///
/// [`flow::complete_authorization`] blocks a whole OS thread inside
/// `wait_for_callback` for as long as the user takes to consent — up to five
/// minutes. Parking a Tokio worker for that would starve the runtime the UI's
/// own commands run on, so the handshake gets its own thread and its own
/// single-threaded runtime, and the caller simply awaits the answer.
pub async fn await_authorization(
    pending: PendingAuthorization,
    config: ClientConfig,
) -> Result<AuthorizedAccount, IpcError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    std::thread::Builder::new()
        .name("mach-oauth-callback".to_string())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| IpcError::internal(format!("could not start the sign-in worker: {e}")))
                .and_then(|runtime| {
                    runtime.block_on(async {
                        let http = GoogleTokenHttp::new();
                        let store = KeychainTokenStore::default();
                        flow::complete_authorization(pending, &config, &http, &store)
                            .await
                            .map_err(IpcError::from)
                    })
                });
            // The receiver is gone only if the caller was dropped, which means
            // nobody is waiting for this any more.
            let _ = tx.send(result);
        })
        .map_err(|e| IpcError::internal(format!("could not start the sign-in worker: {e}")))?;

    rx.await
        .map_err(|_| IpcError::internal("the sign-in worker stopped before finishing"))?
}

/// Insert (or refresh) the account row for a completed authorization.
///
/// Keeps an existing account's palette index — re-authorizing must not shuffle
/// the colours of a rail the user has learned.
pub fn persist_account(db: &Db, email: &str) -> Result<Account, IpcError> {
    let existing = db.read(|conn| queries::account_by_email(conn, email))?;
    let colour_index = match &existing {
        Some(account) => account.colour_index,
        None => next_colour_index(db)?,
    };

    db.write(|conn| {
        queries::upsert_account(
            conn,
            &NewAccount {
                email: email.to_string(),
                display_name: existing.as_ref().and_then(|a| a.display_name.clone()),
                // The Keychain service, not the credential: tokens never touch
                // SQLite. Written from `keychain_service()` rather than the
                // constant so the row records where this instance actually put
                // the token — a QA instance has its own namespace, and a seeded
                // store's inherited rows naming the owner's service are exactly
                // the credentials it does not have.
                token_ref: keychain_service(),
                colour_index,
            },
        )
    })?;

    db.read(|conn| queries::account_by_email(conn, email))?
        .ok_or_else(|| IpcError::internal("the account row vanished immediately after it was written"))
}

/// The next free slot in the UI's five-colour palette.
fn next_colour_index(db: &Db) -> Result<i64, IpcError> {
    let accounts = db.read(queries::list_accounts)?;
    Ok(accounts.len() as i64 % 5)
}
