//! The concrete HTTPS transport for Google's token endpoint.
//!
//! Everything else in `auth` is testable without a network because it goes
//! through the [`TokenHttp`](super::oauth::TokenHttp) seam. This module is the
//! one place that actually opens a socket, which is why it holds no logic worth
//! testing — decisions live on the other side of the seam.

use super::oauth::{HttpResponse, TokenHttp};
use super::AuthError;

/// Talks to Google's token endpoint over HTTPS.
///
/// Prefer [`GoogleTokenHttp::with_client`] and hand it the same
/// `reqwest::Client` the API clients use: a `Client` owns a connection pool, so
/// sharing one keeps five accounts on a single set of TLS connections instead
/// of five.
pub struct GoogleTokenHttp {
    client: reqwest::Client,
}

impl GoogleTokenHttp {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for GoogleTokenHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleTokenHttp {
    /// A bearer-authenticated GET.
    ///
    /// Only used to resolve `userinfo` during account authorization — the
    /// account's email is not known until a token exists. Ordinary API traffic
    /// belongs to the `google` clients, which have their own retry and error
    /// mapping; duplicating that here would be a second, worse HTTP stack.
    pub async fn get_with_bearer(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<HttpResponse, AuthError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;

        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;

        Ok(HttpResponse { status, body })
    }
}

impl TokenHttp for GoogleTokenHttp {
    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<HttpResponse, AuthError> {
        let response = self
            .client
            .post(url)
            .form(form)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;

        // The status is captured before the body, because a non-2xx from the
        // token endpoint still carries a JSON error the caller needs to read.
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;

        Ok(HttpResponse { status, body })
    }
}
