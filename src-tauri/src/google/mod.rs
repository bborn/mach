//! Typed clients for the Gmail and Google Calendar REST APIs.
//!
//! Two deliberate seams run through this module:
//!
//! 1. **The clients do not own authentication.** They ask a [`TokenProvider`]
//!    for a bearer token before every attempt. The OAuth module implements that
//!    trait; neither side knows the other's internals.
//! 2. **The clients do not own HTTP.** Every request goes through an
//!    [`HttpTransport`]. Production wires a real one; tests script responses.
//!    The same seam makes the retry loop testable, because [`Sleeper`] is
//!    injectable too — the backoff tests assert on the delays that *would* have
//!    been slept without ever sleeping.
//!
//! The error enum is the third load-bearing piece. The sync engine's control
//! flow depends on telling a 401 from a 429 from an expired history watermark,
//! so those are separate named variants rather than one HTTP error.

pub mod calendar;
pub mod gmail;
pub mod types;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use url::Url;

/// Default production endpoints. Overridable per client so tests (and any
/// future proxy) can point somewhere else.
pub const GMAIL_BASE_URL: &str = "https://gmail.googleapis.com/gmail/v1";
pub const CALENDAR_BASE_URL: &str = "https://www.googleapis.com/calendar/v3";

/// Boxed future alias. Hand-rolled rather than pulling in `async-trait`, and
/// boxed rather than `impl Future` so the traits stay object safe — the clients
/// hold `Arc<dyn HttpTransport>`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ==================================================================== errors

/// Everything the API layer can fail with, split along the lines the sync
/// engine actually branches on.
#[derive(Debug, thiserror::Error)]
pub enum GoogleError {
    /// 401. The token is dead; refresh it or re-consent.
    #[error("google auth failed (401): {message}")]
    Auth { message: String },

    /// 429, or a 403 whose reason is a rate/quota limit. Back off and retry.
    #[error("google rate limited ({status}): {message}")]
    RateLimited {
        status: u16,
        message: String,
        /// Value of a `Retry-After` header, when Google sent one.
        retry_after: Option<Duration>,
    },

    /// A 403 that is *not* a rate limit — an admin policy, a resource somebody
    /// else owns. Retrying will not help.
    #[error("google forbidden (403): {message}")]
    Forbidden { message: String, reason: Option<String> },

    /// **The grant is too narrow.** A 403 whose reason is
    /// `insufficientPermissions`: the token is alive, the account is fine, and
    /// the scope this endpoint needs was never consented to.
    ///
    /// Its own variant rather than a [`Forbidden`](GoogleError::Forbidden)
    /// because the remedy is different and only the user can perform it. A dead
    /// token refreshes itself; a revoked one produces a 401 and re-authorizing
    /// fixes it; this one survives every refresh forever, because refreshing a
    /// token cannot widen it. It appears the moment [`SCOPES`] grows and it goes
    /// away only when the owner walks through the consent screen again.
    ///
    /// [`SCOPES`]: crate::auth::oauth::SCOPES
    #[error("google refused because the account has not granted this permission (403): {message}")]
    InsufficientScope { message: String },

    /// **The watermark is too old.** `users.history.list` answers 404 when the
    /// stored `historyId` has aged out of Gmail's retention window. It is not a
    /// missing-resource error: it means discard the watermark and do a full
    /// resync. This is the single most important error case in the sync design,
    /// which is why it is its own variant.
    #[error("gmail history id expired; a full resync is required: {message}")]
    HistoryExpired { message: String },

    /// The calendar equivalent: `events.list` answers 410 `fullSyncRequired`
    /// when a stored `syncToken` has expired. Drop the token, re-list the
    /// window.
    #[error("calendar sync token expired; a full resync is required: {message}")]
    SyncTokenExpired { message: String },

    /// 404 on anything other than a history sweep.
    #[error("google resource not found: {message}")]
    NotFound { message: String },

    /// A request this client would not send, caught before it goes out.
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    /// Any other non-2xx.
    #[error("google api error {status}: {message}")]
    Api {
        status: u16,
        message: String,
        reason: Option<String>,
    },

    /// The request never got an answer — DNS, TLS, connect, timeout.
    #[error("network error talking to google: {message}")]
    Network { message: String },

    /// A 2xx whose body was not the shape we expected.
    #[error("could not deserialize google response: {message}")]
    Deserialize { message: String },
}

impl GoogleError {
    pub fn is_auth(&self) -> bool {
        matches!(self, GoogleError::Auth { .. })
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, GoogleError::RateLimited { .. })
    }

    pub fn is_history_expired(&self) -> bool {
        matches!(self, GoogleError::HistoryExpired { .. })
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, GoogleError::NotFound { .. })
    }

    /// True when the account's grant is missing the scope the call needed.
    /// The caller should report the account as needing authorization again.
    pub fn is_insufficient_scope(&self) -> bool {
        matches!(self, GoogleError::InsufficientScope { .. })
    }

    /// True for both expired-watermark cases: the caller must throw away its
    /// incremental cursor and re-sync from scratch.
    pub fn requires_full_resync(&self) -> bool {
        matches!(
            self,
            GoogleError::HistoryExpired { .. } | GoogleError::SyncTokenExpired { .. }
        )
    }

    /// Whether another attempt could plausibly succeed.
    pub fn is_retriable(&self) -> bool {
        match self {
            GoogleError::RateLimited { .. } | GoogleError::Network { .. } => true,
            GoogleError::Api { status, .. } => matches!(status, 500 | 502 | 503 | 504),
            _ => false,
        }
    }
}

/// Google's standard error envelope.
#[derive(Debug, Default, Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    error: ErrorBody,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<ErrorDetail>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    reason: Option<String>,
}

/// The 403 reason Google sends when the *token* is fine and the *grant* is too
/// narrow. Its body reads:
///
/// ```json
/// { "error": { "code": 403, "status": "PERMISSION_DENIED",
///   "message": "Request had insufficient authentication scopes.",
///   "errors": [{ "reason": "insufficientPermissions",
///                "message": "Insufficient Permission" }] } }
/// ```
///
/// The `message` is matched as well as the reason, because Google has shipped
/// this response with `ACCESS_TOKEN_SCOPE_INSUFFICIENT` in `details` and no
/// `errors[].reason` at all, and mistaking it for an ordinary refusal would put
/// "Google refused" on screen in place of the one instruction that fixes it.
const INSUFFICIENT_SCOPE_REASON: &str = "insufficientpermissions";
const INSUFFICIENT_SCOPE_MESSAGE: &str = "insufficient authentication scopes";

/// 403 reasons that mean "slow down" rather than "you may not".
const RATE_LIMIT_REASONS: &[&str] = &[
    "ratelimitexceeded",
    "userratelimitexceeded",
    "quotaexceeded",
    "dailylimitexceeded",
    "usagelimits",
];

fn classify(response: &HttpResponse) -> GoogleError {
    let parsed: ErrorEnvelope = serde_json::from_slice(&response.body).unwrap_or_default();
    let reason = parsed
        .error
        .errors
        .iter()
        .find_map(|d| d.reason.clone())
        .map(|r| r.to_ascii_lowercase());
    let message = if parsed.error.message.is_empty() {
        String::from_utf8_lossy(&response.body)
            .chars()
            .take(512)
            .collect()
    } else {
        parsed.error.message.clone()
    };
    let retry_after = response.retry_after();
    let status = response.status;

    match status {
        401 => GoogleError::Auth { message },
        403 => {
            let limited = reason
                .as_deref()
                .map(|r| RATE_LIMIT_REASONS.contains(&r))
                .unwrap_or(false)
                || parsed.error.status.as_deref() == Some("RESOURCE_EXHAUSTED");
            let scope = reason.as_deref() == Some(INSUFFICIENT_SCOPE_REASON)
                || message
                    .to_ascii_lowercase()
                    .contains(INSUFFICIENT_SCOPE_MESSAGE);
            if limited {
                GoogleError::RateLimited {
                    status,
                    message,
                    retry_after,
                }
            } else if scope {
                GoogleError::InsufficientScope { message }
            } else {
                GoogleError::Forbidden { message, reason }
            }
        }
        404 => GoogleError::NotFound { message },
        410 if reason.as_deref() == Some("fullsyncrequired") => {
            GoogleError::SyncTokenExpired { message }
        }
        429 => GoogleError::RateLimited {
            status,
            message,
            retry_after,
        },
        _ => GoogleError::Api {
            status,
            message,
            reason,
        },
    }
}

// ================================================================= transport

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: HttpMethod,
    /// Fully built, including the query string.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    /// Convenience for tests and for transports that already have a string.
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into().into_bytes(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Retry-After`, when present and expressed in seconds.
    pub fn retry_after(&self) -> Option<Duration> {
        self.header("retry-after")?
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_secs)
    }
}

/// A request that never produced a response.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub message: String,
}

impl TransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The one thing the clients need from the outside world.
pub trait HttpTransport: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>>;
}

/// The production transport.
///
/// Deliberately thin: it converts an [`HttpRequest`] into a reqwest call and
/// the answer back into an [`HttpResponse`]. Everything interesting — retries,
/// error classification, token refresh — lives above it, which is what lets the
/// tests replace this wholesale.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Share one client (and therefore one connection pool) across all five
    /// accounts and the auth module.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport for ReqwestTransport {
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

            let response = builder
                .send()
                .await
                .map_err(|e| TransportError::new(e.to_string()))?;
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
            let body = response
                .bytes()
                .await
                .map_err(|e| TransportError::new(e.to_string()))?
                .to_vec();

            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}

/// Supplies a bearer token, refreshing it if need be. The auth module
/// implements this; the API clients only ever see the string.
pub trait TokenProvider: Send + Sync {
    fn access_token<'a>(&'a self) -> BoxFuture<'a, Result<String, GoogleError>>;
}

/// A fixed token — useful for the OAuth spike and for tests.
pub struct StaticTokenProvider {
    token: String,
}

impl StaticTokenProvider {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl TokenProvider for StaticTokenProvider {
    fn access_token<'a>(&'a self) -> BoxFuture<'a, Result<String, GoogleError>> {
        let token = self.token.clone();
        Box::pin(async move { Ok(token) })
    }
}

/// Injected so the retry tests can assert on delays without spending them.
pub trait Sleeper: Send + Sync {
    fn sleep<'a>(&'a self, duration: Duration) -> BoxFuture<'a, ()>;
}

pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep<'a>(&'a self, duration: Duration) -> BoxFuture<'a, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

// =================================================================== retries

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// Retries *after* the first attempt. `max_retries: 3` means 4 requests.
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// Fraction of the computed delay that may be shaved off at random, to keep
    /// five accounts from re-colliding after a shared 429. `0.0` is
    /// deterministic (what the tests use); `1.0` is full jitter.
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(60),
            jitter: 0.5,
        }
    }
}

impl RetryPolicy {
    /// Fail on the first error. Useful when the caller wants to make its own
    /// scheduling decision.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// `base * 2^attempt`, capped, then jittered downwards.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let shifted = self
            .base_delay
            .as_nanos()
            .saturating_mul(1u128 << attempt.min(63));
        let capped = shifted.min(self.max_delay.as_nanos()).min(u64::MAX as u128);
        let nominal = Duration::from_nanos(capped as u64);
        if self.jitter <= 0.0 {
            return nominal;
        }
        let shave = self.jitter.clamp(0.0, 1.0) * random_unit();
        let jittered = Duration::from_secs_f64((nominal.as_secs_f64() * (1.0 - shave)).max(0.0));
        jittered.min(nominal)
    }
}

/// A uniform value in `[0, 1)`. xorshift64* seeded from the clock — jitter is
/// the only randomness here and it does not need to be cryptographic.
fn random_unit() -> f64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            | 1;
    }
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    STATE.store(s, Ordering::Relaxed);
    ((s >> 11) as f64) / ((1u64 << 53) as f64)
}

// ================================================================= the client

/// The shared plumbing behind [`gmail::GmailClient`] and
/// [`calendar::CalendarClient`]: token, transport, retry loop, URL building.
#[derive(Clone)]
pub struct RestClient {
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn TokenProvider>,
    sleeper: Arc<dyn Sleeper>,
    retry: RetryPolicy,
    base_url: String,
}

impl RestClient {
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn TokenProvider>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            tokens,
            sleeper: Arc::new(TokioSleeper),
            retry: RetryPolicy::default(),
            base_url: base_url.into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        self.retry
    }

    /// Build an endpoint URL. Path segments are percent-encoded, which matters:
    /// calendar ids look like `en.usa#holiday@group.v.calendar.google.com`.
    pub fn endpoint(&self, segments: &[&str]) -> Result<Url, GoogleError> {
        let mut url = Url::parse(&self.base_url).map_err(|e| GoogleError::InvalidRequest {
            message: format!("bad base url {:?}: {e}", self.base_url),
        })?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| GoogleError::InvalidRequest {
                    message: format!("base url {:?} cannot have a path", self.base_url),
                })?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    /// GET/DELETE/... and deserialize a JSON body.
    pub async fn send_json<T: DeserializeOwned>(
        &self,
        method: HttpMethod,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<T, GoogleError> {
        let response = self.send(method, url, body).await?;
        serde_json::from_slice(&response.body).map_err(|e| GoogleError::Deserialize {
            message: format!(
                "{e}; body started: {}",
                String::from_utf8_lossy(&response.body)
                    .chars()
                    .take(256)
                    .collect::<String>()
            ),
        })
    }

    /// For endpoints that answer `204 No Content`.
    pub async fn send_empty(
        &self,
        method: HttpMethod,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<(), GoogleError> {
        self.send(method, url, body).await.map(|_| ())
    }

    /// The retry loop. A fresh token is fetched per attempt so a refresh that
    /// happens mid-backoff is picked up.
    pub async fn send(
        &self,
        method: HttpMethod,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<HttpResponse, GoogleError> {
        self.send_as(method, url, body, None).await
    }

    /// [`send`](Self::send) with the request's `Content-Type` chosen by the
    /// caller.
    ///
    /// Everything in this crate posts JSON except one endpoint: Gmail's upload
    /// host, which takes `multipart/related` with the raw message inside it and
    /// answers `400` to anything labelled `application/json`. That is the whole
    /// reason this variant exists.
    pub async fn send_as(
        &self,
        method: HttpMethod,
        url: Url,
        body: Option<Vec<u8>>,
        content_type: Option<&str>,
    ) -> Result<HttpResponse, GoogleError> {
        let url = url.to_string();
        let content_type = content_type.unwrap_or("application/json; charset=UTF-8");
        let mut attempt: u32 = 0;
        loop {
            let token = self.tokens.access_token().await?;
            let mut headers = vec![
                ("Authorization".to_string(), format!("Bearer {token}")),
                ("Accept".to_string(), "application/json".to_string()),
            ];
            if body.is_some() {
                headers.push(("Content-Type".to_string(), content_type.to_string()));
            }

            let request = HttpRequest {
                method,
                url: url.clone(),
                headers,
                body: body.clone(),
            };

            let outcome = self.transport.execute(request).await;
            let (error, retry_after) = match outcome {
                Ok(response) if response.is_success() => return Ok(response),
                Ok(response) => {
                    let retry_after = response.retry_after();
                    (classify(&response), retry_after)
                }
                Err(transport) => (
                    GoogleError::Network {
                        message: transport.message,
                    },
                    None,
                ),
            };

            if attempt >= self.retry.max_retries || !error.is_retriable() {
                return Err(error);
            }
            // Google's own Retry-After beats our guess.
            let delay = retry_after.unwrap_or_else(|| self.retry.delay_for(attempt));
            self.sleeper.sleep(delay).await;
            attempt += 1;
        }
    }
}

// ================================================================ pagination

/// One page of a list endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_page_token: Option<String>,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, next_page_token: Option<String>) -> Self {
        // Google sometimes sends "" rather than omitting the field.
        Self {
            items,
            next_page_token: next_page_token.filter(|t| !t.is_empty()),
        }
    }

    pub fn has_more(&self) -> bool {
        self.next_page_token.is_some()
    }
}
