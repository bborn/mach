//! Authorizing a new Google account, end to end.
//!
//! Ties together the pieces in [`oauth`](super::oauth) and
//! [`tokens`](super::tokens): bind a loopback listener, send the user to
//! Google, catch the redirect, trade the code for tokens, and find out which
//! account actually consented.
//!
//! # Why the email is discovered rather than asked for
//!
//! Tokens are stored per account email, but at the moment the flow starts we do
//! not know which account the user will pick — the Google account chooser is
//! shown *after* we build the URL. Asking first would let the two disagree: type
//! one address, consent as another, and the tokens land under a key that does
//! not match the mailbox they unlock.
//!
//! So the email comes from the token itself, via `userinfo`, and only then is
//! anything persisted.

use std::time::Duration;

use serde::Deserialize;

use super::http::GoogleTokenHttp;
use super::oauth::{
    authorization_url, validate_callback, AuthSession, LoopbackServer, TokenHttp,
};
use super::tokens::{TokenSet, TokenStore};
use super::{AuthError, ClientConfig};

/// Where an access token is exchanged for the identity that owns it.
const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v3/userinfo";

/// How long to wait for the user to finish consenting before giving up. Long
/// enough to pick an account, read the unverified-app warning, and click
/// through it.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
struct UserInfo {
    email: String,
}

/// A newly authorized account.
#[derive(Debug)]
pub struct AuthorizedAccount {
    pub email: String,
    pub tokens: TokenSet,
}

/// The half of the flow that must happen before the browser opens.
///
/// Split out from [`complete_authorization`] so a caller can show the URL,
/// open a browser, or drive it from a test without this module deciding how a
/// browser gets launched.
pub struct PendingAuthorization {
    session: AuthSession,
    server: LoopbackServer,
    /// Where to send the user.
    pub url: String,
}

impl PendingAuthorization {
    pub fn redirect_uri(&self) -> String {
        self.server.redirect_uri()
    }
}

/// Binds the loopback listener and builds the authorization URL.
///
/// `login_hint` pre-selects an account in Google's chooser, which matters when
/// adding the fourth of five. It is only a hint — the user can still pick
/// someone else, which is exactly why the email is read back from the token
/// rather than trusted from here.
///
/// `authorization_url` always sends `access_type=offline` and `prompt=consent`,
/// so every run returns a refresh token. Google issues one only on first
/// consent otherwise, and a re-authorization without it would yield an access
/// token that expires in an hour with no way to renew.
pub fn begin_authorization(
    config: &ClientConfig,
    login_hint: Option<&str>,
) -> Result<PendingAuthorization, AuthError> {
    let server = LoopbackServer::bind()?;
    let redirect_uri = server.redirect_uri();
    let session = AuthSession::start(&redirect_uri)?;
    let url = authorization_url(config, &session, login_hint);

    Ok(PendingAuthorization {
        session,
        server,
        url,
    })
}

/// Waits for the redirect, exchanges the code, and resolves the account email.
///
/// Nothing is persisted before the email is known — see the module docs.
pub async fn complete_authorization<S: TokenStore>(
    pending: PendingAuthorization,
    config: &ClientConfig,
    http: &GoogleTokenHttp,
    store: &S,
) -> Result<AuthorizedAccount, AuthError> {
    let PendingAuthorization {
        session,
        server,
        url: _,
    } = pending;

    let redirect_uri = server.redirect_uri();
    let callback = server.wait_for_callback(CONSENT_TIMEOUT)?;
    let code = validate_callback(callback, &session.state)?;

    let form = super::oauth::exchange_form(config, &code, session.pkce.verifier(), &redirect_uri);
    let response = http.post_form(super::oauth::TOKEN_ENDPOINT, &form).await?;
    if !response.is_success() {
        return Err(super::oauth::token_error(&response));
    }
    let tokens = TokenSet::from_json(&response.body, chrono::Utc::now(), None)?;

    let email = fetch_email(http, &tokens).await?;

    if let Some(refresh) = tokens.refresh_token.as_ref() {
        store.save_refresh_token(&email, refresh)?;
    }

    Ok(AuthorizedAccount { email, tokens })
}

/// Asks Google which account the token belongs to.
async fn fetch_email(http: &GoogleTokenHttp, tokens: &TokenSet) -> Result<String, AuthError> {
    let body = http
        .get_with_bearer(USERINFO_ENDPOINT, tokens.access_token.expose())
        .await?;

    if !body.is_success() {
        return Err(AuthError::Transport(format!(
            "userinfo returned HTTP {}: {}",
            body.status, body.body
        )));
    }

    let info: UserInfo = serde_json::from_str(&body.body)
        .map_err(|e| AuthError::Transport(format!("userinfo was not the expected shape: {e}")))?;

    Ok(info.email)
}
