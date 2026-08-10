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

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use engine::context::{AttachmentRef, EventSource, HandoffSource, MailMessage, MailSource};
use engine::plan::{self, LaunchPlan, Launched};
use engine::target::{self, HandoffTarget};
use engine::terminal::{self, Terminal};
use engine::{context, HandoffError};

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
