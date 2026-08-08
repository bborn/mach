//! Agent sessions: the invoke surface.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `agent_start` | `prompt`, `context?` | [`SessionSnapshot`] |
//! | `agent_sessions` | `sessionId?` | `SessionSnapshot[]` |
//! | `agent_send` | `sessionId`, `action?`, `message?`, `toolUseId?`, `reason?`, `itemId?` | `{ ok }` or a snapshot |
//! | `agent_status` | — | `{ configured, message, model, completionModel }` |
//! | `agent_complete` | `system`, `prompt`, `maxTokens?` | `{ text }` |
//!
//! # The last two are not sessions
//!
//! `agent_status` and `agent_complete` exist for ghost text, and neither builds
//! the engine: a completion has no history, no tools and nothing to approve, so
//! it needs the credential and a socket and nothing else. `agent_status` is the
//! graceful-fallback half — it answers "is there a key" *without* starting
//! anything, so a webview can decide to stay quiet instead of discovering the
//! problem one failed request at a time.
//!
//! Push, never poll: everything that happens inside a session arrives on the
//! `agent-session` Tauri event as a [`SessionEvent`]. `agent_sessions` exists
//! for the reload case, not the update case.
//!
//! # One command, several actions
//!
//! `agent_send` is a small router, for the same reason `send_message` is: three
//! handler names are registered in `lib.rs`, and `lib.rs` belongs to nobody
//! while several units are in flight. `action` defaults to `message`, which
//! keeps the plain two-argument call honest.
//!
//! | `action` | argument | effect |
//! |---|---|---|
//! | `message` | `message` | another turn in the same session |
//! | `approve` | `toolUseId` | let the parked outbound action run |
//! | `deny` | `toolUseId`, `reason?` | refuse it; the model is told why |
//! | `close` | — | stop and forget the session |
//! | `removeContext` | `itemId` | drop one attached line |
//!
//! # Where the module lives
//!
//! `src-tauri/src/agent/`, declared below with `#[path]` rather than in
//! `lib.rs`, exactly as `ipc::compose` does. Promoting it to `pub mod agent;`
//! at the crate root later is a one-line change that makes
//! `ipc::agent::engine` an alias.
//!
//! # The engine is built once, lazily
//!
//! It needs the `AppHandle` to emit and the `AppState` to act, and neither is
//! available at `bootstrap` time. So the first call constructs it and every
//! later call reuses it. The credential is *not* captured here — it is read per
//! session, so adding `ANTHROPIC_API_KEY` to the environment takes effect on
//! the next ⌘K rather than the next launch.

#[path = "../agent/mod.rs"]
pub mod engine;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use engine::complete::{complete, completion_model, CompletionRequest};
use engine::config::AgentConfig;
use engine::context::ContextItem;
use engine::session::{AgentEngine, Input, SessionEmitter, SessionEvent, SessionSnapshot};
use engine::wire::ReqwestModelTransport;
use engine::AgentError;

use super::error::IpcError;
use super::events;
use super::state::AppState;

/// The one channel every session speaks on.
pub const AGENT_SESSION_EVENT: &str = "agent-session";

// ---------------------------------------------------------------------------
// the emitter
// ---------------------------------------------------------------------------

/// Sends session events to the webview, and tells it when the agent has changed
/// the mailbox — the same `threads-changed` a manual archive emits, because an
/// agent's archive is the same command.
struct TauriEmitter {
    app: AppHandle,
}

impl SessionEmitter for TauriEmitter {
    fn session_event(&self, event: &SessionEvent) {
        // A failed emit means the window is gone, which is not something a
        // running session should die for.
        let _ = self.app.emit(AGENT_SESSION_EVENT, event);
    }

    fn threads_changed(&self) {
        events::emit_threads_changed(&self.app);
    }
}

// ---------------------------------------------------------------------------
// the engine
// ---------------------------------------------------------------------------

static ENGINE: OnceLock<Arc<AgentEngine>> = OnceLock::new();

fn engine(app: &AppHandle, state: &AppState) -> Result<Arc<AgentEngine>, IpcError> {
    if let Some(existing) = ENGINE.get() {
        return Ok(Arc::clone(existing));
    }

    // The agent sends through the composer's outbox, so a scheduled reply is
    // the same row the composer would have written and the same ten-second
    // recall applies.
    let outbox = Arc::new(
        super::compose::engine::outbox::Outbox::new(
            state.db.clone(),
            Arc::clone(&state.dispatcher.clients),
        )
        .map_err(IpcError::from)?,
    );

    let built = Arc::new(AgentEngine::new(
        state.db.clone(),
        Arc::clone(&state.dispatcher),
        outbox,
        Arc::clone(&state.plugins),
        Arc::new(engine::wire::ReqwestModelTransport::new()),
        Arc::new(TauriEmitter { app: app.clone() }),
    ));

    Ok(Arc::clone(ENGINE.get_or_init(|| built)))
}

// ---------------------------------------------------------------------------
// handlers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn agent_start(
    app: AppHandle,
    state: State<'_, AppState>,
    prompt: String,
    context: Option<Vec<ContextItem>>,
) -> Result<SessionSnapshot, IpcError> {
    let engine = engine(&app, &state)?;
    Ok(engine.start(prompt, context.unwrap_or_default())?)
}

#[tauri::command]
pub async fn agent_sessions(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<Vec<SessionSnapshot>, IpcError> {
    let engine = engine(&app, &state)?;
    Ok(match session_id {
        Some(id) => engine.session(&id).into_iter().collect(),
        None => engine.sessions(),
    })
}

#[tauri::command]
pub async fn agent_send(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    action: Option<String>,
    message: Option<String>,
    tool_use_id: Option<String>,
    reason: Option<String>,
    item_id: Option<String>,
) -> Result<Value, IpcError> {
    let engine = engine(&app, &state)?;
    let action = action.unwrap_or_else(|| "message".to_string());

    match action.as_str() {
        "message" => {
            let text = message
                .filter(|m| !m.trim().is_empty())
                .ok_or_else(|| invalid("a message needs some text"))?;
            engine.send(&session_id, Input::Message(text))?;
            Ok(json!({ "ok": true }))
        }
        "approve" => {
            engine.send(
                &session_id,
                Input::Approve {
                    tool_use_id: required(tool_use_id, "toolUseId")?,
                },
            )?;
            Ok(json!({ "ok": true }))
        }
        "deny" => {
            engine.send(
                &session_id,
                Input::Deny {
                    tool_use_id: required(tool_use_id, "toolUseId")?,
                    reason,
                },
            )?;
            Ok(json!({ "ok": true }))
        }
        "close" => {
            engine.close(&session_id)?;
            Ok(json!({ "ok": true }))
        }
        "removeContext" => {
            let context = engine.remove_context(&session_id, &required(item_id, "itemId")?)?;
            Ok(json!({ "ok": true, "context": context }))
        }
        other => Err(invalid(format!("unknown agent action {other:?}"))),
    }
}

// ---------------------------------------------------------------------------
// completions
// ---------------------------------------------------------------------------

/// One HTTP client for every completion, built on first use. Reusing it is what
/// keeps the TLS handshake off the second keystroke.
static COMPLETION_TRANSPORT: OnceLock<ReqwestModelTransport> = OnceLock::new();

fn completion_transport() -> &'static ReqwestModelTransport {
    COMPLETION_TRANSPORT.get_or_init(ReqwestModelTransport::new)
}

/// Whether the agent has a credential — asked before anything is sent.
///
/// Never an error: "not configured" is the answer, not a failure, which is what
/// lets the webview fall back to no ghost text without a single red pixel.
#[tauri::command]
pub async fn agent_status() -> Result<Value, IpcError> {
    Ok(match AgentConfig::load() {
        Ok(config) => json!({
            "configured": true,
            "message": Value::Null,
            "model": config.model,
            "completionModel": completion_model(),
        }),
        Err(AgentError::MissingApiKey(message)) => json!({
            "configured": false,
            "message": message,
            "model": Value::Null,
            "completionModel": Value::Null,
        }),
        Err(other) => json!({
            "configured": false,
            "message": other.to_string(),
            "model": Value::Null,
            "completionModel": Value::Null,
        }),
    })
}

/// One completion, for the grey text under somebody's caret.
#[tauri::command]
pub async fn agent_complete(
    system: String,
    prompt: String,
    max_tokens: Option<u32>,
) -> Result<Value, IpcError> {
    if prompt.trim().is_empty() {
        return Ok(json!({ "text": "" }));
    }
    let config = AgentConfig::load()?;
    let request = CompletionRequest::new(system, prompt, max_tokens);
    let text = complete(completion_transport(), &config, &request).await?;
    Ok(json!({ "text": text }))
}

fn required(value: Option<String>, name: &str) -> Result<String, IpcError> {
    value
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| invalid(format!("{name} is required")))
}

fn invalid(message: impl Into<String>) -> IpcError {
    IpcError::Command(crate::commands::CommandError::Invalid {
        message: message.into(),
    })
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

impl From<AgentError> for IpcError {
    fn from(error: AgentError) -> Self {
        match error {
            // The same shape as missing Google credentials: a state the UI
            // renders as a sentence, not a crash.
            AgentError::MissingApiKey(message) => IpcError::NotConfigured(message),
            AgentError::Db(inner) => IpcError::Db(inner),
            AgentError::Command(inner) => IpcError::Command(inner),
            other => IpcError::Command(crate::commands::CommandError::Invalid {
                message: other.to_string(),
            }),
        }
    }
}
