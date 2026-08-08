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

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::config::AgentConfig;
use super::error::AgentError;
use super::wire::{api_message, call_headers, ModelCall, ModelTransport};

pub const ENV_COMPLETION_MODEL: &str = "MACH_COMPLETION_MODEL";
pub const DEFAULT_COMPLETION_MODEL: &str = "claude-haiku-4-5-20251001";

/// A ceiling on what the frontend may ask for. A completion is a clause; a
/// caller asking for a thousand tokens has misunderstood the feature.
pub const MAX_OUTPUT_TOKENS: u32 = 400;
pub const DEFAULT_OUTPUT_TOKENS: u32 = 160;

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

/// Ask for one completion.
pub async fn complete(
    transport: &dyn ModelTransport,
    config: &AgentConfig,
    request: &CompletionRequest,
) -> Result<String, AgentError> {
    let model = completion_model();
    let mut rx = transport.send(completion_call(config, &model, request)).await?;

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
    fn an_error_body_is_an_error() {
        let body = r#"{"error":{"message":"overloaded"}}"#;
        assert!(matches!(completion_text(body), Err(AgentError::Api { .. })));
        assert!(matches!(
            completion_text("not json"),
            Err(AgentError::Protocol(_))
        ));
    }
}
