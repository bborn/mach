//! The one command the reading pane calls when a body was evicted.
//!
//! Split the same way the rest of the IPC surface is: the `#[tauri::command]` is
//! a wrapper with no decisions in it, and [`restore_message_body`] is the plain
//! function `tests/evict.rs` drives without standing up an application.

use std::sync::Arc;

use tauri::State;

use crate::commands::GoogleClients;
use crate::db::Db;
use crate::ipc::error::IpcError;
use crate::ipc::render::{render_message, RenderedMessage};
use crate::ipc::state::AppState;

use super::refetch::{restore_html, RestoreError};

/// Fetch an evicted body and hand back the re-render.
///
/// `async`, and therefore off the UI thread: the reading pane already has the
/// text on screen and nothing in the window is waiting on this.
#[tauri::command(async)]
pub async fn restore_message_body(
    state: State<'_, AppState>,
    message_id: i64,
    allow_remote_images: bool,
) -> Result<RenderedMessage, IpcError> {
    let db = state.db.clone();
    let clients = Arc::clone(&state.dispatcher.clients);
    restore_message_body_with(&db, &clients, message_id, allow_remote_images).await
}

/// The whole handler, as a plain function.
///
/// A failed fetch is an `Err` and not a degraded `Ok`. The text is already on
/// screen — the frontend keeps what it has and shows the sentence beside it —
/// so returning a successful-looking render with no HTML in it would be the
/// silent failure this project has paid for more than once.
pub async fn restore_message_body_with(
    db: &Db,
    clients: &Arc<dyn GoogleClients>,
    message_id: i64,
    allow_remote_images: bool,
) -> Result<RenderedMessage, IpcError> {
    restore_html(db, clients, message_id)
        .await
        .map_err(to_ipc_error)?;
    render_message(db, message_id, allow_remote_images)
}

/// `RestoreError` in the shape the frontend already branches on.
///
/// The tags are carried through rather than flattened into "internal": "this
/// message is no longer in Gmail" and "could not reach Gmail" are different
/// things to a reader, and only one of them is worth trying again.
fn to_ipc_error(error: RestoreError) -> IpcError {
    match error {
        RestoreError::NotFound(id) => IpcError::not_found("message", id),
        RestoreError::Db(e) => IpcError::Db(e),
        other => IpcError::Restore {
            kind: other.kind(),
            message: other.to_string(),
        },
    }
}
