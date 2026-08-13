//! One-shot completions — the model call behind ghost text.
//!
//! This is deliberately *not* a session. A session has history, tools, an
//! approval gate, an event channel and a state machine, and every one of those
//! is wrong for a suggestion that has to arrive before the next keystroke and
//! is thrown away if it does not. So: one request, no tools, no stream, no
//! state, and the answer is a string.
//!
//! | variable | meaning | default |
//! |---|---|---|
//! | `MACH_COMPLETION_MODEL` | model id for ghost text | `claude-haiku-4-5-20251001` |
//!
//! # Why a different model from the agent
//!
//! The agent runs on Opus because it chains reads into an action a person will
//! be held to. A completion finishes the sentence you are already typing, and
//! the only quality that matters is that it is on screen while you still care —
//! a two-second suggestion is not a suggestion, it is an interruption. Haiku is
//! the fastest model that writes a usable clause, so it is the default, and
//! `MACH_COMPLETION_MODEL` moves it for anyone who disagrees.
//!
//! # Why the request is not streamed
//!
//! Everything else here streams, because a paragraph appearing word by word is
//! better than a paragraph appearing late. Ghost text is the opposite: a
//! suggestion that grows under the caret while you read it is unusable, and it
//! is at most a sentence anyway. `stream: false` also means the response is one
//! JSON document, so this module needs neither the SSE decoder nor the turn
//! accumulator — it collects the chunks and parses once.

//! # Two ways to reach a model, one shape
//!
//! Everything above describes `POST /v1/messages`, which was the only way this
//! module could reach a model and is the wrong *default* for this app: Mach's
//! agent runs on the Claude Code CLI and needs no API key, so a one-shot that
//! could only go over HTTP could only run for somebody holding a credential the
//! app does not otherwise ask for. [`Completer`] is the seam that fixes that —
//! same request, same string back, and the caller does not know which of the
//! two answered.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::google::BoxFuture;

use super::backend::Backend;
use super::config::AgentConfig;
use super::error::AgentError;
use super::wire::{api_message, call_headers, ModelCall, ModelTransport};

pub const ENV_COMPLETION_MODEL: &str = "MACH_COMPLETION_MODEL";
pub const DEFAULT_COMPLETION_MODEL: &str = "claude-haiku-4-5-20251001";

/// A ceiling on what the frontend may ask for. A completion is a clause; a
/// caller asking for a thousand tokens has misunderstood the feature.
pub const MAX_OUTPUT_TOKENS: u32 = 400;
pub const DEFAULT_OUTPUT_TOKENS: u32 = 160;

/// The ceiling for a *structured* one-shot — a caller in this process asking for
/// a JSON document rather than a clause under a caret.
///
/// Separate from [`MAX_OUTPUT_TOKENS`] because the two limits are protecting
/// against different things. That one exists because the webview is untrusted
/// input and a completion that takes a second is not a completion. This one
/// exists because three whole replies is a few thousand tokens and nothing is
/// waiting on them — but a model that starts writing an essay should still stop.
pub const MAX_STRUCTURED_TOKENS: u32 = 2_400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    pub system: String,
    pub prompt: String,
    pub max_tokens: u32,
}

impl CompletionRequest {
    pub fn new(system: String, prompt: String, max_tokens: Option<u32>) -> Self {
        CompletionRequest {
            system,
            prompt,
            max_tokens: max_tokens
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_OUTPUT_TOKENS)
                .min(MAX_OUTPUT_TOKENS),
        }
    }

    /// A one-shot whose answer is a document rather than a clause. Held to
    /// [`MAX_STRUCTURED_TOKENS`]; the caller is in this process, not the
    /// webview.
    pub fn structured(system: String, prompt: String, max_tokens: u32) -> Self {
        CompletionRequest {
            system,
            prompt,
            max_tokens: max_tokens.clamp(1, MAX_STRUCTURED_TOKENS),
        }
    }
}

/// The model ghost text runs on.
pub fn completion_model() -> String {
    std::env::var(ENV_COMPLETION_MODEL)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_COMPLETION_MODEL.to_string())
}

/// The JSON body. No tools, no `output_config` — the effort control is an Opus
/// feature and sending it to a model that does not take it is a 400 for no
/// gain. The system block still carries a cache breakpoint: the same handful of
/// system prompts is sent over and over as somebody types.
pub fn completion_body(model: &str, request: &CompletionRequest) -> Value {
    json!({
        "model": model,
        "max_tokens": request.max_tokens,
        "stream": false,
        "system": [{
            "type": "text",
            "text": request.system,
            "cache_control": { "type": "ephemeral" },
        }],
        "messages": [{
            "role": "user",
            "content": [{ "type": "text", "text": request.prompt }],
        }],
    })
}

pub fn completion_call(config: &AgentConfig, model: &str, request: &CompletionRequest) -> ModelCall {
    // `fallbacks: false` — a server-side fallback would swap the model for a
    // slower one, which is the one thing a completion cannot afford.
    let headers: BTreeMap<String, String> = call_headers(config, false);
    ModelCall {
        url: config.messages_url(),
        headers,
        body: completion_body(model, request).to_string(),
    }
}

/// What one completion consumed, as the API reported it.
///
/// Every field is `Option` because "the response did not say" and "the response
/// said zero" are different facts, and only the first of them is common — a
/// stubbed transport, a proxy that strips `usage`, or a future response shape
/// all produce absence rather than zero. Recording a zero for an answer nobody
/// gave is how a spend ledger comes to under-report.
///
/// Cache fields are carried separately because they are priced differently:
/// a cache read is a tenth of an input token, and a spend figure that charged
/// them at full rate would over-report on exactly the path this feature uses
/// most (the same system prompt, over and over).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
}

impl Usage {
    /// Whether the response said anything at all about what this cost.
    pub fn is_known(&self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some()
    }

    /// Every input token, however it was billed.
    pub fn total_input(&self) -> i64 {
        self.input_tokens.unwrap_or(0)
            + self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0)
    }
}

/// Read a `usage` block off whichever JSON document carries one.
///
/// Both backends have one and they agree on the field names: the API puts it at
/// the top level of the `/v1/messages` response, and Claude Code puts it on its
/// result document. Never an error — a body with no `usage` is a completion
/// whose cost is unknown, which is a state this codebase can represent, and
/// failing the whole generation because the accounting was missing would be the
/// tail wagging the dog.
pub fn usage_of(value: &Value) -> Usage {
    let Some(usage) = value.get("usage") else {
        return Usage::default();
    };
    let field = |name: &str| usage.get(name).and_then(Value::as_i64).filter(|n| *n >= 0);
    Usage {
        input_tokens: field("input_tokens"),
        output_tokens: field("output_tokens"),
        cache_creation_input_tokens: field("cache_creation_input_tokens"),
        cache_read_input_tokens: field("cache_read_input_tokens"),
    }
}

/// The same, off a response body that may not be JSON at all.
pub fn completion_usage(body: &str) -> Usage {
    serde_json::from_str::<Value>(body)
        .map(|value| usage_of(&value))
        .unwrap_or_default()
}

/// What one completion cost, in whichever units the backend that ran it knows.
///
/// The two backends report different things, and neither can be derived from the
/// other:
///
/// | backend | tokens | dollars |
/// |---|---|---|
/// | [`ApiCompleter`] | in the `usage` block | priced by [`super::price`], and only on a key |
/// | [`CliCompleter`] | in the `usage` block | `total_cost_usd`, off the CLI itself |
///
/// `usd` is `Option` and every producer has to say which it means. `None` is
/// "nobody said", never "it was free" — see [`super::price::cost_usd`] for the
/// ways the API path legitimately cannot answer.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cost {
    pub usd: Option<f64>,
    /// Every input token, cached or not.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

impl Cost {
    /// The token half of a [`Usage`], with no opinion about money.
    pub fn of(usage: &Usage) -> Cost {
        Cost {
            usd: None,
            input_tokens: usage.is_known().then(|| usage.total_input()),
            output_tokens: usage.output_tokens,
        }
    }
}

/// One answer, and what it took to get it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completion {
    pub text: String,
    pub cost: Cost,
}

/// The text of a non-streamed `/v1/messages` response.
///
/// Total: a body with no text blocks (a pure refusal, an empty turn) is an
/// empty completion, not an error. Ghost text has no error state.
pub fn completion_text(body: &str) -> Result<String, AgentError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| AgentError::Protocol(format!("the completion was not JSON: {e}")))?;

    if value.get("error").is_some() {
        return Err(AgentError::Api {
            status: 200,
            message: api_message(body),
        });
    }

    let text = value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    Ok(text)
}

/// Ask for one completion, on the ghost-text model.
pub async fn complete(
    transport: &dyn ModelTransport,
    config: &AgentConfig,
    request: &CompletionRequest,
) -> Result<String, AgentError> {
    complete_as(transport, config, &completion_model(), request).await
}

/// The same, naming the model.
///
/// Ghost text is not the only thing in Mach that wants one request, no tools and
/// no stream — reply suggestions want the same shape on a different model, and
/// duplicating the SSE-free request path to change one string would have been
/// the second way to call `/v1/messages`.
pub async fn complete_as(
    transport: &dyn ModelTransport,
    config: &AgentConfig,
    model: &str,
    request: &CompletionRequest,
) -> Result<String, AgentError> {
    complete_as_metered(transport, config, model, request)
        .await
        .map(|(text, _)| text)
}

/// The same again, and what the response said it consumed.
///
/// Split out rather than changing [`complete_as`] because ghost text has no use
/// for the number — it fires on a keystroke and nothing anywhere records it —
/// while reply suggestions run unattended against arriving mail and the number
/// is the whole reason there is a cap.
pub async fn complete_as_metered(
    transport: &dyn ModelTransport,
    config: &AgentConfig,
    model: &str,
    request: &CompletionRequest,
) -> Result<(String, Usage), AgentError> {
    let mut rx = transport.send(completion_call(config, model, request)).await?;

    let mut body = Vec::new();
    while let Some(chunk) = rx.recv().await {
        body.extend_from_slice(&chunk?);
        // A model that ignores `stream: false` would stream forever into this
        // buffer. One megabyte is far past any completion and far short of a
        // problem.
        if body.len() > 1_000_000 {
            return Err(AgentError::Protocol(
                "the completion response was implausibly large".to_string(),
            ));
        }
    }

    let body = String::from_utf8_lossy(&body);
    // Usage before text, so a body that is an error still reports what the
    // attempt consumed — an API error after tokens were spent is still spend.
    let usage = completion_usage(&body);
    completion_text(&body).map(|text| (text, usage))
}

// ===========================================================================
// Whichever way this machine reaches a model
// ===========================================================================

/// One structured completion, however this machine reaches a model.
///
/// Object-safe and future-boxing for the same reason [`super::Brain`] is: the
/// two implementations are an HTTPS request and a child process, and the caller
/// — an unattended pass over newly arrived mail — must not know which it has.
///
/// It is deliberately *not* [`super::Brain`]. A brain gets tools, an approval
/// gate, an event channel and a transcript; this gets a string.
///
/// It answers with a [`Completion`] rather than a `String` because the caller
/// that must not know which backend it has is also the caller that has to write
/// down what the call cost. Each side reports what it actually knows —
/// [`Cost`] — and neither invents the other's number.
pub trait Completer: Send + Sync {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        request: &'a CompletionRequest,
    ) -> BoxFuture<'a, Result<Completion, AgentError>>;

    /// Which brain this is, for the sentence a failure logs.
    fn label(&self) -> String;
}

/// `POST /v1/messages`, for an owner who has configured a key.
pub struct ApiCompleter {
    config: AgentConfig,
    transport: Arc<dyn ModelTransport>,
}

impl ApiCompleter {
    pub fn new(config: AgentConfig, transport: Arc<dyn ModelTransport>) -> ApiCompleter {
        ApiCompleter { config, transport }
    }
}

impl Completer for ApiCompleter {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        request: &'a CompletionRequest,
    ) -> BoxFuture<'a, Result<Completion, AgentError>> {
        Box::pin(async move {
            let (text, usage) =
                complete_as_metered(self.transport.as_ref(), &self.config, model, request).await?;
            // The pricing lives here rather than at the caller because the
            // credential does: `/v1/messages` reports tokens and never money,
            // and whether those tokens have a dollar figure at all is a fact
            // about the credential this completer is holding.
            Ok(Completion {
                text,
                cost: Cost {
                    usd: super::price::cost_usd(&self.config, model, &usage),
                    ..Cost::of(&usage)
                },
            })
        })
    }

    fn label(&self) -> String {
        format!("the Anthropic API ({})", self.config.base_url)
    }
}

/// `claude --print`, which is what this app runs on.
pub struct CliCompleter {
    exe: PathBuf,
    /// Where the child runs. Mach's own directory beside the database — never
    /// the owner's home directory, and never a bundled app's `/`.
    workspace: PathBuf,
}

impl CliCompleter {
    pub fn new(exe: PathBuf, workspace: PathBuf) -> CliCompleter {
        CliCompleter { exe, workspace }
    }
}

impl Completer for CliCompleter {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        request: &'a CompletionRequest,
    ) -> BoxFuture<'a, Result<Completion, AgentError>> {
        Box::pin(super::cli::complete_once(
            &self.exe,
            &self.workspace,
            model,
            request,
        ))
    }

    fn label(&self) -> String {
        format!("Claude Code ({})", self.exe.display())
    }
}

/// The completer a resolved backend implies.
///
/// `model` is not taken from the backend: a one-shot names its own, and the
/// agent's model is the expensive one.
pub fn completer_for(
    backend: Backend,
    transport: Arc<dyn ModelTransport>,
    workspace: PathBuf,
) -> Result<Box<dyn Completer>, AgentError> {
    match backend {
        Backend::ClaudeCli { exe, .. } => Ok(Box::new(CliCompleter::new(exe, workspace))),
        Backend::AnthropicApi(config) => Ok(Box::new(ApiCompleter::new(*config, transport))),
        // A custom command implements the *session* contract in
        // `docs/agent-backends.md` — a tool server, a stream of events, an
        // approval round trip. There is no one-shot in it, and inventing a
        // second contract for a program somebody else wrote is worse than
        // saying so. This is the whole reason `completer_for` returns a
        // `Result`.
        Backend::Command { program, .. } => Err(AgentError::MissingApiKey(format!(
            "Preferences ask for a custom agent command ({}), which answers ⌘K but cannot write \
             a reply on its own — the contract in docs/agent-backends.md is a session, not a \
             one-shot. Choose Claude Code or the Anthropic API in Preferences → Agent, or switch \
             written replies off.",
            program.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::Credential;
    use super::*;

    fn config() -> AgentConfig {
        AgentConfig {
            credential: Credential::ApiKey("k".to_string()),
            model: "claude-opus-5".to_string(),
            effort: "medium".to_string(),
            max_tokens: 32_000,
            base_url: "https://api.anthropic.com".to_string(),
            fallbacks: true,
        }
    }

    #[test]
    fn caps_what_the_frontend_may_ask_for() {
        let request = CompletionRequest::new("s".into(), "p".into(), Some(99_999));
        assert_eq!(request.max_tokens, MAX_OUTPUT_TOKENS);
        let zero = CompletionRequest::new("s".into(), "p".into(), Some(0));
        assert_eq!(zero.max_tokens, DEFAULT_OUTPUT_TOKENS);
        let absent = CompletionRequest::new("s".into(), "p".into(), None);
        assert_eq!(absent.max_tokens, DEFAULT_OUTPUT_TOKENS);
    }

    #[test]
    fn the_body_carries_no_tools_and_does_not_stream() {
        let request = CompletionRequest::new("be brief".into(), "Thanks for the".into(), Some(64));
        let body = completion_body("claude-haiku-4-5-20251001", &request);
        assert_eq!(body["stream"], json!(false));
        assert_eq!(body["max_tokens"], json!(64));
        assert!(body.get("tools").is_none());
        assert!(body.get("output_config").is_none());
        assert_eq!(body["system"][0]["text"], json!("be brief"));
        assert_eq!(body["messages"][0]["content"][0]["text"], json!("Thanks for the"));
    }

    #[test]
    fn the_call_authenticates_without_the_fallback_beta() {
        let request = CompletionRequest::new("s".into(), "p".into(), None);
        let call = completion_call(&config(), "m", &request);
        assert_eq!(call.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(call.headers.get("x-api-key").map(String::as_str), Some("k"));
        assert!(call.headers.get("anthropic-beta").is_none());
    }

    #[test]
    fn text_blocks_are_joined_and_everything_else_ignored() {
        let body = r#"{"content":[{"type":"thinking","thinking":""},
                       {"type":"text","text":"update"},
                       {"type":"text","text":" soon"}]}"#;
        assert_eq!(completion_text(body).unwrap(), "update soon");
    }

    #[test]
    fn an_empty_turn_is_an_empty_completion() {
        assert_eq!(completion_text(r#"{"content":[]}"#).unwrap(), "");
        assert_eq!(completion_text(r#"{}"#).unwrap(), "");
    }

    #[test]
    fn usage_is_read_off_the_body() {
        let body = r#"{"content":[],"usage":{"input_tokens":2100,"output_tokens":380,
                       "cache_creation_input_tokens":0,"cache_read_input_tokens":1024}}"#;
        let usage = completion_usage(body);
        assert_eq!(usage.input_tokens, Some(2100));
        assert_eq!(usage.output_tokens, Some(380));
        assert_eq!(usage.cache_read_input_tokens, Some(1024));
        assert_eq!(usage.total_input(), 3124);
        assert!(usage.is_known());
        assert_eq!(Cost::of(&usage).input_tokens, Some(3124));
    }

    #[test]
    fn a_body_with_no_usage_reports_nothing_rather_than_zero() {
        // The distinction the whole spend ledger rests on: a stub, a proxy that
        // strips the block, or a shape nobody has seen yet must not be recorded
        // as a free generation.
        for body in [r#"{"content":[]}"#, "not json", r#"{"usage":{}}"#] {
            let usage = completion_usage(body);
            assert!(!usage.is_known(), "{body}");
            assert_eq!(usage.input_tokens, None);
            assert_eq!(usage.output_tokens, None);
            assert_eq!(Cost::of(&usage), Cost::default(), "{body}");
        }
    }

    #[test]
    fn a_nonsense_token_count_is_absent_rather_than_negative() {
        let usage = completion_usage(r#"{"usage":{"input_tokens":-5,"output_tokens":"lots"}}"#);
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
    }

    /// A transport that cannot possibly answer, so a test that gets a
    /// completer out of `completer_for` is testing the mapping and nothing else.
    struct NoTransport;

    impl ModelTransport for NoTransport {
        fn send<'a>(
            &'a self,
            _call: ModelCall,
        ) -> BoxFuture<'a, Result<super::super::wire::ChunkStream, AgentError>> {
            Box::pin(async { Err(AgentError::transport("no network here")) })
        }
    }

    /// The rule this whole seam exists for: the CLI is a completer, so a
    /// machine with `claude` on it and no credential anywhere can still write
    /// a reply.
    #[test]
    fn a_claude_code_backend_is_a_completer_without_a_credential() {
        let backend = Backend::ClaudeCli {
            exe: PathBuf::from("/nowhere/claude"),
            model: Some("opus".to_string()),
        };
        let completer =
            completer_for(backend, Arc::new(NoTransport), PathBuf::from("/tmp/mach-agent"))
                .expect("Claude Code is a completer");
        assert!(completer.label().starts_with("Claude Code"));
    }

    #[test]
    fn a_custom_command_is_not_a_completer_and_says_why() {
        let backend = Backend::Command {
            program: PathBuf::from("/usr/local/bin/my-agent"),
            args: Vec::new(),
        };
        let error = completer_for(backend, Arc::new(NoTransport), PathBuf::from("/tmp/mach-agent"))
            .err()
            .expect("a custom command has no one-shot");
        match error {
            AgentError::MissingApiKey(message) => {
                assert!(message.contains("my-agent"), "{message}");
                assert!(message.contains("docs/agent-backends.md"), "{message}");
            }
            other => panic!("expected a named refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_error_body_is_an_error() {
        let body = r#"{"error":{"message":"overloaded"}}"#;
        assert!(matches!(completion_text(body), Err(AgentError::Api { .. })));
        assert!(matches!(
            completion_text("not json"),
            Err(AgentError::Protocol(_))
        ));
    }
}
