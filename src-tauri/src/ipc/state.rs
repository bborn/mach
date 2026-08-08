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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::flow::{self, AuthorizedAccount, PendingAuthorization};
use crate::auth::http::GoogleTokenHttp;
use crate::auth::oauth;
use crate::auth::tokens::{KeychainTokenStore, TokenManager, TokenStore, KEYCHAIN_SERVICE};
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
                .map_err(|e| GoogleError::Auth {
                    message: e.to_string(),
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
    tokens: Option<Arc<Tokens>>,
    /// Accounts whose Keychain entry is gone. Guarded rather than immutable
    /// because completing a sign-in clears an entry from it.
    needs_reauthorization: Mutex<Vec<String>>,
    /// Authorization handshakes waiting for their loopback callback, keyed by
    /// the opaque id handed to the frontend.
    pending: Mutex<HashMap<String, PendingAuthorization>>,
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
        SyncStatusPayload {
            running: status.running,
            accounts: status.accounts,
            last_pass_started_at: status.last_pass_started_at,
            last_pass_finished_at: status.last_pass_finished_at,
            configured: self.config.is_configured(),
            configuration_error: self.config.configuration_error.clone(),
            needs_reauthorization: self.needs_reauthorization(),
        }
    }

    pub fn needs_reauthorization(&self) -> Vec<String> {
        lock(&self.needs_reauthorization).clone()
    }

    pub fn mark_reauthorized(&self, email: &str) {
        lock(&self.needs_reauthorization).retain(|e| e != email);
    }

    pub fn mark_needs_reauthorization(&self, email: &str) {
        let mut list = lock(&self.needs_reauthorization);
        if !list.iter().any(|e| e == email) {
            list.push(email.to_string());
        }
    }

    /// Park a handshake and return the opaque id that reclaims it.
    pub fn store_pending(&self, pending: PendingAuthorization) -> Result<String, IpcError> {
        // The id is the same kind of value as an OAuth `state`: unguessable and
        // meaningless. It never reaches Google — it exists so the frontend can
        // name one of several in-flight sign-ins.
        let id = oauth::generate_state()?;
        lock(&self.pending).insert(id.clone(), pending);
        Ok(id)
    }

    pub fn take_pending(&self, pending_id: &str) -> Result<PendingAuthorization, IpcError> {
        lock(&self.pending)
            .remove(pending_id)
            .ok_or_else(|| IpcError::UnknownPending(pending_id.to_string()))
    }
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
    // `Db::open` creates the directory, applies the pragmas and runs migrations.
    let db = Db::open(&config.database_path)?;

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
    let sync = Arc::new(SyncEngine::new(
        db.clone(),
        sync_clients,
        SyncConfig::default(),
    )?);

    let needs_reauthorization = restore_accounts(&db, &KeychainTokenStore::default())?;

    Ok(AppState {
        db,
        dispatcher,
        sync,
        config,
        tokens,
        needs_reauthorization: Mutex::new(needs_reauthorization),
        pending: Mutex::new(HashMap::new()),
    })
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
                // SQLite.
                token_ref: KEYCHAIN_SERVICE.to_string(),
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
