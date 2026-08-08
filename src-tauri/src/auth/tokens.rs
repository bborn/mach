//! Token lifetime and storage.
//!
//! Refresh tokens go in the macOS Keychain, keyed by account email. Access
//! tokens stay in memory and are refreshed transparently inside a safety margin.
//! Every type in here that holds secret material implements `Debug` by hand so
//! it cannot leak through a stray `{:?}`; `tests/auth.rs` asserts that.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use super::oauth::{self, TokenHttp};
use super::{AuthError, ClientConfig};

/// Refresh this many seconds before the access token actually expires, so a
/// request never leaves with a token that dies in flight.
pub const REFRESH_MARGIN_SECONDS: i64 = 60;

/// Keychain service name. Every account is an entry under this service, with the
/// account's email address as the entry's username.
pub const KEYCHAIN_SERVICE: &str = "com.mach.mail.oauth";

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
#[derive(Debug, Clone)]
pub struct KeychainTokenStore {
    service: String,
}

impl Default for KeychainTokenStore {
    fn default() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_string(),
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
#[derive(Debug, Default)]
pub struct MemoryTokenStore {
    inner: Mutex<HashMap<String, String>>,
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

/// Holds live tokens for every account and keeps them valid.
///
/// [`Self::access_token`] is the only method the rest of the app should need: it
/// returns a token that is good for at least [`REFRESH_MARGIN_SECONDS`],
/// refreshing against Google first if it has to.
///
/// Generic over the HTTP transport and the store so the refresh logic is
/// testable with no network and no Keychain prompt.
pub struct TokenManager<H, S> {
    config: ClientConfig,
    http: H,
    store: S,
    /// Access tokens are memory-only, so this is the whole of their storage.
    cache: Mutex<HashMap<String, TokenSet>>,
}

impl<H, S> TokenManager<H, S>
where
    H: TokenHttp + Send + Sync,
    S: TokenStore,
{
    pub fn new(config: ClientConfig, http: H, store: S) -> Self {
        Self {
            config,
            http,
            store,
            cache: Mutex::new(HashMap::new()),
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
    pub fn insert_tokens(&self, account_email: &str, tokens: TokenSet) {
        self.cache
            .lock()
            .expect("token cache mutex")
            .insert(account_email.to_string(), tokens);
    }

    /// Forgets an account's access token without touching the Keychain.
    pub fn forget_cached(&self, account_email: &str) {
        self.cache
            .lock()
            .expect("token cache mutex")
            .remove(account_email);
    }

    /// Drops the account entirely: cached access token and stored refresh token.
    pub fn sign_out(&self, account_email: &str) -> Result<(), AuthError> {
        self.forget_cached(account_email);
        self.store.delete_refresh_token(account_email)
    }

    /// A valid access token for `account_email`, refreshing if it is expired or
    /// within the safety margin.
    pub async fn access_token(&self, account_email: &str) -> Result<Secret, AuthError> {
        let now = Utc::now();

        // Copy out and drop the guard: the Mutex must not be held across await.
        let cached = {
            let cache = self.cache.lock().expect("token cache mutex");
            cache.get(account_email).cloned()
        };

        if let Some(tokens) = &cached {
            if !tokens.needs_refresh_at(now) {
                return Ok(tokens.access_token.clone());
            }
        }

        // Prefer the refresh token we already have in memory; fall back to the
        // Keychain, which is the cold-start path.
        let refresh_token = match cached.as_ref().and_then(|t| t.refresh_token.clone()) {
            Some(rt) => rt,
            None => self
                .store
                .load_refresh_token(account_email)?
                .ok_or_else(|| AuthError::NotAuthorized(account_email.to_string()))?,
        };

        let tokens = self.refresh_with(account_email, &refresh_token).await?;
        Ok(tokens.access_token)
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
    pub async fn refresh(&self, account_email: &str) -> Result<TokenSet, AuthError> {
        let refresh_token = {
            let cached = {
                let cache = self.cache.lock().expect("token cache mutex");
                cache.get(account_email).cloned()
            };
            match cached.and_then(|t| t.refresh_token) {
                Some(rt) => rt,
                None => self
                    .store
                    .load_refresh_token(account_email)?
                    .ok_or_else(|| AuthError::NotAuthorized(account_email.to_string()))?,
            }
        };
        self.refresh_with(account_email, &refresh_token).await
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
            // on an unverified External app — surfaces. The caller decides
            // whether to re-authorize; we do not silently delete the credential.
            return Err(oauth::token_error(&response));
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
            self.store.save_refresh_token(account_email, rt)?;
        }
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
