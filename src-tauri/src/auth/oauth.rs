//! OAuth 2.0 Authorization Code + PKCE flow with a loopback redirect.
//!
//! See the [module docs][crate::auth] for the configuration this expects and the
//! shape of the flow. This file owns the parts that are pure enough to test
//! without Google: the PKCE transform, URL construction, callback parsing, the
//! `state` check, and the one-shot loopback listener.

use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use super::tokens::Secret;
use super::{AuthError, ClientConfig};

// ---------------------------------------------------------------------------
// endpoints and scopes
// ---------------------------------------------------------------------------

/// Google's authorization endpoint (the page the user sees).
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Google's token endpoint (code -> tokens, refresh -> access token).
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Path the loopback listener answers on.
pub const CALLBACK_PATH: &str = "/oauth/callback";

/// The OAuth scopes Mach needs.
///
/// **Verification implications — read `docs/superpowers/specs/` "Hard parts" #2
/// before changing this list.**
///
/// - `gmail.modify` is a **RESTRICTED** scope and `gmail.send` is **SENSITIVE**.
///   A Google Workspace domain you administer can publish the OAuth app as
///   **Internal**, which skips verification. A personal `@gmail.com` account
///   cannot: the app is External. Publishing an External app to **In production**
///   — which Google permits even while unverified — is what avoids the 7-day
///   refresh-token expiry that *Testing* status imposes; see `skills/mach-setup`.
///   Escaping the "unverified" warning entirely would mean a CASA Tier 2
///   assessment ($500–$4,500/yr). An account inside a Workspace org you do not
///   administer may also be blocked by that org's admin.
/// - `calendar` and `calendar.events` are **SENSITIVE**, not restricted — a
///   lighter verification tier with no CASA requirement.
/// - `gmail.settings.basic` is **SENSITIVE**, the same tier as `gmail.send`, so
///   it adds no verification burden that `gmail.send` had not already added.
/// - `userinfo.email` (with `openid`) is how we learn which account just
///   authorized, so tokens can be keyed by email. It is non-sensitive.
///
/// `gmail.modify` is deliberately chosen over the wider `https://mail.google.com/`:
/// it covers read, label, archive and trash — everything the triage commands
/// need — without granting permanent delete.
///
/// `gmail.settings.basic` is chosen on the same reasoning, one level down.
/// `users.settings.filters` is behind it and behind nothing narrower: Gmail has
/// no per-resource settings scope, so creating a filter costs the whole of
/// "basic settings" — filters, the vacation responder, IMAP/POP, language and
/// the existing send-as aliases. Mach reads and writes only filters. The scope
/// above it, `gmail.settings.sharing`, is **not** requested and must not be:
/// that one adds delegation, sending as another address, and registering a new
/// forwarding destination, which is the difference between a rule that files
/// mail and a rule that can send mail somewhere new. A filter with a `forward`
/// action still works under `basic`, because Gmail only accepts a forwarding
/// address the account has already verified — a gate Mach neither owns nor can
/// open with this scope.
///
/// **`contacts.other.readonly` was requested, measured and taken back out.**
///
/// The thread list draws a monogram per sender and the obvious next step was a
/// real photograph, so the scope went in and one account re-consented.
/// `otherContacts.list` — Google's own auto-collected list of everyone you have
/// corresponded with — takes `photos` in its `readMask`, and against a real
/// mailbox it returned:
///
/// ```text
/// other contacts   : 8854
/// photo field set  : 8854  (100.0%)
/// REAL picture     : 1     (0.0%)
///   default=true   : 8853
/// ```
///
/// Every entry carries a `Photo`, and 8,853 of them are `default: true` —
/// Google's grey silhouette. Google does not hand out a profile picture because
/// somebody emailed you, which is the right call on their part and the end of
/// the idea on ours. A scope on the consent screen that buys one face in nine
/// thousand is surface with nothing behind it.
///
/// The local address book was measured next, since that is where Apple Mail
/// gets its faces (`CNContactImageDataKey`). Of 728 macOS contacts, 196 have a
/// photo, 110 have an email address, 68 have both — and **six** of those have
/// ever sent mail to this account, covering 1.4% of the store and 1.3% of the
/// last ninety days. Reachable, but it needs a Swift shim linked into the
/// binary and a TCC prompt, for six faces.
///
/// So the monogram is not a placeholder waiting for a picture. It is the
/// answer, and the colour rather than the letters is the part doing the work.
///
/// **Adding a scope invalidates every existing grant.** A refresh token issued
/// before this line changed keeps working for mail and calendar and gets a 403
/// from `users.settings.filters`, because the token carries the scopes it was
/// granted, not the ones the app now asks for. Every account has to go through
/// the consent screen again. That failure has to read as what it is rather than
/// as a dead credential — see [`GoogleError::InsufficientScope`] and
/// `SyncStatusPayload::missing_scope`.
///
/// [`GoogleError::InsufficientScope`]: crate::google::GoogleError::InsufficientScope
pub const SCOPES: &[&str] = &[
    "openid",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/gmail.settings.basic",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/calendar.events",
];

/// The space-delimited `scope` parameter value.
pub fn scope_string() -> String {
    SCOPES.join(" ")
}

// ---------------------------------------------------------------------------
// randomness
// ---------------------------------------------------------------------------

/// Cryptographically secure random bytes from the OS.
///
/// Read straight from `/dev/urandom` rather than pulling in `rand`/`getrandom`
/// as a new direct dependency. On macOS and Linux this is the same kernel CSPRNG
/// those crates wrap. Non-unix targets fail loudly instead of degrading to
/// something weaker — Mach is a macOS app.
fn secure_random_bytes(len: usize) -> Result<Vec<u8>, AuthError> {
    #[cfg(unix)]
    {
        let mut file = std::fs::File::open("/dev/urandom")
            .map_err(|e| AuthError::Randomness(format!("open /dev/urandom: {e}")))?;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf)
            .map_err(|e| AuthError::Randomness(format!("read /dev/urandom: {e}")))?;
        Ok(buf)
    }
    #[cfg(not(unix))]
    {
        let _ = len;
        Err(AuthError::Randomness(
            "no secure randomness source on this platform".into(),
        ))
    }
}

/// `len` random bytes rendered as unpadded base64url, which is a subset of the
/// RFC 7636 unreserved character set.
fn random_token(len: usize) -> Result<String, AuthError> {
    Ok(URL_SAFE_NO_PAD.encode(secure_random_bytes(len)?))
}

/// A fresh CSRF `state` value: 32 random bytes, 43 base64url characters.
pub fn generate_state() -> Result<String, AuthError> {
    random_token(32)
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4).
///
/// Implemented here rather than added as a dependency: `sha2` is only in the
/// tree transitively, and PKCE's S256 transform is the single hash this app
/// needs. It is verified against the NIST vectors and the RFC 7636 Appendix B
/// verifier/challenge pair in `tests/auth.rs`. If `sha2` ever becomes a direct
/// dependency, delete this and call it instead.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

/// The S256 transform: `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`.
pub fn s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()))
}

fn validate_verifier(verifier: &str) -> Result<(), AuthError> {
    if !(43..=128).contains(&verifier.len()) {
        return Err(AuthError::InvalidVerifier(format!(
            "length {} is outside the required range 43..=128",
            verifier.len()
        )));
    }
    if let Some(bad) = verifier
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')))
    {
        return Err(AuthError::InvalidVerifier(format!(
            "character {bad:?} is not in the unreserved set [A-Za-z0-9-._~]"
        )));
    }
    Ok(())
}

/// A PKCE verifier/challenge pair (RFC 7636).
///
/// The verifier is a secret: it must reach only Google's token endpoint, never
/// the authorization URL and never a log. `Debug` shows the challenge (public)
/// and redacts the verifier.
#[derive(Clone)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    /// The only challenge method Mach uses. `plain` is not supported.
    pub const CHALLENGE_METHOD: &'static str = "S256";

    /// A fresh pair: 32 random bytes -> a 43-character verifier.
    pub fn generate() -> Result<Self, AuthError> {
        Self::from_verifier(&random_token(32)?)
    }

    /// Wraps an existing verifier, validating it against RFC 7636 §4.1.
    pub fn from_verifier(verifier: &str) -> Result<Self, AuthError> {
        validate_verifier(verifier)?;
        Ok(Self {
            challenge: s256_challenge(verifier),
            verifier: verifier.to_string(),
        })
    }

    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// authorization request
// ---------------------------------------------------------------------------

/// One in-flight authorization attempt: the PKCE pair, the CSRF `state`, and the
/// redirect URI derived from the loopback listener's actual port.
#[derive(Debug, Clone)]
pub struct AuthSession {
    pub pkce: Pkce,
    pub state: String,
    pub redirect_uri: String,
}

impl AuthSession {
    pub fn new(pkce: Pkce, state: String, redirect_uri: String) -> Self {
        Self {
            pkce,
            state,
            redirect_uri,
        }
    }

    /// Fresh PKCE + `state` for a listener that has already been bound.
    pub fn start(redirect_uri: impl Into<String>) -> Result<Self, AuthError> {
        Ok(Self {
            pkce: Pkce::generate()?,
            state: generate_state()?,
            redirect_uri: redirect_uri.into(),
        })
    }
}

/// Builds the URL to open in the user's browser.
///
/// `access_type=offline` + `prompt=consent` are what make Google return a
/// refresh token; without them a re-authorization yields an access token only.
/// `login_hint` pre-selects an account, which matters when adding the fourth of
/// five accounts.
pub fn authorization_url(
    config: &ClientConfig,
    session: &AuthSession,
    login_hint: Option<&str>,
) -> String {
    let mut url = url::Url::parse(AUTH_ENDPOINT).expect("AUTH_ENDPOINT is a valid URL");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", config.client_id());
        q.append_pair("response_type", "code");
        q.append_pair("redirect_uri", &session.redirect_uri);
        q.append_pair("scope", &scope_string());
        q.append_pair("state", &session.state);
        q.append_pair("code_challenge", session.pkce.challenge());
        q.append_pair("code_challenge_method", Pkce::CHALLENGE_METHOD);
        q.append_pair("access_type", "offline");
        q.append_pair("prompt", "consent");
        if let Some(hint) = login_hint {
            q.append_pair("login_hint", hint);
        }
    }
    url.into()
}

// ---------------------------------------------------------------------------
// callback
// ---------------------------------------------------------------------------

/// What came back on the loopback redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    Success {
        code: String,
        state: String,
    },
    Denied {
        error: String,
        description: Option<String>,
        state: Option<String>,
    },
}

/// Parses the query string of the redirect.
///
/// Accepts a bare query (`code=…&state=…`) or a full request target
/// (`/oauth/callback?code=…`). The user-denied case (`error=…`) parses cleanly
/// rather than erroring, so the caller can shut the listener down and report it.
pub fn parse_callback_query(query: &str) -> Result<Callback, AuthError> {
    let query = match query.split_once('?') {
        Some((_, q)) => q,
        None => query,
    };
    let query = query.split('#').next().unwrap_or("");

    // A relative base is fine: we only want the pair decoder.
    let parsed = url::Url::parse(&format!("http://127.0.0.1/?{query}"))
        .map_err(|e| AuthError::Loopback(format!("unparseable callback query: {e}")))?;

    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => description = Some(v.into_owned()),
            _ => {}
        }
    }

    if let Some(error) = error {
        return Ok(Callback::Denied {
            error,
            description,
            state,
        });
    }

    let code = code.ok_or(AuthError::MissingCallbackParameter("code"))?;
    let state = state.ok_or(AuthError::MissingCallbackParameter("state"))?;
    Ok(Callback::Success { code, state })
}

/// Checks the CSRF `state` and hands back the authorization code.
///
/// A mismatch means the callback did not originate from the authorization
/// request we started. The code is discarded, never exchanged.
pub fn validate_callback(callback: Callback, expected_state: &str) -> Result<String, AuthError> {
    match callback {
        Callback::Denied {
            error, description, ..
        } => Err(AuthError::AuthorizationDenied { error, description }),
        Callback::Success { code, state } => {
            if !constant_time_eq(state.as_bytes(), expected_state.as_bytes()) {
                return Err(AuthError::StateMismatch);
            }
            Ok(code)
        }
    }
}

/// Length-checked, non-short-circuiting comparison. `state` is not a MAC, but
/// comparing it in constant time costs nothing and removes the question.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// loopback listener
// ---------------------------------------------------------------------------

const SUCCESS_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Mach</title>\
<style>body{font:16px -apple-system,system-ui,sans-serif;margin:20vh auto;max-width:28rem;text-align:center;color:#111}\
@media(prefers-color-scheme:dark){body{background:#111;color:#eee}}</style>\
<h1>Signed in</h1><p>You can close this window and go back to Mach.</p>";

const DENIED_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Mach</title>\
<style>body{font:16px -apple-system,system-ui,sans-serif;margin:20vh auto;max-width:28rem;text-align:center;color:#111}\
@media(prefers-color-scheme:dark){body{background:#111;color:#eee}}</style>\
<h1>Not signed in</h1><p>Authorization was cancelled. You can close this window and try again in Mach.</p>";

/// A one-shot HTTP listener on `127.0.0.1` for the OAuth redirect.
///
/// Binds port 0 and reads back whatever the OS assigned, so nothing is
/// hardcoded and two accounts can be authorized without a port clash. Binds the
/// loopback address specifically — never `0.0.0.0`, and never the name
/// `localhost` (which can resolve to `::1` and produce a redirect URI Google
/// treats as a different registered value).
pub struct LoopbackServer {
    server: tiny_http::Server,
    port: u16,
}

impl LoopbackServer {
    pub fn bind() -> Result<Self, AuthError> {
        let server = tiny_http::Server::http("127.0.0.1:0")
            .map_err(|e| AuthError::LoopbackBind(e.to_string()))?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| AuthError::LoopbackBind("listener has no IP address".into()))?
            .port();
        Ok(Self { server, port })
    }

    /// The OS-assigned port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URI to send to Google and to echo back at the token endpoint.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, CALLBACK_PATH)
    }

    /// Serves until the OAuth redirect arrives, then answers it and shuts down.
    ///
    /// Consumes `self`, so the socket is released as soon as this returns —
    /// including on the denial and timeout paths. Requests that carry neither
    /// `code` nor `error` (a browser's `/favicon.ico`, a stray probe) get a 404
    /// and do not end the wait.
    pub fn wait_for_callback(self, timeout: Duration) -> Result<Callback, AuthError> {
        let deadline = Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AuthError::CallbackTimeout);
            }

            // Cap the blocking interval so the deadline is honoured even if the
            // listener is woken by unrelated traffic.
            let slice = remaining.min(Duration::from_millis(250));
            let request = match self.server.recv_timeout(slice) {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(e) => return Err(AuthError::Loopback(e.to_string())),
            };

            let target = request.url().to_string();
            let has_params = target.contains("code=") || target.contains("error=");
            if !has_params {
                let _ = request.respond(tiny_http::Response::from_string("not found").with_status_code(404));
                continue;
            }

            let parsed = parse_callback_query(&target);
            let page = match &parsed {
                Ok(Callback::Success { .. }) => SUCCESS_PAGE,
                _ => DENIED_PAGE,
            };
            let response = tiny_http::Response::from_string(page).with_header(
                "Content-Type: text/html; charset=utf-8"
                    .parse::<tiny_http::Header>()
                    .expect("static header parses"),
            );
            // The browser tab is a courtesy; a failure to write it must not lose
            // an authorization code we already hold.
            let _ = request.respond(response);

            return parsed;
        }
    }
}

impl fmt::Debug for LoopbackServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackServer")
            .field("redirect_uri", &self.redirect_uri())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// token endpoint
// ---------------------------------------------------------------------------

/// A response from the token endpoint, reduced to what this module needs.
#[derive(Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Hand-written, like every other `Debug` in `auth`, and for a sharper reason
/// than most: on the path that succeeds, `body` **is** the refresh token. A
/// derived `Debug` here would put one `{:?}` between a stack trace and a
/// permanent credential in a log. Only failure bodies are safe to show, and
/// distinguishing them at format time is exactly the kind of thing that is
/// right until somebody moves a line, so neither is shown.
impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body", &format_args!("<{} bytes, redacted>", self.body.len()))
            .finish()
    }
}

/// The single network operation OAuth needs: an `application/x-www-form-urlencoded`
/// POST that returns a body.
///
/// This is a seam on purpose — every decision in this module is testable through
/// it without a network. It is also the **only** place in `auth` that needs an
/// HTTP client, and `mach` currently has none as a direct dependency (`reqwest`
/// is in the lockfile only transitively, through `tauri`, without a TLS
/// backend). Adding one is a `Cargo.toml` change, which this unit does not own.
///
/// Once `reqwest = { version = "0.13", features = ["json", "rustls-tls"] }` is a
/// direct dependency, the whole implementation is:
///
/// ```ignore
/// pub struct GoogleTokenHttp {
///     client: reqwest::Client,
/// }
///
/// impl TokenHttp for GoogleTokenHttp {
///     async fn post_form(
///         &self,
///         url: &str,
///         form: &[(String, String)],
///     ) -> Result<HttpResponse, AuthError> {
///         let response = self
///             .client
///             .post(url)
///             .form(form)
///             .send()
///             .await
///             .map_err(|e| AuthError::Transport(e.to_string()))?;
///         let status = response.status().as_u16();
///         let body = response
///             .text()
///             .await
///             .map_err(|e| AuthError::Transport(e.to_string()))?;
///         Ok(HttpResponse { status, body })
///     }
/// }
/// ```
///
/// The unit that owns `google/` needs the same client, so the two should share
/// one `reqwest::Client` rather than each building their own connection pool.
pub trait TokenHttp {
    fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> impl std::future::Future<Output = Result<HttpResponse, AuthError>> + Send;
}

fn base_form(config: &ClientConfig) -> Vec<(String, String)> {
    let mut form = vec![("client_id".to_string(), config.client_id().to_string())];
    if let Some(secret) = config.client_secret() {
        form.push(("client_secret".to_string(), secret.expose().to_string()));
    }
    form
}

/// The form body that trades an authorization code for tokens.
///
/// Carries the PKCE **verifier** — the challenge went out in the authorization
/// URL and must not be repeated here. `redirect_uri` must be byte-identical to
/// the one in the authorization request or Google rejects the exchange.
pub fn exchange_form(
    config: &ClientConfig,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Vec<(String, String)> {
    let mut form = base_form(config);
    form.push(("grant_type".into(), "authorization_code".into()));
    form.push(("code".into(), code.to_string()));
    form.push(("code_verifier".into(), code_verifier.to_string()));
    form.push(("redirect_uri".into(), redirect_uri.to_string()));
    form
}

/// The form body that exchanges a refresh token for a new access token.
pub fn refresh_form(config: &ClientConfig, refresh_token: &str) -> Vec<(String, String)> {
    let mut form = base_form(config);
    form.push(("grant_type".into(), "refresh_token".into()));
    form.push(("refresh_token".into(), refresh_token.to_string()));
    form
}

/// The OAuth 2.0 `error` code from a token-endpoint failure body, if it sent
/// one. `invalid_grant`, `invalid_client`, `unauthorized_client`, …
fn error_code(response: &HttpResponse) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&response.body)
        .ok()?
        .get("error")?
        .as_str()
        .map(str::to_string)
}

/// Google's `error` and `error_description`, verbatim, or the first 200
/// characters of whatever it sent instead of a JSON envelope.
///
/// Verbatim because this is the only text that distinguishes a password change
/// from a withdrawn grant from a seven-day expiry, and the owner is the one who
/// has to tell them apart. Paraphrasing it would throw away the answer.
fn error_detail(response: &HttpResponse) -> String {
    serde_json::from_str::<serde_json::Value>(&response.body)
        .ok()
        .and_then(|v| {
            let error = v.get("error")?.as_str()?.to_string();
            let description = v
                .get("error_description")
                .and_then(|d| d.as_str())
                .map(|d| format!(" ({d})"))
                .unwrap_or_default();
            Some(format!("{error}{description}"))
        })
        .unwrap_or_else(|| response.body.chars().take(200).collect())
}

/// Turns a non-2xx token-endpoint response into an error, preserving Google's
/// `error`/`error_description`.
pub fn token_error(response: &HttpResponse) -> AuthError {
    AuthError::TokenEndpoint(format!("HTTP {}: {}", response.status, error_detail(response)))
}

/// The same, for a *refresh* rather than a code exchange.
///
/// On this endpoint `invalid_grant` has exactly one meaning: the refresh token
/// presented is no longer a grant. Google says so for a revoked token, a
/// password change, a withdrawn consent, and the seven-day expiry it puts on
/// tokens issued by an unverified External app — all of which need a person and
/// none of which need another attempt.
///
/// It is deliberately *not* folded into [`token_error`], which also serves the
/// authorization-code exchange. There `invalid_grant` means the code was stale
/// or already spent, which says nothing about any stored credential; treating
/// the two alike would flag an account for a failed sign-in it never had.
pub fn refresh_error(response: &HttpResponse) -> AuthError {
    match error_code(response).as_deref() {
        Some("invalid_grant") => AuthError::CredentialRejected(error_detail(response)),
        _ => token_error(response),
    }
}

/// Convenience for callers that already hold a verifier as a [`Secret`].
pub fn exchange_form_secret(
    config: &ClientConfig,
    code: &str,
    code_verifier: &Secret,
    redirect_uri: &str,
) -> Vec<(String, String)> {
    exchange_form(config, code, code_verifier.expose(), redirect_uri)
}

#[cfg(test)]
mod scope_documentation {
    use super::SCOPES;

    /// The setup guide and this list must name the same scopes.
    ///
    /// They did not, and the way it failed is the reason this test exists.
    /// `gmail.settings.basic` was added to [`SCOPES`] and to nothing else, so
    /// the guide went on saying "six scopes", listed six, and its own
    /// verification step — *all six scopes are listed* — confirmed the wrong
    /// number. Anyone following it built a consent screen the app did not match
    /// and got a 403 from the filter commands with nothing to point at.
    ///
    /// Reading the markdown is not elegant. It is, however, the only check that
    /// fails when somebody changes one list and not the other, which is exactly
    /// what happened.
    const SKILL: &str = include_str!("../../../skills/mach-setup/SKILL.md");
    const README: &str = include_str!("../../../README.md");

    #[test]
    fn the_setup_guide_lists_every_scope_and_no_others() {
        for scope in SCOPES {
            assert!(
                SKILL.contains(scope),
                "skills/mach-setup/SKILL.md does not list `{scope}`; \
                 anyone following it will build a consent screen that is missing it"
            );
        }
        // The other direction: a scope dropped from the code but left in the
        // guide asks people to grant something Mach no longer uses.
        for line in SKILL.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("https://www.googleapis.com/auth/") {
                let scope = format!("https://www.googleapis.com/auth/{rest}");
                assert!(
                    SCOPES.contains(&scope.as_str()),
                    "the setup guide asks for `{scope}`, which Mach does not request"
                );
            }
        }
    }

    #[test]
    fn the_readme_counts_the_scopes_correctly() {
        let spelled = match SCOPES.len() {
            6 => "six",
            7 => "seven",
            8 => "eight",
            n => panic!("no spelling for {n} scopes — add one here and in the docs"),
        };
        assert!(
            README.contains(&format!("Mach requests {spelled} scopes")),
            "README.md does not say `Mach requests {spelled} scopes`"
        );
        for scope in SCOPES {
            let short = scope.rsplit('/').next().unwrap_or(scope);
            assert!(
                README.contains(short),
                "README.md's scope table is missing `{short}`"
            );
        }
    }
}
