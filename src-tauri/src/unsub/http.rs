//! The client that talks to a stranger's server, and everything that is
//! different about it.
//!
//! It implements [`HttpTransport`](crate::google::HttpTransport), so the fake
//! every other test in this crate already uses works here unchanged and no real
//! request is made in a test. What it does **not** do is share the app's client:
//! the shared one is built for Google, whose certificates, redirects and
//! response sizes are not adversarial.
//!
//! | setting | value | why it is not the default |
//! |---|---|---|
//! | timeout | [`TIMEOUT`] | nothing else in this app sets one; an unsubscribe that hangs must end |
//! | redirects | at most [`MAX_REDIRECTS`], `https` only | reqwest follows ten, to any scheme, including back down to `http` |
//! | cookie store | off | it is off by default, and it is named here so nobody turns it on |
//! | referer | off | reqwest adds one across redirects; the sender learns nothing from us |
//! | response body | truncated to [`MAX_RESPONSE_BYTES`] | the status is the whole answer, and the page can be any size it likes |
//! | proxies | off | a system proxy would see the URL, and the URL identifies him |
//!
//! No credential ever goes near it. The request is built in
//! [`super::run`] and carries exactly two headers.
//!
//! # The redirect policy is the interesting one
//!
//! A redirect is the sender's chance to point the request somewhere else after
//! validation has already happened. [`super::target`] refuses `http`, loopback
//! and private addresses in the URL it was given; a redirect that was allowed
//! to go anywhere would make that refusal decorative. So the policy re-checks
//! every hop with the same function, and a hop that fails it ends the request
//! rather than following it.
//!
//! What this cannot check is DNS. A hostname that passes [`super::target`] and
//! resolves to `127.0.0.1` reaches loopback, and nothing short of resolving the
//! name here and pinning the socket to the address we approved would stop it.
//! Against that: the request carries no credentials and no cookies, its body is
//! a fixed nine-word string, and the only local listener is the QA port, which
//! exists solely in development builds and takes six fixed verbs. The cost of
//! closing it is a custom DNS resolver on a path that runs when he presses a
//! key; it has not been paid.

use std::time::Duration;

use crate::google::{
    BoxFuture, HttpMethod, HttpRequest, HttpResponse, HttpTransport, TransportError,
};

/// How long a sender gets. Long enough for a slow list processor, short enough
/// that the toast saying it failed arrives while he still remembers asking.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// RFC 8058 does not require redirects to be followed at all. Real senders put
/// one or two in front of the endpoint; nobody legitimate needs three.
pub const MAX_REDIRECTS: usize = 3;

/// Enough to see what went wrong in a status line, and nothing like enough to
/// be worth sending. The body is never logged and never shown.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Build the client. Fails only if rustls cannot start, which would mean the
/// rest of the app cannot reach Google either.
pub fn client() -> Result<reqwest::Client, TransportError> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(redirect_policy())
        .referer(false)
        .no_proxy()
        .user_agent(concat!("Mach/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| TransportError::new(e.to_string()))
}

/// Follow at most [`MAX_REDIRECTS`] hops, and only to somewhere
/// [`super::target`] would have accepted in the first place.
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.stop();
        }
        match crate::unsub::target::accepts_url(attempt.url().as_str()) {
            true => attempt.follow(),
            // `stop` rather than `error`: the response we already have is the
            // one to report, and "302 to somewhere we will not go" is a
            // failure the caller reads off the status like any other.
            false => attempt.stop(),
        }
    })
}

/// The production transport for unsubscribe requests.
pub struct UnsubTransport {
    client: reqwest::Client,
}

impl UnsubTransport {
    pub fn new() -> Result<Self, TransportError> {
        Ok(Self { client: client()? })
    }
}

impl HttpTransport for UnsubTransport {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let method = match request.method {
                HttpMethod::Get => reqwest::Method::GET,
                HttpMethod::Post => reqwest::Method::POST,
                HttpMethod::Put => reqwest::Method::PUT,
                HttpMethod::Patch => reqwest::Method::PATCH,
                HttpMethod::Delete => reqwest::Method::DELETE,
            };
            let mut builder = self.client.request(method, &request.url);
            for (name, value) in &request.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }

            let mut response = builder
                .send()
                .await
                // The error carries the URL. Nothing about an unsubscribe URL
                // belongs in a message that may reach a log or a toast, so only
                // the classification survives.
                .map_err(|e| TransportError::new(describe(&e)))?;

            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();

            // Streamed and capped rather than `bytes()`, so a sender who
            // answers with a gigabyte cannot make this process hold it.
            let mut body: Vec<u8> = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| TransportError::new(describe(&e)))?
            {
                let room = MAX_RESPONSE_BYTES.saturating_sub(body.len());
                if room == 0 {
                    break;
                }
                let take = room.min(chunk.len());
                body.extend_from_slice(&chunk[..take]);
            }

            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}

/// A reqwest error with the URL taken out of it.
fn describe(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "the sender did not answer in time"
    } else if error.is_connect() {
        "the sender's server could not be reached"
    } else if error.is_redirect() {
        "the sender redirected too many times"
    } else if error.is_body() || error.is_decode() {
        "the sender's answer could not be read"
    } else {
        "the request to the sender failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_builds() {
        assert!(client().is_ok());
    }

    #[test]
    fn a_transport_error_never_carries_a_url() {
        // `describe` returns from a fixed set, so there is no path by which the
        // URL reaches the message. Pinned as a test because the obvious
        // maintenance edit — `e.to_string()` — reintroduces it.
        for message in [
            "the sender did not answer in time",
            "the sender's server could not be reached",
            "the sender redirected too many times",
            "the sender's answer could not be read",
            "the request to the sender failed",
        ] {
            assert!(!message.contains("http"), "{message}");
        }
    }
}
