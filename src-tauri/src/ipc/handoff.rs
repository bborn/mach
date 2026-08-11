//! Handoff commands: read the list, write the list, and throw one.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `handoff_targets` | — | [`HandoffTarget`]`[]` |
//! | `handoff_save_targets` | `targets` | the normalized list |
//! | `handoff_pick_directory` | — | `String?` — a folder, or `null` if cancelled |
//! | `handoff_terminals` | — | [`Terminals`] — what is installed, and any override |
//! | `handoff_preview` | `targetId?`, `note`, `source` | [`HandoffPreview`] |
//! | `handoff_run` | `targetId`, `note`, `source` | [`Launched`] |
//! | `handoff_session_open` | `targetId`, `note`, `source`, `cols`, `rows` | [`SessionStarted`] |
//! | `handoff_sessions` | — | [`SessionStarted`]`[]` — the tabs a reloaded window adopts |
//! | `handoff_session_write` | `sessionId`, `data` | — |
//! | `handoff_session_resize` | `sessionId`, `cols`, `rows` | — |
//! | `handoff_session_close` | `sessionId` | — |
//!
//! The last four are the pane. Output goes the other way, on the
//! [`HANDOFF_SESSION_EVENT`] Tauri event — push, never poll, like every other
//! stream in the app.
//!
//! # A session that can use Mach
//!
//! A target whose program is `claude` is started against Mach's own tool server
//! — the one [`super::agent::engine::mcp`] already serves for the in-app agent — so the
//! session in the pane can search, read, label, archive, snooze, draft and send.
//! [`engine::tools`] decides which targets those are and what the CLI is told;
//! [`attach_tools`] here is the wiring, and the two properties it owes are:
//!
//! * **the token dies with the pane.** The [`McpServer`] is a
//!   [`SessionResource`] held by the session, and `handoff::session::reap` drops
//!   it after the process is dead. Dropping it closes the listener and deletes
//!   the `0600` file the token lives in. There is no other handle to it.
//! * **the approval is answerable.** A `send_draft` from the pane parks on
//!   [`ApprovalDesk`](super::agent::engine::ApprovalDesk), which needs somebody to click.
//!   So the session is also registered with [`AgentEngine::attach`], which gives
//!   it a pill and a drawer — the same Approve and Deny the ⌘K agent uses, for
//!   the same reason and through the same code.
//!
//! The engine is `src-tauri/src/handoff/`, declared below with `#[path]` rather
//! than in `lib.rs` — the same arrangement [`super::compose`] and
//! [`super::attachments`] use, and for the same reason: `lib.rs` belongs to
//! another unit while this is being built. Promoting `pub mod handoff;` to the
//! crate root later makes `ipc::handoff::engine` an alias and changes nothing
//! else.
//!
//! # What this layer is for
//!
//! Turning row ids into text. The frontend says "the thread that is open"; this
//! reads the thread and its messages out of SQLite and hands the engine plain
//! strings. Everything that decides anything — what the prompt says, what argv
//! becomes, how a terminal is opened — is over there, in functions
//! `tests/handoff.rs` can call without an application.

#[path = "../handoff/mod.rs"]
pub mod engine;

use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use engine::context::{AttachmentRef, EventSource, HandoffSource, MailMessage, MailSource};
use engine::plan::{self, LaunchPlan, Launched};
use engine::session::{SessionResource, SessionSink, Sessions};
use engine::target::{self, HandoffTarget};
use engine::terminal::{self, Terminal};
use engine::{context, HandoffError};

use crate::ipc::agent::engine::mcp::McpServer;
use crate::db::{command_queries, queries, Db};

use super::error::IpcError;
use super::state::AppState;

impl From<HandoffError> for IpcError {
    fn from(error: HandoffError) -> Self {
        IpcError::internal(error.to_string())
    }
}

// ===========================================================================
// Payloads
// ===========================================================================

/// What the window was showing. The frontend knows the ids; only this side can
/// turn them into text.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    /// `mail`, `event`, or anything else for "nothing was open".
    pub kind: Option<String>,
    pub thread_id: Option<i64>,
    pub event_id: Option<i64>,
}

/// What the confirmation sheet renders before anything runs.
///
/// It is the *actual* plan — the same argv that will be executed, the same
/// prompt — rather than a description of one, because a preview that is
/// assembled differently from the thing it previews is worse than no preview.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffPreview {
    pub target_id: String,
    pub target_name: String,
    pub mode: String,
    pub dir: String,
    /// argv, joined for reading. Nothing runs this string.
    pub command: String,
    pub argv: Vec<String>,
    /// The whole prompt, exactly as the receiving tool will see it.
    pub prompt: String,
    /// One line naming what the context is — "Katie Ross — Feature request".
    pub context_label: String,
    pub context_file: String,
    /// Whether this target has ever launched anything. Drives the confirmation.
    pub unproven: bool,
}

/// The terminals to choose between, and whether the choice is being made
/// somewhere else.
///
/// `forced` is [`terminal::TERMINAL_APP_ENV`], and it is on the wire so that the
/// editor can say the environment is deciding rather than render a menu whose
/// selection would have no effect.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminals {
    pub installed: Vec<Terminal>,
    pub forced: Option<String>,
}

/// What the pane is handed when a session starts.
///
/// `prompt` is on the wire so the pane can keep showing what was handed over
/// for as long as the session is up. The confirmation sheet has already shown
/// it once — see `HandoffDialog` — and this is the copy that stays on screen,
/// because "what did I actually send that thing" is a question with an answer
/// and the answer should not be a temp file path.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStarted {
    pub session_id: String,
    pub target_name: String,
    pub command: String,
    pub dir: String,
    pub prompt: String,
    pub context_file: String,
    /// What this session was given of Mach itself, by label — "Mach's tools",
    /// or nothing. On the wire so the tab can say so: a session that can send
    /// mail and a session that cannot are not the same thing to have open.
    pub resources: Vec<String>,
}

impl From<engine::session::Started> for SessionStarted {
    fn from(started: engine::session::Started) -> SessionStarted {
        SessionStarted {
            session_id: started.session_id,
            target_name: started.target_name,
            command: started.command,
            dir: started.dir,
            prompt: started.prompt,
            context_file: started.context_file,
            resources: started.resources,
        }
    }
}

// ===========================================================================
// The session pane's end of the pipe
// ===========================================================================

/// The one channel every session pane speaks on.
pub const HANDOFF_SESSION_EVENT: &str = "handoff-session";

/// One thing that happened to the running session.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SessionEvent {
    /// Terminal output. Base64 because it is *bytes* — a pty carries escape
    /// sequences, and a chunk boundary lands in the middle of a UTF-8 sequence
    /// often enough that decoding here would corrupt the stream. The pane
    /// decodes to a `Uint8Array` and hands that to the emulator, which is the
    /// only thing in the system that knows where a character ends.
    #[serde(rename_all = "camelCase")]
    Output {
        session_id: String,
        base64: String,
        /// Bytes dropped in front of this chunk because the process outran the
        /// pane. Zero in every ordinary session.
        dropped: u64,
    },
    #[serde(rename_all = "camelCase")]
    Exited {
        session_id: String,
        status: Option<i32>,
    },
}

/// Sends what the pty produced to the webview.
struct TauriSessionSink {
    app: AppHandle,
}

impl SessionSink for TauriSessionSink {
    fn output(&self, session_id: &str, bytes: Vec<u8>, dropped: u64) {
        // A failed emit means the window is gone, which is not something a
        // running session should die for — `close_all` on exit is what ends it.
        let _ = self.app.emit(
            HANDOFF_SESSION_EVENT,
            SessionEvent::Output {
                session_id: session_id.to_string(),
                base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                dropped,
            },
        );
    }

    fn exited(&self, session_id: &str, status: Option<i32>) {
        let _ = self.app.emit(
            HANDOFF_SESSION_EVENT,
            SessionEvent::Exited {
                session_id: session_id.to_string(),
                status,
            },
        );
    }
}

/// The process-wide registry.
///
/// A static rather than managed state because `lib.rs` has to reach it from the
/// `RunEvent::Exit` callback, where there is an `AppHandle` but no `State`, and
/// because there is exactly one of it for the life of the process either way.
/// `Sessions` itself knows nothing about Tauri, which is what lets
/// `tests/handoff_session.rs` drive a real pty without an application.
pub fn sessions() -> &'static Arc<Sessions> {
    static SESSIONS: OnceLock<Arc<Sessions>> = OnceLock::new();
    SESSIONS.get_or_init(|| Arc::new(Sessions::new()))
}

// ===========================================================================
// Commands
// ===========================================================================

/// The stored targets, in the order he arranged them.
#[tauri::command]
pub fn handoff_targets(state: State<'_, AppState>) -> Result<Vec<HandoffTarget>, IpcError> {
    Ok(state.db.read(target::load)?)
}

/// Replace the whole list.
///
/// Whole-list rather than per-target: the editor holds all of them on screen at
/// once, reordering is a legitimate edit, and a delete has no id to address
/// once it has happened. The list comes back normalized so the dialog is looking
/// at exactly what was stored.
#[tauri::command]
pub fn handoff_save_targets(
    state: State<'_, AppState>,
    targets: Vec<HandoffTarget>,
) -> Result<Vec<HandoffTarget>, IpcError> {
    let normalized = target::normalize(targets)?;
    let now = now_ms();
    state
        .db
        .write(|conn| target::save(conn, &normalized, now))?;
    Ok(normalized)
}

/// The system folder picker, for naming the first target.
///
/// The seeded target is named after a directory he chooses, which means the
/// zero-configuration path needs exactly one question answered and asks it with
/// the panel he already knows rather than a text field he has to type a path
/// into.
#[tauri::command(async)]
pub async fn handoff_pick_directory(app: tauri::AppHandle) -> Result<Option<String>, IpcError> {
    // The panel blocks its thread until answered, which can be minutes; a Tokio
    // worker is not the place for that. Same reasoning as the save panel in
    // `ipc::attachments`.
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog().file().blocking_pick_folder()
    })
    .await
    .map_err(|e| IpcError::internal(format!("the folder panel did not answer: {e}")))?;

    let Some(chosen) = chosen else {
        return Ok(None);
    };
    chosen
        .into_path()
        .map(|path| Some(path.to_string_lossy().into_owned()))
        .map_err(|e| IpcError::internal(format!("that is not a directory Mach can use: {e}")))
}

/// The terminals this Mac has, for the menu in the editor.
///
/// Asked every time the editor opens rather than cached: applications are
/// installed and thrown away while the app is running, and this is four
/// `stat`s in a directory the filesystem has in cache.
#[tauri::command]
pub fn handoff_terminals() -> Result<Terminals, IpcError> {
    Ok(Terminals {
        installed: terminal::installed(),
        forced: terminal::forced(),
    })
}

/// What would be sent, without sending it.
#[tauri::command]
pub fn handoff_preview(
    state: State<'_, AppState>,
    target_id: String,
    note: String,
    source: Option<SourceRef>,
) -> Result<HandoffPreview, IpcError> {
    let (target, plan) = prepare(&state.db, &target_id, &note, source.as_ref())?;
    Ok(HandoffPreview {
        target_id: target.id.clone(),
        target_name: target.name.clone(),
        mode: target.mode.as_str().to_string(),
        dir: plan.dir.to_string_lossy().into_owned(),
        command: plan.display_command(),
        argv: plan.argv.clone(),
        prompt: plan.prompt.clone(),
        context_label: plan.context_label.clone(),
        context_file: plan.context_file.to_string_lossy().into_owned(),
        unproven: target.is_unproven(),
    })
}

/// Launch it, record that this target has now run, and stop caring.
///
/// The only thing written back is `lastRunAt`, and only so the confirmation is
/// asked once per target rather than every time. Mach does not follow what it
/// threw: there is no session, no polling, nothing to come back to.
#[tauri::command(async)]
pub async fn handoff_run(
    state: State<'_, AppState>,
    target_id: String,
    note: String,
    source: Option<SourceRef>,
) -> Result<Launched, IpcError> {
    let (target, plan) = prepare(&state.db, &target_id, &note, source.as_ref())?;

    let launched = match target.mode {
        // A session is not a throw, so it does not come through here: it has a
        // pane that outlives the call, and `handoff_session_open` is the door.
        target::HandoffMode::Session => {
            return Err(IpcError::internal(
                "that target opens a session — use the session pane".to_string(),
            ))
        }
        target::HandoffMode::Inline => plan::run_inline(&plan).await?,
        target::HandoffMode::Terminal => {
            let app = terminal_app(&state.db)?;
            // `open` waits for LaunchServices, which for a cold Terminal is a
            // second or two of doing nothing on a Tokio worker.
            tauri::async_runtime::spawn_blocking(move || {
                plan::open_in_terminal(&plan, app.as_deref())
            })
            .await
            .map_err(|e| IpcError::internal(format!("the launcher did not answer: {e}")))??
        }
    };

    let now = now_ms();
    let id = target.id.clone();
    state.db.write(move |conn| {
        let mut targets = target::load(conn)?;
        if let Some(stored) = targets.iter_mut().find(|t| t.id == id) {
            stored.last_run_at = Some(now);
        }
        target::save(conn, &targets, now)
    })?;

    Ok(launched)
}

// ===========================================================================
// The session pane
// ===========================================================================

/// Start the target's command on a pty and hand the pane its id.
///
/// The plan is built by the same [`prepare`] every other mode goes through, so
/// the argv, the directory, the environment and the prompt are resolved once —
/// and the confirmation sheet the frontend showed before calling this was built
/// from `handoff_preview`, which is the same function again.
#[tauri::command(async)]
pub async fn handoff_session_open(
    app: AppHandle,
    state: State<'_, AppState>,
    target_id: String,
    note: String,
    source: Option<SourceRef>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<SessionStarted, IpcError> {
    let (target, mut plan) = prepare(&state.db, &target_id, &note, source.as_ref())?;
    if target.mode != target::HandoffMode::Session {
        return Err(IpcError::internal(format!(
            "{} is not a session target",
            target.name
        )));
    }

    // Before the pty, because it rewrites argv. A failure here is fatal rather
    // than a session that quietly came up without the tools it was promised —
    // silent degradation is the failure mode this project keeps paying for.
    let resources = attach_tools(&app, &state, &target, &mut plan)?;

    let started = sessions().open(
        &plan,
        cols.unwrap_or(engine::session::DEFAULT_COLS),
        rows.unwrap_or(engine::session::DEFAULT_ROWS),
        Arc::new(TauriSessionSink { app }),
        resources,
    )?;

    let now = now_ms();
    let id = target.id.clone();
    state.db.write(move |conn| {
        let mut targets = target::load(conn)?;
        if let Some(stored) = targets.iter_mut().find(|t| t.id == id) {
            stored.last_run_at = Some(now);
        }
        target::save(conn, &targets, now)
    })?;

    Ok(started.into())
}

/// Every session that is running, for a webview that has just loaded.
///
/// A reload — hot module replacement, a renderer that crashed — leaves the
/// processes running with nothing on screen pointing at them. This is how the
/// pane finds them again, and it is where the tab strip comes back from. What
/// was printed before the reload is gone: the scrollback lived in the emulators
/// that went away with the page, and keeping a copy of it on this side would
/// mean holding every session's entire output in memory for a case that is
/// already rare.
#[tauri::command]
pub fn handoff_sessions() -> Vec<SessionStarted> {
    sessions().list().into_iter().map(SessionStarted::from).collect()
}

// ===========================================================================
// Mach, as something the session can use
// ===========================================================================

/// Give the session Mach's tools, when the session is one Mach gives them to.
///
/// Returns an empty vector for every other target, having started nothing and
/// written nothing. See [`engine::tools`] for why that is one program rather
/// than a preference.
fn attach_tools(
    app: &AppHandle,
    state: &AppState,
    target: &HandoffTarget,
    plan: &mut LaunchPlan,
) -> Result<Vec<Box<dyn SessionResource>>, IpcError> {
    if !engine::tools::wants_tools(&plan.argv[0]) {
        return Ok(Vec::new());
    }

    let dir = tool_config_dir(state);
    sweep_stale_configs(&dir);

    let agent = super::agent::engine(app, state)?;
    let attached = agent.attach(
        format!("{} (session pane)", target.name),
        "Claude Code (session pane)".to_string(),
    );

    let server = McpServer::start(
        Arc::clone(&attached.gate),
        tokio::runtime::Handle::current(),
        &dir,
        &attached.snapshot.id,
    )
    .map_err(|error| {
        // The session was registered a moment ago and nothing is going to drive
        // it, so it must not be left in the dock as a pill that never ends.
        let _ = agent.close(&attached.snapshot.id);
        IpcError::internal(error.to_string())
    })?;

    let attachment = engine::tools::Attachment::new(server, agent, attached.snapshot.id.clone());
    engine::tools::wire(
        &mut plan.argv,
        attachment.config_path(),
        &engine::tools::guidance(&target.name),
    );

    Ok(vec![Box::new(attachment)])
}

/// Where a session's `--mcp-config` is written: beside the database, not in
/// `/tmp`, so it inherits the data directory's ownership and a QA instance
/// writes its own.
fn tool_config_dir(state: &AppState) -> std::path::PathBuf {
    state
        .config
        .database_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("handoff")
}

/// Take last run's token files off the disk, once per launch.
///
/// A crash is guarantee 3: the process dies, the listener dies with it, and the
/// file it wrote is left behind holding a token for a port that no longer
/// exists. Harmless, and still a secret on a disk that gets backed up. Nothing
/// in this directory can belong to a live session at the moment the first one of
/// this run starts, because the listeners live in this process.
fn sweep_stale_configs(dir: &std::path::Path) {
    static SWEPT: OnceLock<()> = OnceLock::new();
    SWEPT.get_or_init(|| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("mcp-") && name.ends_with(".json") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
}

/// Keystrokes, exactly as the emulator encoded them.
#[tauri::command]
pub fn handoff_session_write(session_id: String, data: String) -> Result<(), IpcError> {
    Ok(sessions().write(&session_id, data.as_bytes())?)
}

/// The pane's new size in cells.
#[tauri::command]
pub fn handoff_session_resize(session_id: String, cols: u16, rows: u16) -> Result<(), IpcError> {
    Ok(sessions().resize(&session_id, cols, rows)?)
}

/// End it. Never fails: a pane closing a session that has already exited is the
/// ordinary case, not an error to report.
#[tauri::command]
pub fn handoff_session_close(session_id: String) {
    sessions().close(&session_id);
}

// ===========================================================================
// Rows to text
// ===========================================================================

/// Look the target up, read whatever is on screen, and build the plan.
///
/// Preview and run share this so that the sheet cannot describe one thing and
/// the launch do another. Public because it is where every decision this layer
/// makes actually happens, and `tests/handoff.rs` drives it over a real
/// database — the same split `ipc/mod.rs` describes, where a
/// `#[tauri::command]` holds no decision because it cannot be called without an
/// application.
pub fn prepare(
    db: &Db,
    target_id: &str,
    note: &str,
    source: Option<&SourceRef>,
) -> Result<(HandoffTarget, LaunchPlan), IpcError> {
    let targets = db.read(target::load)?;
    let target = targets
        .into_iter()
        .find(|t| t.id == target_id)
        .ok_or_else(|| {
            IpcError::internal(format!("there is no handoff target with id {target_id:?}"))
        })?;

    let tag = engine::new_tag();
    let source = read_source(db, source)?;
    let context = context::build(&source, &tag);
    let plan = LaunchPlan::prepare(&target, note, &context, &tag)?;
    Ok((target, plan))
}

/// His terminal, as `open -a` will be given it, or `None` for the system's.
///
/// Public for the same reason [`prepare`] is: it is a decision, and
/// `tests/handoff.rs` drives it over a real database rather than over an
/// application it cannot start.
pub fn terminal_app(db: &Db) -> Result<Option<String>, IpcError> {
    let stored = db.read(|conn| super::prefs::get(conn, terminal::TERMINAL_APP_KEY))?;
    // A value of the wrong type is treated as absent, which is the rule the
    // whole preferences layer follows: one bad row costs one setting.
    let stored = stored.as_ref().and_then(|value| value.as_str());
    Ok(terminal::chosen(stored))
}

pub fn read_source(db: &Db, source: Option<&SourceRef>) -> Result<HandoffSource, IpcError> {
    let Some(source) = source else {
        return Ok(HandoffSource::None);
    };
    match source.kind.as_deref() {
        Some("mail") => match source.thread_id {
            Some(id) => read_thread(db, id),
            None => Ok(HandoffSource::None),
        },
        Some("event") => match source.event_id {
            Some(id) => read_event(db, id),
            None => Ok(HandoffSource::None),
        },
        _ => Ok(HandoffSource::None),
    }
}

pub fn read_thread(db: &Db, thread_id: i64) -> Result<HandoffSource, IpcError> {
    let Some(detail) = db.read(|conn| queries::thread_with_messages(conn, thread_id))? else {
        // A thread that has been archived out from under the palette is not an
        // error worth refusing a handoff over — the sentence still means
        // something on its own.
        return Ok(HandoffSource::None);
    };

    let messages = detail
        .messages
        .into_iter()
        .map(|message| MailMessage {
            from: address(&message.from),
            to: message
                .to
                .iter()
                .map(address)
                .collect::<Vec<_>>()
                .join(", "),
            date_ms: message.internal_date,
            body_text: message.body_text,
            body_html: message.body_html,
            snippet: message.snippet,
            attachments: message
                .attachments
                .into_iter()
                .map(|a| AttachmentRef {
                    filename: a.filename,
                    mime_type: a.mime_type,
                    size_bytes: a.size_bytes,
                    // Straight out of the row. A handoff never fetches bytes:
                    // "hand this to a coding agent" is not consent to download
                    // six megabytes of PDFs as a side effect.
                    local_path: a.local_path,
                })
                .collect(),
        })
        .collect();

    Ok(HandoffSource::Mail(Box::new(MailSource {
        subject: detail.thread.subject,
        account_email: detail.thread.account_email,
        gmail_thread_id: detail.thread.gmail_thread_id,
        messages,
    })))
}

pub fn read_event(db: &Db, event_id: i64) -> Result<HandoffSource, IpcError> {
    let Some(event) = db.read(|conn| command_queries::event_by_id(conn, event_id))? else {
        return Ok(HandoffSource::None);
    };

    Ok(HandoffSource::Event(Box::new(EventSource {
        title: event.title,
        start_ms: event.start_ts,
        end_ms: event.end_ts,
        all_day: event.is_all_day,
        location: event.location,
        organizer: event.organizer.as_ref().map(address),
        attendees: event.attendees.iter().map(address).collect(),
        description: event.description,
        html_link: event.html_link,
    })))
}

/// `Katie Ross <katie@example.com>`, or just the address when there is no name.
fn address(participant: &crate::db::models::Participant) -> String {
    match participant.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && name != participant.email => {
            format!("{name} <{}>", participant.email)
        }
        _ => participant.email.clone(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
