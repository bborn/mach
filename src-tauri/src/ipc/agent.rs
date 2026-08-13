//! Agent sessions: the invoke surface.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `agent_start` | `prompt`, `context?` | [`SessionSnapshot`] |
//! | `agent_sessions` | `sessionId?` | `SessionSnapshot[]` |
//! | `agent_send` | `sessionId`, `action?`, `message?`, `toolUseId?`, `reason?`, `itemId?` | `{ ok }` or a snapshot |
//! | `copy_context_text` | `context` | `{ chars, truncated }` — the same block, onto the clipboard |
//! | `agent_backend_status` | — | which brain would answer, and what else is available |
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
//! later call reuses it. The backend is *not* captured here — it is resolved per
//! session, so installing Claude Code (or setting `ANTHROPIC_API_KEY`, or
//! changing the preference) takes effect on the next ⌘K rather than the next
//! launch.
//!
//! # The workspace
//!
//! A backend that spawns a process needs a working directory, and the two
//! obvious candidates are both wrong: the user's home directory is where his
//! files are, and the current directory of a bundled app is `/`. So Mach gives
//! it one of its own, `agent/` beside the database, which contains nothing but
//! the short-lived tool-server configuration. A CLI told to run there and given
//! no file tools has nothing to read even if it wanted to.

#[path = "../agent/mod.rs"]
pub mod engine;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use engine::complete::{complete, completion_model, CompletionRequest};
use engine::config::AgentConfig;
use engine::context::{Audience, ContextItem};
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

pub(crate) fn engine(app: &AppHandle, state: &AppState) -> Result<Arc<AgentEngine>, IpcError> {
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

    let workspace = state
        .config
        .database_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("agent");

    let built = Arc::new(
        AgentEngine::new(
            state.db.clone(),
            Arc::clone(&state.dispatcher),
            outbox,
            Arc::clone(&state.plugins),
            Arc::new(engine::wire::ReqwestModelTransport::new()),
            Arc::new(TauriEmitter { app: app.clone() }),
        )
        .with_workspace(workspace),
    );

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

/// Which brain would answer a ⌘K right now, and what else this machine offers.
///
/// The preferences dialog renders this rather than guessing: "Claude Code
/// (detected)" is only worth saying if it is the same check `agent_start`
/// makes, and it is — both go through [`AgentEngine::resolve_backend`].
///
/// ```json
/// { "backend": "claudeCli", "label": "Claude Code",
///   "claudePath": "/Users/…/.local/bin/claude", "apiKey": false, "message": null }
/// ```
#[tauri::command]
pub async fn agent_backend_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, IpcError> {
    let engine = engine(&app, &state)?;
    let (resolved, available) = engine.resolve_backend();
    let (backend, label, message) = match resolved {
        Ok(backend) => (
            Some(backend.kind().to_string()),
            Some(backend.label()),
            None,
        ),
        Err(error) => (None, None, Some(error.to_string())),
    };
    Ok(json!({
        "backend": backend,
        "label": label,
        "claudePath": available.claude.map(|p| p.to_string_lossy().to_string()),
        "apiKey": available.api_key,
        "message": message,
    }))
}

/// The context block the agent would be given, put on the clipboard instead.
///
/// Not a second serialiser and deliberately so: this is
/// [`engine::context::render_for`] with [`Audience::Clipboard`] rather than
/// [`Audience::Model`], so what lands on the clipboard and what reaches the
/// model can only ever differ in how much of a conversation they carry — never
/// in what the app thinks "this" is.
///
/// A local read and a pasteboard write, and nothing else. It takes no
/// `AppHandle` and never builds the engine: there is no network, no credential
/// and no session involved in turning rows into text. Nothing logs the text —
/// see `crate::clipboard` for why the write is here rather than in the webview.
///
/// `chars` is `0` when the view had nothing in it, in which case the pasteboard
/// is left alone: a keystroke that empties the clipboard would be worse than a
/// keystroke that does nothing. `truncated` says a message was left behind, so
/// the toast can say so too.
///
/// ```json
/// { "chars": 4213, "truncated": false }
/// ```
#[tauri::command]
pub async fn copy_context_text(
    state: State<'_, AppState>,
    context: Vec<ContextItem>,
) -> Result<Value, IpcError> {
    let rendered = engine::context::render_for(&state.db, &context, Audience::Clipboard)?;
    if rendered.text.trim().is_empty() {
        return Ok(json!({ "chars": 0, "truncated": false }));
    }
    crate::clipboard::write(&rendered.text).map_err(IpcError::internal)?;
    Ok(json!({
        "chars": rendered.text.chars().count(),
        "truncated": rendered.truncated,
    }))
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
