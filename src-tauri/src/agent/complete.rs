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

    completion_text(&String::from_utf8_lossy(&body))
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
pub trait Completer: Send + Sync {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        request: &'a CompletionRequest,
    ) -> BoxFuture<'a, Result<String, AgentError>>;

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
    ) -> BoxFuture<'a, Result<String, AgentError>> {
        Box::pin(complete_as(
            self.transport.as_ref(),
            &self.config,
            model,
            request,
        ))
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
    ) -> BoxFuture<'a, Result<String, AgentError>> {
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
