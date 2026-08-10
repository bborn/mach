//! OAuth 2.0 against Google, and the tokens that come out of it.
//!
//! # Flow
//!
//! Authorization Code + PKCE (RFC 7636) with a loopback redirect, which is the
//! only flow Google supports for native desktop apps. Per account:
//!
//! 1. [`oauth::LoopbackServer::bind`] binds `127.0.0.1:0` and reads back the
//!    OS-assigned port. The redirect URI is derived from that port, never
//!    hardcoded.
//! 2. [`oauth::AuthSession`] carries a fresh PKCE verifier/challenge pair and a
//!    random `state`.
//! 3. [`oauth::authorization_url`] is opened in the user's browser.
//! 4. The loopback server answers exactly one request, shows "You can close this
//!    window", and shuts down.
//! 5. [`oauth::validate_callback`] rejects any callback whose `state` does not
//!    match — this is the CSRF check and it is not optional.
//! 6. [`tokens::TokenManager::exchange_code`] trades the code for tokens and
//!    writes the refresh token into the macOS Keychain.
//!
//! Thereafter [`tokens::TokenManager::access_token`] returns a valid access
//! token, refreshing transparently inside a
//! [`tokens::REFRESH_MARGIN_SECONDS`] safety margin.
//!
//! # Configuration the app expects
//!
//! Nothing about the Google Cloud project is hardcoded. Two environment
//! variables are read at startup:
//!
//! | Variable | Required | Value |
//! |---|---|---|
//! | `MACH_GOOGLE_CLIENT_ID` | yes | the OAuth client ID, `…apps.googleusercontent.com` |
//! | `MACH_GOOGLE_CLIENT_SECRET` | practically yes | the `GOCSPX-…` value Google issues alongside it |
//!
//! The OAuth client must be created with application type **Desktop app**.
//! Desktop clients are the only type for which Google accepts a loopback
//! redirect on an arbitrary port; a "Web application" client would require every
//! port to be pre-registered, which is incompatible with the ephemeral port this
//! module binds. Desktop clients still receive a client secret, and Google's
//! token endpoint expects it to be sent — it is not a confidential credential in
//! a native app (RFC 8252 §8.5), which is exactly why PKCE is mandatory here.
//!
//! # Secrets
//!
//! Refresh tokens live in the macOS Keychain (see [`tokens::KeychainTokenStore`]).
//! Access tokens live only in memory. Nothing in this module writes a token to
//! disk or to a log; every type that holds one implements `Debug` by hand so a
//! stray `{:?}` cannot leak it. There are tests for that.
//!
//! The Keychain is global to the machine, so the service name is not. It comes
//! from [`tokens::keychain_service`]: the owner's instance uses
//! [`tokens::KEYCHAIN_SERVICE`], and every QA instance gets a namespace of its
//! own and cannot address his entries at all.

pub mod flow;
pub mod http;
pub mod oauth;
pub mod tokens;

use std::fmt;

/// Environment variable holding the Google OAuth client ID.
pub const ENV_CLIENT_ID: &str = "MACH_GOOGLE_CLIENT_ID";

/// Environment variable holding the Google OAuth client secret.
pub const ENV_CLIENT_SECRET: &str = "MACH_GOOGLE_CLIENT_SECRET";

/// Everything that can go wrong between "open the browser" and "here is a valid
/// access token".
///
/// Deliberately carries no token material in any variant. [`Self::TokenEndpoint`]
/// carries Google's *error* response body, which by definition is not a success
/// response and therefore contains no credentials.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing configuration: set {0}")]
    MissingConfig(&'static str),

    #[error("code_verifier does not satisfy RFC 7636: {0}")]
    InvalidVerifier(String),

    #[error("could not read cryptographically secure randomness: {0}")]
    Randomness(String),

    #[error("could not bind the loopback redirect listener: {0}")]
    LoopbackBind(String),

    #[error("loopback redirect listener failed: {0}")]
    Loopback(String),

    #[error("timed out waiting for the OAuth redirect")]
    CallbackTimeout,

    #[error("OAuth callback is missing the `{0}` parameter")]
    MissingCallbackParameter(&'static str),

    /// The `state` in the callback did not match the one we generated. Treat as
    /// hostile: drop the code on the floor.
    #[error("OAuth callback `state` did not match — possible CSRF, authorization rejected")]
    StateMismatch,

    #[error("authorization was denied: {error}{}", .description.as_ref().map(|d| format!(" ({d})")).unwrap_or_default())]
    AuthorizationDenied {
        error: String,
        description: Option<String>,
    },

    #[error("Google token endpoint rejected the request: {0}")]
    TokenEndpoint(String),

    #[error("could not parse the token response: {0}")]
    MalformedTokenResponse(String),

    #[error("no credentials for {0}; the account must be authorized first")]
    NotAuthorized(String),

    #[error("keychain error: {0}")]
    Keychain(String),

    #[error("http transport error: {0}")]
    Transport(String),
}

/// The Google Cloud OAuth client this app authenticates as.
///
/// Injected, never hardcoded — see the module docs for the env vars.
#[derive(Clone)]
pub struct ClientConfig {
    client_id: String,
    client_secret: Option<tokens::Secret>,
}

impl ClientConfig {
    pub fn new(client_id: impl Into<String>, client_secret: Option<impl Into<String>>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.map(|s| tokens::Secret::new(s)),
        }
    }

    /// Reads [`ENV_CLIENT_ID`] and [`ENV_CLIENT_SECRET`] from the process
    /// environment. Empty values count as absent.
    pub fn from_env() -> Result<Self, AuthError> {
        Self::from_values(
            std::env::var(ENV_CLIENT_ID).ok(),
            std::env::var(ENV_CLIENT_SECRET).ok(),
        )
    }

    /// The env-independent core of [`Self::from_env`], so configuration handling
    /// is testable without mutating a shared process environment.
    pub fn from_values(
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Result<Self, AuthError> {
        let client_id = client_id
            .filter(|v| !v.trim().is_empty())
            .ok_or(AuthError::MissingConfig(ENV_CLIENT_ID))?;
        let client_secret = client_secret
            .filter(|v| !v.trim().is_empty())
            .map(tokens::Secret::new);
        Ok(Self {
            client_id,
            client_secret,
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn client_secret(&self) -> Option<&tokens::Secret> {
        self.client_secret.as_ref()
    }
}

/// Hand-written so the client secret cannot escape through a `{:?}`.
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &if self.client_secret.is_some() {
                    "<redacted>"
                } else {
                    "<unset>"
                },
            )
            .finish()
    }
}
