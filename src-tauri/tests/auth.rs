//! Behaviour tests for the OAuth 2.0 + PKCE flow and token storage (unit U2).
//!
//! There are no Google credentials yet, so nothing here talks to Google. What is
//! tested is everything *around* the round-trip: the PKCE transform against the
//! RFC 7636 test vector, authorization-URL construction, CSRF `state` rejection,
//! callback parsing, the loopback listener, expiry arithmetic, and the promise
//! that secrets never appear in `Debug` output.

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};

use mach_lib::auth::oauth::{
    self, authorization_url, exchange_form, parse_callback_query, refresh_form, sha256,
    validate_callback, AuthSession, Callback, LoopbackServer, Pkce, TokenHttp, SCOPES,
};
use mach_lib::auth::tokens::{
    MemoryTokenStore, Secret, TokenManager, TokenSet, TokenStore, REFRESH_MARGIN_SECONDS,
};
use mach_lib::auth::{AuthError, ClientConfig, ENV_CLIENT_ID, ENV_CLIENT_SECRET};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn test_config() -> ClientConfig {
    ClientConfig::new(
        "1234567890-abcdefg.apps.googleusercontent.com",
        Some("GOCSPX-super-secret-value"),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Records every call and replays canned bodies, so token exchange/refresh can
/// be exercised without a network or credentials.
struct FakeHttp {
    responses: Mutex<Vec<oauth::HttpResponse>>,
    calls: Mutex<Vec<(String, Vec<(String, String)>)>>,
}

impl FakeHttp {
    fn new(responses: Vec<oauth::HttpResponse>) -> Self {
        // popped from the back, so reverse to keep call order readable
        let mut responses = responses;
        responses.reverse();
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn ok(body: &str) -> oauth::HttpResponse {
        oauth::HttpResponse {
            status: 200,
            body: body.to_string(),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn last_form(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().last().unwrap().1.clone()
    }
}

impl TokenHttp for FakeHttp {
    fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> impl Future<Output = Result<oauth::HttpResponse, AuthError>> + Send {
        self.calls
            .lock()
            .unwrap()
            .push((url.to_string(), form.to_vec()));
        let next = self.responses.lock().unwrap().pop();
        async move {
            next.ok_or_else(|| AuthError::TokenEndpoint("fake http: no canned response".into()))
        }
    }
}

fn form_value<'a>(form: &'a [(String, String)], key: &str) -> Option<&'a str> {
    form.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// PKCE (RFC 7636)
// ---------------------------------------------------------------------------

#[test]
fn pkce_verifier_meets_rfc7636_length_and_charset() {
    for _ in 0..64 {
        let pkce = Pkce::generate().expect("generate pkce");
        let v = pkce.verifier();
        assert!(
            (43..=128).contains(&v.len()),
            "verifier length {} outside RFC 7636 range 43..=128",
            v.len()
        );
        assert!(
            v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
            "verifier {v:?} contains characters outside the RFC 7636 unreserved set"
        );
    }
}

#[test]
fn pkce_verifiers_are_not_repeated() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        let pkce = Pkce::generate().unwrap();
        assert!(
            seen.insert(pkce.verifier().to_string()),
            "duplicate verifier generated — randomness is broken"
        );
    }
}

#[test]
fn s256_challenge_matches_rfc7636_appendix_b_vector() {
    // RFC 7636 Appendix B.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    assert_eq!(oauth::s256_challenge(verifier), expected);

    let pkce = Pkce::from_verifier(verifier).unwrap();
    assert_eq!(pkce.challenge(), expected);
    assert_eq!(Pkce::CHALLENGE_METHOD, "S256");
}

#[test]
fn sha256_matches_nist_vectors() {
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    // Multi-block input exercises the padding path across a 64-byte boundary.
    assert_eq!(
        hex(&sha256(&[b'a'; 1000])),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

#[test]
fn pkce_rejects_verifiers_outside_the_spec() {
    assert!(matches!(
        Pkce::from_verifier("too-short"),
        Err(AuthError::InvalidVerifier(_))
    ));
    assert!(matches!(
        Pkce::from_verifier(&"a".repeat(129)),
        Err(AuthError::InvalidVerifier(_))
    ));
    // '+' and '/' are base64 standard-alphabet characters, not RFC 7636 unreserved.
    assert!(matches!(
        Pkce::from_verifier(&format!("{}+/", "a".repeat(43))),
        Err(AuthError::InvalidVerifier(_))
    ));
}

// ---------------------------------------------------------------------------
// scopes
// ---------------------------------------------------------------------------

#[test]
fn scopes_cover_mail_calendar_and_identity() {
    let joined = SCOPES.join(" ");
    for required in [
        "https://www.googleapis.com/auth/gmail.modify",
        "https://www.googleapis.com/auth/gmail.send",
        "https://www.googleapis.com/auth/calendar",
        "https://www.googleapis.com/auth/calendar.events",
        "https://www.googleapis.com/auth/userinfo.email",
    ] {
        assert!(joined.contains(required), "missing scope {required}");
    }
    assert_eq!(oauth::scope_string(), joined);
}

// ---------------------------------------------------------------------------
// authorization URL
// ---------------------------------------------------------------------------

#[test]
fn authorization_url_contains_every_required_parameter() {
    let cfg = test_config();
    let session = AuthSession::new(
        Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap(),
        "state-abc123".to_string(),
        "http://127.0.0.1:49213/oauth/callback".to_string(),
    );

    let raw = authorization_url(&cfg, &session, None);
    let url = url::Url::parse(&raw).expect("authorization url must parse");
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();

    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("accounts.google.com"));

    assert_eq!(q.get("client_id").map(String::as_str), Some(cfg.client_id()));
    assert_eq!(q.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(
        q.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1:49213/oauth/callback")
    );
    assert_eq!(q.get("state").map(String::as_str), Some("state-abc123"));
    assert_eq!(
        q.get("code_challenge").map(String::as_str),
        Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
    );
    assert_eq!(q.get("code_challenge_method").map(String::as_str), Some("S256"));
    assert_eq!(q.get("scope").map(String::as_str), Some(oauth::scope_string().as_str()));
    // Without these Google never issues a refresh token on re-consent.
    assert_eq!(q.get("access_type").map(String::as_str), Some("offline"));
    assert_eq!(q.get("prompt").map(String::as_str), Some("consent"));
}

#[test]
fn authorization_url_never_carries_the_verifier_or_client_secret() {
    let cfg = test_config();
    let session = AuthSession::new(
        Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap(),
        "state-abc123".to_string(),
        "http://127.0.0.1:49213/oauth/callback".to_string(),
    );
    let raw = authorization_url(&cfg, &session, None);
    assert!(
        !raw.contains("dBjftJeZ4CVP"),
        "the code_verifier must never leave the machine in the authorization URL"
    );
    assert!(!raw.contains("GOCSPX"), "client secret leaked into the authorization URL");
}

#[test]
fn authorization_url_carries_login_hint_when_supplied() {
    let cfg = test_config();
    let session = AuthSession::new(Pkce::generate().unwrap(), "s".into(), "http://127.0.0.1:1/x".into());
    let raw = authorization_url(&cfg, &session, Some("alex@example.com"));
    let url = url::Url::parse(&raw).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(
        q.get("login_hint").map(String::as_str),
        Some("alex@example.com")
    );
}

// ---------------------------------------------------------------------------
// CSRF: state validation — the security test that matters most
// ---------------------------------------------------------------------------

#[test]
fn callback_with_mismatched_state_is_rejected() {
    let cb = Callback::Success {
        code: "4/attacker-supplied-code".into(),
        state: "attacker-state".into(),
    };
    let err = validate_callback(cb, "our-real-state").unwrap_err();
    assert!(
        matches!(err, AuthError::StateMismatch),
        "expected StateMismatch, got {err:?}"
    );
}

#[test]
fn callback_with_matching_state_yields_the_code() {
    let cb = Callback::Success {
        code: "4/legit-code".into(),
        state: "our-real-state".into(),
    };
    assert_eq!(validate_callback(cb, "our-real-state").unwrap(), "4/legit-code");
}

#[test]
fn state_comparison_is_not_a_prefix_match() {
    let cb = Callback::Success {
        code: "4/x".into(),
        state: "our-real-state-and-then-some".into(),
    };
    assert!(matches!(
        validate_callback(cb, "our-real-state"),
        Err(AuthError::StateMismatch)
    ));
}

#[test]
fn generated_state_is_long_and_unique() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        let s = oauth::generate_state().unwrap();
        assert!(s.len() >= 32, "state {s:?} is too short to resist guessing");
        assert!(seen.insert(s), "duplicate state generated");
    }
}

// ---------------------------------------------------------------------------
// callback parsing
// ---------------------------------------------------------------------------

#[test]
fn parses_a_successful_callback() {
    let cb = parse_callback_query("code=4%2F0Ab_c-d&state=xyz&scope=email").unwrap();
    match cb {
        Callback::Success { code, state } => {
            assert_eq!(code, "4/0Ab_c-d", "percent-encoding must be decoded");
            assert_eq!(state, "xyz");
        }
        other => panic!("expected success, got {other:?}"),
    }
}

#[test]
fn parses_the_user_denied_callback() {
    let cb = parse_callback_query("error=access_denied&state=xyz").unwrap();
    match cb {
        Callback::Denied { error, .. } => assert_eq!(error, "access_denied"),
        other => panic!("expected denial, got {other:?}"),
    }
}

#[test]
fn user_denied_callback_surfaces_as_an_error_not_a_hang() {
    let cb = parse_callback_query("error=access_denied&error_description=The+user+said+no").unwrap();
    let err = validate_callback(cb, "xyz").unwrap_err();
    match err {
        AuthError::AuthorizationDenied { error, description } => {
            assert_eq!(error, "access_denied");
            assert_eq!(description.as_deref(), Some("The user said no"));
        }
        other => panic!("expected AuthorizationDenied, got {other:?}"),
    }
}

#[test]
fn callback_without_a_code_is_an_error() {
    assert!(matches!(
        parse_callback_query("state=xyz"),
        Err(AuthError::MissingCallbackParameter("code"))
    ));
}

#[test]
fn callback_without_a_state_is_an_error() {
    assert!(matches!(
        parse_callback_query("code=4%2Fabc"),
        Err(AuthError::MissingCallbackParameter("state"))
    ));
}

// ---------------------------------------------------------------------------
// loopback listener
// ---------------------------------------------------------------------------

#[test]
fn loopback_binds_an_ephemeral_port_on_127_0_0_1() {
    let a = LoopbackServer::bind().unwrap();
    let b = LoopbackServer::bind().unwrap();
    assert_ne!(a.port(), 0, "must read back the OS-assigned port");
    assert_ne!(a.port(), b.port(), "ports must not be hardcoded");
    assert_eq!(
        a.redirect_uri(),
        format!("http://127.0.0.1:{}/oauth/callback", a.port())
    );
    assert!(!a.redirect_uri().contains("localhost"));
    assert!(!a.redirect_uri().contains("0.0.0.0"));
}

/// Drives the real listener over a real TCP socket: it must answer once with a
/// human-readable page and then stop listening.
#[test]
fn loopback_serves_a_success_page_and_shuts_down_after_one_callback() {
    let server = LoopbackServer::bind().unwrap();
    let port = server.port();
    let handle = std::thread::spawn(move || server.wait_for_callback(Duration::from_secs(10)));

    let body = http_get(port, "/oauth/callback?code=4%2Fabc&state=st8");
    assert!(
        body.contains("You can close this window"),
        "success page missing its instruction, got: {body}"
    );

    let cb = handle.join().unwrap().unwrap();
    assert!(matches!(cb, Callback::Success { ref code, ref state } if code == "4/abc" && state == "st8"));

    // The listener released the port, so it becomes bindable again.
    //
    // Retried rather than asserted once: the socket that just served the
    // callback can sit in TIME_WAIT briefly, and under a parallel test run that
    // window is wide enough to lose the race. A genuine leak never binds and
    // still fails here; only the timing flake is absorbed.
    let deadline = Instant::now() + Duration::from_secs(2);
    let rebound = loop {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => break Some(l),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => break None.or_else(|| panic!("port never released: {e}")),
        }
    };
    assert!(rebound.is_some());
}

#[test]
fn loopback_handles_the_denied_callback_without_hanging() {
    let server = LoopbackServer::bind().unwrap();
    let port = server.port();
    let handle = std::thread::spawn(move || server.wait_for_callback(Duration::from_secs(10)));

    let body = http_get(port, "/oauth/callback?error=access_denied&state=st8");
    assert!(body.contains("close this window"), "denial page missing, got: {body}");

    let cb = handle.join().unwrap().unwrap();
    assert!(matches!(cb, Callback::Denied { ref error, .. } if error == "access_denied"));
}

#[test]
fn loopback_times_out_instead_of_blocking_forever() {
    let server = LoopbackServer::bind().unwrap();
    let started = std::time::Instant::now();
    let err = server.wait_for_callback(Duration::from_millis(300)).unwrap_err();
    assert!(matches!(err, AuthError::CallbackTimeout));
    assert!(started.elapsed() < Duration::from_secs(5));
}

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to loopback server");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    out
}

// ---------------------------------------------------------------------------
// expiry arithmetic
// ---------------------------------------------------------------------------

#[test]
fn a_token_expiring_in_30_seconds_needs_refreshing() {
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let token = TokenSet::for_test("access", Some("refresh"), now + chrono::Duration::seconds(30));
    assert!(token.needs_refresh_at(now));
    assert!(!token.is_expired_at(now), "30s out is near-expiry, not expired");
}

#[test]
fn a_token_expiring_in_an_hour_does_not_need_refreshing() {
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let token = TokenSet::for_test("access", Some("refresh"), now + chrono::Duration::hours(1));
    assert!(!token.needs_refresh_at(now));
    assert!(!token.is_expired_at(now));
}

#[test]
fn the_refresh_margin_is_at_least_a_minute() {
    assert!(REFRESH_MARGIN_SECONDS >= 60);
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    // Just inside the margin -> refresh; just outside -> don't.
    let inside = TokenSet::for_test(
        "a",
        None,
        now + chrono::Duration::seconds(REFRESH_MARGIN_SECONDS - 1),
    );
    let outside = TokenSet::for_test(
        "a",
        None,
        now + chrono::Duration::seconds(REFRESH_MARGIN_SECONDS + 1),
    );
    assert!(inside.needs_refresh_at(now));
    assert!(!outside.needs_refresh_at(now));
}

#[test]
fn expires_in_becomes_an_absolute_timestamp() {
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let body = r#"{"access_token":"ya29.a0","expires_in":3599,"refresh_token":"1//rt","scope":"https://www.googleapis.com/auth/gmail.modify","token_type":"Bearer"}"#;
    let token = TokenSet::from_json(body, now, None).unwrap();
    assert_eq!(token.expires_at, now + chrono::Duration::seconds(3599));
    assert_eq!(token.access_token.expose(), "ya29.a0");
    assert_eq!(token.refresh_token.as_ref().unwrap().expose(), "1//rt");
    assert_eq!(token.token_type, "Bearer");
}

#[test]
fn a_refresh_response_without_a_refresh_token_keeps_the_existing_one() {
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let body = r#"{"access_token":"ya29.new","expires_in":3600,"token_type":"Bearer"}"#;
    let token = TokenSet::from_json(body, now, Some(Secret::new("1//original"))).unwrap();
    assert_eq!(token.refresh_token.as_ref().unwrap().expose(), "1//original");
}

// ---------------------------------------------------------------------------
// redaction — secrets must not survive a {:?}
// ---------------------------------------------------------------------------

#[test]
fn debug_output_never_reveals_a_secret() {
    let secret = Secret::new("ya29.super-secret-access-token");
    let rendered = format!("{secret:?}");
    assert!(
        !rendered.contains("ya29.super-secret-access-token"),
        "Secret Debug leaked the value: {rendered}"
    );
    assert!(rendered.contains("redacted"), "expected a redaction marker, got {rendered}");
}

#[test]
fn token_set_debug_never_reveals_access_or_refresh_tokens() {
    let now = Utc.with_ymd_and_hms(2026, 8, 7, 12, 0, 0).unwrap();
    let token = TokenSet::for_test("ya29.leaky-access", Some("1//leaky-refresh"), now);
    let rendered = format!("{token:?}");
    assert!(!rendered.contains("ya29.leaky-access"), "leaked access token: {rendered}");
    assert!(!rendered.contains("1//leaky-refresh"), "leaked refresh token: {rendered}");
    // Non-secret fields stay useful for debugging.
    assert!(rendered.contains("expires_at"));
}

#[test]
fn client_config_debug_never_reveals_the_client_secret() {
    let cfg = test_config();
    let rendered = format!("{cfg:?}");
    assert!(!rendered.contains("GOCSPX-super-secret-value"), "leaked client secret: {rendered}");
    // The client id is not a secret and is useful when diagnosing config problems.
    assert!(rendered.contains("apps.googleusercontent.com"));
}

#[test]
fn pkce_debug_never_reveals_the_verifier() {
    let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap();
    let rendered = format!("{pkce:?}");
    assert!(!rendered.contains("dBjftJeZ4CVP"), "leaked code_verifier: {rendered}");
    assert!(rendered.contains("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
}

#[test]
fn auth_error_display_never_echoes_a_token() {
    let err = AuthError::TokenEndpoint("HTTP 400: invalid_grant".into());
    assert!(format!("{err}").contains("invalid_grant"));
    // A body that happens to contain a token is not something we construct, but
    // the variant that carries a raw body must be the *error* body only.
    let redacted = AuthError::StateMismatch;
    assert!(!format!("{redacted}").is_empty());
}

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

#[test]
fn config_env_var_names_are_the_documented_ones() {
    assert_eq!(ENV_CLIENT_ID, "MACH_GOOGLE_CLIENT_ID");
    assert_eq!(ENV_CLIENT_SECRET, "MACH_GOOGLE_CLIENT_SECRET");
}

#[test]
fn config_from_env_reports_a_missing_client_id_clearly() {
    // Deliberately does not mutate the process environment (other tests run in
    // the same process); an empty value is treated as absent.
    let err = ClientConfig::from_values(Some(String::new()), None).unwrap_err();
    assert!(matches!(err, AuthError::MissingConfig(ENV_CLIENT_ID)));
    let err = ClientConfig::from_values(None, None).unwrap_err();
    assert!(matches!(err, AuthError::MissingConfig(ENV_CLIENT_ID)));
}

#[test]
fn config_accepts_a_client_id_without_a_secret() {
    let cfg = ClientConfig::from_values(Some("abc.apps.googleusercontent.com".into()), None).unwrap();
    assert_eq!(cfg.client_id(), "abc.apps.googleusercontent.com");
    assert!(cfg.client_secret().is_none());
}

// ---------------------------------------------------------------------------
// token endpoint request shapes
// ---------------------------------------------------------------------------

#[test]
fn exchange_form_sends_the_verifier_and_never_the_challenge() {
    let cfg = test_config();
    let form = exchange_form(&cfg, "4/code", "the-verifier", "http://127.0.0.1:5/oauth/callback");
    assert_eq!(form_value(&form, "grant_type"), Some("authorization_code"));
    assert_eq!(form_value(&form, "code"), Some("4/code"));
    assert_eq!(form_value(&form, "code_verifier"), Some("the-verifier"));
    assert_eq!(
        form_value(&form, "redirect_uri"),
        Some("http://127.0.0.1:5/oauth/callback")
    );
    assert_eq!(form_value(&form, "client_id"), Some(cfg.client_id()));
    assert_eq!(form_value(&form, "client_secret"), Some("GOCSPX-super-secret-value"));
    assert!(form_value(&form, "code_challenge").is_none());
}

#[test]
fn refresh_form_uses_the_refresh_token_grant() {
    let cfg = test_config();
    let form = refresh_form(&cfg, "1//refresh");
    assert_eq!(form_value(&form, "grant_type"), Some("refresh_token"));
    assert_eq!(form_value(&form, "refresh_token"), Some("1//refresh"));
    assert_eq!(form_value(&form, "client_id"), Some(cfg.client_id()));
    assert!(form_value(&form, "code").is_none());
}

// ---------------------------------------------------------------------------
// token store
// ---------------------------------------------------------------------------

#[test]
fn memory_store_round_trips_per_account() {
    let store = MemoryTokenStore::default();
    assert!(store.load_refresh_token("a@x.com").unwrap().is_none());

    store
        .save_refresh_token("a@x.com", &Secret::new("1//a"))
        .unwrap();
    store
        .save_refresh_token("b@y.com", &Secret::new("1//b"))
        .unwrap();

    assert_eq!(store.load_refresh_token("a@x.com").unwrap().unwrap().expose(), "1//a");
    assert_eq!(store.load_refresh_token("b@y.com").unwrap().unwrap().expose(), "1//b");

    store.delete_refresh_token("a@x.com").unwrap();
    assert!(store.load_refresh_token("a@x.com").unwrap().is_none());
    assert!(store.load_refresh_token("b@y.com").unwrap().is_some());
}

// ---------------------------------------------------------------------------
// TokenManager: transparent refresh
// ---------------------------------------------------------------------------

#[tokio::test]
async fn access_token_is_returned_without_a_network_call_when_still_fresh() {
    let now = Utc::now();
    let http = FakeHttp::new(vec![]);
    let store = MemoryTokenStore::default();
    let manager = TokenManager::new(test_config(), http, store);
    manager.insert_tokens(
        "a@x.com",
        TokenSet::for_test("ya29.fresh", Some("1//r"), now + chrono::Duration::hours(1)),
    );

    let token = manager.access_token("a@x.com").await.unwrap();
    assert_eq!(token.expose(), "ya29.fresh");
    assert_eq!(manager.http().call_count(), 0, "must not hit the network for a fresh token");
}

#[tokio::test]
async fn access_token_refreshes_transparently_when_inside_the_margin() {
    let now = Utc::now();
    let http = FakeHttp::new(vec![FakeHttp::ok(
        r#"{"access_token":"ya29.refreshed","expires_in":3600,"token_type":"Bearer"}"#,
    )]);
    let store = MemoryTokenStore::default();
    let manager = TokenManager::new(test_config(), http, store);
    manager.insert_tokens(
        "a@x.com",
        TokenSet::for_test("ya29.stale", Some("1//r"), now + chrono::Duration::seconds(30)),
    );

    let token = manager.access_token("a@x.com").await.unwrap();
    assert_eq!(token.expose(), "ya29.refreshed");
    assert_eq!(manager.http().call_count(), 1);
    assert_eq!(form_value(&manager.http().last_form(), "grant_type"), Some("refresh_token"));
    assert_eq!(form_value(&manager.http().last_form(), "refresh_token"), Some("1//r"));

    // The refreshed token is cached, so a second call is free.
    let again = manager.access_token("a@x.com").await.unwrap();
    assert_eq!(again.expose(), "ya29.refreshed");
    assert_eq!(manager.http().call_count(), 1);
}

#[tokio::test]
async fn refresh_falls_back_to_the_stored_refresh_token_when_nothing_is_cached() {
    let now = Utc::now();
    let _ = now;
    let http = FakeHttp::new(vec![FakeHttp::ok(
        r#"{"access_token":"ya29.fromkeychain","expires_in":3600,"token_type":"Bearer"}"#,
    )]);
    let store = MemoryTokenStore::default();
    store
        .save_refresh_token("a@x.com", &Secret::new("1//stored"))
        .unwrap();
    let manager = TokenManager::new(test_config(), http, store);

    let token = manager.access_token("a@x.com").await.unwrap();
    assert_eq!(token.expose(), "ya29.fromkeychain");
    assert_eq!(form_value(&manager.http().last_form(), "refresh_token"), Some("1//stored"));
}

#[tokio::test]
async fn a_missing_refresh_token_asks_for_reauthorization_rather_than_panicking() {
    let http = FakeHttp::new(vec![]);
    let manager = TokenManager::new(test_config(), http, MemoryTokenStore::default());
    let err = manager.access_token("nobody@x.com").await.unwrap_err();
    assert!(
        matches!(err, AuthError::NotAuthorized(ref e) if e == "nobody@x.com"),
        "got {err:?}"
    );
}

/// The QA cold start, on the runtime flavour the app actually uses.
///
/// A QA instance addresses its own Keychain service, which holds no items, so
/// every account it inherited from a seeded store looks like this: the store
/// says `Ok(None)` and `access_token` has to *return* `NotAuthorized`. The
/// multi-thread flavour is the one that takes the `block_in_place` branch of
/// `load_refresh_token_unblocking`, which is where the two previous hangs were,
/// and the timeout is the assertion — a task blocked inside its own poll never
/// completes, so finishing at all is the evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_qa_instance_with_no_credentials_returns_rather_than_blocking() {
    let manager = TokenManager::new(
        test_config(),
        FakeHttp::new(vec![]),
        MemoryTokenStore::default(),
    );

    for email in ["one@example.com", "two@example.com"] {
        let answer = tokio::time::timeout(
            Duration::from_secs(5),
            manager.access_token(email),
        )
        .await
        .unwrap_or_else(|_| panic!("{email} never came back — the read blocked"));

        match answer {
            Err(AuthError::NotAuthorized(who)) => assert_eq!(who, email),
            other => panic!("expected NotAuthorized, got {other:?}"),
        }
    }

    // Nothing was sent to Google either: there was no credential to send.
    assert_eq!(manager.http().call_count(), 0);
}

#[tokio::test]
async fn a_token_endpoint_error_is_surfaced_not_swallowed() {
    let http = FakeHttp::new(vec![oauth::HttpResponse {
        status: 400,
        body: r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
            .to_string(),
    }]);
    let store = MemoryTokenStore::default();
    store
        .save_refresh_token("a@x.com", &Secret::new("1//revoked"))
        .unwrap();
    let manager = TokenManager::new(test_config(), http, store);

    let err = manager.access_token("a@x.com").await.unwrap_err();
    match err {
        AuthError::TokenEndpoint(msg) => assert!(msg.contains("invalid_grant"), "got {msg}"),
        other => panic!("expected TokenEndpoint, got {other:?}"),
    }
}

#[tokio::test]
async fn exchanging_a_code_persists_the_refresh_token_to_the_store() {
    let http = FakeHttp::new(vec![FakeHttp::ok(
        r#"{"access_token":"ya29.first","expires_in":3600,"refresh_token":"1//brand-new","token_type":"Bearer"}"#,
    )]);
    let manager = TokenManager::new(test_config(), http, MemoryTokenStore::default());

    manager
        .exchange_code(
            "a@x.com",
            "4/code",
            "verifier-verifier-verifier-verifier-verifier",
            "http://127.0.0.1:5/oauth/callback",
        )
        .await
        .unwrap();

    assert_eq!(
        manager.store().load_refresh_token("a@x.com").unwrap().unwrap().expose(),
        "1//brand-new"
    );
    assert_eq!(manager.access_token("a@x.com").await.unwrap().expose(), "ya29.first");
    assert_eq!(manager.http().call_count(), 1, "no refresh needed right after exchange");
}
