//! Token lifetime and storage.
//!
//! Refresh tokens go in the macOS Keychain, keyed by account email, under a
//! service name that belongs to the instance rather than to the app — see
//! [`keychain_service`], which is what keeps a QA instance off the owner's
//! credentials. Access tokens stay in memory and are refreshed transparently
//! inside a safety margin.
//! Every type in here that holds secret material implements `Debug` by hand so
//! it cannot leak through a stray `{:?}`; `tests/auth.rs` asserts that.

use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use super::oauth::{self, TokenHttp};
use super::{AuthError, ClientConfig};

/// Refresh this many seconds before the access token actually expires, so a
/// request never leaves with a token that dies in flight.
pub const REFRESH_MARGIN_SECONDS: i64 = 60;

/// The owner's Keychain service name. Every account is an entry under this
/// service, with the account's email address as the entry's username.
///
/// Only the owner's instance may name it. Call [`keychain_service`] rather than
/// this constant anywhere a store is being built.
pub const KEYCHAIN_SERVICE: &str = "com.mach.mail.oauth";

/// Prefix for every instance that is not the owner's.
pub const QA_KEYCHAIN_PREFIX: &str = "com.mach.mail.oauth.qa-";

/// What a QA instance is called when its data directory yields nothing usable.
const UNNAMED_QA_INSTANCE: &str = "unnamed";

/// The Keychain service this process is allowed to use.
///
/// # Why this is not a constant
///
/// `scripts/qa` gives a QA instance its own SQLite store under
/// `.qa/<instance>/data`, its own plugin directory and its own attachment
/// cache, so nothing it does can reach the mailbox the owner is reading. The
/// Keychain is the one thing left that is global to the machine, and the
/// service name was a constant — so a QA instance asking for
/// `com.mach.mail.oauth` / `<owner's address>` was asking for the owner's real
/// refresh token.
///
/// macOS answers that request with a modal panel — *"mach wants to access key
/// com.mach.mail.oauth in your keychain"* — because a rebuilt debug binary has
/// a different code signature from the one that created the item, so the item's
/// ACL no longer matches. That panel takes the owner's focus and blocks the
/// reading thread until somebody answers it, which is precisely what a QA
/// instance must never be able to cause.
///
/// Giving each instance its own service closes the route structurally: a QA
/// process cannot *name* the owner's entries, so there is nothing for macOS to
/// ask about. It finds no item, gets `errSecItemNotFound`, and returns.
///
/// [`crate::shell::is_qa_instance`] is the signal — `MACH_DATA_DIR` — because
/// it is already the thing that makes an instance separate. One signal, not two
/// that can disagree.
pub fn keychain_service() -> String {
    if !crate::shell::is_qa_instance() {
        return KEYCHAIN_SERVICE.to_string();
    }
    // `is_qa_instance` is `MACH_DATA_DIR` being set, so this is present; the
    // default is unreachable and would name the instance `unnamed` anyway.
    let data_dir = std::env::var_os("MACH_DATA_DIR").unwrap_or_default();
    format!("{QA_KEYCHAIN_PREFIX}{}", instance_name(Path::new(&data_dir)))
}

/// The instance name embedded in a QA data directory.
///
/// `scripts/qa` lays instances out as `<repo>/.qa/<instance>/data`, so the name
/// is the directory *holding* the store rather than the store directory itself.
/// Anything else — a hand-set `MACH_DATA_DIR` pointing somewhere arbitrary —
/// falls back to the last component, and then to [`UNNAMED_QA_INSTANCE`]. The
/// result only has to be stable and distinct from the owner's service; it is
/// slugged so it stays readable in Keychain Access, where the owner is the one
/// who has to recognise it.
fn instance_name(data_dir: &Path) -> String {
    let mut parts: Vec<&str> = data_dir
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    if parts.last() == Some(&"data") {
        parts.pop();
    }

    let slug: String = parts
        .last()
        .copied()
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        UNNAMED_QA_INSTANCE.to_string()
    } else {
        slug.to_string()
    }
}

// ---------------------------------------------------------------------------
// Secret
// ---------------------------------------------------------------------------

/// A string that must never be printed.
///
/// Implements `Debug` as a redaction and deliberately does **not** implement
/// `Display`, `Serialize` or `AsRef<str>` — reading the value requires calling
/// [`Secret::expose`], which greps as an audit point.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reads the underlying value. Call sites should be few and obvious.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

// ---------------------------------------------------------------------------
// TokenSet
// ---------------------------------------------------------------------------

/// Google's token-endpoint success body.
#[derive(Deserialize)]
struct TokenResponseBody {
    access_token: String,
    /// Seconds from now. Converted to an absolute instant immediately — a
    /// relative lifetime is useless the moment it is stored.
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

/// The live credentials for one account.
#[derive(Clone)]
pub struct TokenSet {
    pub access_token: Secret,
    /// Absent when Google withheld one (it only issues a refresh token on the
    /// first consent, or with `prompt=consent`).
    pub refresh_token: Option<Secret>,
    /// Absolute expiry, not a duration.
    pub expires_at: DateTime<Utc>,
    pub scope: Option<String>,
    pub token_type: String,
}

impl TokenSet {
    /// Parses a token-endpoint success body.
    ///
    /// `existing_refresh_token` is carried over when the response omits one,
    /// which is the normal shape of a refresh response — dropping it there would
    /// silently log the account out at the next expiry.
    pub fn from_json(
        body: &str,
        now: DateTime<Utc>,
        existing_refresh_token: Option<Secret>,
    ) -> Result<Self, AuthError> {
        let parsed: TokenResponseBody = serde_json::from_str(body)
            .map_err(|e| AuthError::MalformedTokenResponse(e.to_string()))?;
        Ok(Self {
            access_token: Secret::new(parsed.access_token),
            refresh_token: parsed
                .refresh_token
                .map(Secret::new)
                .or(existing_refresh_token),
            expires_at: now + Duration::seconds(parsed.expires_in),
            scope: parsed.scope,
            token_type: parsed.token_type.unwrap_or_else(|| "Bearer".to_string()),
        })
    }

    /// True once the token is within [`REFRESH_MARGIN_SECONDS`] of expiry.
    pub fn needs_refresh_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at - Duration::seconds(REFRESH_MARGIN_SECONDS) <= now
    }

    /// True once the token is actually past its expiry.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }

    /// The `Authorization` header value for an API request.
    pub fn authorization_header(&self) -> Secret {
        Secret::new(format!("{} {}", self.token_type, self.access_token.expose()))
    }

    /// Constructor for tests and for rehydrating a set whose parts are already
    /// known. Not used on the network path.
    pub fn for_test(
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            access_token: Secret::new(access_token),
            refresh_token: refresh_token.map(Secret::new),
            expires_at,
            scope: None,
            token_type: "Bearer".to_string(),
        }
    }
}

/// Hand-written: the two secret fields are reduced to a presence flag, the rest
/// stays visible because expiry and scope are what you actually want in a log.
impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &if self.refresh_token.is_some() {
                    "<redacted>"
                } else {
                    "<none>"
                },
            )
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("token_type", &self.token_type)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// TokenStore
// ---------------------------------------------------------------------------

/// Where refresh tokens live between runs, keyed by account email.
///
/// Only the refresh token is persisted. Access tokens are short-lived and are
/// never written anywhere.
pub trait TokenStore: Send + Sync {
    fn save_refresh_token(&self, account_email: &str, token: &Secret) -> Result<(), AuthError>;
    fn load_refresh_token(&self, account_email: &str) -> Result<Option<Secret>, AuthError>;
    fn delete_refresh_token(&self, account_email: &str) -> Result<(), AuthError>;
}

/// The real store: the macOS Keychain, via the `keyring` crate.
///
/// The service name comes from [`keychain_service`], so a QA instance reads and
/// writes its own namespace and the owner's entries are not addressable from it.
#[derive(Debug, Clone)]
pub struct KeychainTokenStore {
    service: String,
}

impl Default for KeychainTokenStore {
    fn default() -> Self {
        Self {
            service: keychain_service(),
        }
    }
}

impl KeychainTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the service name. Exists so a manual keychain smoke test can
    /// use a throwaway namespace instead of the real one.
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// The Keychain service this store addresses.
    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, account_email: &str) -> Result<keyring::Entry, AuthError> {
        keyring::Entry::new(&self.service, account_email)
            .map_err(|e| AuthError::Keychain(e.to_string()))
    }
}

impl TokenStore for KeychainTokenStore {
    fn save_refresh_token(&self, account_email: &str, token: &Secret) -> Result<(), AuthError> {
        self.entry(account_email)?
            .set_password(token.expose())
            .map_err(|e| AuthError::Keychain(e.to_string()))
    }

    fn load_refresh_token(&self, account_email: &str) -> Result<Option<Secret>, AuthError> {
        match self.entry(account_email)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AuthError::Keychain(e.to_string())),
        }
    }

    fn delete_refresh_token(&self, account_email: &str) -> Result<(), AuthError> {
        match self.entry(account_email)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AuthError::Keychain(e.to_string())),
        }
    }
}

/// Process-lifetime store. Used by tests, and usable for a headless run where
/// prompting for keychain access would hang.
#[derive(Default)]
pub struct MemoryTokenStore {
    inner: Mutex<HashMap<String, String>>,
}

/// Test-only in practice, and redacted anyway. The rule this module states —
/// every type holding secret material writes its own `Debug` — does not get an
/// exemption for the store that holds refresh tokens as plain `String`s. A
/// derived `Debug` here prints the whole map.
impl fmt::Debug for MemoryTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let accounts = self
            .inner
            .lock()
            .map(|held| held.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        f.debug_struct("MemoryTokenStore")
            .field("accounts", &accounts)
            .finish()
    }
}

impl TokenStore for MemoryTokenStore {
    fn save_refresh_token(&self, account_email: &str, token: &Secret) -> Result<(), AuthError> {
        self.inner
            .lock()
            .expect("token store mutex")
            .insert(account_email.to_string(), token.expose().to_string());
        Ok(())
    }

    fn load_refresh_token(&self, account_email: &str) -> Result<Option<Secret>, AuthError> {
        Ok(self
            .inner
            .lock()
            .expect("token store mutex")
            .get(account_email)
            .map(Secret::new))
    }

    fn delete_refresh_token(&self, account_email: &str) -> Result<(), AuthError> {
        self.inner
            .lock()
            .expect("token store mutex")
            .remove(account_email);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TokenManager
// ---------------------------------------------------------------------------

/// One account's refresh, and the last refusal it produced.
///
/// The lock is what makes a refresh single-flight: a caller that finds nothing
/// usable in the cache takes it, so the second, third and fifth caller for the
/// same account wait rather than each starting their own. `generation` is
/// bumped every time an attempt finishes under the lock, and a waiter compares
/// it against the value it read *before* it started waiting — a higher number
/// means somebody else's attempt landed while it queued, and that attempt's
/// answer is this caller's answer too.
///
/// Without the generation the waiter cannot tell "the attempt I waited for" from
/// "an attempt that finished long ago", and adopting the latter would serve a
/// stale error forever.
///
/// Only failures are kept here. A success is already in the token cache — that
/// is what `persist` does — so recording it a second time would be one more
/// copy of a refresh token sitting in memory for nothing.
#[derive(Default)]
struct RefreshGate {
    generation: AtomicU64,
    last_failure: tokio::sync::Mutex<Option<AuthError>>,
}

/// Holds live tokens for every account and keeps them valid.
///
/// [`Self::access_token`] is the only method the rest of the app should need: it
/// returns a token that is good for at least [`REFRESH_MARGIN_SECONDS`],
/// refreshing against Google first if it has to.
///
/// Generic over the HTTP transport and the store so the refresh logic is
/// testable with no network and no Keychain prompt.
///
/// # One read per account, not one per caller
///
/// A launch fans out: the sync engine spawns a task per account, each task runs
/// several Gmail and Calendar requests concurrently, and every one of them asks
/// for an access token. On a cold cache each of those was an independent
/// Keychain read *and* an independent refresh POST — five per account, twenty
/// five across five accounts, measured in `securityd`'s log as twenty five
/// password prompts for a single launch.
///
/// Two things stop that. [`Self::known_refresh`] remembers a refresh token the
/// moment it has been read once, so the Keychain is asked at most once per
/// account for the life of the process; [`RefreshGate`] makes the refresh itself
/// single-flight, so the callers that arrive during one share its result instead
/// of each spending a token-endpoint round trip.
pub struct TokenManager<H, S> {
    config: ClientConfig,
    http: H,
    store: S,
    /// Access tokens are memory-only, so this is the whole of their storage.
    cache: Mutex<HashMap<String, TokenSet>>,
    /// Refresh tokens already read out of the store, keyed by account.
    ///
    /// Only *successful* reads are remembered. A missing entry is not cached,
    /// because `errSecItemNotFound` costs nothing to repeat — it never raises a
    /// dialog, there is no item whose ACL macOS would have to ask about — and
    /// caching it would mean an account authorized after launch stayed
    /// `NotAuthorized` until the app was restarted.
    known_refresh: Mutex<HashMap<String, Secret>>,
    /// One gate per account. Five accounts, five entries; it never grows past
    /// the number of accounts that have asked for a token.
    gates: Mutex<HashMap<String, Arc<RefreshGate>>>,
}

impl<H, S> TokenManager<H, S>
where
    H: TokenHttp + Send + Sync,
    S: TokenStore,
{
    /// Read the persisted refresh token without stalling the async runtime.
    ///
    /// [`TokenStore::load_refresh_token`] is synchronous, and on macOS it is a
    /// Keychain call that can block for an unbounded time: if the item's ACL no
    /// longer matches the running binary, `securityd` puts up a password prompt
    /// and does not return until somebody answers it. Called straight from an
    /// `async fn`, that parks a Tokio worker for the whole of that wait, and
    /// everything else scheduled on it stops — which is exactly what happened:
    /// opening an attachment hung on its spinner and took the sync loop with it.
    ///
    /// `block_in_place` tells the multi-thread scheduler to move this worker's
    /// other tasks elsewhere first, so the runtime keeps running while this one
    /// thread waits. It panics on a current-thread runtime, which is what most
    /// `#[tokio::test]`s are, so the flavour is checked rather than assumed.
    ///
    /// What this does *not* do is bound the wait. The task is still blocked, so
    /// a timeout wrapped around the caller cannot fire — a task blocked inside
    /// its own poll is never polled again. Bounding it means moving the read to
    /// `spawn_blocking`, which needs `S: Clone + 'static` and a wider change
    /// than this. The remaining symptom is one stuck request rather than a
    /// stuck application.
    fn load_refresh_token_unblocking(
        &self,
        account_email: &str,
    ) -> Result<Option<Secret>, AuthError> {
        use tokio::runtime::{Handle, RuntimeFlavor};
        match Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(RuntimeFlavor::MultiThread) => {
                tokio::task::block_in_place(|| self.store.load_refresh_token(account_email))
            }
            _ => self.store.load_refresh_token(account_email),
        }
    }

    pub fn new(config: ClientConfig, http: H, store: S) -> Self {
        Self {
            config,
            http,
            store,
            cache: Mutex::new(HashMap::new()),
            known_refresh: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
        }
    }

    /// This account's refresh gate, created on first use.
    fn gate(&self, account_email: &str) -> Arc<RefreshGate> {
        let mut gates = self.gates.lock().expect("token gate mutex");
        Arc::clone(gates.entry(account_email.to_string()).or_default())
    }

    /// The cached access token, if it is good for longer than the safety margin.
    fn cached_access_token(&self, account_email: &str, now: DateTime<Utc>) -> Option<Secret> {
        let cache = self.cache.lock().expect("token cache mutex");
        cache
            .get(account_email)
            .filter(|tokens| !tokens.needs_refresh_at(now))
            .map(|tokens| tokens.access_token.clone())
    }

    /// The refresh token for an account if it is already in memory — either
    /// riding along on a cached access token, or remembered from an earlier
    /// store read. Does not touch the store.
    fn remembered_refresh_token(&self, account_email: &str) -> Option<Secret> {
        if let Some(token) = self
            .cache
            .lock()
            .expect("token cache mutex")
            .get(account_email)
            .and_then(|tokens| tokens.refresh_token.clone())
        {
            return Some(token);
        }
        self.known_refresh
            .lock()
            .expect("known refresh mutex")
            .get(account_email)
            .cloned()
    }

    fn remember_refresh_token(&self, account_email: &str, token: &Secret) {
        self.known_refresh
            .lock()
            .expect("known refresh mutex")
            .insert(account_email.to_string(), token.clone());
    }

    /// The refresh token to send to Google, from memory or, once, from the store.
    fn refresh_token_for(&self, account_email: &str) -> Result<Secret, AuthError> {
        if let Some(token) = self.remembered_refresh_token(account_email) {
            return Ok(token);
        }
        let token = self
            .load_refresh_token_unblocking(account_email)?
            .ok_or_else(|| AuthError::NotAuthorized(account_email.to_string()))?;
        self.remember_refresh_token(account_email, &token);
        Ok(token)
    }

    /// Whether this account still has a stored credential.
    ///
    /// The launch-time check (`ipc::state::restore_accounts_into`) used to build
    /// its own [`KeychainTokenStore`], read every account through it, and throw
    /// the tokens away — so the read that answered "is this account still signed
    /// in?" was one prompt, and the first API call a moment later was another
    /// for the same item. Asking the manager instead keeps what it read, and the
    /// requests behind it find it in memory.
    ///
    /// The store read here is the plain synchronous one, not
    /// [`Self::load_refresh_token_unblocking`]: this runs on a blocking thread
    /// that Tauri spawned for exactly this purpose, so there is no async worker
    /// to yield.
    pub fn has_stored_credential(&self, account_email: &str) -> Result<bool, AuthError> {
        if self.remembered_refresh_token(account_email).is_some() {
            return Ok(true);
        }
        match self.store.load_refresh_token(account_email)? {
            Some(token) => {
                self.remember_refresh_token(account_email, &token);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub fn http(&self) -> &H {
        &self.http
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// Seeds the in-memory cache, e.g. straight after an interactive
    /// authorization performed elsewhere.
    ///
    /// A refresh token arriving this way is remembered too — the sign-in flow
    /// writes to the Keychain through its own store, and without this the next
    /// request would go and read back the item that was just written.
    pub fn insert_tokens(&self, account_email: &str, tokens: TokenSet) {
        if let Some(token) = &tokens.refresh_token {
            self.remember_refresh_token(account_email, token);
        }
        self.cache
            .lock()
            .expect("token cache mutex")
            .insert(account_email.to_string(), tokens);
    }

    /// Forgets an account's access token without touching the Keychain.
    ///
    /// The remembered refresh token stays: it is what the *next* call would read
    /// out of the Keychain anyway, and dropping it here would put the prompt
    /// back. [`Self::sign_out`] is the one that forgets the credential.
    pub fn forget_cached(&self, account_email: &str) {
        self.cache
            .lock()
            .expect("token cache mutex")
            .remove(account_email);
    }

    /// Drops the account entirely: cached access token, remembered refresh
    /// token, and the stored one.
    pub fn sign_out(&self, account_email: &str) -> Result<(), AuthError> {
        self.forget_cached(account_email);
        self.known_refresh
            .lock()
            .expect("known refresh mutex")
            .remove(account_email);
        self.gates
            .lock()
            .expect("token gate mutex")
            .remove(account_email);
        self.store.delete_refresh_token(account_email)
    }

    /// A valid access token for `account_email`, refreshing if it is expired or
    /// within the safety margin.
    ///
    /// Concurrent callers for the same account collapse onto one attempt. The
    /// first through the gate does the Keychain read and the token-endpoint
    /// POST; the rest wait, and then take either the token it cached or, if it
    /// failed, a clone of the error it got.
    pub async fn access_token(&self, account_email: &str) -> Result<Secret, AuthError> {
        if let Some(token) = self.cached_access_token(account_email, Utc::now()) {
            return Ok(token);
        }

        let gate = self.gate(account_email);
        // Read before the wait, so anything published while we wait is visibly
        // newer than what we already knew about.
        let seen = gate.generation.load(Ordering::Acquire);
        let mut last_failure = gate.last_failure.lock().await;

        // Somebody may have refreshed while we were queued. A success landed in
        // the cache, so that check comes first and covers it; only a refusal has
        // to be read back out of the gate.
        if let Some(token) = self.cached_access_token(account_email, Utc::now()) {
            return Ok(token);
        }
        if gate.generation.load(Ordering::Acquire) > seen {
            if let Some(error) = last_failure.as_ref() {
                return Err(error.clone());
            }
        }

        let outcome = self.refresh_once(account_email).await;
        Self::publish(&gate, &mut last_failure, &outcome);
        outcome.map(|tokens| tokens.access_token)
    }

    /// Records an attempt's answer for whoever is waiting behind the gate.
    fn publish(
        gate: &RefreshGate,
        last_failure: &mut Option<AuthError>,
        outcome: &Result<TokenSet, AuthError>,
    ) {
        *last_failure = outcome.as_ref().err().cloned();
        // Released after the answer is stored, so a waiter that sees the new
        // generation sees the answer that goes with it.
        gate.generation.fetch_add(1, Ordering::Release);
    }

    /// One refresh: find the refresh token, spend it. Callers hold the gate.
    async fn refresh_once(&self, account_email: &str) -> Result<TokenSet, AuthError> {
        let refresh_token = self.refresh_token_for(account_email)?;
        self.refresh_with(account_email, &refresh_token).await
    }

    /// Trades an authorization code for tokens and persists the refresh token.
    ///
    /// `redirect_uri` must be the exact string sent in the authorization
    /// request, i.e. [`oauth::LoopbackServer::redirect_uri`] from the same
    /// listener.
    pub async fn exchange_code(
        &self,
        account_email: &str,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenSet, AuthError> {
        let form = oauth::exchange_form(&self.config, code, code_verifier, redirect_uri);
        let response = self.http.post_form(oauth::TOKEN_ENDPOINT, &form).await?;
        if !response.is_success() {
            return Err(oauth::token_error(&response));
        }

        let tokens = TokenSet::from_json(&response.body, Utc::now(), None)?;
        self.persist(account_email, &tokens)?;
        Ok(tokens)
    }

    /// Forces a refresh regardless of expiry. Prefer [`Self::access_token`].
    ///
    /// Takes the account's gate, so it serialises with a lazy refresh rather
    /// than racing one, and publishes its answer to anything waiting. It does
    /// *not* adopt a result that landed while it waited — "force" is the whole
    /// of what this method means.
    pub async fn refresh(&self, account_email: &str) -> Result<TokenSet, AuthError> {
        let gate = self.gate(account_email);
        let mut last_failure = gate.last_failure.lock().await;
        let outcome = self.refresh_once(account_email).await;
        Self::publish(&gate, &mut last_failure, &outcome);
        outcome
    }

    async fn refresh_with(
        &self,
        account_email: &str,
        refresh_token: &Secret,
    ) -> Result<TokenSet, AuthError> {
        let form = oauth::refresh_form(&self.config, refresh_token.expose());
        let response = self.http.post_form(oauth::TOKEN_ENDPOINT, &form).await?;
        if !response.is_success() {
            // `invalid_grant` here is how a revoked token — or the 7-day expiry
            // on an unverified External app — surfaces, and
            // [`oauth::refresh_error`] is what tells it apart from a transient
            // refusal. The credential is not deleted either way: the account
            // row keeps its Keychain entry, and "Sign in again" overwrites it.
            return Err(oauth::refresh_error(&response));
        }

        let tokens = TokenSet::from_json(
            &response.body,
            Utc::now(),
            Some(refresh_token.clone()),
        )?;
        self.persist(account_email, &tokens)?;
        Ok(tokens)
    }

    fn persist(&self, account_email: &str, tokens: &TokenSet) -> Result<(), AuthError> {
        if let Some(rt) = &tokens.refresh_token {
            // Only when it has actually changed. A refresh response almost never
            // carries a new refresh token — [`TokenSet::from_json`] carries the
            // old one forward — so this used to rewrite the same bytes into the
            // Keychain after every refresh, once an hour per account.
            //
            // A write is not free the way an in-memory one is. It reaches into
            // the same item, under the same ACL and the same partition list as a
            // read, and gets asked about on the same terms. Rotation still lands:
            // a token Google *did* replace differs, and is written.
            if self.remembered_refresh_token(account_email).as_ref() != Some(rt) {
                self.store.save_refresh_token(account_email, rt)?;
            }
        }
        // Remembers the refresh token as well as caching the access token.
        self.insert_tokens(account_email, tokens.clone());
        Ok(())
    }
}

impl<H, S> fmt::Debug for TokenManager<H, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let accounts: Vec<String> = self
            .cache
            .lock()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default();
        f.debug_struct("TokenManager")
            .field("config", &self.config)
            .field("cached_accounts", &accounts)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's service name is the one thing here that must not move: every
    /// refresh token he has is filed under it, and changing it would silently
    /// log all five accounts out.
    ///
    /// `MACH_DATA_DIR` is process-wide, so this shares `shell::ENV_LOCK` with
    /// the tests in `shell` and `qa` that mutate the same variable.
    #[test]
    fn the_owners_instance_keeps_the_service_name_it_has_always_had() {
        let _guard = crate::shell::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        std::env::remove_var("MACH_DATA_DIR");
        assert_eq!(keychain_service(), "com.mach.mail.oauth");
        assert_eq!(
            KeychainTokenStore::default().service(),
            "com.mach.mail.oauth",
            "the store the owner's app builds must address the owner's entries"
        );
    }

    /// The defect this exists for: a QA instance asking for the owner's key.
    #[test]
    fn a_qa_instance_cannot_name_the_owners_entries() {
        let _guard = crate::shell::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        std::env::set_var("MACH_DATA_DIR", "/Users/owner/mach/.qa/agent/data");
        let service = keychain_service();
        assert_eq!(service, "com.mach.mail.oauth.qa-agent");
        assert_ne!(service, KEYCHAIN_SERVICE);
        assert_eq!(KeychainTokenStore::default().service(), service);

        // A second instance is a third namespace, not a shared one.
        std::env::set_var("MACH_DATA_DIR", "/Users/owner/mach/.qa/review/data");
        assert_eq!(keychain_service(), "com.mach.mail.oauth.qa-review");

        std::env::remove_var("MACH_DATA_DIR");
        assert_eq!(
            keychain_service(),
            KEYCHAIN_SERVICE,
            "unsetting the variable must restore the owner's namespace"
        );
    }

    #[test]
    fn an_instance_is_named_by_the_directory_holding_its_store() {
        assert_eq!(instance_name(Path::new(".qa/agent/data")), "agent");
        assert_eq!(instance_name(Path::new("/repo/.qa/agent/data/")), "agent");
        // A hand-set MACH_DATA_DIR that is not laid out like `scripts/qa`.
        assert_eq!(instance_name(Path::new("/tmp/mach-qa")), "mach-qa");
        // Anything that is not a name still has to produce one, because the
        // alternative is falling back to the owner's service.
        assert_eq!(instance_name(Path::new("/")), UNNAMED_QA_INSTANCE);
        assert_eq!(instance_name(Path::new("data")), UNNAMED_QA_INSTANCE);
        assert_eq!(instance_name(Path::new("/x/Feature Branch!/data")), "feature-branch");
    }

    /// Whatever the data directory is called, the result is never the owner's
    /// service and is always a well-formed name under the QA prefix.
    #[test]
    fn no_data_directory_can_produce_the_owners_service() {
        for dir in [
            "/",
            "data",
            "com.mach.mail/data",
            "com.mach.mail.oauth",
            "../../Library/Application Support/com.mach.mail",
            "",
        ] {
            let service = format!("{QA_KEYCHAIN_PREFIX}{}", instance_name(Path::new(dir)));
            assert_ne!(service, KEYCHAIN_SERVICE, "{dir} reached the owner's service");
            assert!(service.starts_with(QA_KEYCHAIN_PREFIX), "{dir} -> {service}");
            assert!(service.len() > QA_KEYCHAIN_PREFIX.len(), "{dir} -> {service}");
        }
    }

    /// A QA instance with no credentials of its own must answer, not wait.
    ///
    /// The store double is what a Keychain lookup for a service with no items
    /// does: `errSecItemNotFound`, which `keyring` reports as `NoEntry` and
    /// [`KeychainTokenStore::load_refresh_token`] turns into `Ok(None)`. There
    /// is no dialog on that path because there is no item to authorize.
    #[test]
    fn an_empty_store_answers_immediately() {
        let store = MemoryTokenStore::default();
        let started = std::time::Instant::now();
        let answer = store.load_refresh_token("someone@example.com");
        assert!(matches!(answer, Ok(None)));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
