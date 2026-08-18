//! Where a QA instance loads its frontend from, decided at runtime.
//!
//! # `devUrl` was a compile-time constant, and that hurt twice
//!
//! `tauri.conf.json` says `"devUrl": "http://localhost:1420"`, and
//! `generate_context!` expands that into the binary. Two consequences, both of
//! which cost real time before this file existed.
//!
//! The first was expensive for the person using the machine. Port 1420 is where
//! the owner's own app loads from, and every instance built from that constant
//! wanted it. `scripts/qa up` would start a dev server from whatever checkout
//! invoked it if nothing was listening — so an agent working in a worktree
//! served its in-progress frontend into the window the owner was reading real
//! mail in. Twice in one day.
//!
//! The second was expensive for agents. Pointing an instance at another port
//! meant editing the config and rebuilding, and the rebuild did not happen:
//! `touch src-tauri/src/lib.rs` recompiles the crate but not the *context*, so
//! the binary kept the old URL. `touch src-tauri/build.rs` is also required,
//! which one agent worked out after losing a cycle to it. Until then the app
//! silently loaded the wrong frontend, which looks exactly like "my change did
//! not take".
//!
//! So the URL is an environment variable now, applied to the config before the
//! builder ever sees it. [`Manager::get_app_url`] reads
//! `config.build.dev_url` at window-creation time, and `Context::config_mut`
//! is the supported way to reach it — `shell::suppress_configured_window`
//! already mutates the same struct a line earlier.
//!
//! # It fails loudly, because the silent failure is the dangerous one
//!
//! A QA instance with no `MACH_DEV_URL`, or one pointing at 1420, exits before
//! it opens a window. Rendering somebody else's frontend is not a degraded
//! mode worth continuing in: the screenshots would be of the wrong code, and
//! the last time this happened nobody noticed for an unknown stretch.
//!
//! `main` — the owner's instance, no `MACH_DATA_DIR` — is left exactly as it
//! was: the compiled-in 1420, unless he sets `MACH_DEV_URL` himself.
//!
//! # A dev server that is not up yet is a blank white window
//!
//! Whatever the URL turns out to be, the window loads it over HTTP, and twice
//! in one day the app was started a moment before vite was listening. WKWebView
//! gets `ECONNREFUSED`, renders nothing, and never asks again: a white
//! rectangle, no message, no error, no retry, with a frontend that was healthy
//! the whole time. It looks exactly like the app being broken.
//!
//! So the URL is probed before the window is built. A few hundred milliseconds
//! of grace covers the ordinary race — started a beat early — with nothing on
//! screen. Past that the window is pointed at [`WAIT_URL`] instead, a page
//! served from this process saying which URL it is waiting for, and a thread
//! keeps trying until the server answers and then navigates the window onto it.
//!
//! A custom scheme rather than a `data:` URL because WebKit refuses a top-level
//! navigation to `data:`, and rather than an inlined `eval` because the window
//! has to be able to *start* on the page, before any of the app's JavaScript
//! exists to be evaluated.
//!
//! All of it is behind [`tauri::is_dev`], which is the same condition — `cfg(dev)`
//! in `Manager::get_app_url` — that decides a build loads from a URL at all. A
//! release build reads its frontend out of the bundle, where there is nothing to
//! refuse a connection and nothing to retry.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::OnceLock;
use std::time::Duration;

use tauri::http::{Request, Response};
use tauri::Manager;

/// The port the owner's app loads from. Off limits to every QA instance.
pub const OWNER_PORT: u16 = 1420;

/// The scheme the "waiting for the dev server" page is served from.
pub const WAIT_SCHEME: &str = "machwait";

/// Where the window sits while the dev server is not answering.
pub const WAIT_URL: &str = "machwait://localhost/";

/// How long a connection attempt may hang before it counts as a refusal.
///
/// A refused connection comes back immediately; this bounds the other case, a
/// host that swallows the SYN. Short, because it is paid once per address per
/// attempt and the whole point is to keep answering "not yet" quickly.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

/// Between attempts, before the window exists and after.
const RETRY_EVERY: Duration = Duration::from_millis(500);

/// How long to wait at startup before giving up on a silent start.
///
/// The window comes up on the real frontend either way; this is only about
/// whether the wait page is ever *seen*. Two seconds covers `scripts/qa up`
/// racing its own `bun run dev`, and is short enough that a server which is
/// genuinely absent says so rather than appearing to hang.
const GRACE: Duration = Duration::from_secs(2);

/// The URL the window is meant to end up on, when it could not be reached at
/// startup. Unset in every other case, including a release build.
static DEFERRED: OnceLock<tauri::Url> = OnceLock::new();

/// Which checkout compiled this binary.
///
/// Every worktree shares the parent's target directory — a private one costs
/// several minutes and about 2GB per agent — and they all uplift their
/// artifact to the same `target/debug/mach`. Whoever built last owns that
/// name, so an agent can be screenshotting a window running somebody else's
/// Rust with nothing on screen to say so. One agent worked it out by grepping
/// the running binary for a symbol it did not recognise; there was no
/// cheaper way, because a debug build with `split-debuginfo=unpacked` carries
/// no path strings at all.
///
/// This is that symbol, put there on purpose: baked in at compile time,
/// printed at startup, and greppable from the outside — which is how
/// `scripts/qa` refuses to launch an instance from a binary this checkout did
/// not build.
pub const BUILT_FROM: &str = env!("CARGO_MANIFEST_DIR");

/// What `scripts/qa` sets, and what this module reads.
pub const DEV_URL_VAR: &str = "MACH_DEV_URL";

/// Apply `MACH_DEV_URL` to the context, or explain why we are not starting.
///
/// Returns `Err` with a message meant to be read in `.qa/<instance>/mach.log`
/// by somebody who has just watched `qa up` fail. The caller exits on it.
pub fn resolve(is_qa: bool, requested: Option<&str>) -> Result<Option<tauri::Url>, String> {
    let Some(raw) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        if is_qa {
            return Err(format!(
                "{DEV_URL_VAR} is not set, and a QA instance must not fall back to the \
                 compiled-in http://localhost:{OWNER_PORT} — that is the port the owner's \
                 own window loads from. Launch through `scripts/qa up`, which derives a \
                 dev-server port for this instance and serves it."
            ));
        }
        return Ok(None);
    };

    let url = tauri::Url::parse(raw)
        .map_err(|e| format!("{DEV_URL_VAR} is not a URL: {raw:?} ({e})"))?;

    // `port_or_known_default` rather than `port`, so a bare `http://localhost`
    // is understood as 80 rather than as "no port and therefore fine".
    if is_qa && url.port_or_known_default() == Some(OWNER_PORT) {
        return Err(format!(
            "{DEV_URL_VAR}={raw} points at port {OWNER_PORT}. That is the owner's dev \
             server, feeding the window he is reading mail in; a QA instance may never \
             load from it. `scripts/qa up` picks a port in 1430–1928 for each instance."
        ));
    }

    Ok(Some(url))
}

/// Read the environment, decide, and either write the config or exit.
///
/// Called from `run()` before the builder is constructed. Exits the process
/// rather than returning an error, because there is no caller above this that
/// could do anything more useful with one, and a half-started mail client with
/// the wrong frontend is the outcome being prevented.
pub fn apply(context: &mut tauri::Context) {
    // First line in the log, so "which code is this?" is answerable from
    // `qa logs` without anybody having to think of grepping a binary.
    eprintln!("mach: built from {BUILT_FROM}");

    let requested = std::env::var(DEV_URL_VAR).ok();
    match resolve(crate::shell::is_qa_instance(), requested.as_deref()) {
        Ok(None) => {}
        Ok(Some(url)) => {
            eprintln!("qa dev url: {url}");
            context.config_mut().build.dev_url = Some(url);
        }
        Err(why) => {
            eprintln!("mach: {why}");
            std::process::exit(1);
        }
    }

    // Only a build that loads its frontend over HTTP has a connection to be
    // refused. `is_dev` is `cfg(dev)`, which is the same flag `get_app_url`
    // reads when it chooses `dev_url` over the bundled assets.
    if tauri::is_dev() {
        defer_until_the_dev_server_answers(context);
    }
}

/// Can something be reached at this URL right now?
///
/// A TCP connect rather than a request: vite serves as soon as it is listening,
/// and a bare connect needs no HTTP client and cannot be confused by a status
/// code. Every resolved address is tried, because `localhost` resolves to `::1`
/// first on this machine and a server bound only to `127.0.0.1` would otherwise
/// read as absent — the same trap `scripts/qa`'s `port_is_free` documents.
fn answers(url: &tauri::Url) -> bool {
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return false;
    };
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    addresses.into_iter().any(|address| {
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).is_ok()
    })
}

/// Point the window at the wait page when the dev server is not up yet.
///
/// Called with the config already carrying whichever URL this instance is meant
/// to load. Returns having changed nothing in the ordinary case.
///
/// # The window's URL, and not `build.dev_url`
///
/// Rewriting `dev_url` is the obvious move and it breaks the app. `dev_url` is
/// what [`WebviewWindow::is_local_url`] measures an origin against, and that is
/// the gate on IPC: point it at the wait page and the real frontend, once
/// loaded, is a *remote* document — every `invoke` refused, which is a far
/// worse failure than the white window, and a silent one.
///
/// So `dev_url` keeps naming the dev server, and only the window's starting URL
/// moves. `WebviewUrl::App` — the default — is resolved against `dev_url` at
/// creation time; `External` is taken literally.
fn defer_until_the_dev_server_answers(context: &mut tauri::Context) {
    let Some(url) = context.config().build.dev_url.clone() else {
        return;
    };

    let deadline = std::time::Instant::now() + GRACE;
    loop {
        if answers(&url) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(RETRY_EVERY);
    }

    eprintln!("mach: nothing is answering at {url} — showing the waiting page and retrying");
    let _ = DEFERRED.set(url);

    // The owner's instance opens the window Tauri builds from config. A QA
    // instance builds its own in `setup` and reads [`start_url`] there instead;
    // `suppress_configured_window` has already cleared `create` on this one.
    let wait = start_url();
    for window in context.config_mut().app.windows.iter_mut() {
        window.url = wait.clone();
    }
}

/// Where a window should open: the wait page, or the frontend as usual.
///
/// `scripts/qa`'s instances build their own window rather than taking the
/// configured one, so they have to ask.
pub fn start_url() -> tauri::WebviewUrl {
    match DEFERRED.get() {
        Some(_) => tauri::WebviewUrl::External(
            tauri::Url::parse(WAIT_URL).expect("WAIT_URL is a literal"),
        ),
        None => tauri::WebviewUrl::default(),
    }
}

/// Serve the wait page. Registered on [`WAIT_SCHEME`] in `lib.rs`.
pub fn wait_page(_request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    // Whatever we are waiting for, named on screen. `DEFERRED` is always set by
    // the time a request can arrive — the window only exists at this URL
    // because it was.
    let url = DEFERRED
        .get()
        .map(tauri::Url::to_string)
        .unwrap_or_else(|| "the dev server".to_string());

    Response::builder()
        .status(200)
        .header("content-type", "text/html; charset=utf-8")
        // No scripts, no network. The page is a sentence and a pulsing dot.
        .header(
            "content-security-policy",
            "default-src 'none'; style-src 'unsafe-inline'",
        )
        .header("x-content-type-options", "nosniff")
        .body(wait_html(&url).into_bytes())
        .expect("static wait response")
}

/// The page itself, kept separate so a test can read it without a webview.
fn wait_html(url: &str) -> String {
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>Waiting for the dev server</title>
<style>
  :root {{ color-scheme: light dark; }}
  html, body {{ height: 100%; margin: 0; }}
  body {{
    display: flex; flex-direction: column; gap: .75rem;
    align-items: center; justify-content: center;
    font: 13px/1.5 -apple-system, BlinkMacSystemFont, sans-serif;
    background: #fbfbfd; color: #1d1d1f;
  }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #1c1c1e; color: #f2f2f7; }}
  }}
  h1 {{ font-size: 15px; font-weight: 600; margin: 0; }}
  code {{ font-family: ui-monospace, SFMono-Regular, monospace; font-size: 12px; }}
  p {{ margin: 0; opacity: .6; }}
  .dot {{
    width: 6px; height: 6px; border-radius: 50%; background: currentColor;
    opacity: .35; animation: pulse 1.4s ease-in-out infinite;
  }}
  @keyframes pulse {{ 0%, 100% {{ opacity: .15 }} 50% {{ opacity: .7 }} }}
</style>
<div class="dot"></div>
<h1>No dev server at <code>{url}</code></h1>
<p>Retrying. This window loads as soon as it answers.</p>
<p>Start one with <code>bun run dev</code>.</p>
"#
    )
}

/// Keep trying, and load the real frontend the moment it is there.
///
/// A no-op unless [`defer_until_the_dev_server_answers`] gave up at startup.
/// `navigate` goes through the webview dispatcher, which posts to the event
/// loop, so calling it from this thread is fine.
pub fn resume_when_ready<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(url) = DEFERRED.get().cloned() else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || loop {
        if answers(&url) {
            eprintln!("mach: {url} is answering — loading it");
            if let Some(window) = app.get_webview_window(crate::shell::MAIN_WINDOW) {
                if let Err(e) = window.navigate(url.clone()) {
                    eprintln!("mach: could not navigate to {url}: {e}");
                }
            }
            return;
        }
        std::thread::sleep(RETRY_EVERY);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owners_instance_keeps_the_compiled_in_url() {
        assert_eq!(resolve(false, None), Ok(None));
        assert_eq!(resolve(false, Some("   ")), Ok(None));
    }

    /// The owner may still move his own dev server; nothing here stops him.
    #[test]
    fn the_owners_instance_may_still_be_pointed_somewhere() {
        let resolved = resolve(false, Some("http://localhost:1420")).expect("allowed");
        assert_eq!(resolved.map(|u| u.port()), Some(Some(1420)));
    }

    #[test]
    fn a_qa_instance_without_one_refuses_to_start() {
        let refusal = resolve(true, None).expect_err("must not fall back to 1420");
        assert!(refusal.contains("1420"), "{refusal}");
    }

    /// The property the whole file exists for.
    #[test]
    fn a_qa_instance_may_never_load_the_owners_port() {
        for pointed_at_the_owner in [
            "http://localhost:1420",
            "http://127.0.0.1:1420",
            "http://[::1]:1420/",
        ] {
            assert!(
                resolve(true, Some(pointed_at_the_owner)).is_err(),
                "{pointed_at_the_owner} must be refused"
            );
        }
    }

    #[test]
    fn a_derived_port_is_accepted() {
        let resolved = resolve(true, Some("http://localhost:1573")).expect("accepted");
        assert_eq!(resolved.expect("some").port(), Some(1573));
    }

    #[test]
    fn nonsense_is_a_refusal_rather_than_a_fallback() {
        assert!(resolve(true, Some("not a url")).is_err());
    }

    /// The question the retry loop asks, and the one the startup grace asks.
    /// Both directions, because "always true" and "always false" are the two
    /// ways this fails silently — one hides the wait page, the other shows it
    /// over a perfectly good dev server.
    #[test]
    fn a_listening_port_answers_and_a_closed_one_does_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        let url = tauri::Url::parse(&format!("http://127.0.0.1:{port}/")).expect("url");

        assert!(answers(&url), "a bound port must read as up");

        drop(listener);
        assert!(!answers(&url), "a refused connection must read as down");
    }

    #[test]
    fn the_wait_page_says_what_it_is_waiting_for() {
        let html = wait_html("http://localhost:1573/");
        assert!(html.contains("http://localhost:1573/"), "{html}");
        assert!(html.contains("Retrying"), "{html}");
    }

    /// The wait page must not be able to reach the network or run script. It is
    /// the first thing a window loads in the one state where nothing else has
    /// checked anything.
    #[test]
    fn the_wait_page_is_inert() {
        let request = Request::builder()
            .uri("machwait://localhost/")
            .body(Vec::new())
            .expect("request");
        let response = wait_page(&request);
        let csp = response
            .headers()
            .get("content-security-policy")
            .expect("a policy")
            .to_str()
            .expect("ascii");

        assert!(csp.starts_with("default-src 'none'"), "{csp}");
        assert!(!csp.contains("script-src"), "no script at all: {csp}");
    }
}
