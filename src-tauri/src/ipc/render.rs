//! Message-body rendering for the reading pane. Owned by the rendering unit.
//!
//! This module is a seam, not a policy: every decision about what is safe to
//! emit lives in [`crate::render`], which is separately tested against 52
//! attacks. What is decided *here* is only which part of the stored message to
//! feed it (HTML when there is one, plain text otherwise) and what metadata the
//! WebView needs in order to hold up its half of
//! `docs/message-rendering-invariants.md`.
//!
//! # The command is `async`
//!
//! Invariant 7 says rendering happens off the UI thread. Tauri runs a plain
//! `#[tauri::command]` on the main thread; `#[tauri::command(async)]` puts it on
//! the async runtime instead. An 8 MiB body is a bounded amount of CPU, but it
//! is not an amount the window should stop repainting for.
//!
//! # What the frontend still has to do
//!
//! Nothing returned from here is safe to inject into the app document. It is
//! safe to put inside a sandboxed, CSP-restricted iframe, which is what
//! `src/components/mail/MessageFrame.tsx` does. Rust cannot sandbox the
//! WebView and cannot stop it navigating; see the invariants doc.

use rusqlite::OptionalExtension;
use serde::Serialize;
use tauri::State;

use crate::db::models::Message;
use crate::db::{queries, Db};
use crate::render::{self, RenderOptions, RenderedBody};

use super::error::IpcError;
use super::state::AppState;

/// Which part of the stored message the HTML came from.
///
/// The UI shows this nowhere, but it is the difference between "this message
/// has no body" and "this message has a body we failed to render", and the
/// tests assert on it rather than on the HTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BodyFormat {
    /// `text/html` part, through the sanitizer.
    Html,
    /// `text/plain` part, through the sanitizer's text path.
    Text,
    /// Neither part is stored yet — Gmail's snippet, through the text path.
    /// Happens for a message the sync loop has listed but not fetched.
    Snippet,
    /// Nothing at all to render.
    Empty,
}

/// What `render_message_body` hands back.
///
/// [`RenderedBody`]'s own fields are flattened in rather than nested, so the
/// frontend reads one flat object. Every key is camelCase — `tests/render_ipc.rs`
/// asserts that on the serialized value, because a snake_case key here compiles
/// fine and silently blanks the reading pane.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedMessage {
    pub message_id: i64,
    pub format: BodyFormat,
    /// Echoed back so the UI can tell a "remote images allowed" render from a
    /// stale blocked one without tracking the request it made.
    pub remote_images_allowed: bool,
    #[serde(flatten)]
    pub body: RenderedBody,
}

#[tauri::command(async)]
pub fn render_message_body(
    state: State<'_, AppState>,
    message_id: i64,
    allow_remote_images: bool,
) -> Result<RenderedMessage, IpcError> {
    render_message(&state.db, message_id, allow_remote_images)
}

/// The whole handler, as a plain function over `&Db`.
///
/// Same split as [`super::reads`]: a `#[tauri::command]` cannot be called
/// without an application, so it holds no decisions and `tests/render_ipc.rs`
/// drives this instead.
pub fn render_message(
    db: &Db,
    message_id: i64,
    allow_remote_images: bool,
) -> Result<RenderedMessage, IpcError> {
    let message = load_message(db, message_id)?;
    Ok(render_stored_message(&message, allow_remote_images))
}

/// Render one already-loaded message.
///
/// HTML wins when there is one: it is what the sender wrote, and the text part
/// of a multipart/alternative is usually a degraded copy. When there is no HTML
/// part the plain text goes through [`render::render_text_with`] — the
/// sanitizer's own text path, which escapes and autolinks — rather than through
/// anything hand-rolled here.
pub fn render_stored_message(message: &Message, allow_remote_images: bool) -> RenderedMessage {
    let opts = RenderOptions {
        allow_remote_images,
    };

    let (format, body) = match (
        non_blank(message.body_html.as_deref()),
        non_blank(message.body_text.as_deref()),
    ) {
        (Some(html), _) => (BodyFormat::Html, render::render_html_with(html, opts)),
        (None, Some(text)) => (BodyFormat::Text, render::render_text_with(text, opts)),
        (None, None) => match non_blank(Some(&message.snippet)) {
            Some(snippet) => (BodyFormat::Snippet, render::render_text_with(snippet, opts)),
            None => (BodyFormat::Empty, RenderedBody::default()),
        },
    };

    RenderedMessage {
        message_id: message.id,
        format,
        remote_images_allowed: allow_remote_images,
        body,
    }
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.trim().is_empty())
}

/// One message by row id.
///
/// There is no `message_by_id` in [`crate::db::queries`] and this unit does not
/// own that module, so the thread is resolved first and the store's own row
/// mapping does the rest. Two indexed reads, no second copy of the participant
/// and attachment decoding.
fn load_message(db: &Db, message_id: i64) -> Result<Message, IpcError> {
    let found = db.read(|conn| {
        let thread_id: Option<i64> = conn
            .query_row(
                "SELECT thread_id FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(thread_id) = thread_id else {
            return Ok(None);
        };
        Ok(queries::messages_for_thread(conn, thread_id)?
            .into_iter()
            .find(|m| m.id == message_id))
    })?;

    found.ok_or_else(|| IpcError::not_found("message", message_id))
}

/// Opens a URL in the user's browser, from Rust.
///
/// The frontend used to call `@tauri-apps/plugin-opener` directly and swallow
/// any failure in a `console.warn`, which meant a link that did nothing gave
/// no signal at all — three separate theories were checked before this existed.
///
/// Doing it here has two advantages: the JS binding and its dynamic import
/// leave the path entirely, and a refusal lands in the app log where it can be
/// read. The scheme is re-validated on this side too; the frontend's check is
/// for UX, not security, since anything in the webview is reachable by a
/// sufficiently determined message body.
#[tauri::command]
pub async fn open_external(app: tauri::AppHandle, url: String) -> Result<(), IpcError> {
    let parsed = url::Url::parse(&url)
        .map_err(|e| IpcError::internal(format!("not a URL: {url} ({e})")))?;

    match parsed.scheme() {
        "http" | "https" | "mailto" | "tel" => {}
        other => {
            return Err(IpcError::internal(format!(
                "refusing to open a {other}: URL from a message"
            )))
        }
    }

    // Via the app handle, so no direct dependency on the plugin crate is needed.
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url.clone(), None::<&str>).map_err(|e| {
        eprintln!("open_external failed for {url}: {e}");
        IpcError::internal(format!("could not open {url}: {e}"))
    })
}
