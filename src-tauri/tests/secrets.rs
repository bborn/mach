//! Nothing that is a credential may reach a formatter.
//!
//! `tests/auth.rs` already pins the redaction on the four types that were
//! written with it in mind — [`Secret`], `TokenSet`, `ClientConfig`, `Pkce`.
//! This file is about the ones that hold the same material one indirection
//! later and were reached by `#[derive(Debug)]`: a request struct whose headers
//! carry the live access token, a response struct whose body *is* the refresh
//! token on the success path, a token store that keeps refresh tokens as plain
//! `String`s in a map.
//!
//! # Why this is worth a file
//!
//! None of these was being printed. The whole risk is one `eprintln!("{:?}")`
//! away, added by somebody debugging a sync failure at the wrong moment — and
//! the app's stdout is piped into `.qa/<instance>/log` and read back by
//! `scripts/qa logs`, which is to say into an agent's transcript. A credential
//! that reaches a log has to be treated as disclosed, and a Google refresh
//! token does not expire on its own.
//!
//! So each test below writes the `Debug` output the way a careless line would
//! and asserts the secret is not in it. They fail if a `#[derive(Debug)]` is
//! ever put back.

use std::collections::BTreeMap;

use mach_lib::auth::tokens::{MemoryTokenStore, Secret, TokenStore};
use mach_lib::auth::{oauth, ClientConfig};
use mach_lib::google::{HttpMethod, HttpRequest};
use mach_lib::ipc::agent::engine::backend::Backend;
use mach_lib::ipc::agent::engine::config::{AgentConfig, Credential};
use mach_lib::ipc::agent::engine::wire::ModelCall;

/// A value distinctive enough that a substring search for it is meaningful.
const REFRESH: &str = "1//0gTHIS-IS-A-REFRESH-TOKEN-DO-NOT-PRINT-ME";
const ACCESS: &str = "ya29.THIS-IS-AN-ACCESS-TOKEN-DO-NOT-PRINT-ME";

// ===========================================================================
// The Google request
// ===========================================================================

/// Every Google call carries `Authorization: Bearer <live access token>`, and
/// `HttpRequest` is the struct a transport reaches for when a call goes wrong.
#[test]
fn a_google_request_does_not_print_the_bearer_token() {
    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "https://gmail.googleapis.com/gmail/v1/users/me/messages".into(),
        headers: vec![
            ("Authorization".into(), format!("Bearer {ACCESS}")),
            ("Accept".into(), "application/json".into()),
        ],
        body: None,
    };

    let printed = format!("{request:?}");
    assert!(
        !printed.contains(ACCESS),
        "the access token was printed: {printed}"
    );
    assert!(printed.contains("<redacted>"), "{printed}");

    // Redacting is not the same as hiding. Which headers a request carried, and
    // where it went, are the things a debugging session actually needs.
    assert!(printed.contains("Authorization"), "{printed}");
    assert!(printed.contains("Accept"), "{printed}");
    assert!(printed.contains("application/json"), "{printed}");
    assert!(printed.contains("gmail.googleapis.com"), "{printed}");

    // Case is the sender's choice, not ours.
    let lower = HttpRequest {
        method: HttpMethod::Get,
        url: "https://example.test/".into(),
        headers: vec![("authorization".into(), format!("Bearer {ACCESS}"))],
        body: None,
    };
    assert!(!format!("{lower:?}").contains(ACCESS));

    // And the accessor still works — redaction is a formatting decision, not a
    // change to what the struct holds.
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {ACCESS}").as_str())
    );
}

/// A request body can be a draft, an address book, or a whole message. It is
/// not a credential, but it is the owner's mail, and a length is all a log
/// needs.
#[test]
fn a_google_request_does_not_print_its_body() {
    let request = HttpRequest {
        method: HttpMethod::Post,
        url: "https://gmail.googleapis.com/gmail/v1/users/me/messages/send".into(),
        headers: vec![],
        body: Some(b"Subject: the private thing\r\n\r\nthe private thing".to_vec()),
    };
    let printed = format!("{request:?}");
    assert!(!printed.contains("private thing"), "{printed}");
    assert!(printed.contains("47 bytes"), "{printed}");
}

// ===========================================================================
// The token endpoint response
// ===========================================================================

/// On the path that *succeeds*, this body is the refresh token. That is the
/// whole reason its `Debug` is hand-written: the failure body is safe and the
/// success body is a permanent credential, they are the same field, and
/// choosing between them at format time is the kind of correctness that
/// survives exactly until somebody moves a line.
#[test]
fn a_token_endpoint_response_never_prints_its_body() {
    let success = oauth::HttpResponse {
        status: 200,
        body: format!(
            r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","expires_in":3599}}"#
        ),
    };

    let printed = format!("{success:?}");
    assert!(!printed.contains(REFRESH), "the refresh token was printed");
    assert!(!printed.contains(ACCESS), "the access token was printed");
    assert!(printed.contains("200"), "the status is still useful: {printed}");
    assert!(printed.contains("redacted"), "{printed}");

    // The failure body is redacted too. It carries nothing secret, but a rule
    // with an exception in it is a rule somebody has to get right twice.
    let failure = oauth::HttpResponse {
        status: 400,
        body: r#"{"error":"invalid_grant"}"#.into(),
    };
    let printed = format!("{failure:?}");
    assert!(!printed.contains("invalid_grant"), "{printed}");
    assert!(printed.contains("400"), "{printed}");
}

/// The reason a redacted failure body costs nothing: the classification the
/// user actually sees is built by `AuthError`, not by `Debug`.
#[test]
fn a_refused_grant_still_says_so_in_prose() {
    let failure = oauth::HttpResponse {
        status: 400,
        body: r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
            .into(),
    };
    assert!(!failure.is_success());
    let message = oauth::refresh_error(&failure).to_string();
    assert!(
        message.contains("invalid_grant") || message.contains("expired") || message.contains("revoked"),
        "a user has to be told why they are being asked to sign in again: {message}"
    );
}

// ===========================================================================
// The in-memory token store
// ===========================================================================

/// The store `auth`'s own rule was written for, which had been reached by a
/// derive. It is test-and-headless-only, and a map of every refresh token the
/// process holds is not a thing to print anywhere.
#[test]
fn the_memory_token_store_prints_accounts_and_not_tokens() {
    let store = MemoryTokenStore::default();
    store
        .save_refresh_token("bruno@example.test", &Secret::new(REFRESH))
        .unwrap();

    let printed = format!("{store:?}");
    assert!(!printed.contains(REFRESH), "a refresh token was printed: {printed}");
    assert!(
        printed.contains("bruno@example.test"),
        "which accounts are held is the useful half: {printed}"
    );

    // It still works.
    assert_eq!(
        store
            .load_refresh_token("bruno@example.test")
            .unwrap()
            .map(|s| s.expose().to_string()),
        Some(REFRESH.to_string())
    );
}

// ===========================================================================
// The agent's own credential
// ===========================================================================

/// The Anthropic key, which reached three derives.
///
/// `auth`'s rule — every type holding secret material writes `Debug` by hand —
/// stopped at the module boundary, and `agent` had grown its own credential
/// since. The key is in `Credential`, which is in `AgentConfig`, which is in
/// `Backend`; and it is in `ModelCall`'s headers, which is the one that would
/// actually have printed, because every test double in the suite collects those
/// into a `Vec` and a failing `assert_eq!` prints both sides.
const API_KEY: &str = "sk-ant-THIS-IS-AN-API-KEY-DO-NOT-PRINT-ME";

#[test]
fn the_agent_credential_prints_its_kind_and_not_its_key() {
    let key = Credential::ApiKey(API_KEY.to_string());
    let printed = format!("{key:?}");
    assert!(!printed.contains(API_KEY), "the API key was printed: {printed}");
    assert!(printed.contains("redacted"), "{printed}");
    // Which kind it is, is the whole question anybody debugging this asks.
    assert!(printed.contains("ApiKey"), "{printed}");

    let token = Credential::BearerToken(ACCESS.to_string());
    let printed = format!("{token:?}");
    assert!(!printed.contains(ACCESS), "{printed}");
    assert!(printed.contains("BearerToken"), "{printed}");
}

#[test]
fn an_agent_config_and_the_backend_holding_it_print_no_key() {
    let config = AgentConfig {
        credential: Credential::ApiKey(API_KEY.to_string()),
        model: "claude-opus-5".into(),
        effort: "medium".into(),
        max_tokens: 32_000,
        base_url: "https://api.anthropic.com".into(),
        fallbacks: true,
    };

    let printed = format!("{config:?}");
    assert!(!printed.contains(API_KEY), "{printed}");
    // Everything about a config that is not the credential is inert and useful.
    assert!(printed.contains("claude-opus-5"), "{printed}");
    assert!(printed.contains("api.anthropic.com"), "{printed}");

    let backend = Backend::AnthropicApi(Box::new(config));
    let printed = format!("{backend:?}");
    assert!(!printed.contains(API_KEY), "{printed}");
    assert!(printed.contains("claude-opus-5"), "{printed}");
}

/// The other half of `Backend`: a command line the owner typed, which Mach never
/// looks inside and therefore cannot clear.
#[test]
fn a_configured_command_backend_prints_its_program_and_not_its_arguments() {
    let backend = Backend::Command {
        program: "/usr/local/bin/my-brain".into(),
        args: vec!["--api-key".into(), API_KEY.into()],
    };
    let printed = format!("{backend:?}");
    assert!(!printed.contains(API_KEY), "{printed}");
    assert!(printed.contains("my-brain"), "the program is the useful half: {printed}");
    assert!(printed.contains("redacted"), "{printed}");
}

/// The one with the short path to a real disclosure: a failing `assert_eq!` on
/// a `Vec<ModelCall>`, on a machine with a live key in `.env.local`.
#[test]
fn a_model_call_prints_its_header_names_and_not_their_values() {
    let call = ModelCall {
        url: "https://api.anthropic.com/v1/messages".into(),
        headers: BTreeMap::from([
            ("x-api-key".to_string(), API_KEY.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]),
        body: r#"{"model":"claude-opus-5"}"#.into(),
    };

    let printed = format!("{call:?}");
    assert!(!printed.contains(API_KEY), "the API key was printed: {printed}");

    // Which headers went out is what a wire bug is about, so the names stay.
    assert!(printed.contains("x-api-key"), "{printed}");
    assert!(printed.contains("anthropic-version"), "{printed}");
    assert!(printed.contains("content-type"), "{printed}");
    assert!(printed.contains("api.anthropic.com"), "{printed}");
    // Every value goes, not just the credential's: a rule with an exception in
    // it is a rule somebody has to get right twice, and the next header that
    // carries a secret will not be called `x-api-key`.
    assert!(!printed.contains("2023-06-01"), "{printed}");

    // The body is the prompt, not a credential, and it is what a scripted
    // transport is asserted against.
    assert!(printed.contains("claude-opus-5"), "{printed}");

    // Redaction is a formatting decision; the value is still there to send.
    assert_eq!(call.headers.get("x-api-key").map(String::as_str), Some(API_KEY));
}

/// The tripwire under all four: a bearer token reaches the same headers when
/// there is no key, so the redaction cannot be about the *name* `x-api-key`.
#[test]
fn the_oauth_flavour_of_the_agent_credential_is_redacted_the_same_way() {
    let config = AgentConfig {
        credential: Credential::BearerToken(ACCESS.to_string()),
        model: "claude-opus-5".into(),
        effort: "medium".into(),
        max_tokens: 32_000,
        base_url: "https://api.anthropic.com".into(),
        fallbacks: false,
    };
    let call = ModelCall {
        url: "https://api.anthropic.com/v1/messages".into(),
        headers: mach_lib::ipc::agent::engine::wire::call_headers(&config, false),
        body: String::new(),
    };
    let printed = format!("{call:?}");
    assert!(!printed.contains(ACCESS), "{printed}");
    assert!(printed.contains("authorization"), "{printed}");
}

// ===========================================================================
// The invariants the safe paths depend on
// ===========================================================================

/// `auth::http` turns a `reqwest` failure into `AuthError::Transport` using
/// `reqwest::Error`'s `Display`, **which includes the request URL**. That is
/// safe today for exactly one reason: the OAuth code, the PKCE verifier and the
/// client secret travel in the POST *body*, and the endpoint constants carry no
/// query string at all.
///
/// This test is the tripwire under that reason. If anyone ever moves a
/// parameter into the URL, a transport error starts printing it.
#[test]
fn the_oauth_endpoints_carry_no_query_string() {
    for endpoint in [oauth::TOKEN_ENDPOINT, oauth::AUTH_ENDPOINT] {
        assert!(
            !endpoint.contains('?'),
            "{endpoint} has a query string, and a transport error prints the URL"
        );
    }
}

/// The client secret lives in an env var read from `.env.local` and reaches
/// exactly one place: the POST body of the token exchange. It must never be in
/// the URL the browser is sent to — that URL is displayed, logged by the
/// browser, and kept in its history.
///
/// (`tests/auth.rs` pins the verifier's absence from the same URL; this pins
/// the secret's, which is the half that would be permanent.)
#[test]
fn the_authorization_url_never_carries_the_client_secret() {
    let config = ClientConfig::new(
        "client-id-123.apps.googleusercontent.com",
        Some("client-secret-xyz"),
    );
    let session = oauth::AuthSession::start("http://127.0.0.1:9999/callback").unwrap();
    let url = oauth::authorization_url(&config, &session, None);

    assert!(!url.contains("client-secret-xyz"), "{url}");
    assert!(!url.contains("client_secret"), "{url}");
    assert!(url.contains("code_challenge_method=S256"), "{url}");
    assert!(url.contains("client-id-123"), "{url}");

    // And the config that holds it does not print it either.
    let printed = format!("{config:?}");
    assert!(!printed.contains("client-secret-xyz"), "{printed}");
}
