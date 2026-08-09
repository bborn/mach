//! A way for an agent to drive the real window without touching the keyboard.
//!
//! # The problem this exists for
//!
//! An agent can already *look* at the running app: `scripts/qa shoot` captures
//! the window's own buffer by CGWindowID, so nothing is raised and nobody's
//! focus moves. What it could not do is *act*. To open Preferences you press
//! ⌘,; to reach the calendar, ⌘2; to open a composer, `c`. The only way to
//! deliver a keystroke to a native window is synthetic OS input, which goes
//! wherever focus happens to be and takes the keyboard out of the owner's hands
//! mid-sentence.
//!
//! So agents fell back to a headless browser against the Vite dev server, which
//! is a different engine with no native window chrome. Four defects shipped
//! "verified" that way: preferences with its title under the traffic lights
//! (screenshotted in a browser, which has none), trackpad period navigation
//! that could not be exercised in WebKit at all, discard-draft and send-draft
//! both broken because opening a composer needs a keystroke, and a dead-link
//! bug that survived months because WebKit will not fire a listener in a
//! scripting-disabled document and Blink will.
//!
//! # Three verbs, and deliberately no fourth
//!
//! | verb | argument | what it does |
//! |---|---|---|
//! | `key` | a keymap token — `mod+2`, `g i`, `?` | the frontend synthesises the keystroke and hands it to the keymap |
//! | `click` | a CSS selector | dispatches a click on the first match, in-page |
//! | `ui` | — | reports mode, cursor, selection size, overlay, visible rows |
//!
//! There is no `eval`. An endpoint that runs arbitrary JavaScript inside a mail
//! client is remote code execution, and "it is compiled out of release builds"
//! is a promise rather than a guarantee — one `#[cfg]` removed by accident and
//! the whole mailbox is scriptable by anything that can reach a loopback port.
//! A closed set of three verbs cannot be turned into one. Rust never receives
//! code, and the frontend never evaluates a string: [`Verb`] is an enum, and
//! anything that is not one of its three spellings is a 400 before a window
//! ever hears about it.
//!
//! # It fails closed, four ways
//!
//! - **It is not in a release build at all.** `lib.rs` declares this module
//!   under `#[cfg(debug_assertions)]`, so `cargo build --release` does not
//!   compile it — not a runtime `if` that could be reached with a flag, an
//!   absence. `nm` on a release binary finds no symbol from it.
//! - **It refuses to exist outside a QA instance.** [`is_permitted`] is
//!   [`crate::shell::is_qa_instance`], which is `MACH_DATA_DIR` being set — the
//!   same signal that gives an instance its own store and its Accessory
//!   activation policy. The `main` instance, the one reading the owner's real
//!   mailbox, never opens this port even in a debug build.
//! - **Loopback and a bearer token.** `127.0.0.1:0` — never `0.0.0.0`, and an
//!   ephemeral port so nothing is guessable. Every request must carry
//!   `Authorization: Bearer <token>`, 32 bytes of `/dev/urandom` compared in
//!   constant time; anything carrying an `Origin` header is refused outright,
//!   because a browser is the only thing that sends one and no browser has
//!   business here. This is [`mcp::check_headers`] — the same function, not a
//!   second copy of it.
//! - **The port dies with the process.** The listener is an fd; there is no
//!   service left behind. The token file goes on drop.
//!
//! Somebody who reached this port *with the token* could press keys and click
//! in the QA window — which is to say, exactly what a person sitting at that
//! window could do, against a throwaway store, in a debug build, on this
//! machine. Without the token they get a 401 and nothing else.
//!
//! # Shape: the frontend implements the verbs
//!
//! Rust receives `{verb, argument}` and emits [`REQUEST_EVENT`]. A listener in
//! `src/lib/qa-bridge.ts` translates it into a synthetic DOM event or a keymap
//! dispatch and emits [`RESPONSE_EVENT`] back with the outcome. That split is
//! the point: the half that is reachable from a socket knows only three words,
//! and the half that knows how to press a key is not reachable from a socket.
//!
//! The transport follows `agent::mcp` exactly — `tiny_http` on `127.0.0.1:0`,
//! bearer token in a `0600` file, `Origin` rejected, two worker threads so a
//! slow verb does not make the server look hung. It is the same pattern
//! because there should not be two.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Listener, Manager, Runtime};

use crate::ipc::agent::engine::mcp;

/// The one path served. Anything else is a 404 — a loopback port that answers
/// on every path is a loopback port that is being scanned.
pub const PATH: &str = "/qa";

/// Rust → the window: `{ id, verb, argument }`.
pub const REQUEST_EVENT: &str = "mach://qa/request";

/// The window → Rust: `{ id, ... }`, whatever the verb had to say.
pub const RESPONSE_EVENT: &str = "mach://qa/response";

/// Where `scripts/qa` looks for the port and the token, under the instance's
/// own data directory. Never written for the owner's instance, which has no
/// `MACH_DATA_DIR` and therefore no control port.
pub const ENDPOINT_FILE: &str = "qa-control.json";

/// How long a verb may take before the answer is "the window did not answer".
///
/// Generous, because the window may still be mounting against Vite when the
/// first verb arrives, and a timeout that fires during startup reads as "the
/// bridge is broken" rather than "ask again in a moment".
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a worker wakes to notice that it should stop.
const POLL: Duration = Duration::from_millis(200);

/// The longest argument accepted. A keymap token is a handful of characters and
/// a CSS selector is a line; anything past this is not one of those.
const MAX_ARGUMENT: usize = 512;

// ===========================================================================
// The vocabulary
// ===========================================================================

/// Everything this port can be asked to do. There is no other variant, and
/// adding one is a deliberate act in this file rather than a string that
/// happens to arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Synthesise a keystroke and hand it to the keymap.
    Key,
    /// Dispatch a click on the first element matching a CSS selector.
    Click,
    /// Report what the interface currently is.
    Ui,
}

impl Verb {
    pub fn parse(name: &str) -> Option<Verb> {
        match name {
            "key" => Some(Verb::Key),
            "click" => Some(Verb::Click),
            "ui" => Some(Verb::Ui),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Key => "key",
            Verb::Click => "click",
            Verb::Ui => "ui",
        }
    }

    /// `ui` is a question; the other two are instructions and need a subject.
    fn takes_argument(self) -> bool {
        matches!(self, Verb::Key | Verb::Click)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub verb: Verb,
    pub argument: String,
}

/// Parse and validate a request body. `Err` is what the caller is told, and
/// nothing reaches the window until this has said yes.
pub fn parse_request(body: &str) -> Result<Request, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| format!("that is not JSON: {e}"))?;

    let name = value.get("verb").and_then(Value::as_str).unwrap_or_default();
    let verb = Verb::parse(name)
        .ok_or_else(|| format!("\"{name}\" is not a verb — this port knows key, click and ui"))?;

    let argument = value
        .get("argument")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if verb.takes_argument() && argument.trim().is_empty() {
        return Err(format!("{} needs an argument", verb.as_str()));
    }
    if !verb.takes_argument() && !argument.is_empty() {
        return Err(format!("{} takes no argument", verb.as_str()));
    }
    if argument.len() > MAX_ARGUMENT {
        return Err(format!(
            "that argument is {} bytes; the limit is {MAX_ARGUMENT}",
            argument.len()
        ));
    }

    Ok(Request { verb, argument })
}

// ===========================================================================
// The gate
// ===========================================================================

/// Whether this process may open a control port at all.
///
/// The whole answer is [`crate::shell::is_qa_instance`], and that is on purpose:
/// the thing that makes an instance safe to drive is that it has its own store
/// and its own window, and that is exactly what `MACH_DATA_DIR` means. One
/// signal, not two that can disagree.
///
/// The `#[cfg(debug_assertions)]` on the module declaration in `lib.rs` is the
/// other half. This function cannot be reached in a release build because it is
/// not in one.
pub fn is_permitted() -> bool {
    crate::shell::is_qa_instance()
}

// ===========================================================================
// The server
// ===========================================================================

/// A live control port. Dropping it stops the workers and takes the token off
/// the disk; the process exiting does the same thing to the socket.
pub struct Control {
    port: u16,
    endpoint_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Control {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint_path
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.endpoint_path);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Requests waiting for the window to answer, by id.
type Pending = Arc<Mutex<HashMap<u64, mpsc::SyncSender<Value>>>>;

/// Start the port, if this process is allowed one, and keep it alive.
///
/// Called from `setup`. Returns quietly when this is not a QA instance —
/// "there is no control port here" is the normal case, not a failure, and the
/// owner's own app must never log something that reads like one.
pub fn install<R: Runtime>(app: &AppHandle<R>, data_dir: &Path) {
    if !is_permitted() {
        return;
    }
    match start(app, data_dir) {
        Ok(control) => {
            eprintln!(
                "qa control: 127.0.0.1:{}{PATH}  ({})",
                control.port(),
                control.endpoint_path().display()
            );
            // Managed so it lives as long as the app does. Dropped at exit,
            // which removes the token file.
            app.manage(control);
        }
        Err(e) => eprintln!("qa control: not listening — {e}"),
    }
}

/// Bind, write the endpoint file, and start serving.
pub fn start<R: Runtime>(app: &AppHandle<R>, data_dir: &Path) -> Result<Control, String> {
    if !is_permitted() {
        return Err("this is not a QA instance; MACH_DATA_DIR is unset".to_string());
    }

    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| format!("could not open the control port: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| "the control port has no address".to_string())?
        .port();

    let token = mcp::random_token().map_err(|e| e.to_string())?;
    let endpoint_path = write_endpoint(data_dir, port, &token)?;

    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

    // The window's half of the conversation. Registered before the first
    // request can be emitted, so no answer can arrive with nowhere to go.
    let inbox = Arc::clone(&pending);
    app.listen_any(RESPONSE_EVENT, move |event| {
        let Ok(value) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let Some(id) = value.get("id").and_then(Value::as_u64) else {
            return;
        };
        let waiting = inbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        if let Some(tx) = waiting {
            let _ = tx.send(value);
        }
    });

    let server = Arc::new(server);
    let shutdown = Arc::new(AtomicBool::new(false));

    // Two, for the same reason `agent::mcp` has two: a verb that parks — a
    // click that opens a dialog which takes a moment to mount — must not make
    // the next `ui` look like a hung server.
    let mut workers = Vec::new();
    for _ in 0..2 {
        let server = Arc::clone(&server);
        let shutdown = Arc::clone(&shutdown);
        let pending = Arc::clone(&pending);
        let app = app.clone();
        let expected = token.clone();
        workers.push(std::thread::spawn(move || {
            serve(server, shutdown, app, pending, expected);
        }));
    }

    Ok(Control {
        port,
        endpoint_path,
        shutdown,
        workers,
    })
}

fn serve<R: Runtime>(
    server: Arc<tiny_http::Server>,
    shutdown: Arc<AtomicBool>,
    app: AppHandle<R>,
    pending: Pending,
    expected_token: String,
) {
    while !shutdown.load(Ordering::SeqCst) {
        let mut request = match server.recv_timeout(POLL) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            // The listener is gone, which is what shutdown looks like from here.
            Err(_) => return,
        };

        if request.url() != PATH {
            let _ = request.respond(text_response(404, "not found"));
            continue;
        }
        // The same check the MCP port makes, from the same function: bearer
        // token in constant time, loopback `Host`, and no `Origin` at all.
        if let Some(refusal) = mcp::check_headers(&request, &expected_token) {
            let _ = request.respond(refusal);
            continue;
        }
        if request.method() != &tiny_http::Method::Post {
            let _ = request.respond(text_response(405, "this port only accepts POST"));
            continue;
        }

        let mut body = String::new();
        if std::io::Read::read_to_string(request.as_reader(), &mut body).is_err() {
            let _ = request.respond(text_response(400, "unreadable body"));
            continue;
        }

        let response = match parse_request(&body) {
            Ok(parsed) => json_response(200, &deliver(&app, &pending, &parsed)),
            Err(why) => json_response(400, &json!({ "ok": false, "error": why })),
        };
        let _ = request.respond(response);
    }
}

/// Hand one validated request to the window and wait for its answer.
fn deliver<R: Runtime>(app: &AppHandle<R>, pending: &Pending, request: &Request) -> Value {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);

    let (tx, rx) = mpsc::sync_channel(1);
    pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, tx);

    if let Err(e) = app.emit(
        REQUEST_EVENT,
        json!({ "id": id, "verb": request.verb.as_str(), "argument": request.argument }),
    ) {
        pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        return json!({ "ok": false, "error": format!("could not reach the window: {e}") });
    }

    match rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(answer) => answer,
        Err(_) => {
            pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            json!({
                "ok": false,
                "error": "the window did not answer — is the frontend loaded? (qa logs)",
            })
        }
    }
}

fn json_response(status: u16, payload: &Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(payload.to_string())
        .with_status_code(status)
        .with_header(
            "Content-Type: application/json"
                .parse::<tiny_http::Header>()
                .expect("static header parses"),
        )
}

fn text_response(status: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body).with_status_code(status)
}

/// Write the port and token with an owner-only mode.
///
/// Created with the mode set rather than written and then chmod-ed: the window
/// between the two is small, but it is a window in which a bearer token is
/// world-readable. Same reasoning, same shape, as `agent::mcp::write_config`.
pub fn write_endpoint(dir: &Path, port: u16, token: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(ENDPOINT_FILE);
    let body = json!({ "port": port, "token": token }).to_string();

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, body)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_is_exactly_three_words() {
        assert_eq!(Verb::parse("key"), Some(Verb::Key));
        assert_eq!(Verb::parse("click"), Some(Verb::Click));
        assert_eq!(Verb::parse("ui"), Some(Verb::Ui));

        // The point of the whole design: there is no way to spell "run this".
        for forbidden in ["eval", "js", "script", "exec", "run", "Key", "KEY", ""] {
            assert_eq!(Verb::parse(forbidden), None, "{forbidden} must not be a verb");
        }
    }

    #[test]
    fn a_request_is_a_verb_and_at_most_one_string() {
        assert_eq!(
            parse_request(r#"{"verb":"key","argument":"mod+2"}"#),
            Ok(Request { verb: Verb::Key, argument: "mod+2".into() })
        );
        assert_eq!(
            parse_request(r#"{"verb":"ui"}"#),
            Ok(Request { verb: Verb::Ui, argument: String::new() })
        );
    }

    #[test]
    fn anything_that_is_not_one_of_the_three_is_refused_before_the_window_hears_it() {
        assert!(parse_request(r#"{"verb":"eval","argument":"alert(1)"}"#).is_err());
        assert!(parse_request(r#"{"argument":"mod+2"}"#).is_err());
        assert!(parse_request("not json at all").is_err());
    }

    #[test]
    fn an_instruction_needs_a_subject_and_a_question_does_not_take_one() {
        assert!(parse_request(r#"{"verb":"key","argument":"   "}"#).is_err());
        assert!(parse_request(r#"{"verb":"click"}"#).is_err());
        assert!(parse_request(r#"{"verb":"ui","argument":"anything"}"#).is_err());
    }

    #[test]
    fn an_argument_has_a_ceiling() {
        let huge = "a".repeat(MAX_ARGUMENT + 1);
        let body = json!({ "verb": "click", "argument": huge }).to_string();
        assert!(parse_request(&body).is_err());
    }

    /// The gate that keeps this port off the owner's own mailbox.
    ///
    /// `MACH_DATA_DIR` is process-wide, so this shares a lock with the test in
    /// `shell` that mutates the same variable — without it the two race and
    /// each sees the other's writes.
    #[test]
    fn the_port_refuses_to_exist_outside_a_qa_instance() {
        let _guard = crate::shell::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        std::env::remove_var("MACH_DATA_DIR");
        assert!(
            !is_permitted(),
            "the instance reading the owner's real mailbox must never open a control port"
        );

        std::env::set_var("MACH_DATA_DIR", "/tmp/mach-qa-control");
        assert!(is_permitted(), "an instance with its own store is drivable");

        std::env::remove_var("MACH_DATA_DIR");
    }

    #[test]
    fn the_token_file_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("mach-qa-endpoint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_endpoint(&dir, 4321, "deadbeef").expect("writes");

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["port"], 4321);
        assert_eq!(written["token"], "deadbeef");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a bearer token must not be world-readable");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
