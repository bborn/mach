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
//! WebView; see the invariants doc.
//!
//! Stopping it *navigating* is this module's job after all — see
//! [`link_guard`], which is the only layer that can, because the frame the
//! WebView is asked to navigate cannot run the script that would say no.

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
    /// This is the text of a message whose HTML was evicted, and a request will
    /// upgrade it. `format` alone cannot say that: a message that never had an
    /// HTML part renders `Text` too, and asking Gmail for it would be a round
    /// trip per open forever. See [`crate::evict`].
    pub html_evicted: bool,
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
        // Only ever what the sender declared, and only ever from the stored
        // column. A message synced before migration 11 reads `false` here and
        // keeps every line break it arrived with.
        text_flowed: message.body_text_flowed,
        text_delsp: message.body_text_delsp,
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
        html_evicted: message.html_evicted,
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
    open_in_system_browser(&app, &url).map_err(IpcError::internal)
}

/// The one place a URL leaves the app, for both callers.
///
/// `Err` is a sentence fit to put on screen, because both callers put it there.
fn open_in_system_browser<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    url: &str,
) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("not a URL: {url} ({e})"))?;

    if !OPENABLE_SCHEMES.contains(&parsed.scheme()) {
        return Err(format!(
            "refusing to open a {}: URL from a message",
            parsed.scheme()
        ));
    }

    // Via the app handle, so no direct dependency on the plugin crate is needed.
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|e| {
        eprintln!("open_external failed for {url}: {e}");
        format!("could not open {url}: {e}")
    })
}

/// The four schemes a message body is allowed to carry, and the only four this
/// app will hand to the system. Mirrors `EXTERNAL_SCHEMES` in
/// `src/lib/message-body.ts`.
const OPENABLE_SCHEMES: [&str; 4] = ["http", "https", "mailto", "tel"];

/// Hosts that are the app itself rather than somewhere on the internet.
///
/// `localhost` is the Vite dev server and the OAuth loopback listener; the
/// `*.localhost` names are Tauri's own custom-protocol origins on the platforms
/// that serve them over http.
fn is_app_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
        || host.ends_with(".localhost")
}

/// Is this a navigation the app owns, or one that belongs in a browser?
///
/// Everything the app itself navigates to is either one of its own schemes
/// (`tauri:`, `asset:`, `plugin:`, `about:` for the message frames) or one of
/// its own hosts. Nothing else can appear here: the main frame only ever loads
/// Mach, sign-in goes out through [`open_external`] rather than through the
/// WebView, and the sanitizer restricts a message's `href` to four schemes. So
/// an http(s), `mailto:` or `tel:` URL pointing anywhere else is a link in a
/// message, and a link in a message is never followed in here.
pub fn is_external_link(url: &url::Url) -> bool {
    if !OPENABLE_SCHEMES.contains(&url.scheme()) {
        return false;
    }
    match url.host_str() {
        Some(host) => !is_app_host(host),
        // `mailto:` and `tel:` have no host and are always somebody else's.
        None => true,
    }
}

/// The event a failed open lands on. `src/lib/message-body.ts` listens for it.
pub const LINK_FAILED_EVENT: &str = "link-failed";

#[derive(Debug, Clone, Serialize)]
struct LinkFailure {
    message: String,
}

/// Links in message bodies, opened where they belong.
///
/// # Why this is not in the WebView
///
/// It was, and it never once ran. `MessageFrame` attaches a capture-phase click
/// listener to the message frame's document, which is the documented way to do
/// this and is what every invariant assumed. WebKit will not invoke a listener
/// whose target document has scripting disabled, and a frame sandboxed without
/// `allow-scripts` — which is the first invariant of this whole unit — has
/// scripting disabled. So the listener attached, the click did nothing, and
/// there was no error in any log: a dead click, twice reported, with three
/// theories checked against the wrong layer each time.
///
/// `on_navigation` is below the engine. It is the same hook WebKit uses to ask
/// whether a navigation may proceed, it is consulted for subframes and for
/// new-window navigations alike, and no sandbox flag can silence it. Cancelling
/// here also happens *before* the engine asks anything to provide a window, so
/// a message's page is never rendered inside Mach even for an instant.
///
/// Registered as a plugin rather than on a window builder because the window
/// comes from `tauri.conf.json`; a plugin's hook reaches it either way.
///
/// # It reaches one window too many
///
/// A plugin hook is consulted for *every* webview, and `crate::browser` opens
/// one whose whole job is to be somewhere else on the internet. Without the
/// skip below, the first `https` navigation in that window would be classified
/// as "a link in a message", cancelled, and pushed out to the system browser —
/// so the in-app page would open a system browser tab and show nothing, which
/// is precisely the bug this hook exists to prevent, inverted.
///
/// The skip is by label and it is not a hole: that window has its own
/// `on_navigation` from its own builder, and both hooks are consulted, so
/// nothing it navigates to is unchecked. See `browser::may_navigate`, which is
/// a narrower allowlist than this one.
pub fn link_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("mach-links")
        .on_navigation(|webview, url| {
            if webview.window().label() == crate::browser::WINDOW_LABEL {
                return true;
            }
            if !is_external_link(url) {
                return true;
            }
            use tauri::Manager;
            hand_to_browser(webview.app_handle(), url.as_str());
            false
        })
        .build()
}

/// Open a message's link, and say so on screen if it cannot be opened.
///
/// Invariant: a click that cannot open a link says so. Rust is the only layer
/// that knows — the frame that was clicked cannot run a line of script — so it
/// is the only one that can report it.
fn hand_to_browser<R: tauri::Runtime>(app: &tauri::AppHandle<R>, url: &str) {
    if let Err(message) = open_in_system_browser(app, url) {
        use tauri::Emitter;
        let _ = app.emit(
            LINK_FAILED_EVENT,
            LinkFailure {
                message: format!("Could not open that link: {message}"),
            },
        );
    }
}

/// The other half of [`link_guard`], for WebKitGTK.
///
/// # Why there is a second half at all
///
/// Every anchor the sanitizer emits carries `target="_blank"`, because that is
/// what [`crate::render`] needs in order for the click to reach a policy hook
/// at all — see `FRAME_SANDBOX` in `src/lib/message-body.ts` for the three ways
/// a link in a sandboxed frame can die inside the engine.
///
/// A `_blank` navigation is not the same event as an ordinary one, and the two
/// engines do not agree on that. WKWebView asks
/// `decidePolicyForNavigationAction` about both, which is why one hook covered
/// everything on macOS. WebKitGTK splits them: an ordinary navigation is
/// `WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION` and a `_blank` one is
/// `WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION`, two values of the same
/// `decide-policy` signal — and wry's navigation handler answers only the
/// first, returning "not mine" for every other decision type
/// (`wry::webkitgtk`, the `connect_decide_policy` block). So
/// `Builder::on_navigation` is never consulted, `link_guard` never runs, and
/// WebKit's default for an unanswered decision is to allow it and then ask
/// something to provide a window. Nothing in this app answers that either.
///
/// The result is the dead click this unit already has a long comment about,
/// arrived at by a fourth route: the listener attaches, the guard is
/// registered, every log is empty, and clicking a link in a message does
/// nothing at all. It is not `xdg-open`, and it is not the browser opening the
/// page somewhere the reader cannot see it — the URL never leaves the process.
///
/// # Why it is a signal handler and not a builder option
///
/// Tauri does expose the new-window case as `WebviewWindowBuilder::on_new_window`,
/// and it is no use here for the reason `link_guard` is a plugin: the window is
/// declared in `tauri.conf.json`, so there is no builder to hang it on, and a
/// plugin has `on_navigation` but no `on_new_window`. `with_webview` reaches
/// the same `decide-policy` signal wry is already on. Ours is connected second
/// and only ever answers the decision wry declined, so the two do not race.
///
/// Cancelling here keeps the property the macOS path has: `ignore()` happens
/// *before* the engine asks anything to provide a window, so a message's page
/// is never rendered inside Mach even for an instant.
#[cfg(target_os = "linux")]
pub fn route_new_windows(app: &tauri::AppHandle) {
    use tauri::Manager;

    // The main window only. `crate::browser`'s window is somewhere on the
    // internet on purpose and has its own navigation guard; message frames,
    // which are what this exists for, only ever live here.
    let Some(window) = app.get_webview_window(crate::shell::MAIN_WINDOW) else {
        eprintln!("links: no main window to route new-window navigations from");
        return;
    };

    let app = app.clone();
    let attached = window.with_webview(move |platform| {
        use webkit2gtk::glib::prelude::Cast;
        use webkit2gtk::{
            NavigationPolicyDecision, NavigationPolicyDecisionExt, PolicyDecisionExt,
            PolicyDecisionType, URIRequestExt, WebViewExt,
        };

        platform
            .inner()
            .connect_decide_policy(move |_webview, decision, kind| {
                if kind != PolicyDecisionType::NewWindowAction {
                    return false;
                }
                let Some(uri) = decision
                    .dynamic_cast_ref::<NavigationPolicyDecision>()
                    .and_then(|d| d.navigation_action())
                    .and_then(|action| action.request())
                    .and_then(|request| request.uri())
                else {
                    return false;
                };
                // The same question `link_guard` asks, of the same function: a
                // new window for one of the app's own origins is not a link in
                // a message, and is left to whatever asked for it.
                let Ok(url) = url::Url::parse(uri.as_str()) else {
                    return false;
                };
                if !is_external_link(&url) {
                    return false;
                }
                decision.ignore();
                hand_to_browser(&app, url.as_str());
                true
            });
    });

    if let Err(e) = attached {
        // Said out loud rather than swallowed: without this handler every link
        // in every message is a dead click, and a dead click is silent by
        // definition.
        eprintln!("links: could not watch for new-window navigations: {e}");
    }
}

/// Nothing to do: WKWebView asks [`link_guard`] about a `_blank` navigation
/// like any other, so there is no second case to answer.
#[cfg(not(target_os = "linux"))]
pub fn route_new_windows(_app: &tauri::AppHandle) {}
