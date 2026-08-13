//! Behaviour tests for agent sessions (U16).
//!
//! **No Anthropic calls.** Every test drives a scripted [`ModelTransport`] that
//! replays the exact SSE bytes the Messages API would send, against a real
//! SQLite database and the real command layer over a scripted `HttpTransport`.
//!
//! The load-bearing tests are the ones that pin the design claims rather than
//! the mechanics:
//!
//!  * `the_tool_surface_is_the_command_catalogue` — no mail tool is
//!    hand-written. Add a command and the agent can use it.
//!  * `a_tool_call_dispatches_the_typed_command` — the agent's archive is the
//!    keyboard's archive, undo and all.
//!  * `the_approval_gate_holds_an_outbound_action` — the session parks and the
//!    outbox stays empty until the owner says yes. An unsent email is better
//!    than an undone one.
//!  * `reply_to_this_next_tuesday_works_end_to_end` — the sentence from the
//!    spec, from context block to a scheduled outbox row.
//!  * `a_missing_api_key_is_a_typed_error` — not a panic, not a 401 from
//!    Anthropic, not "internal error".

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use mach_lib::commands::{AccountClients, Command, CommandDispatcher, GoogleClients};
use mach_lib::db::models::{
    LabelType, NewAccount, NewCalendar, NewLabel, NewMessage, NewThread, Participant, RsvpStatus,
};
use mach_lib::db::{queries, Db};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, StaticTokenProvider, TransportError,
};
use mach_lib::ipc::agent::engine::config::{AgentConfig, Credential};
use mach_lib::ipc::agent::engine::context::{render, render_for, Audience, ContextItem};
use mach_lib::ipc::agent::engine::error::AgentError;
use mach_lib::ipc::agent::engine::session::{
    AgentEngine, Entry, Input, SessionEmitter, SessionEvent, SessionSnapshot, SessionStatus,
    ToolState,
};
use mach_lib::ipc::agent::engine::tools::{self, ToolContext, ToolPolicy};
use mach_lib::ipc::agent::engine::wire::{
    self, ChunkStream, ModelCall, ModelTransport, SseDecoder, TurnAccumulator,
};
use mach_lib::ipc::compose::engine::outbox::Outbox;
use mach_lib::ipc::IpcError;

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
            "mach-agent-test-{}-{}-{}.sqlite3",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).expect("open temp db");
        TempDb { path, db }
    }
}

impl std::ops::Deref for TempDb {
    type Target = Db;
    fn deref(&self) -> &Db {
        &self.db
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

/// Replays scripted SSE bodies and records every request.
struct FakeModel {
    bodies: Mutex<VecDeque<String>>,
    calls: Mutex<Vec<ModelCall>>,
    /// How many chunks to split each body into — 1 is a single read, higher
    /// values cut SSE frames in half on purpose.
    pieces: usize,
}

impl FakeModel {
    fn new(bodies: Vec<String>) -> Arc<Self> {
        Arc::new(FakeModel {
            bodies: Mutex::new(bodies.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
            pieces: 7,
        })
    }

    fn calls(&self) -> Vec<ModelCall> {
        self.calls.lock().unwrap().clone()
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// Fill in a turn that could not be written until an earlier one had run —
    /// a send needs the draft id the draft tool minted.
    fn replace_next(&self, body: String) {
        let mut bodies = self.bodies.lock().unwrap();
        let slot = bodies
            .iter_mut()
            .find(|b| b.is_empty())
            .expect("no placeholder turn to fill in");
        *slot = body;
    }
}

impl ModelTransport for FakeModel {
    fn send<'a>(&'a self, call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>> {
        self.calls.lock().unwrap().push(call);
        let pieces = self.pieces;
        Box::pin(async move {
            // A placeholder turn is one the test cannot write until an earlier
            // one has run. Wait for it rather than racing it.
            let body = loop {
                let popped = {
                    let mut bodies = self.bodies.lock().unwrap();
                    match bodies.front() {
                        Some(front) if front.is_empty() => None,
                        _ => Some(bodies.pop_front()),
                    }
                };
                match popped {
                    Some(Some(body)) => break body,
                    Some(None) => break sse(&[json!({ "type": "message_stop" })]),
                    None => tokio::time::sleep(Duration::from_millis(2)).await,
                }
            };
            self.stream(body, pieces).await
        })
    }
}

impl FakeModel {
    /// Deliver one body in `pieces` chunks, cutting SSE frames wherever they
    /// happen to fall — the decoder has to cope with that.
    async fn stream(&self, body: String, pieces: usize) -> Result<ChunkStream, AgentError> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            let bytes = body.into_bytes();
            let size = bytes.len().div_ceil(pieces).max(1);
            for chunk in bytes.chunks(size) {
                if tx.send(Ok(chunk.to_vec())).await.is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    }
}

/// Collects everything a session emits.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<SessionEvent>>,
    threads_changed: AtomicUsize,
}

impl Recorder {
    fn events(&self) -> Vec<SessionEvent> {
        self.events.lock().unwrap().clone()
    }

    fn deltas(&self) -> String {
        self.events()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Delta { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn kinds(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|e| {
                serde_json::to_value(e)
                    .unwrap()
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
}

impl SessionEmitter for Recorder {
    fn session_event(&self, event: &SessionEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
    fn threads_changed(&self) {
        self.threads_changed.fetch_add(1, Ordering::SeqCst);
    }
}

/// Answers every Gmail call with 200 `{}` and remembers the requests.
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

struct Harness {
    db: TempDb,
    google: Arc<FakeGoogle>,
    dispatcher: Arc<CommandDispatcher>,
    outbox: Arc<Outbox>,
    plugins: Arc<mach_lib::plugins::PluginRuntime>,
    recorder: Arc<Recorder>,
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
        // A plugin runtime with nowhere to install to and no verified
        // sandbox: every one of these tests is about the core tools, and an
        // empty runtime is what "no plugins installed" actually looks like.
        let plugins = Arc::new(mach_lib::plugins::PluginRuntime::new(
            Arc::new(mach_lib::plugins::PluginStore::new(
                &std::env::temp_dir().join(format!("mach-agent-test-{}", std::process::id())),
                false,
            )),
            Vec::new(),
        ));
        Harness {
            db,
            google,
            dispatcher,
            outbox,
            plugins,
            recorder: Arc::new(Recorder::default()),
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

    fn engine(&self, bodies: Vec<String>) -> (Arc<AgentEngine>, Arc<FakeModel>) {
        let model = FakeModel::new(bodies);
        let engine = Arc::new(
            AgentEngine::new(
                self.db.db.clone(),
                Arc::clone(&self.dispatcher),
                Arc::clone(&self.outbox),
                Arc::clone(&self.plugins),
                Arc::clone(&model) as Arc<dyn ModelTransport>,
                Arc::clone(&self.recorder) as Arc<dyn SessionEmitter>,
            )
            .with_config(test_config()),
        );
        (engine, model)
    }
}

// --------------------------------------------------------------------------
// fixtures
// --------------------------------------------------------------------------

/// One account, one INBOX label, one two-message thread from Tawny.
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
                body_text: Some(
                    "Any chance you can send the data room link before the partner meeting?"
                        .into(),
                ),
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

// --------------------------------------------------------------------------
// SSE authoring
// --------------------------------------------------------------------------

/// The wire format, exactly: `event:` line, `data:` line, blank line.
fn sse(events: &[Value]) -> String {
    let mut out = String::new();
    for event in events {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("message");
        out.push_str(&format!("event: {kind}\ndata: {event}\n\n"));
    }
    out
}

/// A turn that says something and stops.
fn text_turn(text: &str) -> String {
    sse(&[
        json!({ "type": "message_start", "message": { "model": "claude-opus-5" } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } }),
        json!({ "type": "message_stop" }),
    ])
}

/// A turn that calls one tool. Arguments stream as `input_json_delta`, split
/// mid-JSON, because that is how they really arrive.
fn tool_turn(id: &str, name: &str, input: Value) -> String {
    let raw = input.to_string();
    let cut = raw.len() / 2;
    sse(&[
        json!({ "type": "message_start", "message": { "model": "claude-opus-5" } }),
        json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} } }),
        json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": &raw[..cut] } }),
        json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": &raw[cut..] } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" } }),
        json!({ "type": "message_stop" }),
    ])
}

/// Poll until `check` passes or the budget runs out. Sessions run on their own
/// task, so the alternative is a sleep long enough to be flaky.
async fn wait_for(label: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {label}");
}

// ===========================================================================
// the tool surface
// ===========================================================================

#[test]
fn the_tool_surface_is_the_command_catalogue() {
    let tools = tools::command_tools();

    // Every command in the catalogue is a tool, named by its own `kind`. This
    // is the whole claim: nothing here is hand-written per command.
    assert_eq!(tools.len(), Command::catalogue().len());
    for spec in Command::catalogue() {
        let tool = tools
            .iter()
            .find(|t| t.definition.name == spec.kind)
            .unwrap_or_else(|| panic!("no tool for command {}", spec.kind));
        assert!(
            tool.definition.description.starts_with(spec.summary),
            "{} lost its summary",
            spec.kind
        );
    }
}

#[test]
fn parameter_types_become_json_schema() {
    let tools = tools::command_tools();
    let archive = &tools
        .iter()
        .find(|t| t.definition.name == "archive")
        .unwrap()
        .definition
        .input_schema;

    assert_eq!(archive["type"], "object");
    assert_eq!(archive["properties"]["threadIds"]["type"], "array");
    assert_eq!(archive["properties"]["threadIds"]["items"]["type"], "integer");
    assert_eq!(archive["required"], json!(["threadIds"]));
    assert_eq!(archive["additionalProperties"], json!(false));

    let snooze = &tools
        .iter()
        .find(|t| t.definition.name == "snooze")
        .unwrap()
        .definition
        .input_schema;
    assert_eq!(snooze["properties"]["until"]["type"], "integer");
    assert!(snooze["properties"]["until"]["description"]
        .as_str()
        .unwrap()
        .contains("milliseconds"));

    let rsvp = &tools
        .iter()
        .find(|t| t.definition.name == "rsvp")
        .unwrap()
        .definition
        .input_schema;
    assert_eq!(
        rsvp["properties"]["response"]["enum"],
        json!(["accepted", "declined", "tentative", "needsAction"])
    );

    // The optional restore parameter must not be required, or undo becomes
    // impossible to call.
    let unarchive = &tools
        .iter()
        .find(|t| t.definition.name == "unarchive")
        .unwrap()
        .definition
        .input_schema;
    assert_eq!(unarchive["required"], json!(["threadIds"]));
}

#[test]
fn reads_and_compose_sit_beside_the_commands() {
    let names: Vec<String> = tools::tools()
        .into_iter()
        .map(|t| t.definition.name)
        .collect();
    for expected in [
        "list_threads",
        "search_threads",
        "get_thread",
        "list_events",
        "list_labels",
        "list_accounts",
        "draft_reply",
        "draft_message",
        "send_draft",
        "archive",
        "snooze",
        "rsvp",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
}

/// The declaration is the whole contract with the model, so it is pinned.
///
/// `to`, `subject` and `body` are required and there is **no** `threadId`:
/// nothing about this tool can be answered by an existing conversation, which
/// is the point of it existing beside `draft_reply`.
#[test]
fn the_compose_tool_asks_for_what_a_new_message_needs_and_nothing_else() {
    let tool = tools::find("draft_message").expect("draft_message");
    let schema = &tool.definition.input_schema;

    assert_eq!(schema["required"], json!(["to", "subject", "body"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    assert!(
        schema["properties"].get("threadId").is_none(),
        "a new message has no thread: {schema}"
    );
    for optional in ["cc", "bcc", "accountId"] {
        assert!(
            schema["properties"].get(optional).is_some(),
            "missing {optional}"
        );
    }
    assert_eq!(schema["properties"]["to"]["minItems"], json!(1));
    // And the description tells the model which of the two drafting tools this
    // is, because picking the wrong one is the original defect.
    assert!(
        tool.definition.description.contains("draft_reply"),
        "{}",
        tool.definition.description
    );
}

#[test]
fn only_what_touches_another_human_needs_approval() {
    // Reading is free, and so is anything the command layer can take back.
    for auto in [
        "list_threads",
        "search_threads",
        "get_thread",
        "archive",
        "trash",
        "snooze",
        "label",
        // Both drafting tools are free. A draft is a row in the Drafts mailbox
        // that nobody has been told about; `send_draft` below is the gate.
        "draft_reply",
        "draft_message",
    ] {
        assert_eq!(tools::policy_for(auto), ToolPolicy::Auto, "{auto}");
    }
    // Sending mail and answering an invitation are not undoable in the way
    // that matters: the other person has already been told.
    for gated in ["send_draft", "rsvp"] {
        assert_eq!(tools::policy_for(gated), ToolPolicy::Approve, "{gated}");
    }
}

#[test]
fn a_tool_call_becomes_the_typed_command() {
    assert_eq!(
        tools::command_from_call("archive", &json!({ "threadIds": [7, 9] })).unwrap(),
        Command::Archive {
            thread_ids: vec![7, 9]
        }
    );
    assert_eq!(
        tools::command_from_call("markRead", &json!({ "threadIds": [3], "read": false })).unwrap(),
        Command::MarkRead {
            thread_ids: vec![3],
            read: false
        }
    );
    assert_eq!(
        tools::command_from_call("snooze", &json!({ "threadIds": [1], "until": 1_754_000_000_000i64 }))
            .unwrap(),
        Command::Snooze {
            thread_ids: vec![1],
            until: 1_754_000_000_000
        }
    );
    assert_eq!(
        tools::command_from_call("rsvp", &json!({ "eventId": 4, "response": "declined" })).unwrap(),
        Command::Rsvp {
            event_id: 4,
            response: RsvpStatus::Declined,
            // Left off the call, so the organizer is told — the default the
            // whole calendar half of the vocabulary now leans on.
            comment: None,
            notify: None,
        }
    );

    // A malformed call is an invalid-argument error, not a panic: the session
    // hands it back to the model as a tool error and it tries again.
    let error = tools::command_from_call("archive", &json!({ "threadIds": "all" })).unwrap_err();
    assert_eq!(error.kind(), "invalid");
    assert!(error.to_string().contains("archive"));
}

// ===========================================================================
// dispatching
// ===========================================================================

#[tokio::test]
async fn a_tool_call_dispatches_the_typed_command() {
    let harness = Harness::new("dispatch");
    let (_account_id, thread_id) = seed(&harness.db);

    let outcome = tools::execute(
        &harness.tool_context(),
        "archive",
        &json!({ "threadIds": [thread_id] }),
    )
    .await
    .expect("archive ran");

    // It went through the command layer: the local row lost INBOX...
    let labels = harness
        .db
        .read(|conn| queries::thread_summary(conn, thread_id))
        .unwrap()
        .unwrap()
        .label_ids;
    assert!(!labels.contains(&"INBOX".to_string()));

    // ...and Gmail was called by the dispatcher, not by the agent.
    let requests = harness.google.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].url.contains("/messages/"),
        "the agent must reach Gmail through the command layer, not directly: {}",
        requests[0].url
    );

    // The inverse came back, so the agent's archive is as undoable as the
    // keyboard's.
    assert!(outcome.mutated);
    assert_eq!(
        outcome.payload["undo"]["kind"], "unarchive",
        "the command layer must hand back an inverse"
    );
}

#[tokio::test]
async fn read_tools_answer_from_the_local_store() {
    let harness = Harness::new("reads");
    let (_account_id, thread_id) = seed(&harness.db);

    let listed = tools::execute(&harness.tool_context(), "list_threads", &json!({}))
        .await
        .unwrap();
    assert_eq!(listed.payload["threads"][0]["threadId"], thread_id);
    assert_eq!(listed.payload["threads"][0]["subject"], "Series A data room");
    assert!(!listed.mutated);

    let read = tools::execute(
        &harness.tool_context(),
        "get_thread",
        &json!({ "threadId": thread_id }),
    )
    .await
    .unwrap();
    assert!(read.payload["messages"][0]["body"]
        .as_str()
        .unwrap()
        .contains("data room link"));

    // Not one of them went near Google.
    assert!(harness.google.requests().is_empty());
}

// ===========================================================================
// the protocol
// ===========================================================================

#[test]
fn the_decoder_reassembles_frames_split_across_reads() {
    let body = text_turn("hello");
    let mut decoder = SseDecoder::new();
    let mut payloads = Vec::new();
    // One byte at a time — the worst case a socket can produce.
    for byte in body.as_bytes() {
        payloads.extend(decoder.push(&[*byte]));
    }
    assert_eq!(payloads.len(), 6);
    assert!(payloads[0].contains("message_start"));
    assert!(payloads.last().unwrap().contains("message_stop"));
}

#[test]
fn the_accumulator_rebuilds_blocks_for_the_next_request() {
    let mut accumulator = TurnAccumulator::new();
    let events = [
        json!({ "type": "message_start", "message": { "model": "claude-opus-5" } }),
        // A thinking block arrives with empty text (display defaults to
        // omitted) and still has to be echoed back untouched.
        json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
        json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "signature_delta", "signature": "sig-abc" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "content_block_start", "index": 1,
                "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 1,
                "delta": { "type": "text_delta", "text": "on it" } }),
        json!({ "type": "content_block_stop", "index": 1 }),
        json!({ "type": "content_block_start", "index": 2,
                "content_block": { "type": "tool_use", "id": "tu-1", "name": "archive", "input": {} } }),
        json!({ "type": "content_block_delta", "index": 2,
                "delta": { "type": "input_json_delta", "partial_json": "{\"threadI" } }),
        json!({ "type": "content_block_delta", "index": 2,
                "delta": { "type": "input_json_delta", "partial_json": "ds\":[4]}" } }),
        json!({ "type": "content_block_stop", "index": 2 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" } }),
        json!({ "type": "message_stop" }),
    ];
    for event in events {
        accumulator.apply(&event.to_string()).expect("applied");
    }

    let turn = accumulator.finish();
    assert_eq!(turn.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(turn.text(), "on it");
    assert_eq!(turn.content[0]["type"], "thinking");
    assert_eq!(turn.content[0]["signature"], "sig-abc");
    let calls = turn.tool_uses();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "archive");
    assert_eq!(calls[0].input, json!({ "threadIds": [4] }));
}

#[test]
fn a_mid_stream_error_event_is_an_error() {
    let mut accumulator = TurnAccumulator::new();
    let error = accumulator
        .apply(&json!({ "type": "error", "error": { "message": "overloaded" } }).to_string())
        .unwrap_err();
    assert!(error.to_string().contains("overloaded"));
}

#[test]
fn the_request_carries_the_credential_and_the_effort() {
    let config = test_config();
    let call = wire::build_call(
        &config,
        &wire::TurnRequest {
            system: "system".into(),
            messages: vec![wire::user_text("hello")],
            tools: vec![],
        },
        true,
    );

    assert_eq!(call.url, "https://api.anthropic.test/v1/messages");
    assert_eq!(call.headers.get("x-api-key").unwrap(), "test-key");
    assert_eq!(call.headers.get("anthropic-version").unwrap(), "2023-06-01");
    assert!(call
        .headers
        .get("anthropic-beta")
        .unwrap()
        .contains("server-side-fallback"));

    let body: Value = serde_json::from_str(&call.body).unwrap();
    assert_eq!(body["model"], "claude-opus-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["output_config"]["effort"], "medium");
    assert_eq!(body["fallbacks"], "default");
    // budget_tokens is rejected outright on Claude Opus 5; effort is the knob.
    assert!(body.get("thinking").is_none());
    assert!(body.get("temperature").is_none());
}

// ===========================================================================
// configuration
// ===========================================================================

#[test]
fn a_missing_api_key_is_a_typed_error() {
    let error = AgentConfig::from_values(None, None, None, None, None, None)
        .expect_err("no credential means no agent");

    assert!(matches!(error, AgentError::MissingApiKey(_)));
    assert_eq!(error.kind(), "agentNotConfigured");
    let message = error.to_string();
    assert!(message.contains("ANTHROPIC_API_KEY"), "{message}");
    assert!(message.contains(".env.local"), "{message}");

    // It crosses the IPC boundary as the same shape as missing Google
    // credentials — a state the UI renders, not a crash.
    let ipc: IpcError = AgentConfig::from_values(None, None, None, None, None, None)
        .unwrap_err()
        .into();
    assert_eq!(ipc.kind(), "notConfigured");
    assert!(ipc.to_string().contains("ANTHROPIC_API_KEY"));
}

#[test]
fn an_empty_api_key_counts_as_missing() {
    assert!(AgentConfig::from_values(Some("   ".into()), None, None, None, None, None).is_err());
}

#[test]
fn an_oauth_token_is_accepted_when_there_is_no_key() {
    let config =
        AgentConfig::from_values(None, Some("oat-1".into()), None, None, None, None).unwrap();
    assert_eq!(config.credential, Credential::BearerToken("oat-1".into()));

    let call = wire::build_call(
        &config,
        &wire::TurnRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
        },
        false,
    );
    // OAuth goes on Authorization, never x-api-key, and needs its own beta.
    assert_eq!(call.headers.get("authorization").unwrap(), "Bearer oat-1");
    assert!(call.headers.get("x-api-key").is_none());
    assert!(call.headers.get("anthropic-beta").unwrap().contains("oauth"));
}

#[test]
fn defaults_are_the_current_model() {
    let config =
        AgentConfig::from_values(Some("k".into()), None, None, None, None, None).unwrap();
    assert_eq!(config.model, "claude-opus-5");
    assert_eq!(config.messages_url(), "https://api.anthropic.com/v1/messages");
}

// ===========================================================================
// sessions
// ===========================================================================

#[tokio::test]
async fn a_session_streams_tokens_and_finishes() {
    let harness = Harness::new("stream");
    seed(&harness.db);
    let (engine, model) = harness.engine(vec![text_turn("Archived it.")]);

    let session = engine
        .start("what is in my inbox?".into(), vec![])
        .expect("started");
    assert_eq!(session.status, SessionStatus::Running);
    assert_eq!(session.title, "What is in my inbox?");

    wait_for("the session to finish", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    // Tokens arrived as deltas, not as one lump at the end.
    assert_eq!(harness.recorder.deltas(), "Archived it.");
    assert_eq!(model.call_count(), 1);

    let finished = engine.session(&session.id).unwrap();
    assert!(matches!(
        finished.entries.as_slice(),
        [Entry::User { .. }, Entry::Agent { .. }]
    ));
}

#[tokio::test]
async fn the_context_the_owner_was_looking_at_reaches_the_model() {
    let harness = Harness::new("context");
    let (_account_id, thread_id) = seed(&harness.db);
    let (engine, model) = harness.engine(vec![text_turn("ok")]);

    let session = engine
        .start(
            "reply to this next tues".into(),
            vec![ContextItem {
                id: "thread:1".into(),
                kind: "thread".into(),
                label: "Series A data room".into(),
                thread_id: Some(thread_id),
                event_id: None,
                detail: None,
            }],
        )
        .expect("started");

    wait_for("the turn", || model.call_count() == 1).await;

    let body: Value = serde_json::from_str(&model.calls()[0].body).unwrap();
    let first = body["messages"][0]["content"][0]["text"].as_str().unwrap();
    // "this" is resolved from the store, not taken on trust from the frontend.
    assert!(first.contains("<context>"), "{first}");
    assert!(first.contains("Series A data room"), "{first}");
    assert!(first.contains("data room link"), "{first}");
    assert!(first.ends_with("reply to this next tues"), "{first}");

    // The system prompt grounds relative dates, which is what makes "next
    // tues" resolvable at all.
    let system = body["system"][0]["text"].as_str().unwrap();
    assert!(system.contains("unix milliseconds"), "{system}");

    // And the tool list is the catalogue plus reads and compose.
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"archive"));
    assert!(names.contains(&"get_thread"));
    assert!(names.contains(&"send_draft"));

    // The attached line is visible on the session, so it can be shown and
    // removed.
    let snapshot = engine.session(&session.id).unwrap();
    assert_eq!(snapshot.context[0].label, "Series A data room");
    let remaining = engine.remove_context(&session.id, "thread:1").unwrap();
    assert!(remaining.is_empty());
}

#[tokio::test]
async fn a_session_runs_a_tool_then_answers() {
    let harness = Harness::new("toolloop");
    let (_account_id, thread_id) = seed(&harness.db);
    let (engine, model) = harness.engine(vec![
        tool_turn("tu-1", "archive", json!({ "threadIds": [thread_id] })),
        text_turn("Archived."),
    ]);

    let session = engine.start("archive this".into(), vec![]).expect("started");
    wait_for("the session to finish", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    assert_eq!(model.call_count(), 2);

    // The tool result went back in a single user message, as required.
    let second: Value = serde_json::from_str(&model.calls()[1].body).unwrap();
    let results = second["messages"][2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["type"], "tool_result");
    assert_eq!(results[0]["tool_use_id"], "tu-1");

    // The drawer saw the tool run and then succeed, under one id.
    let entries = engine.session(&session.id).unwrap().entries;
    let tool = entries
        .iter()
        .find_map(|e| match e {
            Entry::Tool { id, state, summary, .. } if id == "tu-1" => {
                Some((*state, summary.clone()))
            }
            _ => None,
        })
        .expect("a tool entry");
    assert_eq!(tool.0, ToolState::Ok);
    assert!(tool.1.contains("Archived"));

    // And the mailbox changed under the UI, so it was told.
    assert!(harness.recorder.threads_changed.load(Ordering::SeqCst) >= 1);
}

// ===========================================================================
// approval
// ===========================================================================

/// Puts a real draft in the store, the way the composer would.
async fn seed_draft(harness: &Harness, thread_id: i64) -> String {
    let outcome = tools::execute(
        &harness.tool_context(),
        "draft_reply",
        &json!({ "threadId": thread_id, "body": "sending it over now" }),
    )
    .await
    .expect("drafted");
    outcome.payload["draft"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn the_approval_gate_holds_an_outbound_action() {
    let harness = Harness::new("approval");
    let (_account_id, thread_id) = seed(&harness.db);
    let draft_id = seed_draft(&harness, thread_id).await;

    // Genuinely in the future: the composer treats a past `scheduleAt` as
    // "send now", which would make this test pass for the wrong reason.
    let send_at = mach_lib::ipc::compose::now_ms() + 5 * 86_400_000;
    let (engine, model) = harness.engine(vec![
        tool_turn(
            "tu-send",
            "send_draft",
            json!({ "draftId": draft_id, "sendAt": send_at }),
        ),
        text_turn("Scheduled."),
    ]);

    let session = engine
        .start("reply to this next tues".into(), vec![])
        .expect("started");

    wait_for("the approval prompt", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::AwaitingApproval)
    })
    .await;

    // Nothing has been queued. This is the whole point.
    assert!(
        harness.outbox.list().unwrap().is_empty(),
        "an outbound action ran before it was approved"
    );
    assert_eq!(model.call_count(), 1, "the loop must not run on ahead");

    let pending = engine.session(&session.id).unwrap().pending.expect("pending");
    assert_eq!(pending.tool_use_id, "tu-send");
    assert_eq!(pending.name, "send_draft");
    // The sentence has to name the consequence, or approving it is a guess.
    assert!(pending.summary.contains("Send"), "{}", pending.summary);
    assert!(
        pending.summary.contains("tawny@example.com"),
        "{}",
        pending.summary
    );

    // Now say yes.
    engine
        .send(
            &session.id,
            Input::Approve {
                tool_use_id: "tu-send".into(),
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
    assert_eq!(queued[0].state.as_str(), "holding");
    // Still nothing on the wire: it is scheduled, not sent.
    assert!(harness
        .google
        .requests()
        .iter()
        .all(|r| !r.url.contains("/messages/send")));
}

#[tokio::test]
async fn a_denial_is_reported_to_the_model_and_nothing_is_sent() {
    let harness = Harness::new("denial");
    let (_account_id, thread_id) = seed(&harness.db);
    let draft_id = seed_draft(&harness, thread_id).await;

    let (engine, model) = harness.engine(vec![
        tool_turn("tu-send", "send_draft", json!({ "draftId": draft_id })),
        text_turn("Understood — left it as a draft."),
    ]);

    let session = engine.start("send this".into(), vec![]).expect("started");
    wait_for("the approval prompt", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::AwaitingApproval)
    })
    .await;

    engine
        .send(
            &session.id,
            Input::Deny {
                tool_use_id: "tu-send".into(),
                reason: Some("Not until I have seen the numbers.".into()),
            },
        )
        .unwrap();

    wait_for("the session to finish", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    assert!(harness.outbox.list().unwrap().is_empty(), "a denied send left the app");

    // The model was told why, as a tool error, so it can adjust rather than
    // silently retrying.
    let second: Value = serde_json::from_str(&model.calls()[1].body).unwrap();
    let result = &second["messages"][2]["content"][0];
    assert_eq!(result["is_error"], true);
    assert!(result["content"]
        .as_str()
        .unwrap()
        .contains("Not until I have seen the numbers."));

    let entries = engine.session(&session.id).unwrap().entries;
    assert!(entries.iter().any(|e| matches!(
        e,
        Entry::Tool { state: ToolState::Denied, .. }
    )));
}

#[tokio::test]
async fn reply_to_this_next_tuesday_works_end_to_end() {
    let harness = Harness::new("nexttues");
    let (_account_id, thread_id) = seed(&harness.db);

    // Whatever the model computed from the clock in the system prompt — here,
    // five days out.
    let next_tuesday = mach_lib::ipc::compose::now_ms() + 5 * 86_400_000;

    let (engine, model) = harness.engine(vec![
        tool_turn("tu-read", "get_thread", json!({ "threadId": thread_id })),
        tool_turn(
            "tu-draft",
            "draft_reply",
            json!({
                "threadId": thread_id,
                "body": "sending the data room link over — talk Tuesday.",
            }),
        ),
        // The model does not invent a draft id: it uses the one draft_reply
        // handed back, which is why this turn is scripted from the tool result.
        String::new(), // replaced below
        text_turn("Scheduled your reply for Tuesday morning."),
    ]);

    let session = engine
        .start(
            "reply to this next tues".into(),
            vec![ContextItem {
                id: "thread:1".into(),
                kind: "thread".into(),
                label: "Series A data room".into(),
                thread_id: Some(thread_id),
                event_id: None,
                detail: None,
            }],
        )
        .expect("started");

    // Wait until the draft exists, then script the send with its real id — the
    // same order the model works in.
    wait_for("the draft", || {
        mach_lib::ipc::compose::engine::draft::load_draft_for_thread(&harness.db.db, thread_id)
            .map(|d| d.is_some())
            .unwrap_or(false)
    })
    .await;
    let draft_id =
        mach_lib::ipc::compose::engine::draft::load_draft_for_thread(&harness.db.db, thread_id)
            .unwrap()
            .unwrap()
            .id;
    model.replace_next(tool_turn(
        "tu-send",
        "send_draft",
        json!({ "draftId": draft_id, "sendAt": next_tuesday }),
    ));

    wait_for("the approval prompt", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::AwaitingApproval)
    })
    .await;
    assert!(harness.outbox.list().unwrap().is_empty());

    engine
        .send(
            &session.id,
            Input::Approve {
                tool_use_id: "tu-send".into(),
            },
        )
        .unwrap();

    wait_for("the session to finish", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    // A scheduled reply, in the composer's own outbox, addressed and threaded
    // by the composer — the agent supplied the words and the time.
    let queued = harness.outbox.list().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].send_after, next_tuesday);
    assert_eq!(queued[0].subject, "Re: Series A data room");
    assert_eq!(queued[0].state.as_str(), "holding");
}

// ===========================================================================
// lifecycle
// ===========================================================================

#[tokio::test]
async fn sessions_are_concurrent_and_closable() {
    let harness = Harness::new("concurrent");
    seed(&harness.db);
    let (engine, _model) = harness.engine(vec![text_turn("one"), text_turn("two")]);

    let first = engine.start("first task".into(), vec![]).unwrap();
    let second = engine.start("second task".into(), vec![]).unwrap();

    assert_eq!(engine.sessions().len(), 2);
    assert_ne!(first.id, second.id);
    assert_eq!(first.title, "First task");

    wait_for("both to finish", || {
        engine
            .sessions()
            .iter()
            .all(|s| s.status == SessionStatus::Done)
    })
    .await;

    engine.close(&first.id).unwrap();
    assert_eq!(engine.sessions().len(), 1);
    assert!(harness
        .recorder
        .kinds()
        .contains(&"closed".to_string()));

    // Closing twice is not an error, and sending to a closed session is a
    // typed one rather than a panic.
    engine.close(&first.id).unwrap();
    let error = engine
        .send(&first.id, Input::Message("still there?".into()))
        .unwrap_err();
    assert_eq!(error.kind(), "unknownSession");
}

#[tokio::test]
async fn a_finished_session_takes_another_message() {
    let harness = Harness::new("followup");
    seed(&harness.db);
    let (engine, model) = harness.engine(vec![text_turn("first answer"), text_turn("second answer")]);

    let session = engine.start("a question".into(), vec![]).unwrap();
    wait_for("the first answer", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Done)
    })
    .await;

    engine
        .send(&session.id, Input::Message("and what about tomorrow?".into()))
        .unwrap();

    wait_for("the second answer", || model.call_count() == 2).await;
    wait_for("the second answer to land", || {
        engine
            .session(&session.id)
            .map(|s| s.entries.len())
            .unwrap_or(0)
            == 4
    })
    .await;

    // The conversation continued rather than starting over.
    let body: Value = serde_json::from_str(&model.calls()[1].body).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_transport_failure_fails_the_session_visibly() {
    let harness = Harness::new("failure");
    seed(&harness.db);

    struct Broken;
    impl ModelTransport for Broken {
        fn send<'a>(&'a self, _call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>> {
            Box::pin(async { Err(AgentError::transport("connection refused")) })
        }
    }

    let engine = Arc::new(
        AgentEngine::new(
            harness.db.db.clone(),
            Arc::clone(&harness.dispatcher),
            Arc::clone(&harness.outbox),
            Arc::clone(&harness.plugins),
            Arc::new(Broken) as Arc<dyn ModelTransport>,
            Arc::clone(&harness.recorder) as Arc<dyn SessionEmitter>,
        )
        .with_config(test_config()),
    );

    let session = engine.start("anything".into(), vec![]).unwrap();
    wait_for("the failure", || {
        engine.session(&session.id).map(|s| s.status) == Some(SessionStatus::Failed)
    })
    .await;

    let failed = engine.session(&session.id).unwrap();
    assert!(failed.error.unwrap().contains("connection refused"));
    assert!(harness.recorder.kinds().contains(&"failed".to_string()));
}

// ===========================================================================
// titles
// ===========================================================================

#[test]
fn a_pill_is_titled_by_its_task() {
    use mach_lib::ipc::agent::engine::session::derive_title;

    assert_eq!(derive_title("reply to this next tues"), "Reply to this next tues");
    // A question keeps its question mark; a pill that drops it reads like a
    // claim about what the agent did.
    assert_eq!(
        derive_title("what did tawny say about the data room?"),
        "What did tawny say about the data room?"
    );
    // Several sentences are cut after the first.
    assert_eq!(
        derive_title("archive this. then find the invoice."),
        "Archive this."
    );
    let long = derive_title(
        "archive everything from linkedin and unsubscribe me from the newsletters as well",
    );
    assert!(long.chars().count() <= 50, "{long}");
    assert!(long.ends_with('…'));
    // Cut on a word boundary, not mid-word.
    assert!(!long.contains("newsl…"));
}

/// A snapshot is what the frontend reducer is written against; the key names
/// are part of the contract and a rename here compiles fine.
#[test]
fn the_snapshot_is_camel_case_on_the_wire() {
    let snapshot = SessionSnapshot {
        id: "agent-1".into(),
        title: "Reply".into(),
        status: SessionStatus::AwaitingApproval,
        created_at: 1,
        context: vec![ContextItem {
            id: "thread:1".into(),
            kind: "thread".into(),
            label: "Re: data room".into(),
            thread_id: Some(9),
            event_id: None,
            detail: None,
        }],
        entries: vec![
            Entry::User { text: "hi".into() },
            Entry::Tool {
                id: "tu-1".into(),
                name: "send_draft".into(),
                summary: "Send it".into(),
                state: ToolState::Running,
                artifact: None,
            },
        ],
        pending: None,
        error: None,
        backend: Some("Claude Code".into()),
    };

    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(json["backend"], "Claude Code");
    assert_eq!(json["createdAt"], 1);
    assert_eq!(json["status"], "awaitingApproval");
    assert_eq!(json["context"][0]["threadId"], 9);
    assert_eq!(json["entries"][0]["role"], "user");
    assert_eq!(json["entries"][1]["role"], "tool");
    assert_eq!(json["entries"][1]["state"], "running");
    // Absent rather than null, so the reducer never has to distinguish them.
    assert!(json.get("pending").is_none());

    let event = SessionEvent::Delta {
        session_id: "agent-1".into(),
        text: "hi".into(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "delta");
    assert_eq!(json["sessionId"], "agent-1");
}

// ===========================================================================
// What the agent made
// ===========================================================================
//
// A tool that brings something into being hands back an `Artifact`, and the
// drawer renders it as a button. Before this, `draft_reply` printed a sentence
// and the draft was unreachable from anywhere in the app — which is how the
// owner ended up looking at "Nothing in DRAFT" after being told a reply had
// been drafted.

#[tokio::test]
async fn drafting_a_reply_hands_back_something_to_open() {
    let harness = Harness::new("artifact-draft");
    let (_account_id, thread_id) = seed(&harness.db);

    let outcome = tools::execute(
        &harness.tool_context(),
        "draft_reply",
        &json!({ "threadId": thread_id, "body": "Both tax items are handled." }),
    )
    .await
    .expect("drafted");

    let artifact = outcome.artifact.expect("a draft is a thing, not a sentence");
    match artifact {
        tools::Artifact::Draft {
            draft_id,
            thread_id: on_thread,
            label,
            ..
        } => {
            assert_eq!(
                Some(draft_id.as_str()),
                outcome.payload["draft"]["id"].as_str(),
                "the button has to open the draft the tool actually wrote"
            );
            assert_eq!(on_thread, Some(thread_id));
            assert!(label.starts_with("Re: "), "{label}");
        }
        other => panic!("expected a draft artifact, got {other:?}"),
    }

    // And the list is stale, so the Drafts mailbox repaints without a relaunch.
    assert!(outcome.mutated);
}

#[tokio::test]
async fn a_read_produces_no_artifact_because_it_made_nothing() {
    let harness = Harness::new("artifact-read");
    let (_account_id, thread_id) = seed(&harness.db);

    let outcome = tools::execute(
        &harness.tool_context(),
        "get_thread",
        &json!({ "threadId": thread_id }),
    )
    .await
    .expect("read");
    assert!(outcome.artifact.is_none());
}

/// The seam is not a special case for drafts: a created event carries one too,
/// with the instant the grid has to be scrolled to.
#[test]
fn an_artifact_survives_the_wire_with_its_ids_intact() {
    let entry = Entry::Tool {
        id: "tu-9".into(),
        name: "createEvent".into(),
        summary: "Created “Coffee”".into(),
        state: ToolState::Ok,
        artifact: Some(tools::Artifact::Event {
            event_id: 42,
            start_ms: 1_775_000_000_000,
            label: "Coffee".into(),
        }),
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["artifact"]["kind"], "event");
    assert_eq!(json["artifact"]["eventId"], 42);
    assert_eq!(json["artifact"]["startMs"], 1_775_000_000_000i64);
    assert_eq!(json["artifact"]["label"], "Coffee");

    // A tool line that made nothing does not carry the key at all, rather than
    // carrying a null the frontend would have to test for.
    let plain = serde_json::to_value(Entry::Tool {
        id: "tu-1".into(),
        name: "archive".into(),
        summary: "Archived 3 conversations".into(),
        state: ToolState::Ok,
        artifact: None,
    })
    .unwrap();
    assert!(plain.get("artifact").is_none());
}

// ===========================================================================
// Writing to somebody, rather than answering them
// ===========================================================================
//
// The agent could only compose off an existing thread, so asked to write to
// Molly it replied to a month-old Venmo request and reported that the draft
// had inherited the subject "Re: Molly Swenson requests $288.00". Sending that
// reads as a mistake to the person receiving it. These pin the other half: a
// draft on no conversation, with the subject it was given.

/// A second account, so "which account" is a real question.
fn add_account(db: &Db, email: &str) -> i64 {
    db.write(|conn| {
        queries::upsert_account(
            conn,
            &NewAccount {
                email: email.into(),
                display_name: None,
                token_ref: "keychain".into(),
                colour_index: 1,
            },
        )
    })
    .expect("account")
}

/// How many rows the composer holds — what a refused call must not change.
fn draft_count(db: &Db) -> i64 {
    db.read(|conn| Ok(conn.query_row("SELECT count(*) FROM compose_drafts", [], |row| row.get(0))?))
        .expect("count")
}

/// A complete `draft_message` call, with the fields under test overridden.
fn compose(overrides: Value) -> Value {
    let mut base = json!({
        "to": ["molly.swenson@example.com"],
        "subject": "Thursday",
        "body": "does 4 work?",
    });
    for (key, value) in overrides.as_object().expect("an object") {
        base[key] = value.clone();
    }
    base
}

#[tokio::test]
async fn a_new_message_keeps_the_subject_it_was_given_and_joins_no_conversation() {
    let harness = Harness::new("compose-new");
    let (account_id, thread_id) = seed(&harness.db);

    let outcome = tools::execute(
        &harness.tool_context(),
        "draft_message",
        &json!({
            "to": ["Molly Swenson <molly.swenson@example.com>"],
            "cc": ["sam@example.com"],
            "subject": "Thursday, and the roof quote",
            "body": "quote came in at 4.2 — happy to walk you through it thursday.",
        }),
    )
    .await
    .expect("drafted");

    let draft = &outcome.payload["draft"];

    // The whole point: verbatim, with nothing prefixed to it.
    assert_eq!(draft["subject"], "Thursday, and the roof quote");
    assert_eq!(draft["kind"], "new");

    // Addressed as given, display name and all.
    assert_eq!(draft["to"][0]["email"], "molly.swenson@example.com");
    assert_eq!(draft["to"][0]["name"], "Molly Swenson");
    assert_eq!(draft["cc"][0]["email"], "sam@example.com");
    assert_eq!(draft["accountId"], account_id);

    // Not on Tawny's thread, and Tawny's thread has not acquired a draft.
    assert_ne!(draft["threadId"], json!(thread_id));
    assert!(
        mach_lib::ipc::compose::engine::draft::load_draft_for_thread(&harness.db.db, thread_id)
            .unwrap()
            .is_none(),
        "the existing conversation was left alone"
    );

    // The conversation it was given instead is its own, titled with the same
    // subject and filed under DRAFT — which is the row in the Drafts mailbox.
    let own_thread = draft["threadId"].as_i64().expect("a conversation of its own");
    let summary = harness
        .db
        .read(|conn| queries::thread_summary(conn, own_thread))
        .unwrap()
        .expect("the draft is listed");
    assert_eq!(summary.subject, "Thursday, and the roof quote");
    assert!(summary.label_ids.iter().any(|l| l == "DRAFT"), "{summary:?}");

    // And the message it would build carries that subject and no threading
    // headers, which is the version of "no thread" the recipient sees.
    let preview = mach_lib::ipc::compose::dispatch(
        &harness.db.db,
        &harness.outbox,
        json!({ "op": "preview", "draft": draft }),
    )
    .await
    .expect("previewed");
    let headers = preview["headers"].as_str().unwrap();
    assert!(
        headers.contains("Subject: Thursday, and the roof quote"),
        "{headers}"
    );
    assert!(!headers.contains("In-Reply-To"), "{headers}");
    assert!(!headers.contains("References"), "{headers}");
    assert!(preview["gmailThreadId"].is_null(), "{preview}");

    assert!(outcome.mutated);
}

/// The result has to be openable, the way `draft_reply`'s is: the drawer shows
/// "Open draft" beside it, and pressing it resumes *that* draft by id.
#[tokio::test]
async fn a_new_message_hands_back_a_draft_the_drawer_can_open() {
    let harness = Harness::new("compose-artifact");
    let (account_id, _thread_id) = seed(&harness.db);

    let outcome = tools::execute(
        &harness.tool_context(),
        "draft_message",
        &compose(json!({ "subject": "Coffee?" })),
    )
    .await
    .expect("drafted");

    match outcome.artifact.expect("a draft is a thing, not a sentence") {
        tools::Artifact::Draft {
            draft_id,
            thread_id,
            account_id: on_account,
            label,
        } => {
            assert_eq!(
                Some(draft_id.as_str()),
                outcome.payload["draft"]["id"].as_str(),
                "the button has to open the draft the tool actually wrote"
            );
            assert_eq!(label, "Coffee?", "the button is named for the message");
            assert_eq!(on_account, account_id);
            // The draft has a conversation of its own — the mirror gives it one
            // so it is in the Drafts mailbox — and the button navigates there
            // before resuming it.
            assert!(thread_id.is_some());

            // What "Open draft" does: load by id.
            let resumed =
                mach_lib::ipc::compose::engine::draft::load_draft(&harness.db.db, &draft_id)
                    .unwrap()
                    .expect("the draft is in the store");
            assert_eq!(resumed.subject, "Coffee?");
            assert_eq!(resumed.to[0].email, "molly.swenson@example.com");
        }
        other => panic!("expected a draft artifact, got {other:?}"),
    }
}

#[tokio::test]
async fn the_account_is_the_default_preference_unless_the_call_names_one() {
    let harness = Harness::new("compose-account");
    let (first, _thread_id) = seed(&harness.db);
    let second = add_account(&harness.db, "alex@work.example");

    let account_of = |overrides: Value| async {
        tools::execute(
            &harness.tool_context(),
            "draft_message",
            &compose(overrides),
        )
        .await
        .map(|outcome| outcome.payload["draft"]["accountId"].as_i64().unwrap())
    };

    let set_default = |value: Value| {
        harness
            .db
            .write(|conn| {
                mach_lib::ipc::prefs::set(
                    conn,
                    mach_lib::ipc::prefs::DEFAULT_ACCOUNT_KEY,
                    &value,
                    0,
                )
            })
            .expect("preference written");
    };

    // Nothing chosen: the first account, which is the order the sidebar uses.
    assert_eq!(account_of(json!({})).await.unwrap(), first);

    // The preference, once there is one. This is the case a new message has —
    // no thread, no list scope, nothing else to go on.
    set_default(json!(second));
    assert_eq!(account_of(json!({})).await.unwrap(), second);

    // A caller that knows better wins over it.
    assert_eq!(
        account_of(json!({ "accountId": first })).await.unwrap(),
        first
    );

    // A preference left pointing at a removed account is not fatal: it falls
    // back rather than refusing to write.
    set_default(json!(9_999));
    assert_eq!(account_of(json!({})).await.unwrap(), first);

    // But an account the *call* named and that does not exist is an error.
    // Sending from a different address than the one asked for is the silent
    // substitution this tool exists to avoid.
    let error = account_of(json!({ "accountId": 9_999 }))
        .await
        .expect_err("no such account");
    let message = error.to_string();
    assert!(message.contains("9999"), "{message}");
    assert!(message.contains("alex@example.com"), "{message}");
    assert!(error.is_recoverable_by_model());
}

#[tokio::test]
async fn a_malformed_address_fails_visibly_and_writes_nothing() {
    let harness = Harness::new("compose-address");
    seed(&harness.db);

    // One good draft first, so the count below means something.
    tools::execute(
        &harness.tool_context(),
        "draft_message",
        &compose(json!({})),
    )
    .await
    .expect("drafted");
    assert_eq!(draft_count(&harness.db), 1);

    for bad in [
        json!(["Molly"]),
        json!(["molly at example.com"]),
        json!(["molly@example"]),
        json!(["molly@example..com"]),
        json!([""]),
        json!([]),
        json!([7]),
    ] {
        let error = tools::execute(
            &harness.tool_context(),
            "draft_message",
            &compose(json!({ "to": bad })),
        )
        .await
        .expect_err(&format!("{bad} addresses nobody"));
        assert!(matches!(error, AgentError::Invalid(_)), "{bad}: {error:?}");
        // Reported to the model rather than killing the session, so it can go
        // and find the real address.
        assert!(error.is_recoverable_by_model(), "{bad}");
    }

    // The offender is named, because "invalid recipients" is not a fix.
    let error = tools::execute(
        &harness.tool_context(),
        "draft_message",
        &compose(json!({ "to": ["Molly"] })),
    )
    .await
    .expect_err("not an address");
    assert!(error.to_string().contains("Molly"), "{error}");

    // A cc nobody can deliver to is refused the same way as a to.
    tools::execute(
        &harness.tool_context(),
        "draft_message",
        &compose(json!({ "cc": ["sam@"] })),
    )
    .await
    .expect_err("not an address");

    // Nothing was written by any of it.
    assert_eq!(draft_count(&harness.db), 1);
}

// ===========================================================================
// Naming a calendar
// ===========================================================================
//
// Asked to put eleven recurring "Molly Care" events on a calendar he named —
// "Dad/Ben Schedule" — the agent gave up and wrote him the list to paste into
// Google himself. Three things were wrong, and only the first looked like the
// bug:
//
//  1. There was no `list_calendars`. `createEvent` takes a `calendarId`, its
//     own parameter description said "as returned by list_calendars", and
//     nothing returned one. A name could not become an id.
//  2. `attendees` was advertised as an array of strings against a
//     `Vec<Participant>`, so naming a guest was a deserialize error.
//  3. `recurrence` was advertised as one string against a `Vec<String>`, so
//     "every week" was a second one, on the same call.
//
// `the_original_request_now_succeeds` is the regression: the name, resolved,
// with a guest on it.

/// A calendar row, as `calendarList.list` would have left it.
fn add_calendar(db: &Db, account_id: i64, calendar_id: &str, name: &str, role: &str) {
    db.write(|conn| {
        queries::upsert_calendar(
            conn,
            &NewCalendar {
                account_id,
                calendar_id: calendar_id.into(),
                summary: Some(name.into()),
                access_role: Some(role.into()),
                is_primary: calendar_id == "primary",
                selected: true,
                synced_at: 1_754_000_000_000,
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .expect("calendar");
}

/// One calendar out of a `list_calendars` payload, by the name a person uses.
fn calendar_named<'a>(payload: &'a Value, name: &str) -> &'a Value {
    payload["calendars"]
        .as_array()
        .expect("calendars is an array")
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no calendar named {name} in {payload}"))
}

#[tokio::test]
async fn list_calendars_names_every_calendar_across_accounts() {
    let harness = Harness::new("calendars-list");
    let (first, _thread) = seed(&harness.db);
    let second = add_account(&harness.db, "ben@example.com");

    add_calendar(&harness.db, first, "primary", "Alex", "owner");
    add_calendar(
        &harness.db,
        first,
        "dadben@group.calendar.google.com",
        "Dad/Ben Schedule",
        "writer",
    );
    add_calendar(&harness.db, second, "primary", "Ben", "owner");

    let outcome = tools::execute(&harness.tool_context(), "list_calendars", &json!({}))
        .await
        .expect("listed");

    // It spans accounts, which is what makes "which account owns it" a
    // question with an answer.
    //
    // The second account's primary is listed as its address rather than as
    // "Ben": Google's `summary` for a primary calendar is the account's own
    // email, and `ipc::reads::list_calendars` substitutes the account's display
    // name — which this one does not have. The name the model sees is the name
    // the sidebar shows, which is the point.
    let names: Vec<&str> = outcome.payload["calendars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Alex", "Dad/Ben Schedule", "ben@example.com"]);

    // Every row carries the id `createEvent` takes, beside the account it has
    // to be created on.
    let shared = calendar_named(&outcome.payload, "Dad/Ben Schedule");
    assert_eq!(shared["calendarId"], "dadben@group.calendar.google.com");
    assert_eq!(shared["accountId"], first);
    assert_eq!(shared["accountEmail"], "alex@example.com");
    assert_eq!(shared["primary"], false);
    assert_eq!(calendar_named(&outcome.payload, "Alex")["primary"], true);
    assert_eq!(
        calendar_named(&outcome.payload, "ben@example.com")["accountId"],
        second
    );

    // A read. Nothing to open, nothing to repaint, nothing to approve.
    assert!(!outcome.mutated);
    assert!(outcome.artifact.is_none());
    assert_eq!(tools::policy_for("list_calendars"), ToolPolicy::Auto);

    // Restricting to one account is the same list, filtered.
    let one = tools::execute(
        &harness.tool_context(),
        "list_calendars",
        &json!({ "accountId": second }),
    )
    .await
    .expect("listed");
    assert_eq!(one.payload["calendars"].as_array().unwrap().len(), 1);
    assert_eq!(
        calendar_named(&one.payload, "ben@example.com")["accountId"],
        second
    );
}

#[tokio::test]
async fn a_read_only_calendar_says_so_before_the_write_is_attempted() {
    let harness = Harness::new("calendars-writable");
    let (account_id, _thread) = seed(&harness.db);

    add_calendar(&harness.db, account_id, "primary", "Alex", "owner");
    add_calendar(&harness.db, account_id, "team@example.com", "Team", "writer");
    add_calendar(
        &harness.db,
        account_id,
        "en.usa#holiday@group.v.calendar.google.com",
        "Holidays in United States",
        "reader",
    );
    add_calendar(
        &harness.db,
        account_id,
        "busy@example.com",
        "Sam (free/busy)",
        "freeBusyReader",
    );

    let outcome = tools::execute(&harness.tool_context(), "list_calendars", &json!({}))
        .await
        .expect("listed");

    // The same two role names `Calendar::writable` and `canEditEvent` turn on,
    // and no third opinion about it.
    for (name, writable) in [
        ("Alex", true),
        ("Team", true),
        ("Holidays in United States", false),
        ("Sam (free/busy)", false),
    ] {
        assert_eq!(
            calendar_named(&outcome.payload, name)["writable"],
            json!(writable),
            "{name}"
        );
    }
}

#[tokio::test]
async fn an_account_with_no_calendars_is_an_empty_list_not_an_error() {
    let harness = Harness::new("calendars-empty");
    let (account_id, _thread) = seed(&harness.db);

    // An account exists and its calendars have never been swept. Failing here
    // would have the agent report a broken app rather than "there are no
    // calendars on that account yet".
    let outcome = tools::execute(&harness.tool_context(), "list_calendars", &json!({}))
        .await
        .expect("an empty store is not a failure");
    assert_eq!(outcome.payload["calendars"], json!([]));
    assert_eq!(outcome.summary, "0 calendars");

    let named = tools::execute(
        &harness.tool_context(),
        "list_calendars",
        &json!({ "accountId": account_id }),
    )
    .await
    .expect("still not a failure");
    assert_eq!(named.payload["calendars"], json!([]));
}

/// The sentence from his transcript, end to end: a calendar named in words,
/// resolved to an id, with a recurring event and a guest put on it.
///
/// The guest is a fixture address. Nothing here reaches Google — `FakeGoogle`
/// answers every call and records it — and the assertion is on the request that
/// *would* have been sent, which is the only honest way to test a write whose
/// effect is mail to another person.
#[tokio::test]
async fn the_original_request_now_succeeds() {
    let harness = Harness::new("calendars-regression");
    let (account_id, _thread) = seed(&harness.db);
    add_calendar(&harness.db, account_id, "primary", "Alex", "owner");
    add_calendar(
        &harness.db,
        account_id,
        "dadben@group.calendar.google.com",
        "Dad/Ben Schedule",
        "writer",
    );

    // 1. The name he used, resolved to the id and the account the write needs.
    let listed = tools::execute(&harness.tool_context(), "list_calendars", &json!({}))
        .await
        .expect("listed");
    let target = calendar_named(&listed.payload, "Dad/Ben Schedule");
    assert_eq!(target["writable"], json!(true), "it is his to write to");
    let calendar_id = target["calendarId"].as_str().unwrap().to_string();
    let on_account = target["accountId"].as_i64().unwrap();

    // 2. The event, with a guest and a weekly rule — written the way a model
    // writes it, addresses as bare strings and one RRULE rather than a list.
    let start = 1_775_000_000_000i64;
    let outcome = tools::execute(
        &harness.tool_context(),
        "createEvent",
        &json!({
            "accountId": on_account,
            "calendarId": calendar_id,
            "draft": {
                "title": "Molly Care",
                "startTs": start,
                "endTs": start + 3_600_000,
                "attendees": ["guest@example.com"],
                "recurrence": "RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=11",
                "reminderMinutes": 30,
            },
        }),
    )
    .await
    .expect("the request he actually made");

    assert!(outcome.mutated);
    assert_eq!(outcome.payload["ok"], json!(true));

    // It landed on the calendar he named, not on the primary.
    let event_id = match outcome.artifact {
        Some(tools::Artifact::Event { event_id, .. }) => event_id,
        other => panic!("a created event has something to open: {other:?}"),
    };
    let stored = harness
        .db
        .read(move |conn| queries::events_in_range(conn, start, start + 3_600_000, None))
        .unwrap()
        .into_iter()
        .find(|e| e.id == event_id)
        .expect("the row it made");
    assert_eq!(stored.calendar_id, "dadben@group.calendar.google.com");
    assert_eq!(stored.account_id, on_account);
    assert_eq!(stored.attendees[0].email, "guest@example.com");

    // 3. The request that would have gone to Google: the right calendar, the
    // guest, and one recurring event rather than eleven copies.
    let insert = harness
        .google
        .requests()
        .into_iter()
        .find(|r| r.url.contains("/events"))
        .expect("an insert was prepared");
    assert!(
        insert.url.contains("dadben%40group.calendar.google.com/events")
            || insert.url.contains("dadben@group.calendar.google.com/events"),
        "the insert must name the calendar he asked for: {}",
        insert.url
    );
    let body: Value =
        serde_json::from_slice(insert.body.as_deref().expect("a body")).expect("json");
    assert_eq!(body["summary"], "Molly Care");
    assert_eq!(body["attendees"][0]["email"], "guest@example.com");
    assert_eq!(
        body["recurrence"],
        json!(["RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=11"])
    );

    // And it still asks before it runs, because it mails the guest.
    assert_eq!(tools::policy_for("createEvent"), ToolPolicy::Approve);
}

/// The schema now describes what the command layer actually holds, and takes
/// the singular form of each list anyway.
#[test]
fn the_event_schema_no_longer_describes_a_shape_that_is_refused() {
    let create = tools::command_tools()
        .into_iter()
        .find(|t| t.definition.name == "createEvent")
        .expect("createEvent is a tool")
        .definition
        .input_schema;
    let draft = &create["properties"]["draft"]["properties"];

    // Advertised as the types `EventDraft` holds — a list of guests, of RRULE
    // lines, of reminder offsets.
    assert_eq!(draft["attendees"]["type"], "array");
    assert_eq!(draft["attendees"]["items"]["type"], "object");
    assert_eq!(draft["attendees"]["items"]["required"], json!(["email"]));
    assert_eq!(draft["recurrence"]["type"], "array");
    assert_eq!(draft["recurrence"]["items"]["type"], "string");
    assert_eq!(draft["reminderMinutes"]["type"], "array");
    assert_eq!(draft["reminderMinutes"]["items"]["type"], "integer");

    // Both spellings of each reach the same command, so a model writing the
    // singular does not lose a turn to a serde error.
    let event = |attendees: Value, recurrence: Value, reminders: Value| {
        json!({
            "accountId": 1,
            "calendarId": "c",
            "draft": {
                "title": "Molly Care",
                "startTs": 1i64,
                "endTs": 2i64,
                "attendees": attendees,
                "recurrence": recurrence,
                "reminderMinutes": reminders,
            },
        })
    };
    let plural = tools::command_from_call(
        "createEvent",
        &event(
            json!([{ "email": "guest@example.com" }]),
            json!(["RRULE:FREQ=WEEKLY"]),
            json!([30]),
        ),
    )
    .expect("the documented form");
    let singular = tools::command_from_call(
        "createEvent",
        &event(
            json!(["guest@example.com"]),
            json!("RRULE:FREQ=WEEKLY"),
            json!(30),
        ),
    )
    .expect("what a model writes");
    assert_eq!(plural, singular);

    // A patch is widened by the same rule, so changing an existing event's
    // guest list works the same way as setting one.
    assert_eq!(
        tools::command_from_call(
            "updateEvent",
            &json!({ "eventId": 4, "patch": { "attendees": ["guest@example.com"] } }),
        )
        .expect("a patch"),
        tools::command_from_call(
            "updateEvent",
            &json!({ "eventId": 4, "patch": { "attendees": [{ "email": "guest@example.com" }] } }),
        )
        .expect("a patch"),
    );

    // Something in neither shape is still serde's error to report, rather than
    // a field quietly dropped on the floor.
    let error = tools::command_from_call(
        "createEvent",
        &json!({
            "accountId": 1,
            "calendarId": "c",
            "draft": { "title": "x", "startTs": 1i64, "endTs": 2i64, "attendees": [7] },
        }),
    )
    .unwrap_err();
    assert_eq!(error.kind(), "invalid");
}

/// `createEvent`'s own parameter description has always pointed at
/// `list_calendars`. It now points at something.
#[test]
fn the_tool_the_catalogue_names_exists() {
    let named: Vec<&str> = Command::catalogue()
        .iter()
        .flat_map(|spec| spec.params.iter())
        .filter(|param| param.description.contains("list_calendars"))
        .map(|param| param.name)
        .collect();
    assert!(
        named.contains(&"calendarId"),
        "createEvent still has to say where the id comes from"
    );
    assert!(
        tools::find("list_calendars").is_some(),
        "and the tool it names has to be on the surface"
    );
}

// ===========================================================================
// the clipboard flavour of the context block
// ===========================================================================

/// One more reply on the thread, written the way a mail client writes one: the
/// new sentence, an attribution line, and the whole history quoted underneath.
fn seed_reply(db: &Db, account_id: i64, thread_id: i64, n: i64, history: &str) -> String {
    let new = format!("Reply number {n} - the only sentence here that is new.");
    let body = format!(
        "{new}\n\nOn Tue, 5 Aug 2026 at 15:04 Tawny Chen <tawny@example.com> wrote:\n{history}"
    );
    db.write(|conn| {
        queries::upsert_message(
            conn,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: format!("m-r{n}"),
                from: Participant {
                    name: Some("Tawny Chen".into()),
                    email: "tawny@example.com".into(),
                },
                subject: "Series A data room".into(),
                body_text: Some(body.clone()),
                snippet: new.clone(),
                internal_date: 1_754_000_000_000 + n * 60_000,
                ..Default::default()
            },
        )
    })
    .expect("reply");
    body
}

fn thread_context(thread_id: i64) -> Vec<ContextItem> {
    vec![ContextItem {
        id: format!("thread:{thread_id}"),
        kind: "thread".into(),
        label: "Series A data room".into(),
        thread_id: Some(thread_id),
        event_id: None,
        detail: None,
    }]
}

/// ⌘⌥C on an open conversation puts *the conversation* on the clipboard — every
/// message, not the three the model gets — and puts each sentence there once
/// rather than once per reply that quoted it.
#[test]
fn the_clipboard_gets_the_whole_conversation_with_its_quoted_tails_removed() {
    let temp = TempDb::new("clipcopy");
    let (account_id, thread_id) = seed(&temp.db);

    let mut history = String::from("> Any chance you can send the data room link?");
    for n in 1..=6 {
        let body = seed_reply(&temp.db, account_id, thread_id, n, &history);
        history = body
            .lines()
            .map(|line| format!("> {line}\n"))
            .collect::<String>();
    }

    let clipboard =
        render_for(&temp.db, &thread_context(thread_id), Audience::Clipboard).expect("rendered");

    // Every message, oldest first — the model's three-message window does not
    // apply to a payload nobody can call get_thread against.
    for n in 1..=6 {
        assert!(
            clipboard
                .text
                .contains(&format!("Reply number {n} - the only sentence here that is new.")),
            "message {n} missing:\n{}",
            clipboard.text
        );
    }
    assert!(!clipboard.text.contains("not shown"), "{}", clipboard.text);
    assert!(!clipboard.truncated);

    // And once each. With quotes kept, reply 1's sentence would appear five
    // more times inside the tails of replies 2..6.
    let repeats = clipboard.text.matches("Reply number 1 -").count();
    assert_eq!(repeats, 1, "quoted history came through:\n{}", clipboard.text);
    assert!(
        !clipboard.text.contains("> Any chance you can send"),
        "{}",
        clipboard.text
    );

    // It is addressed by him, not about him: the model's wording would read as
    // a third party describing him to whoever he pastes this to.
    assert!(clipboard.text.contains("This is what I am looking at"));

    // The model's block is unchanged by any of this.
    let model = render(&temp.db, &thread_context(thread_id)).expect("rendered");
    assert!(model.contains("What the owner is looking at right now"));
    assert!(
        model.contains("4 earlier message(s) not shown"),
        "the model still gets three and a pointer at get_thread:\n{model}"
    );
}

/// A conversation too big for the ceiling stops, and says where — in the text
/// and on the flag the toast reads. Silent truncation is the failure mode.
#[test]
fn a_conversation_over_the_ceiling_says_what_it_left_behind() {
    let temp = TempDb::new("clipcap");
    let (account_id, thread_id) = seed(&temp.db);

    // 40 × ~5k characters of new text, which is past the 60k ceiling well
    // before the last message.
    let filler = "x".repeat(5_000);
    for n in 1..=40i64 {
        temp.db
            .write(|conn| {
                queries::upsert_message(
                    conn,
                    &NewMessage {
                        thread_id,
                        account_id,
                        gmail_message_id: format!("m-big{n}"),
                        from: Participant {
                            name: Some("Tawny Chen".into()),
                            email: "tawny@example.com".into(),
                        },
                        subject: "Series A data room".into(),
                        body_text: Some(format!("message {n}\n{filler}")),
                        snippet: format!("message {n}"),
                        internal_date: 1_754_000_000_000 + n * 60_000,
                        ..Default::default()
                    },
                )
            })
            .expect("message");
    }

    let clipboard =
        render_for(&temp.db, &thread_context(thread_id), Audience::Clipboard).expect("rendered");

    assert!(clipboard.truncated, "the toast has to be able to say so");
    assert!(
        clipboard.text.contains("more message(s) left out"),
        "and the text has to say it in place"
    );
    assert!(
        clipboard.text.chars().count() < 70_000,
        "{} characters",
        clipboard.text.chars().count()
    );
    // The block is still well formed, so it parses wherever it is pasted.
    assert!(clipboard.text.ends_with("</context>\n\n"));
}

/// The surfaces that are not a conversation still copy something. A selected
/// event and the listing the frontend attached both come back as text rather
/// than as an empty clipboard.
#[test]
fn the_surfaces_that_are_not_a_conversation_still_copy_something() {
    let temp = TempDb::new("clipsurfaces");
    let (account_id, _thread_id) = seed(&temp.db);

    temp.db
        .write(|conn| {
            queries::upsert_calendar(
                conn,
                &NewCalendar {
                    account_id,
                    calendar_id: "primary".into(),
                    summary: Some("Alex".into()),
                    ..Default::default()
                },
            )
        })
        .expect("calendar");

    let event_id = temp
        .db
        .write(|conn| {
            queries::upsert_event(
                conn,
                &mach_lib::db::models::NewEvent {
                    account_id,
                    calendar_id: "primary".into(),
                    google_event_id: "e-1".into(),
                    title: "Partner meeting".into(),
                    start_ts: 1_754_000_000_000,
                    end_ts: 1_754_003_600_000,
                    location: Some("Room 4".into()),
                    ..Default::default()
                },
            )
        })
        .expect("event");

    let items = vec![
        ContextItem {
            id: format!("event:{event_id}"),
            kind: "event".into(),
            label: "Partner meeting".into(),
            thread_id: None,
            event_id: Some(event_id),
            detail: None,
        },
        ContextItem {
            id: "listing".into(),
            kind: "selection".into(),
            label: "2 events in view".into(),
            thread_id: None,
            event_id: None,
            detail: Some("2 events in view:\n- 09:00 Standup\n- 15:00 Partner meeting".into()),
        },
    ];

    let rendered = render_for(&temp.db, &items, Audience::Clipboard).expect("rendered");
    assert!(rendered.text.contains("Partner meeting"));
    assert!(rendered.text.contains("Room 4"));
    // The listing the frontend attached comes through verbatim, which is what
    // makes "the mailbox I am looking at" copyable at all.
    assert!(rendered.text.contains("2 events in view"));
    assert!(rendered.text.contains("- 09:00 Standup"));
    assert!(!rendered.truncated);
}
