//! Drafting and sending. Owned by the composer unit.
//!
//! # One command, several operations
//!
//! `send_message` is the only compose entry point registered in `lib.rs`, and
//! `lib.rs` belongs to nobody while three units are being built at once. So
//! this handler is a small router: the payload carries an `op`, and the shapes
//! below are the contract. An absent `op` means `send`, which keeps the
//! original one-argument signature honest.
//!
//! | `op` | argument | returns |
//! |---|---|---|
//! | `prepare` | `threadId`, `kind` | `{ draft }` |
//! | `loadDraft` | `draftId` or `threadId` | `{ draft \| null }` |
//! | `saveDraft` | `draft` | `{ draft }` |
//! | `discardDraft` | `draftId` | `{ ok }` |
//! | `preview` | `draft` | `{ rfc822, headers }` |
//! | `send` | `draft`, `scheduleAt?` | `{ entry, undoUntil }` |
//! | `undo` | `outboxId` | `{ cancelled }` |
//! | `flush` | `now?` | `{ outcomes, pending }` |
//! | `outbox` | — | `{ pending }` |
//! | `retry` / `discard` | `outboxId` | `{ ok }` |
//!
//! Collapsing these into one command is a constraint, not a design. When the
//! composer's IPC surface is promoted they become ordinary sibling commands and
//! this router disappears; nothing above it changes, because `src/lib/compose.ts`
//! already exposes them as separate functions.
//!
//! # Where the module lives
//!
//! The implementation is `src-tauri/src/compose/`, declared below with
//! `#[path]` rather than in `lib.rs`, for the same reason
//! `command_queries::ensure_command_schema` exists: another unit owns that file
//! today. Moving `pub mod compose;` up to the crate root later is a one-line
//! change that makes `ipc::compose::engine` an alias.

#[path = "../compose/mod.rs"]
pub mod engine;

use std::sync::Arc;

use serde_json::{json, Value};

use engine::draft::{self, Draft, DraftKind};
use engine::mime::build_rfc822;
use engine::outbox::{Outbox, UNDO_WINDOW_MS};
use engine::ComposeError;

use super::error::IpcError;
use super::state::AppState;

// ---------------------------------------------------------------------------
// the handler
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn send_message(
    state: tauri::State<'_, AppState>,
    draft: Value,
) -> Result<Value, IpcError> {
    let outbox = Outbox::new(
        state.db.clone(),
        Arc::clone(&state.dispatcher.clients),
    )
    .map_err(IpcError::from)?;
    dispatch(&state.db, &outbox, draft).await.map_err(Into::into)
}

/// The router, as a plain function over `&Db` — a `#[tauri::command]` cannot be
/// called without an application, so it is not allowed to hold a decision.
/// `tests/compose.rs` drives this.
pub async fn dispatch(
    db: &crate::db::Db,
    outbox: &Outbox,
    payload: Value,
) -> Result<Value, ComposeError> {
    let op = payload
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or("send")
        .to_string();
    let now = payload
        .get("now")
        .and_then(Value::as_i64)
        .unwrap_or_else(now_ms);

    match op.as_str() {
        "prepare" => {
            let thread_id = required_i64(&payload, "threadId")?;
            let kind = payload
                .get("kind")
                .and_then(Value::as_str)
                .map(DraftKind::parse)
                .unwrap_or(DraftKind::Reply);
            let id = payload
                .get("draftId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| new_draft_id(now));
            let prepared = draft::prepare(db, thread_id, kind, id)?;
            Ok(json!({ "draft": prepared }))
        }

        "loadDraft" => {
            let found = match payload.get("draftId").and_then(Value::as_str) {
                Some(id) => draft::load_draft(db, id)?,
                None => match payload.get("threadId").and_then(Value::as_i64) {
                    Some(thread_id) => draft::load_draft_for_thread(db, thread_id)?,
                    None => {
                        return Err(ComposeError::invalid(
                            "loadDraft needs a draftId or a threadId",
                        ))
                    }
                },
            };
            Ok(json!({ "draft": found }))
        }

        "saveDraft" => {
            let parsed = parse_draft(&payload)?;
            let saved = draft::save_draft(db, &parsed, now)?;
            Ok(json!({ "draft": saved }))
        }

        "discardDraft" => {
            let id = required_str(&payload, "draftId")?;
            draft::delete_draft(db, &id)?;
            Ok(json!({ "ok": true }))
        }

        // The generated message, without sending it. Exists because "is the
        // threading right?" is a question about bytes, and the only honest way
        // to answer it is to look at them.
        "preview" => {
            let parsed = parse_draft(&payload)?;
            let built = draft::build(db, &parsed, now, entropy(now))?;
            let bytes = build_rfc822(&built.outgoing)?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let headers = text.split("\r\n\r\n").next().unwrap_or_default().to_string();
            Ok(json!({
                "rfc822": text,
                "headers": headers,
                "gmailThreadId": built.gmail_thread_id,
            }))
        }

        "send" => {
            let parsed = parse_draft(&payload)?;
            if parsed.to.is_empty() && parsed.cc.is_empty() && parsed.bcc.is_empty() {
                return Err(ComposeError::invalid("this message has no recipients"));
            }
            let built = draft::build(db, &parsed, now, entropy(now))?;
            // A scheduled send and an ordinary one differ only in this number.
            let send_after = payload
                .get("scheduleAt")
                .and_then(Value::as_i64)
                .filter(|at| *at > now)
                .unwrap_or(now + UNDO_WINDOW_MS);
            let entry = outbox.queue(&built, now, send_after)?;
            // The draft has become a message; keeping it would re-open an empty
            // composer on the thread that now contains the reply.
            draft::delete_draft(db, &parsed.id)?;
            Ok(json!({
                "entry": entry,
                "undoUntil": send_after,
                "scheduled": send_after > now + UNDO_WINDOW_MS,
            }))
        }

        "undo" => {
            let id = required_str(&payload, "outboxId")?;
            let cancelled = outbox.cancel(&id)?;
            Ok(json!({ "cancelled": cancelled }))
        }

        "flush" => {
            let outcomes = outbox.flush_due(now).await?;
            // A day is long enough for "sent" to have been seen and short
            // enough that the table does not become a mail archive.
            let _ = outbox.forget_sent(now - 86_400_000);
            Ok(json!({ "outcomes": outcomes, "pending": outbox.pending()? }))
        }

        "outbox" => Ok(json!({ "pending": outbox.pending()?, "all": outbox.list()? })),

        "retry" => {
            let id = required_str(&payload, "outboxId")?;
            Ok(json!({ "ok": outbox.retry(&id, now)? }))
        }

        "discard" => {
            let id = required_str(&payload, "outboxId")?;
            Ok(json!({ "ok": outbox.discard(&id)? }))
        }

        other => Err(ComposeError::invalid(format!(
            "unknown compose operation {other:?}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// payload helpers
// ---------------------------------------------------------------------------

fn parse_draft(payload: &Value) -> Result<Draft, ComposeError> {
    // Accept both `{ draft: {…} }` and a bare draft, so the original
    // `send_message(draft)` signature still means what it says.
    let raw = payload.get("draft").unwrap_or(payload);
    serde_json::from_value(raw.clone())
        .map_err(|e| ComposeError::invalid(format!("that is not a draft: {e}")))
}

fn required_i64(payload: &Value, key: &str) -> Result<i64, ComposeError> {
    payload
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| ComposeError::invalid(format!("{key} is required")))
}

fn required_str(payload: &Value, key: &str) -> Result<String, ComposeError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ComposeError::invalid(format!("{key} is required")))
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn new_draft_id(now: i64) -> String {
    format!("draft-{now:x}-{:x}", entropy(now))
}

/// Enough variation to keep two drafts started in the same millisecond apart.
/// Not a security boundary — the ids are local row keys.
fn entropy(now: i64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (now as u64).rotate_left(17) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

impl From<ComposeError> for IpcError {
    fn from(error: ComposeError) -> Self {
        match error {
            ComposeError::Db(inner) => IpcError::Db(inner),
            ComposeError::Command(inner) => IpcError::Command(inner),
            ComposeError::Mime(message) => IpcError::internal(format!(
                "the message could not be assembled: {message}"
            )),
            other => IpcError::Command(crate::commands::CommandError::Invalid {
                message: other.to_string(),
            }),
        }
    }
}
