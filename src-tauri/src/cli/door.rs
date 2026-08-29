//! The door: a loopback port that lives as long as the app does, and hands
//! whatever arrives to [`ToolGate::run`].
//!
//! # This widens `agent::mcp`, and here is the argument for it
//!
//! [`super::super::ipc::agent::engine::mcp`] opens a port with the same four
//! defences this one has, and then says, in as many words:
//!
//! > And the port dies with the session: the listener is dropped when the
//! > session ends, so there is no long-lived local service, only one that exists
//! > for as long as a question is being answered.
//!
//! That sentence was correct and this file contradicts it. A command line needs
//! an address that is there before it runs, so the lifetime goes from "one
//! question" to "one app". A future reader is entitled to find the reason here
//! rather than deduce that somebody stopped caring.
//!
//! **What actually changed is the lifetime, and only the lifetime.** Everything
//! else `mcp.rs` argues for is unchanged and shared with it by calling the same
//! functions rather than by resembling them:
//!
//! - it binds `127.0.0.1:0` — loopback only, never `0.0.0.0`, and still an
//!   ephemeral port, so the number is not guessable and the owner's app and six
//!   QA instances do not collide;
//! - every request must carry `Authorization: Bearer <token>`, 32 bytes from
//!   `/dev/urandom` via [`mcp::random_token`], compared in constant time by
//!   [`mcp::check_headers`] — the same function, not a second copy;
//! - the token is never in an argument vector. It goes into a `0600` file under
//!   the instance's data directory and the CLI reads it from there, so it is
//!   absent from `ps`, from the environment, and from shell history;
//! - a request carrying an `Origin` header is refused outright, which is the
//!   DNS-rebinding defence the MCP specification asks for and the only one that
//!   matters against a browser, because a browser can be made to POST blind at a
//!   loopback port even though it can never read the token.
//!
//! So the question is narrow: is a token file that exists for hours worse than
//! one that exists for seconds? It is worse, and it is worse in one specific
//! way — the window in which a process running as this user could read the file
//! is now the whole session rather than the length of one answer. That is a
//! real change and it should be said plainly.
//!
//! It is acceptable because of what is on the other side of that window. A
//! process running as this user does not need the door: it can already read the
//! store, the Keychain items are unlocked for this login session, and it could
//! simply run the app. The door adds nothing to an attacker who is already
//! inside the account. What it defends against is everything *outside* it —
//! another account on the machine, a page in a browser, something on the local
//! network — and against all three, the ephemeral-port-plus-token-plus-`Origin`
//! design is exactly as strong for an hour as it is for a second. Lifetime is
//! not one of the things those attacks are sensitive to.
//!
//! The thing that would genuinely have been worse is a *fixed* port. It would
//! have made the address guessable, made two instances collide, and turned "is
//! anything listening on 4870" into a reliable oracle for whether the owner has
//! his mail open. The stable thing here is the path of a file, not a number on
//! a socket — see [`super::endpoint`].
//!
//! # It is the same path, not a second one
//!
//! A `tools/call` here goes through [`ToolGate::run`] and nowhere else, which
//! means it goes through [`CommandDispatcher::execute`], which writes locally in
//! one transaction, calls Google, reverts exactly on failure and returns its own
//! inverse. There is no second write path and there must never be one: a local
//! write Google never accepted has no revert outside the app, and
//! `users.history.list` only reports changes that *happened*, so it would
//! survive silently until a full resync.
//!
//! # Approval, with no window to approve in
//!
//! The gate's normal answer to a [`ToolPolicy::Approve`] tool is to park on the
//! owner in Mach's own window. No shell pipeline can answer a window, so the
//! consent is collected on the invocation instead and carried in with the
//! request — and it is decided **here**, by [`protocol::decide`], not by the
//! CLI's argument parser. A CLI-side check is advice to whoever typed the
//! command; this is the thing that has to be unpersuadable. The decision is
//! then handed to the gate as an [`ApprovalDesk::standing`] answer, so a
//! refusal travels the same code path an owner's "no" would, and the gate is
//! still the only thing that runs a tool.
//!
//! [`ToolGate::run`]: crate::ipc::agent::engine::gate::ToolGate::run
//! [`ToolPolicy::Approve`]: crate::ipc::agent::engine::tools::ToolPolicy::Approve
//! [`CommandDispatcher::execute`]: crate::commands::CommandDispatcher::execute

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use crate::ipc::agent::engine::gate::{GateResult, ToolGate};
use crate::ipc::agent::engine::mcp;
use crate::ipc::agent::engine::session::{
    ApprovalDesk, ApprovalOutcome, SessionSnapshot, SessionStatus, SessionUi,
};
use crate::ipc::agent::engine::tools::{ToolContext, ToolOutcome};
use crate::ipc::compose::engine::draft::load_draft;
use crate::ipc::compose::engine::outbox::Outbox;
use crate::ipc::state::AppState;

use super::endpoint::{self, Endpoint};
use super::protocol::{self, Class, Decision, DoorRequest, Refusal};

/// The one path served. Anything else is a 404 — a loopback port that answers
/// on every path is a loopback port that is being scanned.
pub const PATH: &str = "/cli";

/// How often a worker wakes to notice that the app is going away.
const POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// The largest request body accepted. A tool call is a small JSON object; the
/// biggest legitimate one is a draft body, and a megabyte is far past any of
/// them.
const MAX_BODY: u64 = 1024 * 1024;

// ===========================================================================
// lifecycle
// ===========================================================================

/// A live door. Dropping it stops the workers and takes the token off the disk.
pub struct Door {
    port: u16,
    endpoint_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Door {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint_path
    }
}

impl Drop for Door {
    /// Stop listening and take the token off the disk.
    ///
    /// The file goes immediately, because a bearer token that outlives the
    /// process it authenticates is exactly the kind of thing that turns up in a
    /// backup. A hard kill skips this, which is why the file also carries the
    /// pid — see [`super::endpoint`].
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.endpoint_path);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Open the door and keep it open for the life of the app.
///
/// Called from `setup`, after the state is managed — the door has nothing to
/// dispatch to before that. A failure is reported and not fatal: an app that
/// refused to start because a loopback port was taken would be worse than an
/// app with no command line.
pub fn install<R: Runtime>(app: &AppHandle<R>, data_dir: &Path, runtime: tokio::runtime::Handle) {
    match start(app, data_dir, runtime) {
        Ok(door) => {
            eprintln!(
                "cli door: 127.0.0.1:{}{PATH}  ({})",
                door.port(),
                door.endpoint_path().display()
            );
            // Managed so it lives as long as the app does, and is dropped —
            // token file and all — when the app exits.
            app.manage(door);
        }
        Err(e) => eprintln!("cli door: not listening — {e}"),
    }
}

/// Bind, write the endpoint file, and start serving.
pub fn start<R: Runtime>(
    app: &AppHandle<R>,
    data_dir: &Path,
    runtime: tokio::runtime::Handle,
) -> Result<Door, String> {
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| format!("could not open the cli port: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| "the cli port has no address".to_string())?
        .port();

    let token = mcp::random_token().map_err(|e| e.to_string())?;
    let endpoint = Endpoint {
        port,
        token: token.clone(),
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let endpoint_path = endpoint::write(data_dir, &endpoint)?;

    let server = Arc::new(server);
    let shutdown = Arc::new(AtomicBool::new(false));

    // Two workers, for the reason `agent::mcp` has two: a call that reaches
    // Google can take seconds, and the next `tools` request must not look like a
    // hung server while it does. Actual tool execution is still serialised —
    // each request builds its own gate, and each gate takes its own lock for the
    // whole call.
    let mut workers = Vec::new();
    for _ in 0..2 {
        let server = Arc::clone(&server);
        let shutdown = Arc::clone(&shutdown);
        let app = app.clone();
        let runtime = runtime.clone();
        let expected = token.clone();
        workers.push(std::thread::spawn(move || {
            serve_with(server, shutdown, expected, move |request| {
                dispatch(&app, &runtime, request)
            });
        }));
    }

    Ok(Door {
        port,
        endpoint_path,
        shutdown,
        workers,
    })
}

/// The listener loop, with what it does about a well-formed request left to the
/// caller.
///
/// Split that way so `tests/cli.rs` can start a real socket and assert the four
/// refusals — no token, wrong token, an `Origin` header, the wrong path — against
/// **this** loop rather than against a copy of it that might not have drifted
/// yet. The handler is the only difference between the door the tests drive and
/// the door the app opens.
pub fn serve_with(
    server: Arc<tiny_http::Server>,
    shutdown: Arc<AtomicBool>,
    expected_token: String,
    handle: impl Fn(DoorRequest) -> Value,
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
        // The same four checks the MCP port makes, from the same function:
        // bearer token in constant time, loopback `Host`, and no `Origin` at
        // all.
        if let Some(refusal) = mcp::check_headers(&request, &expected_token) {
            let _ = request.respond(refusal);
            continue;
        }
        if request.method() != &tiny_http::Method::Post {
            let _ = request.respond(text_response(405, "this door only accepts POST"));
            continue;
        }

        let mut body = String::new();
        if request
            .as_reader()
            .take(MAX_BODY)
            .read_to_string(&mut body)
            .is_err()
        {
            let _ = request.respond(text_response(400, "unreadable body"));
            continue;
        }

        let answer = match serde_json::from_str::<DoorRequest>(&body) {
            Ok(parsed) => handle(parsed),
            Err(e) => refused("badRequest", format!("that is not a door request: {e}")),
        };
        let _ = request.respond(json_response(200, &answer));
    }
}

// ===========================================================================
// the two operations
// ===========================================================================

fn dispatch<R: Runtime>(
    app: &AppHandle<R>,
    runtime: &tokio::runtime::Handle,
    request: DoorRequest,
) -> Value {
    match request {
        DoorRequest::Tools => tools(app),
        DoorRequest::Call {
            tool,
            input,
            consent,
        } => call(app, runtime, &tool, &input, &consent),
    }
}

/// The whole surface, from [`ToolGate::tools`] — which is
/// [`Command::catalogue`](crate::commands::Command::catalogue) plus the local
/// reads, the composer and the installed plugins.
///
/// Not a list written for the command line. The verbs the CLI offers *are* this
/// answer, so `mach` cannot drift from what ⌘K can do; the two read the same
/// function on the same launch.
fn tools<R: Runtime>(app: &AppHandle<R>) -> Value {
    let gate = match gate_for(app, ApprovalOutcome::Approved) {
        Ok(gate) => gate,
        Err(refusal) => return json!({ "ok": false, "error": refusal }),
    };
    let items: Vec<Value> = gate
        .tools()
        .iter()
        .map(|tool| {
            let name = &tool.definition.name;
            json!({
                "name": name,
                "description": tool.definition.description,
                "inputSchema": tool.definition.input_schema,
                // What it costs to authorise, and whether the app has to be
                // running for it. Both are computed here rather than guessed at
                // by the CLI — see `protocol`.
                "consent": protocol::classify(name).as_str(),
                "local": protocol::is_local(name),
            })
        })
        .collect();
    json!({ "ok": true, "tools": items })
}

/// Run one tool, with the consent rule in front of it and the gate under it.
fn call<R: Runtime>(
    app: &AppHandle<R>,
    runtime: &tokio::runtime::Handle,
    tool: &str,
    input: &Value,
    consent: &protocol::Consent,
) -> Value {
    // The recipients a send is actually addressed to, read out of the draft
    // exactly as `ToolGate::approval_summary` reads them for the approval bar.
    // Only fetched for a send: every other class of call has nobody to name, and
    // a lookup on every archive would be a query for nothing.
    let recipients = match protocol::classify(tool) {
        Class::Outbound => draft_recipients(app, input),
        _ => None,
    };

    // Built before the decision rather than after it, so that "the door could
    // not get at the app's state" is one answer and not two. Constructing a gate
    // costs a clone and a plugin list; it dispatches nothing.
    let gate = match gate_for(app, ApprovalOutcome::Approved) {
        Ok(gate) => gate,
        Err(refusal) => return json!({ "ok": false, "error": refusal }),
    };

    decide_and_run(gate, runtime, tool, input, consent, recipients.as_deref())
}

/// The consent rule, and then the gate — with everything the app supplies
/// already resolved.
///
/// Separated from [`call`] so `tests/cli.rs` can drive it against a real gate
/// over a real store with no `AppHandle` in sight, and assert the thing that
/// matters: **a mutation without `--yes` refuses and the mailbox does not
/// move.** The refusal happens here, in front of the gate, rather than as a
/// `Denied` answer inside it, because most of what the CLI can do is
/// [`ToolPolicy::Auto`] and would never reach the desk to be denied — the gate
/// would simply run it.
pub fn decide_and_run(
    gate: Arc<ToolGate>,
    runtime: &tokio::runtime::Handle,
    tool: &str,
    input: &Value,
    consent: &protocol::Consent,
    recipients: Option<&[String]>,
) -> Value {
    if let Decision::Refuse(refusal) = protocol::decide(tool, consent, recipients) {
        return json!({ "ok": false, "error": refusal });
    }

    let call_id = door_call_id();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let name = tool.to_string();
    let arguments = input.clone();
    // Spawned onto the runtime and waited on over a plain channel rather than
    // `block_on` here, for the reason `agent::mcp` gives: `block_on` from a
    // listener thread needs the runtime to be driven by somebody else at that
    // exact moment, which is true in the app and not always true in a test.
    runtime.spawn(async move {
        let outcome = gate.run(&call_id, &name, &arguments).await;
        let _ = tx.send(outcome);
    });

    match rx.recv() {
        Ok(GateResult::Ok(outcome)) => answered(tool, &outcome),
        // The gate's own refusal: a name outside the surface, bad arguments, a
        // thread that is not there, a write Google would not take.
        Ok(GateResult::Refused(message)) => refused("refused", message),
        Ok(GateResult::Closed) => refused("refused", "Mach closed this call. Nothing was done."),
        Ok(GateResult::Fatal(error)) => refused(error.kind(), error.to_string()),
        Err(_) => refused(
            "refused",
            "Mach stopped before this could run. Nothing was done.",
        ),
    }
}

/// One tool run that reached the end, turned into an answer — and turned into a
/// **failure** when the command layer says the write did not land.
///
/// # Why the gate saying `Ok` is not enough
///
/// [`CommandResult`](crate::commands::CommandResult) carries its own `ok`, and
/// `false` there means Google refused the call and the local write was rolled
/// back exactly. The gate reports that as a successful *tool call*, which is
/// right for a model: the model reads the payload and tells the owner in a
/// sentence.
///
/// A shell reads the exit code. `mach archive 49028 --yes` against an account
/// whose credential has expired printed "Archived 0 conversations" and exited
/// 0 — the failure was in the payload, three lines down, under a heading that
/// said `ok`. That is silent failure, which is the specific thing that has cost
/// this project the most time, arriving through a new door.
///
/// So the command layer's verdict becomes the envelope's, and the message
/// carries what actually happened: what did land, what did not, and why.
fn answered(tool: &str, outcome: &ToolOutcome) -> Value {
    // Only a command result has an `ok`. A read, a draft or a plugin answer has
    // no such field and is reported exactly as it came.
    if outcome.payload.get("ok").and_then(Value::as_bool) == Some(false) {
        return refused(failure_kind(&outcome.payload), failure_message(outcome));
    }
    json!({
        "ok": true,
        "tool": tool,
        "summary": outcome.summary,
        "payload": outcome.payload,
        "mutated": outcome.mutated,
    })
}

/// The first failure's own kind — `auth`, `rateLimited`, `notFound` — so a
/// caller can tell "reconnect the account" from "try again in a minute" from
/// "that thread is gone at Google" without reading English.
fn failure_kind(payload: &Value) -> &str {
    payload
        .get("failed")
        .and_then(Value::as_array)
        .and_then(|failures| failures.first())
        .and_then(|failure| failure.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("refused")
}

/// What happened, in one sentence: the command layer's own message, then every
/// reason it gave, and whether the store was put back.
fn failure_message(outcome: &ToolOutcome) -> String {
    let payload = &outcome.payload;
    let mut said = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or(&outcome.summary)
        .to_string();

    let empty = Vec::new();
    let failures = payload
        .get("failed")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for failure in failures {
        let ids = failure
            .get("ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_i64)
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let why = failure
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Google refused it");
        // Google's own sentences do not end in a full stop, and the first line
        // this produced read "…must be authorized first The local change was
        // put back." One is added when it is missing rather than always, so a
        // message that already has one does not get two.
        said.push_str(&format!(" \u{2014} {ids}: {}", stopped(why)));
        if failure.get("rolledBack").and_then(Value::as_bool) == Some(true) {
            said.push_str(" The local change was put back.");
        }
    }
    said
}

/// A sentence with a full stop on the end of it.
fn stopped(sentence: &str) -> String {
    let trimmed = sentence.trim_end();
    match trimmed.ends_with(['.', '!', '?', ':']) {
        true => trimmed.to_string(),
        false => format!("{trimmed}."),
    }
}

// ===========================================================================
// building a gate, per call
// ===========================================================================

/// The outbox, built once and shared.
///
/// The same queue the composer uses, so a message sent from the command line
/// gets the same row and the same ten-second recall as one sent with ⌘⏎. Cached
/// because `Outbox::new` checks the compose schema, and doing that on every
/// invocation would be a write per call for nothing.
static OUTBOX: OnceLock<Arc<Outbox>> = OnceLock::new();

fn outbox<R: Runtime>(app: &AppHandle<R>) -> Result<Arc<Outbox>, String> {
    if let Some(existing) = OUTBOX.get() {
        return Ok(Arc::clone(existing));
    }
    let state = app.state::<AppState>();
    let built = Arc::new(
        Outbox::new(state.db.clone(), Arc::clone(&state.dispatcher.clients))
            .map_err(|e| e.to_string())?
            // A send from the command line that Google refuses still has to
            // reach the person at the window, and this is the only path by
            // which it can: the CLI process has already exited by the time the
            // undo window lapses and the flush runs in here.
            .reporting_to(Arc::new(crate::ipc::events::SendFailures::new(app.clone()))),
    );
    Ok(Arc::clone(OUTBOX.get_or_init(|| built)))
}

/// A gate for one call, with the answer to any approval already in it.
///
/// Built per call rather than once, for the same reason a session builds its
/// own: the tool list a call is judged against has to be the list that existed
/// when it arrived, and a plugin installed a minute ago should be usable
/// without relaunching.
fn gate_for<R: Runtime>(
    app: &AppHandle<R>,
    outcome: ApprovalOutcome,
) -> Result<Arc<ToolGate>, Refusal> {
    let outbox = outbox(app).map_err(|e| Refusal::new("notReady", e))?;
    let state = app.state::<AppState>();
    let ctx = ToolContext {
        db: state.db.clone(),
        dispatcher: Arc::clone(&state.dispatcher),
        outbox,
        plugins: Arc::clone(&state.plugins),
    };
    let plugins = ctx.plugin_list();

    // A UI that drops the running commentary — there is no drawer here — but
    // still tells the window its lists are stale, because an archive from the
    // command line has to repaint the mailbox somebody is looking at.
    let ui = Arc::new(SessionUi::new(
        "cli",
        Arc::new(Mutex::new(blank_snapshot())),
        Arc::new(DoorEmitter { app: app.clone() }),
    ));
    let desk = Arc::new(ApprovalDesk::standing(Arc::clone(&ui), outcome));
    Ok(Arc::new(ToolGate::new(ctx, plugins, ui, desk)))
}

/// Drops the session commentary, forwards the one event that matters.
///
/// `session_event` has nowhere to go: there is no drawer open on a shell
/// pipeline, and putting the CLI's tool calls into the agent pane would be Mach
/// inventing a conversation nobody had. `threads_changed` is different — the
/// mailbox actually moved, and the window is showing a list that is now wrong.
struct DoorEmitter<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> crate::ipc::agent::engine::session::SessionEmitter for DoorEmitter<R> {
    fn session_event(&self, _event: &crate::ipc::agent::engine::session::SessionEvent) {}

    fn threads_changed(&self) {
        crate::ipc::events::emit_threads_changed(&self.app);
    }
}

fn blank_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        id: "cli".to_string(),
        title: String::new(),
        status: SessionStatus::Running,
        created_at: 0,
        context: Vec::new(),
        entries: Vec::new(),
        pending: None,
        error: None,
        backend: None,
    }
}

/// Who a draft is addressed to: `to` + `cc` + `bcc`, in that order.
///
/// The bcc is included and it is the reason the check is worth having. A draft
/// with a blind copy on it reaches somebody the operator did not necessarily
/// have in mind, and the whole claim `--to` makes is "I know who this goes to".
fn draft_recipients<R: Runtime>(app: &AppHandle<R>, input: &Value) -> Option<Vec<String>> {
    let draft_id = input.get("draftId").and_then(Value::as_str)?;
    let state = app.state::<AppState>();
    let draft = load_draft(&state.db, draft_id).ok().flatten()?;
    Some(
        draft
            .to
            .iter()
            .chain(draft.cc.iter())
            .chain(draft.bcc.iter())
            .map(|m| m.email.clone())
            .collect(),
    )
}

// ===========================================================================
// plumbing
// ===========================================================================

/// The id one call is known by inside the gate. Opaque; nothing round-trips it.
fn door_call_id() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("cli-{:x}", N.fetch_add(1, Ordering::Relaxed))
}

fn refused(kind: &str, message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": Refusal::new(kind, message) })
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
