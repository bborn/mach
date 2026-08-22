//! The one window in Mach that runs a stranger's JavaScript.
//!
//! Everything else in this application is built so that sender-controlled
//! markup can never execute: a message body goes into a frame sandboxed
//! *without* `allow-scripts` under a `script-src 'none'` policy, which is what
//! makes the whole `<animate>`/`var()`/post-sanitize-rewrite family of
//! mail-client CVEs structurally unreachable here. See
//! `docs/security-threat-model.md`.
//!
//! This module is the exception, and it exists for one case. A
//! `List-Unsubscribe` that offers only an `https` link — no RFC 8058 one-click,
//! no `mailto:` — is a page with a form on it, and the form has to be read and
//! submitted by a person. Until now that went to the system browser. It still
//! can; this is the other choice, a window inside Mach with the page in it.
//!
//! **Nothing here automates the page.** It renders, he reads it, he clicks. No
//! model call, no synthetic submit, no reading the DOM back.
//!
//! # Why not an iframe
//!
//! Because most of these pages refuse to be framed. `X-Frame-Options: DENY` and
//! `frame-ancestors 'none'` are near-universal on anything with a form on it,
//! and a frame would fail for exactly the senders this is for. A separate
//! webview is also the more honest object: it is a second browser, and it looks
//! like one.
//!
//! # What isolates it
//!
//! | boundary | mechanism | where it is asserted |
//! |---|---|---|
//! | cannot call a Tauri command | no capability names [`WINDOW_LABEL`], and the app ACL is consulted | `tests/capabilities.rs`, `tests/browser.rs` |
//! | cannot become the app | [`may_navigate`] is an allowlist of one scheme | `tests/browser.rs` |
//! | cannot read the app's own assets | [`refuse_app_asset`] blanks every `tauri://` response here | below |
//! | shares no cookies or storage | `incognito(true)` → a fresh non-persistent `WKWebsiteDataStore` | below |
//! | cannot open a second window | [`on_new_window`](open) refuses, or navigates this one | below |
//! | cannot write a file | the download handler returns `false` | below |
//!
//! The capability line is the important one and it is worth being exact about
//! what it does and does not claim. Tauri injects `window.__TAURI_INTERNALS__`
//! into **every** webview it creates — the property is defined non-writable and
//! non-configurable before any script this module controls could run, so it
//! cannot be deleted, and a page in this window can call `invoke` and reach
//! Rust with a valid invoke key. What stops it is the other end:
//! `Webview::on_message` resolves the request against the ACL, and the lookup is
//! keyed on the **window label**. No capability file names this window, so every
//! command resolves to nothing and every call is rejected before a handler runs.
//! The object is present; the door it knocks on does not open.
//!
//! # Two locks, and it took a second commit to get the first one
//!
//! That paragraph used to end with "which forces an ACL lookup" — true only for
//! a *remote* origin. The gate is one condition in
//! `tauri::webview::Webview::on_message` (2.11.5):
//!
//! ```text
//! if (plugin_command.is_some() || has_app_acl_manifest || !is_local)
//!     && invoke.acl.is_none() { reject }
//! ```
//!
//! Mach had no app-level ACL manifest, so `has_app_acl_manifest` was false and
//! a **local** origin skipped the ACL entirely — in every window, this one
//! included. The empty capability grant refused this window only at a remote
//! origin, and the whole isolation rested on [`may_navigate`] never letting it
//! reach a local one.
//!
//! `src-tauri/permissions/mach.toml` closed that. It names Mach's own commands,
//! which is all `tauri-build` needs to emit the `__app-acl__` manifest, which is
//! all `has_app_acl_manifest` asks about. The ACL is now consulted for every
//! invoke regardless of origin, and this window is refused on two independent
//! grounds — it cannot get to a local origin, and it would be refused if it did.
//! `tests/browser.rs::the_page_window_is_refused_on_both_grounds` drives both
//! through the real gate with the real generated ACL.
//!
//! [`may_navigate`] is unchanged and still refuses `tauri:`, `plugin:`, `ipc:`
//! and every `*.localhost` host rather than only refusing "somewhere dangerous".
//! Tauri's `is_local_url` is what decides which of the two locks is doing the
//! work, and an allowlist one scheme wide is what keeps that from being decided
//! by an oversight.
//!
//! # `fetch` is not a navigation
//!
//! [`may_navigate`] governs where the *document* goes. It says nothing about a
//! subresource request, and Tauri registers its `tauri://` scheme handler on
//! every webview it creates — including this one, which never loads an app
//! asset in its life. Worse, the handler's `Access-Control-Allow-Origin` is
//! computed from the webview's **own** URL (`manager/webview.rs`, `window_origin`),
//! on the assumption that a Tauri webview shows a Tauri frontend. For this
//! window that assumption is exactly backwards: the header comes back naming
//! the sender's origin, so a `fetch("tauri://localhost/index.html")` from the
//! page is not merely unblocked, it is *granted*. In a debug build the reply is
//! "asset not found" — the assets live on the Vite dev server. In a release
//! build it is `index.html` and, from there, the whole embedded bundle.
//!
//! [`refuse_app_asset`] is the answer, and it is a per-window one: Tauri calls
//! the `on_web_resource_request` hook from exactly one place — the `tauri://`
//! handler, `protocol/tauri.rs` — for the webview that registered it. So the
//! main window's frontend is untouched and this window's every `tauri://`
//! response comes back a headerless, bodyless 403. With no
//! `Access-Control-Allow-Origin` on it the page cannot read the status either;
//! the `fetch` rejects.
//!
//! What was exposed, stated plainly: the frontend bundle, which is public
//! source code — Mach is at `github.com/bborn/mach` and the bundle carries no
//! key, no token and no message. `tauri://` serves the embedded asset map and
//! nothing else (`AppManager::get_asset` is a lookup with an `index.html`
//! fallback, not a filesystem read), and the app's two other schemes were
//! already closed: `plugin://` sets no `Access-Control-Allow-Origin` at all, and
//! an `ipc://` POST is an invoke, which the ACL above refuses. So this is a
//! disclosure of nothing secret — and it is closed anyway, because "the thing it
//! reads happens to be public" is a fact about today's bundle rather than a
//! boundary.
//!
//! ## What is left, and it is upstream's
//!
//! `get_response` calls the hook on its **success** path only. When
//! `get_asset` returns `Err`, `protocol::tauri::get` builds a 500 inline —
//! body `asset not found: <path>`, and `Access-Control-Allow-Origin` set from
//! the same `window_origin` — and never consults the hook. That response is
//! readable from the page.
//!
//! It costs nothing and it only exists in a *debug* build. `tauri-codegen`
//! embeds `EmbeddedAssets::default()` — an empty set — when `dev` and a
//! `devUrl` are both configured, so every asset is missing and every request
//! takes the error arm. A release build embeds `frontendDist`, `index.html` is
//! the last fallback for any path, and `get_asset` therefore cannot fail: the
//! success path is the only one, and the hook always runs. Verified both ways
//! by building with `devUrl` removed, which flips a debug build onto the
//! release asset path: without the hook the page read 470 bytes of the real
//! `index.html`, with it the fetch failed CORS.
//!
//! The fix belongs in `tauri`, and it is two lines: call
//! `web_resource_request_handler` on the error response too. The deeper one is
//! `window_origin` — computing a custom protocol's `Access-Control-Allow-Origin`
//! from the webview's own URL is right for a webview showing the app and wrong
//! for one built with `WebviewUrl::External`, which is a shape Tauri supports.
//! `protocol/asset.rs` has the same line. Until then the sentence to hold onto
//! is that Mach controls what it serves through that hole in the build it
//! ships, and nothing that is not already on GitHub goes through it in either.
//!
//! # The address is drawn by AppKit, not by us
//!
//! A chrome-less window showing a page a stranger chose is a phishing gift, so
//! the current host is always on screen — as the **window title**, which macOS
//! draws in the title bar outside the web content. The page cannot style it,
//! cover it, or scroll it away, and `on_document_title_changed` is left unset
//! so `document.title` does not reach it either. It is re-set on every
//! navigation, which includes every redirect.
//!
//! The host is shown exactly as [`url::Url`] holds it, which is punycode: a
//! homograph domain reads as `xn--pple-43d.com` here rather than as `аpple.com`.
//!
//! A second webview drawing a proper address bar would be prettier. It would
//! also need Tauri's `unstable` feature and would put the chrome inside the web
//! engine and inside the same *window* as the hostile page, where the boundary
//! becomes a capability glob over webview labels rather than the absence of any
//! capability at all. That trade was not worth it.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::http::{Response, StatusCode};
use tauri::webview::{DownloadEvent, NewWindowResponse};
#[cfg(target_os = "macos")]
use tauri::TitleBarStyle;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use url::Url;

/// The window's label, and therefore the name no capability may mention.
///
/// Capabilities in Tauri 2 are matched on this string. `capabilities/default.json`
/// lists `"main"` and nothing else, so this window inherits an empty grant —
/// which is the entire isolation story. `tests/capabilities.rs` asserts it.
pub const WINDOW_LABEL: &str = "mach-page";

/// Reported to the main window when this one refuses to do something.
///
/// The same event `ipc::render::link_guard` uses, because it is the same fact
/// from the user's side: something was clicked and Mach would not follow it.
/// `src/lib/message-body.ts` already turns it into a toast.
pub const BLOCKED_EVENT: &str = crate::ipc::render::LINK_FAILED_EVENT;

/// Escape closed the window, so the monitor knows to swallow the keystroke.
///
/// A plain flag rather than a lock: the monitor block runs on the main thread
/// and only there.
static WINDOW_OPEN: AtomicBool = AtomicBool::new(false);

/// One extra origin this window may reach, for a local fixture.
///
/// The production path — `ipc::commands::open_unsubscribe_page` — never
/// constructs one of these. `qa` does, and only for a loopback address, so an
/// agent can point the window at a page it wrote instead of at a real
/// unsubscribe URL out of somebody's mail. It is per-window state captured in
/// that window's own navigation closure, not a global relaxation of the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    origin: String,
}

impl Fixture {
    /// A loopback origin, or nothing. Refuses anything that is not `http` or
    /// `https` on 127.0.0.1, `::1` or `localhost`.
    pub fn loopback(url: &Url) -> Option<Fixture> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let host = url.host_str()?;
        if !matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]") {
            return None;
        }
        Some(Fixture {
            origin: origin_of(url)?,
        })
    }

    fn covers(&self, url: &Url) -> bool {
        origin_of(url).is_some_and(|o| o == self.origin)
    }
}

/// `scheme://host:port`, lowercased by `Url` already. `None` for a URL with no
/// host, which can never match a fixture.
fn origin_of(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// Why a navigation was refused. Each variant is a sentence fit for a toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not `https`. Includes `http`, and every app-internal scheme.
    Scheme,
    /// `https`, but at the machine, the LAN, or one of Mach's own origins.
    Host,
    /// Longer than a URL anybody wrote on purpose.
    TooLong,
    /// Not a URL at all.
    Malformed,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Scheme => "that page tried to go somewhere that is not https",
            Refusal::Host => "that page tried to reach this machine",
            Refusal::TooLong => "that address is too long",
            Refusal::Malformed => "that is not an address",
        }
    }
}

/// May this window go there?
///
/// An allowlist of one scheme, and the host rules are
/// [`crate::unsub::target::accepts_url`] — the same function the `List-Unsubscribe`
/// parser and the redirect policy in `unsub::http` use, so there is one copy of
/// "this host is not somewhere a stranger may point us" in the tree rather than
/// three. It already refuses userinfo, `localhost`, `*.localhost`, and loopback,
/// private, link-local, unique-local and carrier-grade-NAT IP literals — which
/// between them are the app's own origins, the QA control port and the Vite dev
/// server.
///
/// `about:blank` is allowed because WebKit navigates to it on its own and it
/// carries nothing.
pub fn may_navigate(url: &Url, fixture: Option<&Fixture>) -> Result<(), Refusal> {
    if url.as_str() == "about:blank" {
        return Ok(());
    }
    if fixture.is_some_and(|f| f.covers(url)) {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(Refusal::Scheme);
    }
    if url.as_str().len() > crate::unsub::target::MAX_URL_LEN {
        return Err(Refusal::TooLong);
    }
    if crate::unsub::target::accepts_url(url.as_str()) {
        Ok(())
    } else {
        Err(Refusal::Host)
    }
}

/// Throw away an app asset before the page in this window can read it.
///
/// Called for every `tauri://` response served to [`WINDOW_LABEL`] — see the
/// module doc for why one is served at all, and why its CORS header names the
/// sender. The response is emptied in place: no body, no headers, and a 403 so
/// the log and a devtools trace both say what happened rather than showing a
/// mysterious empty 200.
///
/// **The headers go first and that is the point.** Tauri put
/// `Access-Control-Allow-Origin: <the sender's origin>` on this response, and
/// leaving it there while emptying the body would still let the page read the
/// status code and the fact that a path exists. Clearing them makes the `fetch`
/// reject on CORS, which is the same answer any other origin on the internet
/// would get. `Content-Security-Policy` is in there too, and the app's CSP is
/// not this window's business either.
///
/// Separate from the closure so it can be tested without a webview, the same
/// split [`is_bare_escape`] makes.
pub fn refuse_app_asset(response: &mut Response<Cow<'static, [u8]>>) {
    response.headers_mut().clear();
    *response.status_mut() = StatusCode::FORBIDDEN;
    *response.body_mut() = Cow::Borrowed(&[]);
}

/// What the title bar says: the current host, and the port when there is one.
///
/// Punycode, not Unicode — see the module doc. A URL with no host cannot be
/// navigated to (`may_navigate` refuses it), so the fallback is only ever seen
/// for `about:blank`.
pub fn address_label(url: &Url) -> String {
    match url.host_str() {
        Some(host) => match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        },
        None => "blank page".to_string(),
    }
}

/// Open `url` in the page window, or point the existing window at it.
///
/// `url` has already been validated by the caller — `open_unsubscribe_page`
/// re-reads the header from the store and re-runs the whole rule — and is
/// validated again here, because this function is the thing that decides what a
/// webview loads and it should not depend on a promise made elsewhere.
///
/// One window at a time, on purpose. A second unsubscribe page replaces the
/// first rather than stacking, so there is never a pile of somebody else's
/// pages running in the background with nothing pointing at them.
pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    url: &str,
    fixture: Option<Fixture>,
) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|_| Refusal::Malformed.as_str().to_string())?;
    may_navigate(&parsed, fixture.as_ref()).map_err(|r| r.as_str().to_string())?;

    let title = address_label(&parsed);

    if let Some(existing) = app.get_webview_window(WINDOW_LABEL) {
        // Navigating rather than rebuilding: the same navigation guard runs,
        // and destroying and recreating a window in one tick races the runtime.
        existing
            .navigate(parsed.clone())
            .map_err(|e| format!("the page could not be opened: {e}"))?;
        let _ = existing.set_title(&title);
        let _ = existing.show();
        let _ = existing.set_focus();
        return Ok(());
    }

    let guard_app = app.clone();
    let guard_fixture = fixture.clone();
    let popup_app = app.clone();
    let popup_fixture = fixture.clone();
    let download_app = app.clone();

    let builder = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(parsed.clone()))
        .title(&title)
        .inner_size(900.0, 760.0)
        .min_inner_size(420.0, 320.0);

    // The ordinary macOS title bar, because the title *is* the address
    // readout. The main window hides its own (`Overlay` + `hiddenTitle`);
    // this one must not.
    //
    // The chain breaks here because both of these are macOS-only *methods* on
    // the builder rather than macOS-only arguments to one: elsewhere the title
    // bar belongs to the window manager and Tauri does not offer to describe
    // it. Nothing is lost by their absence — a plain decorated window with the
    // address as its title is exactly what they ask for.
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(TitleBarStyle::Visible)
        .hidden_title(false);

    builder
        // A fresh non-persistent `WKWebsiteDataStore`: no cookies, no
        // localStorage, no cache shared with anything, and nothing left on disk
        // when it closes. The cost is real and is the right cost — a page that
        // wants him signed in will not find a session here, and the
        // system-browser route is what covers that.
        .incognito(true)
        // Nothing in here is ours to debug, and an inspector attached to a
        // hostile page is a surface rather than a tool.
        .devtools(false)
        .browser_extensions_enabled(false)
        // Every `tauri://` request this window makes, refused. Tauri calls this
        // hook from its own `tauri://` handler and from nowhere else, and only
        // for the webview that registered it — so the mailbox window still
        // loads its frontend exactly as before. See `refuse_app_asset`.
        .on_web_resource_request(|request, response| {
            eprintln!(
                "browser: refused an app asset the page asked for: {}",
                request.uri()
            );
            refuse_app_asset(response);
        })
        .on_navigation(move |url| match may_navigate(url, guard_fixture.as_ref()) {
            Ok(()) => {
                // The title is re-set from the URL the engine is actually going
                // to, which is how a redirect chain stays visible.
                if let Some(window) = guard_app.get_webview_window(WINDOW_LABEL) {
                    let _ = window.set_title(&address_label(url));
                }
                true
            }
            Err(refusal) => {
                report(&guard_app, refusal.as_str());
                false
            }
        })
        // `window.open` and `target=_blank`.
        //
        // Never a second window: a popup with no title bar of its own is the
        // phishing shape this whole module is trying not to be, and
        // `NewWindowResponse::Create` would hand it a webview built from the
        // caller's configuration rather than from this builder's. So the
        // request is turned into a navigation of *this* window when it passes
        // the same policy — the flow keeps working and the address bar keeps
        // telling the truth — and is refused out loud when it does not.
        .on_new_window(move |url, _features| {
            match may_navigate(&url, popup_fixture.as_ref()) {
                Ok(()) => {
                    if let Some(window) = popup_app.get_webview_window(WINDOW_LABEL) {
                        let _ = window.set_title(&address_label(&url));
                        let _ = window.navigate(url);
                    }
                }
                Err(refusal) => report(&popup_app, refusal.as_str()),
            }
            NewWindowResponse::Deny
        })
        // Nothing this window loads writes a file. Returning `false` cancels
        // the download; saying so is the rest of the contract, because a link
        // that visibly does nothing is the bug this codebase has paid for most.
        .on_download(move |_webview, event| {
            if let DownloadEvent::Requested { .. } = event {
                report(
                    &download_app,
                    "that page tried to download a file — Mach will not save one from it",
                );
            }
            false
        })
        .build()
        .map_err(|e| format!("the page could not be opened: {e}"))?;

    WINDOW_OPEN.store(true, Ordering::SeqCst);
    Ok(())
}

/// Close it, if it is there. Called by ⌘W's close handler and by Escape.
pub fn close<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        // Destroyed, not hidden. `shell::intercept_close` hides the *main*
        // window so ⌘W cannot strand the app; a hidden window here would be a
        // stranger's page still running with nothing on screen pointing at it.
        let _ = window.destroy();
    }
    WINDOW_OPEN.store(false, Ordering::SeqCst);
}

/// Say, in the main window, that this one refused something.
///
/// Broadcast rather than `emit_to(MAIN_WINDOW, …)`, which is what this was
/// first written as and which delivered nothing. `emit_to` filters on the
/// *listener's* declared target, and `@tauri-apps/api`'s `listen` registers
/// with `EventTarget::Any` — a target that `filter_target` matches for no
/// label at all, so a labelled emit reaches a plain `listen` in no window.
/// `ipc::render::link_guard` broadcasts for the same reason.
///
/// Broadcasting does not put the sentence in front of the page: delivery into a
/// webview needs a listener registered through `plugin:event|listen`, and this
/// window's empty capability grant refuses that command.
///
/// It goes to the log as well as to the toast. A refusal the window itself
/// cannot show — because the window that refused has no UI of its own — is
/// exactly the shape of failure this codebase has paid for most.
fn report<R: Runtime>(app: &AppHandle<R>, message: &str) {
    #[derive(serde::Serialize, Clone)]
    struct Blocked {
        message: String,
    }
    eprintln!("browser: {message}");
    if let Err(e) = app.emit(
        BLOCKED_EVENT,
        Blocked {
            message: format!("Unsubscribe page: {message}"),
        },
    ) {
        eprintln!("browser: could not report that to the window: {e}");
    }
}

/// `kVK_Escape`, from `Carbon/HIToolbox/Events.h`. A virtual key code, so it is
/// the physical key rather than the character, on every layout.
const ESCAPE_KEY_CODE: u16 = 53;

/// Is this key-down the plain Escape that closes the page window?
///
/// Split out of the event monitor because the monitor cannot be reached from a
/// test — it needs an `NSEvent`, a run loop, and a focused window, and a QA
/// instance runs under an `Accessory` activation policy precisely so that it
/// can never have the focus. The same split `scroll::classify` makes, for the
/// same reason.
///
/// The modifier check is what keeps the swallow narrow. ⌘⎋, ⌥⎋ and ⌃⎋ belong to
/// the system and to text input; only the bare key is ours. `modifiers` is the
/// raw `NSEventModifierFlags` bit field, which also carries CapsLock, Fn and
/// NumericPad — those are ignored, because a page window is no less closable
/// with CapsLock on.
#[must_use]
pub fn is_bare_escape(key_code: u16, modifiers: u64) -> bool {
    // NSEventModifierFlagShift/Control/Option/Command.
    const DECORATING: u64 = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20);
    key_code == ESCAPE_KEY_CODE && modifiers & DECORATING == 0
}

/// Escape closes the window, from below the web engine.
///
/// It has to be here rather than in a `keydown` listener, for the same reason
/// `ipc::render::link_guard` is in Rust: the page is a document Mach does not
/// control and anything it is asked to run for us it can decline to run. A
/// `keydown` handler injected into it can be removed by one line of the
/// sender's script, and then the only way out of a full-screen page would be
/// the mouse — which `CLAUDE.md` forbids outright.
///
/// `+[NSEvent addLocalMonitorForEventsMatchingMask:handler:]` is ahead of
/// `-[NSApplication sendEvent:]`, so the keystroke is seen before the webview
/// is offered it, and no script in the page can be in front of that. The same
/// mechanism `scroll` uses, with the opposite rule about swallowing: this one
/// *does* return null, but only for one key code and only while this window has
/// the focus, so nothing else in the application can lose a keystroke to it.
#[cfg(target_os = "macos")]
pub fn install_escape<R: Runtime>(app: &AppHandle<R>) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::ptr::NonNull;

    let app = app.clone();
    let handler = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the block a live event for the duration of the
        // call, and nothing here retains it past the return.
        let ev: &NSEvent = unsafe { event.as_ref() };

        // The cheap half of the rule first, so an ordinary keystroke pays two
        // integer comparisons and never reaches the runtime.
        if !WINDOW_OPEN.load(Ordering::SeqCst) || !is_bare_escape(ev.keyCode(), ev.modifierFlags().0 as u64)
        {
            return event.as_ptr();
        }

        // Safe to call from here: `send_user_message` runs inline when it is
        // already on the main thread, which an event monitor always is, so
        // there is no round trip through an event loop that is not running.
        let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
            WINDOW_OPEN.store(false, Ordering::SeqCst);
            return event.as_ptr();
        };
        if !window.is_focused().unwrap_or(false) {
            return event.as_ptr();
        }

        let _ = window.destroy();
        WINDOW_OPEN.store(false, Ordering::SeqCst);
        // Swallowed — and this is the only path in the block that swallows.
        std::ptr::null_mut()
    });

    // SAFETY: the mask is a documented constant and the block matches the
    // signature AppKit calls it with.
    let token: Option<Retained<AnyObject>> = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
    };
    match token {
        // Lives as long as the process; dropping the token removes the monitor.
        Some(token) => std::mem::forget(token),
        None => eprintln!(
            "browser: could not install the Escape monitor; \
             the page window will still close with ⌘W"
        ),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install_escape<R: Runtime>(_app: &AppHandle<R>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::MAIN_WINDOW;

    fn url(s: &str) -> Url {
        Url::parse(s).expect(s)
    }

    #[test]
    fn the_label_is_not_the_one_the_capability_file_grants() {
        assert_ne!(WINDOW_LABEL, MAIN_WINDOW);
    }

    #[test]
    fn a_fixture_is_only_ever_loopback() {
        assert!(Fixture::loopback(&url("http://127.0.0.1:8975/page")).is_some());
        assert!(Fixture::loopback(&url("http://localhost:8975/")).is_some());
        for elsewhere in [
            "https://example.com/",
            "http://192.168.1.4:8975/",
            "http://127.0.0.1.evil.example/",
            "file:///etc/passwd",
        ] {
            assert!(
                Fixture::loopback(&url(elsewhere)).is_none(),
                "{elsewhere} was accepted as a fixture"
            );
        }
    }

    #[test]
    fn a_fixture_covers_its_own_origin_and_no_other() {
        let fixture = Fixture::loopback(&url("http://127.0.0.1:8975/unsub")).unwrap();
        assert!(fixture.covers(&url("http://127.0.0.1:8975/other/page?x=1")));
        // A different port on the same host is a different server.
        assert!(!fixture.covers(&url("http://127.0.0.1:8976/")));
        assert!(!fixture.covers(&url("https://127.0.0.1:8975/")));
        assert!(!fixture.covers(&url("http://localhost:8975/")));
    }

    /// The keystroke that closes it, and every neighbouring one that must not.
    ///
    /// The monitor swallows the event it acts on, and it sits in front of the
    /// whole application's key handling — so the rule has to be narrow enough
    /// that nothing else can lose a keystroke to it.
    #[test]
    fn only_a_bare_escape_closes_the_page_window() {
        const SHIFT: u64 = 1 << 17;
        const CONTROL: u64 = 1 << 18;
        const OPTION: u64 = 1 << 19;
        const COMMAND: u64 = 1 << 20;
        const CAPS_LOCK: u64 = 1 << 16;
        const FUNCTION: u64 = 1 << 23;

        assert!(is_bare_escape(ESCAPE_KEY_CODE, 0));
        // Neither of these is a modifier a person pressed on purpose with ⎋.
        assert!(is_bare_escape(ESCAPE_KEY_CODE, CAPS_LOCK));
        assert!(is_bare_escape(ESCAPE_KEY_CODE, FUNCTION));

        for decorated in [SHIFT, CONTROL, OPTION, COMMAND, COMMAND | SHIFT] {
            assert!(
                !is_bare_escape(ESCAPE_KEY_CODE, decorated),
                "a decorated escape ({decorated:#x}) belongs to the system"
            );
        }
        // Every other key on the keyboard, near-ish and far.
        for other in [0u16, 36 /* Return */, 51 /* Delete */, 48 /* Tab */, 49 /* Space */] {
            assert!(!is_bare_escape(other, 0), "key code {other} is not escape");
        }
    }

    /// The app's own `index.html`, on its way to a stranger's page, emptied.
    ///
    /// The response built here is the one `protocol::tauri::get_response`
    /// hands over: the asset bytes, the app's CSP, and an
    /// `Access-Control-Allow-Origin` naming the page that asked — which is what
    /// made the read work rather than what failed to stop it. All three are
    /// asserted gone, because leaving any one of them would leave the page
    /// something to read.
    #[test]
    fn an_app_asset_never_leaves_this_window_with_a_body_or_a_cors_header() {
        let mut response = Response::builder()
            .status(200)
            .header("content-type", "text/html")
            .header("content-security-policy", "default-src 'self'")
            .header("access-control-allow-origin", "https://newsletter.example")
            .body(Cow::Owned(b"<!doctype html><script src=/assets/app.js>".to_vec()))
            .expect("a response");

        refuse_app_asset(&mut response);

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(response.body().is_empty(), "the bundle came back anyway");
        assert!(
            response.headers().is_empty(),
            "a header survived: {:?} — the CORS one is what lets the page read \
             the status, and the CSP is not this window's business either",
            response.headers()
        );
    }

    #[test]
    fn the_title_is_the_host_and_never_the_path() {
        assert_eq!(address_label(&url("https://example.com/u/abc?t=1")), "example.com");
        assert_eq!(address_label(&url("https://example.com:8443/x")), "example.com:8443");
        // Punycode, so a homograph reads as one.
        assert_eq!(address_label(&url("https://аpple.com/")), "xn--pple-43d.com");
    }
}
