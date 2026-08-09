//! Mach's command layer, spoken as MCP, so a brain in another process can use
//! it without being given anything else.
//!
//! The Claude Code CLI knows how to read files and run shell commands. It knows
//! nothing about mail. The idiomatic way to teach it — and the only way that
//! does not involve Mach shipping a second copy of its own command layer — is
//! the Model Context Protocol: Mach serves its tools, the CLI is started with
//! `--mcp-config` pointing at that server, and "reply to this next Tuesday"
//! becomes a tool call that lands back inside this process.
//!
//! # Why HTTP on loopback, and not stdio
//!
//! The usual MCP transport is stdio: the client spawns the server as a child and
//! talks over its pipes. That is the wrong shape here, and it is worth being
//! precise about why. The tools are not a library — they are *this running
//! app's* state: an open SQLite handle, a command dispatcher with the OAuth
//! tokens, an outbox with a ten-second recall timer, and a window that can ask
//! the owner a question. A stdio server would be a second process holding none
//! of that, which would then need its own channel back into the app. That is the
//! same socket problem, plus a process.
//!
//! So the server is in-process, and the transport is HTTP on `127.0.0.1`. That
//! is a port that can archive mail and send email, which is a serious thing to
//! open on a machine, so it is closed in four ways at once:
//!
//! - **it binds `127.0.0.1:0`** — loopback only, never `0.0.0.0`, and an
//!   ephemeral port so nothing is guessable and two sessions never collide;
//! - **every request must carry `Authorization: Bearer <token>`**, where the
//!   token is 32 bytes from `/dev/urandom`, minted per session and compared in
//!   constant time. No token, no answer — not even `initialize`;
//! - **the token never appears in an argument vector.** It is written to a
//!   `0600` file that the CLI is pointed at, and the file is deleted when the
//!   session ends;
//! - **`Origin` must be absent.** A browser cannot read the token, but it can be
//!   made to POST blind at a loopback port; refusing anything that arrives with
//!   an `Origin` header removes that class outright (this is the DNS-rebinding
//!   defence the MCP specification asks for).
//!
//! And the port dies with the session: the listener is dropped when the session
//! ends, so there is no long-lived local service, only one that exists for as
//! long as a question is being answered.
//!
//! # What it exposes
//!
//! Exactly [`ToolGate::tools`], which is exactly
//! [`Command::catalogue`](crate::commands::Command::catalogue) plus the local
//! reads, the composer, and the installed plugins — the same list, from the same
//! function, that the Anthropic backend puts in its `tools` array. Not a
//! parallel catalogue written for MCP; the same one. `tools/call` goes through
//! [`ToolGate::run`], so the approval rule holds identically no matter what the
//! CLI thinks its own permissions are.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use super::error::AgentError;
use super::gate::{GateResult, ToolGate};
use super::tools::Tool;

/// The MCP revision this server implements. Sent back verbatim on `initialize`
/// unless the client asked for something older, in which case theirs wins —
/// version negotiation is the client's to lead.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The server name. It is also the prefix the CLI gives every tool
/// (`mcp__mach__archive`), which is what `--allowedTools mcp__mach` matches.
pub const SERVER_NAME: &str = "mach";

/// The one path served. Anything else is a 404 — a loopback port that answers on
/// every path is a loopback port that is being scanned.
pub const PATH: &str = "/mcp";

// ===========================================================================
// The protocol, as pure functions
// ===========================================================================

/// One JSON-RPC message, as far as this server cares.
#[derive(Debug, Clone, PartialEq)]
pub enum McpRequest {
    Initialize { id: Value },
    Ping { id: Value },
    ToolsList { id: Value },
    ToolsCall { id: Value, name: String, arguments: Value },
    /// A notification (no `id`): acknowledged with 202 and no body.
    Notification,
    /// A method this server does not implement.
    Unknown { id: Value, method: String },
    /// Not JSON-RPC at all.
    Malformed,
}

pub fn parse(message: &Value) -> McpRequest {
    let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
    let id = message.get("id").cloned();

    // A message with no id is a notification, and the specification is explicit
    // that a notification never gets a response.
    let Some(id) = id.filter(|v| !v.is_null()) else {
        return if method.is_empty() {
            McpRequest::Malformed
        } else {
            McpRequest::Notification
        };
    };

    match method {
        "initialize" => McpRequest::Initialize { id },
        "ping" => McpRequest::Ping { id },
        "tools/list" => McpRequest::ToolsList { id },
        "tools/call" => {
            let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let arguments = params
                .get("arguments")
                .cloned()
                .filter(|v| !v.is_null())
                .unwrap_or_else(|| json!({}));
            McpRequest::ToolsCall { id, name, arguments }
        }
        "" => McpRequest::Malformed,
        other => McpRequest::Unknown {
            id,
            method: other.to_string(),
        },
    }
}

pub fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub fn initialize_result(client_version: Option<&str>) -> Value {
    json!({
        "protocolVersion": client_version.unwrap_or(PROTOCOL_VERSION),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The tool list, in MCP's spelling of the same thing the Messages API is sent.
///
/// `inputSchema` rather than `input_schema` is the only difference between the
/// two wire formats, which is exactly why this takes [`Tool`] rather than
/// building its own descriptions: one catalogue, two spellings, no second
/// source of truth.
pub fn tools_list_result(tools: &[Tool]) -> Value {
    let items: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.definition.name,
                "description": tool.definition.description,
                "inputSchema": tool.definition.input_schema,
            })
        })
        .collect();
    json!({ "tools": items })
}

/// A `tools/call` result. MCP reports a tool's own failure as a *successful*
/// JSON-RPC response carrying `isError`, so the model sees it and can correct
/// itself — the same distinction the Messages API draws with `is_error` on a
/// `tool_result` block.
pub fn call_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

// ===========================================================================
// The server
// ===========================================================================

/// Where the CLI should talk, and what it must say to be heard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
}

impl Endpoint {
    /// The `--mcp-config` document. One server, named [`SERVER_NAME`].
    pub fn config_json(&self) -> Value {
        json!({
            "mcpServers": {
                SERVER_NAME: {
                    "type": "http",
                    "url": self.url,
                    "headers": { "Authorization": format!("Bearer {}", self.token) },
                }
            }
        })
    }
}

/// A live MCP server, for the life of one session.
pub struct McpServer {
    endpoint: Endpoint,
    config_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl McpServer {
    /// Bind, write the config file, and start serving.
    ///
    /// `runtime` is the Tokio handle the gate's futures are driven on. The
    /// listener is blocking (tiny_http, already in the tree for the OAuth
    /// loopback) and lives on its own OS threads, so `block_on` here is legal
    /// and cannot stall a runtime worker.
    pub fn start(
        gate: Arc<ToolGate>,
        runtime: tokio::runtime::Handle,
        dir: &Path,
        session_id: &str,
    ) -> Result<McpServer, AgentError> {
        let server = tiny_http::Server::http("127.0.0.1:0")
            .map_err(|e| AgentError::transport(format!("could not open the tool port: {e}")))?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| AgentError::transport("the tool port has no address"))?
            .port();

        let endpoint = Endpoint {
            url: format!("http://127.0.0.1:{port}{PATH}"),
            token: random_token()?,
        };

        let config_path = write_config(dir, session_id, &endpoint)?;

        let server = Arc::new(server);
        let shutdown = Arc::new(AtomicBool::new(false));

        // Two threads, and the reason is not throughput. A `tools/call` can park
        // for as long as the owner takes to read an approval prompt; a second
        // thread means the CLI's `tools/list` or `ping` is still answered while
        // that is happening, instead of looking like a hung server. Actual tool
        // execution is still serialised — the gate holds a lock across the whole
        // call, deliberately.
        let mut workers = Vec::new();
        for _ in 0..2 {
            let server = Arc::clone(&server);
            let shutdown = Arc::clone(&shutdown);
            let gate = Arc::clone(&gate);
            let runtime = runtime.clone();
            let expected = endpoint.token.clone();
            workers.push(std::thread::spawn(move || {
                serve(server, shutdown, gate, runtime, expected);
            }));
        }

        Ok(McpServer {
            endpoint,
            config_path,
            shutdown,
            workers,
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The path to hand to `--mcp-config`.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

impl Drop for McpServer {
    /// Stop listening and take the token off the disk.
    ///
    /// The threads notice within one poll interval; the file goes immediately,
    /// because a bearer token that outlives the session it authenticates is
    /// exactly the kind of thing that turns up in a backup.
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.config_path);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// How often a worker wakes to notice that the session is over.
const POLL: std::time::Duration = std::time::Duration::from_millis(200);

fn serve(
    server: Arc<tiny_http::Server>,
    shutdown: Arc<AtomicBool>,
    gate: Arc<ToolGate>,
    runtime: tokio::runtime::Handle,
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
        if let Some(refusal) = check_headers(&request, &expected_token) {
            let _ = request.respond(refusal);
            continue;
        }
        if request.method() != &tiny_http::Method::Post {
            // GET is how a client opens a server-initiated event stream. This
            // server never initiates anything, and the specification's answer to
            // that is 405.
            let _ = request.respond(text_response(405, "this server only accepts POST"));
            continue;
        }

        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            let _ = request.respond(text_response(400, "unreadable body"));
            continue;
        }

        let message: Value = match serde_json::from_str(&body) {
            Ok(value) => value,
            Err(e) => {
                let payload = error(&Value::Null, -32700, &format!("parse error: {e}"));
                let _ = request.respond(json_response(200, &payload));
                continue;
            }
        };

        match dispatch(&message, &gate, &runtime) {
            Some(response) => {
                let _ = request.respond(json_response(200, &response));
            }
            None => {
                let _ = request.respond(tiny_http::Response::empty(202));
            }
        }
    }
}

/// The JSON-RPC half, with the blocking bridge to the gate.
///
/// `None` means "notification: no response".
fn dispatch(
    message: &Value,
    gate: &Arc<ToolGate>,
    runtime: &tokio::runtime::Handle,
) -> Option<Value> {
    match parse(message) {
        McpRequest::Initialize { id } => {
            let asked = message
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(result(&id, initialize_result(asked.as_deref())))
        }
        McpRequest::Ping { id } => Some(result(&id, json!({}))),
        McpRequest::ToolsList { id } => Some(result(&id, tools_list_result(gate.tools()))),
        McpRequest::ToolsCall { id, name, arguments } => {
            let call_id = mcp_call_id();
            // Hand the call to the runtime and wait on a plain channel rather
            // than calling `block_on` here. `block_on` from a listener thread
            // requires the runtime to be driven by somebody else at that exact
            // moment, which is true in the app and not always true in a test;
            // spawning is correct in both, and the wait is the same wait.
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let spawned = Arc::clone(gate);
            let name_for_task = name.clone();
            let arguments_for_task = arguments.clone();
            runtime.spawn(async move {
                let outcome = spawned
                    .run(&call_id, &name_for_task, &arguments_for_task)
                    .await;
                let _ = tx.send(outcome);
            });
            let outcome = rx.recv().unwrap_or_else(|_| {
                GateResult::Refused("Mach stopped before this could run.".to_string())
            });
            Some(match outcome {
                GateResult::Ok(o) => result(&id, call_result(&o.payload.to_string(), false)),
                GateResult::Refused(message) => result(&id, call_result(&message, true)),
                GateResult::Closed => result(
                    &id,
                    call_result("The owner closed this session. Nothing was done.", true),
                ),
                // A fatal error is fatal for the session, not for the protocol:
                // the brain is told, and the session's own failure path — the
                // child exiting, or the transport erroring — takes it from there.
                GateResult::Fatal(error) => result(&id, call_result(&error.to_string(), true)),
            })
        }
        McpRequest::Notification => None,
        McpRequest::Unknown { id, method } => Some(error(
            &id,
            -32601,
            &format!("{method} is not implemented by Mach's tool server"),
        )),
        McpRequest::Malformed => Some(error(&Value::Null, -32600, "not a JSON-RPC request")),
    }
}

/// The id a gated call is known by when it did not come from a model that mints
/// its own. Opaque everywhere except the round trip through the drawer.
fn mcp_call_id() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("mcp-{:x}", N.fetch_add(1, Ordering::Relaxed))
}

/// `None` when the request may proceed; otherwise the refusal to send.
fn check_headers(
    request: &tiny_http::Request,
    expected: &str,
) -> Option<tiny_http::Response<std::io::Empty>> {
    let mut authorized = false;
    let mut host_ok = false;
    let mut has_origin = false;

    for header in request.headers() {
        let field = header.field.as_str().as_str().to_ascii_lowercase();
        let value = header.value.as_str();
        match field.as_str() {
            "authorization" => {
                if let Some(token) = value.strip_prefix("Bearer ") {
                    authorized = constant_time_eq(token.as_bytes(), expected.as_bytes());
                }
            }
            "host" => {
                let name = value.rsplit_once(':').map(|(h, _)| h).unwrap_or(value);
                host_ok = name == "127.0.0.1" || name == "localhost";
            }
            "origin" => has_origin = true,
            _ => {}
        }
    }

    if has_origin || !host_ok {
        // A browser is the only thing that sends `Origin`, and no browser has
        // any business here. A `Host` that is not loopback means somebody is
        // routing something at us.
        return Some(tiny_http::Response::empty(403));
    }
    if !authorized {
        return Some(tiny_http::Response::empty(401));
    }
    None
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

// ===========================================================================
// Secrets on disk
// ===========================================================================

/// Write the `--mcp-config` document with an owner-only mode.
///
/// The token could have been passed on the command line — `--mcp-config` takes
/// a JSON string as happily as a path — but an argument vector is readable by
/// every process the user runs, is captured by crash reports, and shows up in
/// shell history when someone reproduces a bug by hand. A `0600` file is the
/// smaller surface, and it is deleted the moment the session ends.
fn write_config(dir: &Path, session_id: &str, endpoint: &Endpoint) -> Result<PathBuf, AgentError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AgentError::transport(format!("could not create {}: {e}", dir.display())))?;
    let path = dir.join(format!("mcp-{session_id}.json"));

    // Create with the mode set, rather than writing and then chmod-ing: the
    // window between the two is small, but it is a window in which a token is
    // world-readable.
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
            .map_err(|e| AgentError::transport(format!("could not write {}: {e}", path.display())))?;
        file.write_all(endpoint.config_json().to_string().as_bytes())
            .map_err(|e| AgentError::transport(format!("could not write {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, endpoint.config_json().to_string())
            .map_err(|e| AgentError::transport(format!("could not write {}: {e}", path.display())))?;
    }

    Ok(path)
}

/// 32 bytes of `/dev/urandom`, hex-encoded.
///
/// Straight from the device rather than pulling in `rand`, exactly as
/// `auth::oauth` does for the PKCE verifier — one fewer dependency in the
/// authentication path, and the kernel's CSPRNG is the thing those crates
/// ultimately ask anyway.
fn random_token() -> Result<String, AgentError> {
    let mut bytes = [0u8; 32];
    let mut file = std::fs::File::open("/dev/urandom")
        .map_err(|e| AgentError::transport(format!("open /dev/urandom: {e}")))?;
    file.read_exact(&mut bytes)
        .map_err(|e| AgentError::transport(format!("read /dev/urandom: {e}")))?;
    Ok(hex::encode(bytes))
}

/// Length-checked, non-short-circuiting comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
