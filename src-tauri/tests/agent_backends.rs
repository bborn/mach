//! Behaviour tests for the swappable brain (U17).
//!
//! **No model is called.** Detection is a pure function over "what is on this
//! machine"; the MCP surface is a pure function over the tool catalogue; and the
//! `command` backend is driven by a shell script, which is a perfectly good
//! stand-in for any third-party agent and a much better one than a mock,
//! because it goes over the real socket with the real token.
//!
//! The load-bearing tests are the ones that pin the security claims:
//!
//!  * `a_plugged_in_agent_cannot_send_without_the_owner` — a backend that calls
//!    `send_draft` directly over MCP is parked by Mach, and a denial leaves the
//!    outbox empty. This is the property that makes "configure any agent you
//!    like" a safe offer.
//!  * `the_tool_server_refuses_a_tool_outside_the_surface` — the MCP server
//!    exposes exactly the command catalogue, and a call for anything else is
//!    refused before it is looked at.
//!  * `the_tool_port_is_shut_to_everything_without_the_token` — an
//!    unauthenticated local port that can archive mail would be a hole, so
//!    there is no unauthenticated request that gets an answer.
//!  * `nothing_installed_says_what_to_do` — the sentence names both remedies,
//!    which is the whole reason this unit exists.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use mach_lib::commands::{AccountClients, Command, CommandDispatcher, GoogleClients};
use mach_lib::db::models::{
    LabelType, NewAccount, NewLabel, NewMessage, NewThread, Participant,
};
use mach_lib::db::{queries, Db};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, StaticTokenProvider, TransportError,
};
use mach_lib::ipc::agent::engine::backend::{
    self, Availability, Backend, BackendChoice, BackendPrefs,
};
use mach_lib::ipc::agent::engine::config::{AgentConfig, Credential};
use mach_lib::ipc::agent::engine::error::AgentError;
use mach_lib::ipc::agent::engine::gate::ToolGate;
use mach_lib::ipc::agent::engine::mcp::{self, McpServer};
use mach_lib::ipc::agent::engine::session::{
    AgentEngine, ApprovalDesk, Entry, Input, NullEmitter, SessionEmitter, SessionEvent,
    SessionSnapshot, SessionStatus, SessionUi, ToolState,
};
use mach_lib::ipc::agent::engine::tools::{self, ToolContext};
use mach_lib::ipc::agent::engine::wire::{ChunkStream, ModelCall, ModelTransport};
use mach_lib::ipc::compose::engine::outbox::Outbox;

// ===========================================================================
// harness
// ===========================================================================

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDb {
    path: PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mach-backend-test-{}-{}-{}.sqlite3",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).expect("open temp db");
        TempDb { path, db }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.path.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(path));
        }
    }
}

/// Answers every Gmail call with 200 and remembers the requests.
struct FakeGoogle {
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeGoogle {
    fn new() -> Arc<Self> {
        Arc::new(FakeGoogle {
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for FakeGoogle {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async { Ok(HttpResponse::json(200, "{\"id\":\"sent-1\"}")) })
    }
}

/// A transport that would panic if anything used it. Every test in this file is
/// about a backend that is *not* the Messages API, and the engine still wants
/// one, so this is the honest shape of "never called".
struct NoModel;

impl ModelTransport for NoModel {
    fn send<'a>(&'a self, _call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>> {
        Box::pin(async { Err(AgentError::transport("no model in this test")) })
    }
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<SessionEvent>>,
    threads_changed: AtomicUsize,
}

impl SessionEmitter for Recorder {
    fn session_event(&self, event: &SessionEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
    fn threads_changed(&self) {
        self.threads_changed.fetch_add(1, Ordering::SeqCst);
    }
}

struct Harness {
    db: TempDb,
    google: Arc<FakeGoogle>,
    dispatcher: Arc<CommandDispatcher>,
    outbox: Arc<Outbox>,
    plugins: Arc<mach_lib::plugins::PluginRuntime>,
    recorder: Arc<Recorder>,
    workspace: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Harness {
        let db = TempDb::new(tag);
        let google = FakeGoogle::new();
        let clients: Arc<dyn GoogleClients> = Arc::new(
            AccountClients::new(Arc::clone(&google) as Arc<dyn HttpTransport>)
                .with_account(1, Arc::new(StaticTokenProvider::new("token"))),
        );
        let dispatcher = Arc::new(
            CommandDispatcher::new(db.db.clone(), Arc::clone(&clients)).expect("dispatcher"),
        );
        let outbox = Arc::new(Outbox::new(db.db.clone(), clients).expect("outbox"));
        let plugins = Arc::new(mach_lib::plugins::PluginRuntime::new(
            Arc::new(mach_lib::plugins::PluginStore::new(
                &std::env::temp_dir().join(format!("mach-backend-test-{}", std::process::id())),
                false,
            )),
            Vec::new(),
        ));
        let workspace = std::env::temp_dir().join(format!(
            "mach-backend-ws-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        Harness {
            db,
            google,
            dispatcher,
            outbox,
            plugins,
            recorder: Arc::new(Recorder::default()),
            workspace,
        }
    }

    fn tool_context(&self) -> ToolContext {
        ToolContext {
            db: self.db.db.clone(),
            dispatcher: Arc::clone(&self.dispatcher),
            outbox: Arc::clone(&self.outbox),
            plugins: Arc::clone(&self.plugins),
        }
    }

    /// A gate with nothing but a null drawer behind it — enough to serve MCP.
    fn bare_gate(&self) -> Arc<ToolGate> {
        let snapshot = Arc::new(Mutex::new(blank_snapshot()));
        let ui = Arc::new(SessionUi::new(
            "agent-test",
            snapshot,
            Arc::new(NullEmitter) as Arc<dyn SessionEmitter>,
        ));
        let desk = Arc::new(ApprovalDesk::new(Arc::clone(&ui)));
        Arc::new(ToolGate::new(self.tool_context(), Vec::new(), ui, desk))
    }

    /// An engine pinned to a `command` backend that runs `script`.
    fn engine_running(&self, script: &str, tag: &str) -> Arc<AgentEngine> {
        let path = write_script(&self.workspace, tag, script);
        Arc::new(
            AgentEngine::new(
                self.db.db.clone(),
                Arc::clone(&self.dispatcher),
                Arc::clone(&self.outbox),
                Arc::clone(&self.plugins),
                Arc::new(NoModel),
                Arc::clone(&self.recorder) as Arc<dyn SessionEmitter>,
            )
            .with_workspace(self.workspace.clone())
            .with_backend(Backend::Command {
                program: path,
                args: Vec::new(),
            }),
        )
    }
}

impl Drop for Harness {
    /// The scratch directory holds the tool-server config and the stand-in
    /// agent scripts. Both are per-run; leaving them in `/tmp` would be litter.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

fn blank_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        id: "agent-test".into(),
        title: "test".into(),
        status: SessionStatus::Running,
        created_at: 0,
        context: Vec::new(),
        entries: Vec::new(),
        pending: None,
        error: None,
        backend: None,
    }
}

fn write_script(dir: &std::path::Path, tag: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("workspace");
    let path = dir.join(format!("agent-{tag}.sh"));
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).expect("script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path
}

fn seed(db: &Db) -> (i64, i64) {
    let account_id = db
        .write(|conn| {
            queries::upsert_account(
                conn,
                &NewAccount {
                    email: "alex@example.com".into(),
                    display_name: Some("Alex".into()),
                    token_ref: "keychain".into(),
                    colour_index: 0,
                },
            )
        })
        .expect("account");

    db.write(|conn| {
        queries::upsert_label(
            conn,
            &NewLabel {
                account_id,
                gmail_label_id: "INBOX".into(),
                name: "Inbox".into(),
                label_type: LabelType::System,
            },
        )
    })
    .expect("label");

    let thread_id = db
        .write(|conn| {
            queries::upsert_thread(
                conn,
                &NewThread {
                    account_id,
                    gmail_thread_id: "t-1".into(),
                    participants: vec![Participant {
                        name: Some("Tawny Chen".into()),
                        email: "tawny@example.com".into(),
                    }],
                    subject: "Series A data room".into(),
                    snippet: "Any chance you can send the data room link?".into(),
                    last_message_at: 1_754_000_000_000,
                    is_unread: true,
                    message_count: 1,
                    has_attachments: false,
                    label_ids: vec!["INBOX".into(), "UNREAD".into()],
                },
            )
        })
        .expect("thread");

    db.write(|conn| {
        queries::upsert_message(
            conn,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: "m-1".into(),
                rfc822_message_id: Some("<m1@example.com>".into()),
                from: Participant {
                    name: Some("Tawny Chen".into()),
                    email: "tawny@example.com".into(),
                },
                to: vec![Participant {
                    name: Some("Alex".into()),
                    email: "alex@example.com".into(),
                }],
                subject: "Series A data room".into(),
                body_text: Some("Any chance you can send the data room link?".into()),
                snippet: "Any chance you can send the data room link?".into(),
                internal_date: 1_754_000_000_000,
                is_unread: true,
                ..Default::default()
            },
        )
    })
    .expect("message");

    (account_id, thread_id)
}

async fn wait_for(label: &str, check: impl FnMut() -> bool) {
    wait_up_to(6, label, check).await
}

/// The same wait with a budget, for the one test that drives a real CLI: a
/// model round trip is seconds, not milliseconds, and a timeout that fires
/// while the child is still thinking is a flaky test rather than a finding.
async fn wait_up_to(seconds: u64, label: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..(seconds * 100) {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {label}");
}

// ===========================================================================
// detection
// ===========================================================================

fn test_config() -> AgentConfig {
    AgentConfig {
        credential: Credential::ApiKey("test-key".to_string()),
        model: "claude-opus-5".to_string(),
        effort: "medium".to_string(),
        max_tokens: 4096,
        base_url: "https://api.anthropic.test".to_string(),
        fallbacks: true,
    }
}

fn with_claude() -> Availability {
    Availability {
        claude: Some(PathBuf::from("/usr/local/bin/claude")),
        api_key: false,
    }
}

fn bare_machine() -> Availability {
    Availability {
        claude: None,
        api_key: false,
    }
}

#[test]
fn auto_prefers_claude_code_when_it_is_installed() {
    // The whole complaint, in one assertion: a machine with Claude Code on it
    // and no API key anywhere must still have a working agent, with nothing
    // configured by anyone.
    let resolved = backend::resolve(&BackendPrefs::default(), &with_claude(), None).expect("resolved");
    assert_eq!(resolved.kind(), "claudeCli");
    assert_eq!(resolved.label(), "Claude Code");

    // And it beats an API key that also happens to be present, because the key
    // costs money the subscription has already paid.
    let both = Availability {
        claude: Some(PathBuf::from("/usr/local/bin/claude")),
        api_key: true,
    };
    let resolved = backend::resolve(&BackendPrefs::default(), &both, Some(&test_config()))
        .expect("resolved");
    assert_eq!(resolved.kind(), "claudeCli");
}

#[test]
fn auto_falls_back_to_the_api_when_there_is_no_cli() {
    let available = Availability {
        claude: None,
        api_key: true,
    };
    let resolved =
        backend::resolve(&BackendPrefs::default(), &available, Some(&test_config())).expect("resolved");
    assert_eq!(resolved.kind(), "anthropicApi");
    assert_eq!(resolved.label(), "Anthropic API (claude-opus-5)");
}

#[test]
fn a_model_preference_reaches_both_backends() {
    let prefs = BackendPrefs {
        model: Some("sonnet".into()),
        ..BackendPrefs::default()
    };
    match backend::resolve(&prefs, &with_claude(), None).expect("resolved") {
        Backend::ClaudeCli { model, .. } => assert_eq!(model.as_deref(), Some("sonnet")),
        other => panic!("expected the CLI, got {}", other.kind()),
    }

    let available = Availability {
        claude: None,
        api_key: true,
    };
    match backend::resolve(&prefs, &available, Some(&test_config())).expect("resolved") {
        Backend::AnthropicApi(config) => assert_eq!(config.model, "sonnet"),
        other => panic!("expected the API, got {}", other.kind()),
    }
}

#[test]
fn nothing_installed_says_what_to_do() {
    let error = backend::resolve(&BackendPrefs::default(), &bare_machine(), None)
        .expect_err("a bare machine has no brain");
    let message = error.to_string();

    // The old sentence named a variable and stopped. This one has to name both
    // ways out, and the cheap one first.
    assert!(message.contains("Claude Code"), "{message}");
    assert!(message.contains("install.sh"), "{message}");
    assert!(message.contains("ANTHROPIC_API_KEY"), "{message}");
    assert_eq!(error.kind(), "agentNotConfigured");
}

#[test]
fn an_explicit_choice_is_never_quietly_substituted() {
    // Asking for the CLI on a machine without one is an error, not a silent
    // fallback to an API key the owner did not choose to spend.
    let prefs = BackendPrefs {
        choice: BackendChoice::ClaudeCli,
        ..BackendPrefs::default()
    };
    let available = Availability {
        claude: None,
        api_key: true,
    };
    let error = backend::resolve(&prefs, &available, Some(&test_config())).expect_err("no cli");
    assert!(error.to_string().contains("MACH_CLAUDE_BIN"), "{error}");

    // And asking for a custom command without giving one says so.
    let prefs = BackendPrefs {
        choice: BackendChoice::Command,
        ..BackendPrefs::default()
    };
    let error = backend::resolve(&prefs, &with_claude(), None).expect_err("no command");
    assert!(error.to_string().contains("Preferences"), "{error}");
}

#[test]
fn an_unreadable_preference_is_auto_rather_than_a_dead_agent() {
    assert_eq!(BackendChoice::parse(None), BackendChoice::Auto);
    assert_eq!(BackendChoice::parse(Some("")), BackendChoice::Auto);
    assert_eq!(BackendChoice::parse(Some("something-else")), BackendChoice::Auto);
    assert_eq!(BackendChoice::parse(Some("claudeCli")), BackendChoice::ClaudeCli);
}

#[test]
fn a_command_line_is_split_the_way_a_person_means_it() {
    let (program, args) = backend::split_command("  my-agent --flag  value ").expect("split");
    assert_eq!(program, PathBuf::from("my-agent"));
    assert_eq!(args, vec!["--flag", "value"]);

    let (program, args) =
        backend::split_command("\"/Applications/My Agent/bin/run\" --mode 'read only'")
            .expect("split");
    assert_eq!(program, PathBuf::from("/Applications/My Agent/bin/run"));
    assert_eq!(args, vec!["--mode", "read only"]);

    assert!(backend::split_command("   ").is_none());
}

#[test]
fn the_backend_preference_is_read_from_the_store() {
    let db = TempDb::new("prefs");
    db.db
        .write(|conn| {
            mach_lib::ipc::prefs::set(conn, "agentBackend", &json!("anthropicApi"), 0)?;
            mach_lib::ipc::prefs::set(conn, "agentModel", &json!("opus"), 0)?;
            mach_lib::ipc::prefs::set(conn, "agentCommand", &json!("/bin/echo hi"), 0)
        })
        .expect("write prefs");

    let prefs = BackendPrefs::load(&db.db);
    assert_eq!(prefs.choice, BackendChoice::AnthropicApi);
    assert_eq!(prefs.model.as_deref(), Some("opus"));
    assert_eq!(prefs.command.as_deref(), Some("/bin/echo hi"));
}

// ===========================================================================
// the MCP surface
// ===========================================================================

#[test]
fn the_mcp_surface_is_exactly_the_command_layer_and_nothing_more() {
    let harness = Harness::new("surface");
    let gate = harness.bare_gate();

    let listed = mcp::tools_list_result(gate.tools());
    let names: Vec<String> = listed["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();

    // Same function, same list: the MCP server does not have a catalogue of its
    // own that could drift from the one the Messages API is sent.
    let expected: Vec<String> = tools::tools_with(&[])
        .into_iter()
        .map(|t| t.definition.name)
        .collect();
    assert_eq!(names, expected);

    // Every command is there…
    for spec in Command::catalogue() {
        assert!(names.iter().any(|n| n == spec.kind), "missing {}", spec.kind);
    }
    // …and nothing that is not a command, a read, or the composer is.
    let allowed_extras = [
        "list_threads",
        "search_threads",
        "get_thread",
        "list_events",
        "list_labels",
        "list_accounts",
        tools::DRAFT_TOOL,
        tools::NEW_DRAFT_TOOL,
        tools::SEND_TOOL,
    ];
    for name in &names {
        let is_command = Command::catalogue().iter().any(|s| s.kind == name);
        assert!(
            is_command || allowed_extras.contains(&name.as_str()),
            "{name} is on the MCP surface and is not part of the command layer"
        );
    }

    // MCP spells the schema `inputSchema`; the Messages API spells it
    // `input_schema`. Same value, and a client that got neither would silently
    // call tools with no arguments.
    let first = &listed["tools"][0];
    assert!(first["inputSchema"].is_object(), "{first}");
    assert!(first["description"].as_str().is_some_and(|d| !d.is_empty()));
}

#[test]
fn the_protocol_answers_only_what_it_implements() {
    assert!(matches!(
        mcp::parse(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })),
        mcp::McpRequest::ToolsList { .. }
    ));
    // A notification has no id, and the specification is explicit that it never
    // gets a response — answering one is how a client ends up waiting forever.
    assert!(matches!(
        mcp::parse(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
        mcp::McpRequest::Notification
    ));
    assert!(matches!(
        mcp::parse(&json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" })),
        mcp::McpRequest::Unknown { .. }
    ));

    let call = mcp::parse(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "archive", "arguments": { "threadIds": [1] } }
    }));
    match call {
        mcp::McpRequest::ToolsCall { name, arguments, .. } => {
            assert_eq!(name, "archive");
            assert_eq!(arguments["threadIds"][0], 1);
        }
        other => panic!("expected a call, got {other:?}"),
    }
}

// ===========================================================================
// the tool port
// ===========================================================================

/// One HTTP request over a raw socket, so the test is not using the same code
/// the server does. Returns `(status, body)`.
fn request(url: &str, method: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("http url");
    let (authority, path) = rest.split_once('/').expect("path");
    let path = format!("/{path}");

    let mut stream = TcpStream::connect(authority).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("timeout");

    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    stream.write_all(request.as_bytes()).expect("write");

    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("").to_string();
    (status, body)
}

async fn call_tool(url: &str, token: &str, name: &str, arguments: Value) -> Value {
    let url = url.to_string();
    let auth = format!("Bearer {token}");
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    })
    .to_string();
    tokio::task::spawn_blocking(move || {
        let (status, body) = request(
            &url,
            "POST",
            &[
                ("Authorization", &auth),
                ("Content-Type", "application/json"),
            ],
            &body,
        );
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<Value>(&body).expect("json response")
    })
    .await
    .expect("request")
}

#[tokio::test]
async fn the_tool_port_is_shut_to_everything_without_the_token() {
    let harness = Harness::new("port");
    let server = McpServer::start(
        harness.bare_gate(),
        tokio::runtime::Handle::current(),
        &harness.workspace,
        "session-1",
    )
    .expect("server");

    let url = server.endpoint().url.clone();
    let token = server.endpoint().token.clone();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert_eq!(token.len(), 64, "32 bytes, hex");

    let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();
    let auth = format!("Bearer {token}");

    let checks = {
        let url = url.clone();
        let auth = auth.clone();
        let list = list.clone();
        tokio::task::spawn_blocking(move || {
            let no_token = request(&url, "POST", &[], &list).0;
            let wrong_token =
                request(&url, "POST", &[("Authorization", "Bearer nope")], &list).0;
            // A browser is the only thing that sends Origin, and no browser has
            // any business here — this is the DNS-rebinding case.
            let from_a_page = request(
                &url,
                "POST",
                &[("Authorization", &auth), ("Origin", "https://evil.example")],
                &list,
            )
            .0;
            let wrong_path = request(
                &url.replace("/mcp", "/nope"),
                "POST",
                &[("Authorization", &auth)],
                &list,
            )
            .0;
            let a_stream = request(&url, "GET", &[("Authorization", &auth)], "").0;
            let good = request(&url, "POST", &[("Authorization", &auth)], &list);
            (no_token, wrong_token, from_a_page, wrong_path, a_stream, good)
        })
        .await
        .expect("checks")
    };

    assert_eq!(checks.0, 401, "an unauthenticated call was answered");
    assert_eq!(checks.1, 401, "a wrong token was answered");
    assert_eq!(checks.2, 403, "a request from a web page was answered");
    assert_eq!(checks.3, 404);
    assert_eq!(checks.4, 405);
    assert_eq!(checks.5 .0, 200);

    let listed: Value = serde_json::from_str(&checks.5 .1).expect("json");
    assert!(listed["result"]["tools"].as_array().unwrap().len() > 5);

    // The token is on disk for the CLI to read, and only for its owner.
    let config = std::fs::read_to_string(server.config_path()).expect("config file");
    assert!(config.contains(&token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(server.config_path())
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the token file is readable by others");
    }

    let path = server.config_path().to_path_buf();
    drop(server);
    assert!(!path.exists(), "the token outlived the session");
}

#[tokio::test]
async fn the_tool_server_refuses_a_tool_outside_the_surface() {
    let harness = Harness::new("outside");
    let server = McpServer::start(
        harness.bare_gate(),
        tokio::runtime::Handle::current(),
        &harness.workspace,
        "session-2",
    )
    .expect("server");
    let endpoint = server.endpoint().clone();

    // The names a coding agent would reach for by reflex.
    for name in ["Bash", "Read", "WebFetch", "mcp__mach__archive"] {
        let response = call_tool(&endpoint.url, &endpoint.token, name, json!({})).await;
        assert_eq!(response["result"]["isError"], true, "{name} was not refused");
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not one of Mach's tools"), "{text}");
    }

    // Nothing reached Google, because nothing ran.
    assert!(harness.google.requests().is_empty());
}

// ===========================================================================
// a plugged-in agent
// ===========================================================================

/// The `command` backend contract, exercised by a shell script: read the
/// message on stdin, use the MCP server named in the environment, print an
/// answer. Any agent that can do that is a Mach backend.
#[tokio::test]
async fn a_plugged_in_agent_acts_through_the_command_layer() {
    let harness = Harness::new("plugged");
    let (_account, thread_id) = seed(&harness.db.db);

    let engine = harness.engine_running(
        &format!(
            r#"
read -r PROMPT
/usr/bin/curl -s -X POST "$MACH_MCP_URL" \
  -H "Authorization: Bearer $MACH_MCP_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"archive","arguments":{{"threadIds":[{thread_id}]}}}}}}' \
  > /dev/null
printf 'archived it'
"#
        ),
        "archive",
    );

    let session = engine
        .start("archive this".into(), vec![])
        .expect("started");

    wait_for("the plugged-in agent to finish", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    // It went through the command layer, which means Gmail was called with the
    // same request the keyboard would have made.
    let requests = harness.google.requests();
    let modified = requests
        .iter()
        .find(|r| r.url.contains("/modify"))
        .unwrap_or_else(|| panic!("the archive did not reach the command layer: {requests:?}"));
    let body = String::from_utf8_lossy(modified.body.as_deref().unwrap_or_default()).to_string();
    assert!(body.contains("\"removeLabelIds\":[\"INBOX\"]"), "{body}");

    let snapshot = engine.session(&session.id).unwrap();
    assert!(
        snapshot.entries.iter().any(|e| matches!(
            e,
            Entry::Tool { name, state: ToolState::Ok, .. } if name == "archive"
        )),
        "the drawer did not show the tool call: {:?}",
        snapshot.entries
    );
    assert!(snapshot
        .entries
        .iter()
        .any(|e| matches!(e, Entry::Agent { text } if text == "archived it")));
    // The session says which brain answered.
    assert_eq!(snapshot.backend.as_deref(), Some("agent-archive.sh"));
}

/// The one that matters. A backend calls `send_draft` straight down the MCP
/// socket — no model, no permission prompt of its own, nothing between it and
/// the mailbox except Mach — and Mach parks it on the owner anyway.
#[tokio::test]
async fn a_plugged_in_agent_cannot_send_without_the_owner() {
    let harness = Harness::new("nosend");
    let (_account, thread_id) = seed(&harness.db.db);

    let draft = tools::execute(
        &harness.tool_context(),
        "draft_reply",
        &json!({ "threadId": thread_id, "body": "sending it over now" }),
    )
    .await
    .expect("drafted");
    let draft_id = draft.payload["draft"]["id"].as_str().unwrap().to_string();

    let engine = harness.engine_running(
        &format!(
            r#"
read -r PROMPT
RESULT=$(/usr/bin/curl -s -X POST "$MACH_MCP_URL" \
  -H "Authorization: Bearer $MACH_MCP_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"send_draft","arguments":{{"draftId":"{draft_id}"}}}}}}')
printf '%s' "$RESULT" > "$PWD/send-result.json"
printf 'done'
"#
        ),
        "send",
    );

    let session = engine.start("send it".into(), vec![]).expect("started");

    wait_for("the approval prompt", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::AwaitingApproval)
    })
    .await;

    // Nothing has been queued. This is the whole point.
    assert!(
        harness.outbox.list().unwrap().is_empty(),
        "a backend sent mail without the owner"
    );

    let pending = engine.session(&session.id).unwrap().pending.expect("pending");
    assert_eq!(pending.name, "send_draft");
    // The id was minted by Mach — the caller never supplied one — and the
    // sentence still names the consequence.
    assert!(pending.tool_use_id.starts_with("mcp-"), "{pending:?}");
    assert!(pending.summary.contains("tawny@example.com"), "{pending:?}");

    engine
        .send(
            &session.id,
            Input::Deny {
                tool_use_id: pending.tool_use_id.clone(),
                reason: Some("Not until I have seen the numbers.".into()),
            },
        )
        .unwrap();

    wait_for("the session to finish", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    assert!(
        harness.outbox.list().unwrap().is_empty(),
        "a denied send left the app"
    );

    // The refusal was reported to the backend as a tool error, with the reason,
    // so an agent can say what it would have done instead of retrying blindly.
    let raw = std::fs::read_to_string(harness.workspace.join("send-result.json")).expect("result");
    let response: Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("declined"), "{text}");
    assert!(text.contains("Not until I have seen the numbers."), "{text}");

    let entries = engine.session(&session.id).unwrap().entries;
    assert!(entries
        .iter()
        .any(|e| matches!(e, Entry::Tool { state: ToolState::Denied, .. })));
}

#[tokio::test]
async fn approving_lets_exactly_one_message_through() {
    let harness = Harness::new("approve");
    let (_account, thread_id) = seed(&harness.db.db);

    let draft = tools::execute(
        &harness.tool_context(),
        "draft_reply",
        &json!({ "threadId": thread_id, "body": "here it is" }),
    )
    .await
    .expect("drafted");
    let draft_id = draft.payload["draft"]["id"].as_str().unwrap().to_string();
    let send_at = mach_lib::ipc::compose::now_ms() + 5 * 86_400_000;

    let engine = harness.engine_running(
        &format!(
            r#"
read -r PROMPT
/usr/bin/curl -s -X POST "$MACH_MCP_URL" \
  -H "Authorization: Bearer $MACH_MCP_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"send_draft","arguments":{{"draftId":"{draft_id}","sendAt":{send_at}}}}}}}' > /dev/null
printf 'scheduled'
"#
        ),
        "approve",
    );

    let session = engine
        .start("send this next tuesday".into(), vec![])
        .expect("started");

    wait_for("the approval prompt", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::AwaitingApproval)
    })
    .await;

    let pending = engine.session(&session.id).unwrap().pending.expect("pending");
    engine
        .send(
            &session.id,
            Input::Approve {
                tool_use_id: pending.tool_use_id,
            },
        )
        .unwrap();

    wait_for("the session to finish", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    let queued = harness.outbox.list().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].send_after, send_at);
    // Scheduled, not sent: nothing on the wire yet.
    assert!(harness
        .google
        .requests()
        .iter()
        .all(|r| !r.url.contains("/messages/send")));
}

// ===========================================================================
// the real thing
// ===========================================================================

/// The default backend, end to end, against the actual `claude` on this machine.
///
/// Ignored by default: it spawns a real CLI, calls a real model, and spends the
/// owner's subscription. Run it deliberately —
///
/// ```text
/// cargo test --test agent_backends -- --ignored the_real_cli
/// ```
///
/// — when changing anything about the child process's arguments, because the
/// flags are the security boundary and nothing else checks that they are still
/// accepted by the CLI that is installed.
///
/// Read-only on purpose: it asks a question, and the only tools it can reach
/// that would change anything are gated anyway.
#[tokio::test]
#[ignore = "spawns the real Claude Code CLI and spends the owner's subscription"]
async fn the_real_cli_answers_from_the_local_store() {
    let Some(exe) = backend::find_claude() else {
        panic!("no claude executable found — set MACH_CLAUDE_BIN");
    };

    let harness = Harness::new("real");
    seed(&harness.db.db);

    let engine = Arc::new(
        AgentEngine::new(
            harness.db.db.clone(),
            Arc::clone(&harness.dispatcher),
            Arc::clone(&harness.outbox),
            Arc::clone(&harness.plugins),
            Arc::new(NoModel),
            Arc::clone(&harness.recorder) as Arc<dyn SessionEmitter>,
        )
        .with_workspace(harness.workspace.clone())
        .with_backend(Backend::ClaudeCli { exe, model: None }),
    );

    let session = engine
        .start(
            "what is in my inbox? name the sender and the subject.".into(),
            vec![],
        )
        .expect("started");

    wait_up_to(60, "the CLI to answer", || {
        matches!(
            engine.session(&session.id).map(|s| s.status),
            Some(SessionStatus::Done) | Some(SessionStatus::Failed)
        )
    })
    .await;

    let snapshot = engine.session(&session.id).unwrap();
    for entry in &snapshot.entries {
        println!("{entry:?}");
    }
    assert_eq!(snapshot.status, SessionStatus::Done, "{:?}", snapshot.error);

    // It used Mach's tools rather than answering from nothing…
    assert!(
        snapshot.entries.iter().any(|e| matches!(
            e,
            Entry::Tool { state: ToolState::Ok, .. }
        )),
        "the CLI answered without calling a tool: {:?}",
        snapshot.entries
    );
    // …and the answer contains what only the local store could have told it.
    let answer = snapshot
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Agent { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(answer.contains("Tawny"), "{answer}");
    assert!(answer.contains("data room"), "{answer}");

    // A follow-up resumes the same CLI conversation rather than starting a new
    // one, which is the only thing `--resume` is load-bearing for.
    engine
        .send(
            &session.id,
            Input::Message("who was that from? one word.".into()),
        )
        .expect("sent");

    let before = snapshot.entries.len();
    wait_up_to(60, "the follow-up", || {
        engine
            .session(&session.id)
            .map(|s| s.status == SessionStatus::Done && s.entries.len() > before + 1)
            .unwrap_or(false)
    })
    .await;

    let snapshot = engine.session(&session.id).unwrap();
    for entry in &snapshot.entries {
        println!("{entry:?}");
    }
    let last = snapshot
        .entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Agent { text } => Some(text.clone()),
            _ => None,
        })
        .expect("a second answer");
    // It answered from the conversation, without being told again who "that" is.
    assert!(last.contains("Tawny"), "{last}");
}

#[tokio::test]
async fn a_backend_that_fails_fails_the_session_visibly() {
    let harness = Harness::new("fails");
    let engine = harness.engine_running("echo 'no model configured' >&2\nexit 3\n", "fail");

    let session = engine.start("do something".into(), vec![]).expect("started");

    wait_for("the failure", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Failed)
    })
    .await;

    let error = engine.session(&session.id).unwrap().error.expect("error");
    assert!(error.contains("no model configured"), "{error}");
}
