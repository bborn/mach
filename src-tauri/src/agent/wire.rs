//! The Anthropic Messages API, over raw HTTPS.
//!
//! There is no official Anthropic SDK for Rust, so this is the sanctioned path:
//! `POST /v1/messages` with `stream: true`, parsed as Server-Sent Events. Three
//! pieces, in dependency order:
//!
//! | piece | job |
//! |---|---|
//! | [`ModelTransport`] | one request → a channel of byte chunks |
//! | [`SseDecoder`] | byte chunks → complete `data:` payloads |
//! | [`TurnAccumulator`] | payloads → an assistant message, block by block |
//!
//! The split is what makes the whole thing testable without a network: the
//! decoder and the accumulator are pure and `tests/agent.rs` drives them with a
//! scripted transport that hands back the exact bytes Anthropic would.
//!
//! # Why the accumulator rebuilds whole content blocks
//!
//! A streamed turn arrives as `content_block_start` + deltas. The next request
//! has to carry the assistant turn back **unchanged** — including `thinking`
//! blocks, which Claude Opus 5 emits by default and which must be echoed
//! verbatim on the same model. So the accumulator starts from the block object
//! the API sent and applies deltas into it, rather than synthesising a block
//! from the text it happened to collect. What goes back on the wire is what
//! came off it.
//!
//! # Thinking
//!
//! Nothing sets `thinking` on the request. On Claude Opus 5 thinking is on by
//! default and `budget_tokens` is rejected outright; `output_config.effort` is
//! the depth control. `display` is left at its default (`omitted`), so thinking
//! blocks arrive with empty text — they are still echoed back, and the drawer
//! shows tool activity rather than reasoning.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::google::BoxFuture;

use super::config::{AgentConfig, Credential, API_VERSION, FALLBACK_BETA};
use super::error::AgentError;

// ===========================================================================
// Transport
// ===========================================================================

/// One HTTP request, already fully formed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCall {
    pub url: String,
    /// Sorted, so a test can assert on them without caring about insertion
    /// order and a header is never silently sent twice.
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

/// The chunks of one streaming response.
pub type ChunkStream = mpsc::Receiver<Result<Vec<u8>, AgentError>>;

/// The seam. Production wires reqwest; tests script the bytes.
///
/// Returning a channel rather than a stream object keeps the trait object-safe
/// without a `Stream` dependency, and lets a fake deliver a response in as many
/// pieces as it likes — including pieces that split an SSE frame in half, which
/// is the case the decoder exists for.
pub trait ModelTransport: Send + Sync {
    fn send<'a>(&'a self, call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>>;
}

/// The production transport.
pub struct ReqwestModelTransport {
    client: reqwest::Client,
}

impl ReqwestModelTransport {
    pub fn new() -> Self {
        ReqwestModelTransport {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestModelTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelTransport for ReqwestModelTransport {
    fn send<'a>(&'a self, call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>> {
        Box::pin(async move {
            let mut builder = self.client.post(&call.url).body(call.body);
            for (name, value) in &call.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }

            let response = builder
                .send()
                .await
                .map_err(|e| AgentError::transport(e.to_string()))?;

            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                let body = response.text().await.unwrap_or_default();
                return Err(AgentError::Api {
                    status,
                    message: api_message(&body),
                });
            }

            // A bounded channel means a slow consumer applies backpressure to
            // the socket instead of buffering a whole answer in memory.
            let (tx, rx) = mpsc::channel(32);
            tokio::spawn(async move {
                let mut response = response;
                loop {
                    match response.chunk().await {
                        Ok(Some(bytes)) => {
                            if tx.send(Ok(bytes.to_vec())).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => return,
                        Err(e) => {
                            let _ = tx.send(Err(AgentError::transport(e.to_string()))).await;
                            return;
                        }
                    }
                }
            });
            Ok(rx)
        })
    }
}

/// Pull a readable sentence out of an error body, falling back to the raw text.
pub fn api_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(512).collect())
}

// ===========================================================================
// Request
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Everything one turn needs. `messages` are raw content-block values because
/// they are round-tripped, not interpreted.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub system: String,
    pub messages: Vec<Value>,
    pub tools: Vec<ToolDefinition>,
}

/// The JSON body for `POST /v1/messages`.
pub fn request_body(config: &AgentConfig, request: &TurnRequest, fallbacks: bool) -> Value {
    let mut body = json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "stream": true,
        // A cache breakpoint on the last (only) system block covers tools and
        // system together — they render before messages and never change
        // inside a session, which is the whole prefix worth caching.
        "system": [{
            "type": "text",
            "text": request.system,
            "cache_control": { "type": "ephemeral" },
        }],
        "messages": request.messages,
        "tools": request.tools,
        "output_config": { "effort": config.effort },
    });

    if fallbacks {
        body["fallbacks"] = json!("default");
    }
    body
}

pub fn build_call(config: &AgentConfig, request: &TurnRequest, fallbacks: bool) -> ModelCall {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("anthropic-version".to_string(), API_VERSION.to_string());

    let mut betas: Vec<&str> = Vec::new();
    match &config.credential {
        Credential::ApiKey(key) => {
            headers.insert("x-api-key".to_string(), key.clone());
        }
        Credential::BearerToken(token) => {
            headers.insert("authorization".to_string(), format!("Bearer {token}"));
            // OAuth tokens go on `Authorization`, and `/v1/messages` requires
            // this beta alongside them.
            betas.push("oauth-2025-04-20");
        }
    }
    if fallbacks {
        betas.push(FALLBACK_BETA);
    }
    if !betas.is_empty() {
        headers.insert("anthropic-beta".to_string(), betas.join(","));
    }

    ModelCall {
        url: config.messages_url(),
        headers,
        body: request_body(config, request, fallbacks).to_string(),
    }
}

/// Whether a 400 is the API rejecting the fallback beta rather than the
/// request. One retry without it is cheaper than making every account enable a
/// beta before the agent works at all.
pub fn is_fallback_rejection(error: &AgentError) -> bool {
    match error {
        AgentError::Api { status: 400, message } => {
            let m = message.to_ascii_lowercase();
            m.contains("fallback") || m.contains("beta")
        }
        _ => false,
    }
}

// ===========================================================================
// SSE
// ===========================================================================

/// Turns a byte stream into `data:` payloads.
///
/// SSE frames are separated by a blank line and a frame can be split across any
/// number of TCP reads, so this holds a buffer and only yields complete lines.
/// `event:` lines are ignored: every Anthropic event carries its own `type` in
/// the JSON, so the payload is self-describing and the header would be a second
/// source of truth.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        SseDecoder::default()
    }

    /// Feed bytes, get back every complete `data:` payload they completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut out = Vec::new();

        while let Some(newline) = self.buffer.find('\n') {
            let line: String = self.buffer.drain(..=newline).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() && data != "[DONE]" {
                    out.push(data.to_string());
                }
            }
        }
        out
    }
}

// ===========================================================================
// Accumulation
// ===========================================================================

/// What one assistant turn came to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantTurn {
    /// The content blocks, exactly as the API sent them, ready to be echoed
    /// back as the assistant message on the next request.
    pub content: Vec<Value>,
    pub stop_reason: Option<String>,
    /// The model that actually answered — differs from the requested one when
    /// a server-side fallback ran.
    pub model: Option<String>,
}

impl AssistantTurn {
    /// The visible text, blocks joined.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_uses(&self) -> Vec<ToolUse> {
        self.content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter_map(|b| {
                Some(ToolUse {
                    id: b.get("id")?.as_str()?.to_string(),
                    name: b.get("name")?.as_str()?.to_string(),
                    input: b.get("input").cloned().unwrap_or_else(|| json!({})),
                })
            })
            .collect()
    }

    pub fn wants_tools(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use") || !self.tool_uses().is_empty()
    }

    pub fn is_refusal(&self) -> bool {
        self.stop_reason.as_deref() == Some("refusal")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// What the session loop reacts to while a turn streams.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamSignal {
    /// Visible text, incrementally.
    TextDelta(String),
    /// The model started assembling a tool call — the name is known before the
    /// arguments are, which is exactly what "shows what it is doing" needs.
    ToolStarted { id: String, name: String },
    /// The turn is complete.
    Done,
}

/// Applies stream events to a turn under construction.
#[derive(Debug, Default)]
pub struct TurnAccumulator {
    turn: AssistantTurn,
    /// Partial `input_json_delta` text, per block index.
    partial_json: BTreeMap<usize, String>,
}

impl TurnAccumulator {
    pub fn new() -> Self {
        TurnAccumulator::default()
    }

    /// Apply one decoded SSE payload. Returns anything the caller should act on.
    pub fn apply(&mut self, payload: &str) -> Result<Vec<StreamSignal>, AgentError> {
        let event: Value = serde_json::from_str(payload)
            .map_err(|e| AgentError::Protocol(format!("{e} in {payload:.200}")))?;
        let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
        let mut signals = Vec::new();

        match kind {
            "message_start" => {
                self.turn.model = event
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }

            "content_block_start" => {
                let index = index_of(&event);
                let block = event
                    .get("content_block")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "text", "text": "" }));
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    if let (Some(id), Some(name)) = (
                        block.get("id").and_then(Value::as_str),
                        block.get("name").and_then(Value::as_str),
                    ) {
                        signals.push(StreamSignal::ToolStarted {
                            id: id.to_string(),
                            name: name.to_string(),
                        });
                    }
                    self.partial_json.insert(index, String::new());
                }
                self.set_block(index, block);
            }

            "content_block_delta" => {
                let index = index_of(&event);
                let delta = event.get("delta").cloned().unwrap_or_default();
                match delta.get("type").and_then(Value::as_str).unwrap_or_default() {
                    "text_delta" => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or_default();
                        self.append_str(index, "text", text);
                        if !text.is_empty() {
                            signals.push(StreamSignal::TextDelta(text.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        let text = delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        self.append_str(index, "thinking", text);
                    }
                    "signature_delta" => {
                        if let Some(sig) = delta.get("signature").and_then(Value::as_str) {
                            self.set_field(index, "signature", json!(sig));
                        }
                    }
                    "input_json_delta" => {
                        let fragment = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        self.partial_json.entry(index).or_default().push_str(fragment);
                    }
                    _ => {}
                }
            }

            "content_block_stop" => {
                let index = index_of(&event);
                if let Some(raw) = self.partial_json.remove(&index) {
                    // An empty argument object is streamed as no deltas at all.
                    let parsed = if raw.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&raw).map_err(|e| {
                            AgentError::Protocol(format!("tool arguments were not JSON: {e}"))
                        })?
                    };
                    self.set_field(index, "input", parsed);
                }
            }

            "message_delta" => {
                if let Some(reason) = event
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.turn.stop_reason = Some(reason.to_string());
                }
            }

            "message_stop" => signals.push(StreamSignal::Done),

            // The API reports mid-stream failures as an event, not a status.
            "error" => {
                let message = event
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("the model stream failed");
                return Err(AgentError::Api {
                    status: 200,
                    message: message.to_string(),
                });
            }

            _ => {}
        }

        Ok(signals)
    }

    pub fn finish(self) -> AssistantTurn {
        self.turn
    }

    fn set_block(&mut self, index: usize, block: Value) {
        while self.turn.content.len() <= index {
            self.turn.content.push(json!({ "type": "text", "text": "" }));
        }
        self.turn.content[index] = block;
    }

    fn set_field(&mut self, index: usize, key: &str, value: Value) {
        if let Some(Value::Object(map)) = self.turn.content.get_mut(index) {
            map.insert(key.to_string(), value);
        }
    }

    fn append_str(&mut self, index: usize, key: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(Value::Object(map)) = self.turn.content.get_mut(index) {
            let entry = map
                .entry(key.to_string())
                .or_insert_with(|| Value::String(String::new()));
            if let Value::String(existing) = entry {
                existing.push_str(text);
            }
        }
    }
}

fn index_of(event: &Value) -> usize {
    event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize
}

// ===========================================================================
// Message helpers
// ===========================================================================

pub fn user_text(text: impl Into<String>) -> Value {
    json!({
        "role": "user",
        "content": [{ "type": "text", "text": text.into() }],
    })
}

pub fn assistant_message(content: &[Value]) -> Value {
    json!({ "role": "assistant", "content": content })
}

/// One `tool_result` block. `is_error` is how a refused or failed tool is
/// reported *to the model* — it retries or explains rather than the session
/// dying.
pub fn tool_result(tool_use_id: &str, content: &str, is_error: bool) -> Value {
    let mut map = Map::new();
    map.insert("type".into(), json!("tool_result"));
    map.insert("tool_use_id".into(), json!(tool_use_id));
    map.insert("content".into(), json!(content));
    if is_error {
        map.insert("is_error".into(), json!(true));
    }
    Value::Object(map)
}

/// Every tool result for one assistant turn goes back in a **single** user
/// message — splitting them teaches the model to stop calling tools in
/// parallel.
pub fn tool_results_message(results: Vec<Value>) -> Value {
    json!({ "role": "user", "content": results })
}
