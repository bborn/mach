//! The command line: the door, the consent rule, and the read path.
//!
//! **No Google calls and no window.** The store is a real temp SQLite file, the
//! command layer runs against a scripted `HttpTransport` that records every
//! request, and the door's listener is the real one — `door::serve_with`, the
//! same loop the app runs, with the handler swapped for a stub so the four
//! refusals can be asserted against the code that ships rather than against a
//! copy of it.
//!
//! The tests that pin the design claims, rather than the mechanics:
//!
//! * `the_door_refuses_a_request_with_no_token`, `…_a_wrong_token` and
//!   `…_anything_carrying_an_origin` — the three ways in that must not work.
//! * `a_mutation_without_yes_refuses_and_the_mailbox_does_not_move` — and
//!   Google is never called, which is the half that would otherwise be
//!   unrecoverable.
//! * `yes_is_not_enough_to_send` — the flag that authorises every archive on
//!   the machine does not authorise one letter.
//! * `a_read_answers_from_a_store_nothing_can_write_to` — the whole reason
//!   reads need no app.
//! * `every_verb_comes_from_the_tool_surface` — there is no hand-written
//!   vocabulary, and this fails the moment somebody writes one.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use mach_lib::cli::endpoint::{self, Endpoint};
use mach_lib::cli::protocol::{self, Class, Consent, Decision, DoorRequest};
use mach_lib::cli::{args, door, render};
use mach_lib::commands::{AccountClients, CommandDispatcher, GoogleClients};
use mach_lib::db::models::{LabelType, NewAccount, NewLabel, NewMessage, NewThread, Participant};
use mach_lib::db::{queries, Db};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, StaticTokenProvider, TransportError,
};
use mach_lib::ipc::agent::engine::gate::ToolGate;
use mach_lib::ipc::agent::engine::session::{
    ApprovalDesk, ApprovalOutcome, NullEmitter, SessionEmitter, SessionSnapshot, SessionStatus,
    SessionUi,
};
use mach_lib::ipc::agent::engine::tools::{self, ToolContext};
use mach_lib::ipc::compose::engine::outbox::Outbox;

// ===========================================================================
// harness
// ===========================================================================

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "mach-cli-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        TempDir(dir)
    }

    fn store(&self) -> PathBuf {
        mach_lib::config::database_path(&self.0)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Records every request and answers with whatever it was told to answer.
struct Scripted {
    requests: Mutex<Vec<HttpRequest>>,
    answer: Mutex<HttpResponse>,
}

impl Scripted {
    fn new() -> Arc<Self> {
        Arc::new(Scripted {
            requests: Mutex::new(Vec::new()),
            answer: Mutex::new(HttpResponse::json(200, "{}")),
        })
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn answers(&self, response: HttpResponse) {
        *self.answer.lock().unwrap() = response;
    }
}

impl HttpTransport for Scripted {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let answer = self.answer.lock().unwrap().clone();
        Box::pin(async move { Ok(answer) })
    }
}

struct Harness {
    dir: TempDir,
    db: Db,
    google: Arc<Scripted>,
    dispatcher: Arc<CommandDispatcher>,
    outbox: Arc<Outbox>,
    plugins: Arc<mach_lib::plugins::PluginRuntime>,
    thread_id: i64,
}

impl Harness {
    fn new(tag: &str) -> Harness {
        let dir = TempDir::new(tag);
        let db = Db::open(dir.store()).expect("open store");
        let google = Scripted::new();
        let (account_id, thread_id) = seed(&db);
        let clients: Arc<dyn GoogleClients> = Arc::new(
            AccountClients::new(Arc::clone(&google) as Arc<dyn HttpTransport>)
                .with_account(account_id, Arc::new(StaticTokenProvider::new("token"))),
        );
        let dispatcher =
            Arc::new(CommandDispatcher::new(db.clone(), Arc::clone(&clients)).expect("dispatcher"));
        let outbox = Arc::new(Outbox::new(db.clone(), clients).expect("outbox"));
        let plugins = Arc::new(mach_lib::plugins::PluginRuntime::new(
            Arc::new(mach_lib::plugins::PluginStore::new(&dir.0, false)),
            Vec::new(),
        ));
        Harness {
            dir,
            db,
            google,
            dispatcher,
            outbox,
            plugins,
            thread_id,
        }
    }

    /// A gate exactly as the door builds one: the same context, and an approval
    /// answered in advance.
    fn gate(&self) -> Arc<ToolGate> {
        let ui = Arc::new(SessionUi::new(
            "cli-test",
            Arc::new(Mutex::new(SessionSnapshot {
                id: "cli-test".into(),
                title: String::new(),
                status: SessionStatus::Running,
                created_at: 0,
                context: Vec::new(),
                entries: Vec::new(),
                pending: None,
                error: None,
                backend: None,
            })),
            Arc::new(NullEmitter) as Arc<dyn SessionEmitter>,
        ));
        let desk = Arc::new(ApprovalDesk::standing(
            Arc::clone(&ui),
            ApprovalOutcome::Approved,
        ));
        Arc::new(ToolGate::new(
            ToolContext {
                db: self.db.clone(),
                dispatcher: Arc::clone(&self.dispatcher),
                outbox: Arc::clone(&self.outbox),
                plugins: Arc::clone(&self.plugins),
            },
            Vec::new(),
            ui,
            desk,
        ))
    }

    fn labels(&self) -> Vec<String> {
        self.db
            .read(|conn| queries::thread_with_messages(conn, self.thread_id))
            .expect("read")
            .expect("thread")
            .thread
            .label_ids
    }
}

fn seed(db: &Db) -> (i64, i64) {
    let account_id = db
        .write(|conn| {
            queries::upsert_account(
                conn,
                &NewAccount {
                    email: "alex@lumen.example".into(),
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
                    email: "alex@lumen.example".into(),
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

// ===========================================================================
// the door, on a real socket
// ===========================================================================

/// The real listener loop with a stub handler, on an ephemeral port.
struct TestDoor {
    port: u16,
    token: String,
    shutdown: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    seen: Arc<Mutex<Vec<DoorRequest>>>,
}

impl TestDoor {
    fn start() -> TestDoor {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
        let port = server.server_addr().to_ip().expect("ip").port();
        let token = "0123456789abcdef0123456789abcdef".to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<Vec<DoorRequest>>> = Arc::new(Mutex::new(Vec::new()));

        let worker = {
            let server = Arc::clone(&server);
            let shutdown = Arc::clone(&shutdown);
            let token = token.clone();
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                door::serve_with(server, shutdown, token, move |request| {
                    seen.lock().unwrap().push(request);
                    json!({ "ok": true, "summary": "stub", "payload": {} })
                });
            })
        };

        TestDoor {
            port,
            token,
            shutdown,
            worker: Some(worker),
            seen,
        }
    }

    /// A raw request, so a test can leave out a header the CLI would always
    /// send or add one it never would.
    fn raw(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", self.port)).expect("connect to door");
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n", self.port);
        for (field, value) in headers {
            request.push_str(&format!("{field}: {value}\r\n"));
        }
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        stream.write_all(request.as_bytes()).expect("send");

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read");
        let text = String::from_utf8_lossy(&raw).into_owned();
        let status = text
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        (status, body.to_string())
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

impl Drop for TestDoor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
fn the_door_refuses_a_request_with_no_token() {
    let door = TestDoor::start();
    let (status, _) = door.raw("POST", "/cli", &[], r#"{"op":"tools"}"#);
    assert_eq!(status, 401);
    assert!(door.seen.lock().unwrap().is_empty(), "nothing may be handled");
}

#[test]
fn the_door_refuses_a_wrong_token() {
    let door = TestDoor::start();
    let (status, _) = door.raw(
        "POST",
        "/cli",
        &[("Authorization", "Bearer 00000000000000000000000000000000")],
        r#"{"op":"tools"}"#,
    );
    assert_eq!(status, 401);
    assert!(door.seen.lock().unwrap().is_empty());

    // A token of the right shape but the wrong length must not pass either —
    // the comparison is length-checked before it is constant-time.
    let (short, _) = door.raw(
        "POST",
        "/cli",
        &[("Authorization", "Bearer 0123")],
        r#"{"op":"tools"}"#,
    );
    assert_eq!(short, 401);
}

/// A browser cannot read the token, but it can be made to POST blind at a
/// loopback port. Refusing anything that arrives with an `Origin` removes that
/// class outright — even when the token is correct.
#[test]
fn the_door_refuses_anything_carrying_an_origin() {
    let door = TestDoor::start();
    let (status, _) = door.raw(
        "POST",
        "/cli",
        &[
            ("Authorization", &door.bearer()),
            ("Origin", "https://evil.example"),
        ],
        r#"{"op":"tools"}"#,
    );
    assert_eq!(status, 403);
    assert!(door.seen.lock().unwrap().is_empty());
}

#[test]
fn the_door_answers_one_path_and_one_method() {
    let door = TestDoor::start();
    let auth = [("Authorization", door.bearer())];
    let auth: Vec<(&str, &str)> = auth.iter().map(|(k, v)| (*k, v.as_str())).collect();

    // A port that answers on every path is a port that is being scanned. The
    // 404 comes before the token check, so it says nothing about the token.
    assert_eq!(door.raw("POST", "/", &auth, "{}").0, 404);
    assert_eq!(door.raw("POST", "/mcp", &auth, "{}").0, 404);
    assert_eq!(door.raw("GET", "/cli", &auth, "").0, 405);

    let (status, body) = door.raw("POST", "/cli", &auth, r#"{"op":"tools"}"#);
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["ok"],
        json!(true)
    );
    assert_eq!(door.seen.lock().unwrap().len(), 1);
}

#[test]
fn a_body_that_is_not_a_door_request_is_a_named_refusal_rather_than_a_crash() {
    let door = TestDoor::start();
    let auth = [("Authorization", door.bearer())];
    let auth: Vec<(&str, &str)> = auth.iter().map(|(k, v)| (*k, v.as_str())).collect();

    for body in [r#"{"op":"eval","code":"1"}"#, "not json", "{}"] {
        let (status, answer) = door.raw("POST", "/cli", &auth, body);
        assert_eq!(status, 200, "{body}");
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(answer["ok"], json!(false), "{body}");
        assert_eq!(answer["error"]["kind"], json!("badRequest"), "{body}");
    }
    assert!(door.seen.lock().unwrap().is_empty());
}

// ===========================================================================
// consent
// ===========================================================================

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn a_mutation_without_yes_refuses_and_the_mailbox_does_not_move() {
    let harness = Harness::new("consent");
    let runtime = runtime();
    assert!(harness.labels().contains(&"INBOX".to_string()));

    let answer = door::decide_and_run(
        harness.gate(),
        runtime.handle(),
        "archive",
        &json!({ "threadIds": [harness.thread_id] }),
        &Consent::default(),
        None,
    );

    assert_eq!(answer["ok"], json!(false));
    assert_eq!(answer["error"]["kind"], json!("consentRequired"));
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--yes"),
        "{answer}"
    );

    // The two things that make it a refusal rather than a warning.
    assert!(
        harness.labels().contains(&"INBOX".to_string()),
        "the thread left the inbox without consent"
    );
    assert_eq!(harness.google.count(), 0, "Google was called");
}

#[test]
fn the_same_call_with_yes_archives_through_the_command_layer() {
    let harness = Harness::new("consent-yes");
    let runtime = runtime();

    let answer = door::decide_and_run(
        harness.gate(),
        runtime.handle(),
        "archive",
        &json!({ "threadIds": [harness.thread_id] }),
        &Consent {
            mutate: true,
            recipients: None,
        },
        None,
    );

    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["mutated"], json!(true));
    assert!(
        !harness.labels().contains(&"INBOX".to_string()),
        "the archive did not reach the store"
    );
    // Locally first, then Google — the command layer's contract, unchanged by
    // arriving over a socket.
    assert_eq!(harness.google.count(), 1);
}

/// The one that shipped wrong the first time. A write Google refuses is rolled
/// back exactly, and the command layer reports that in its payload — under a
/// field called `ok`, three lines down. Reported as a successful *tool call* it
/// exited 0 and printed "Archived 0 conversations", which is silent failure
/// with a reassuring sentence on it. The envelope has to carry the command
/// layer's verdict.
#[test]
fn a_write_google_refuses_is_a_failure_and_not_a_zero() {
    let harness = Harness::new("refused");
    let runtime = runtime();
    harness.google.answers(HttpResponse::json(
        401,
        r#"{"error":{"code":401,"message":"Invalid Credentials"}}"#,
    ));

    let answer = door::decide_and_run(
        harness.gate(),
        runtime.handle(),
        "archive",
        &json!({ "threadIds": [harness.thread_id] }),
        &Consent {
            mutate: true,
            recipients: None,
        },
        None,
    );

    assert_eq!(answer["ok"], json!(false), "{answer}");
    // The command layer's own kind, so a caller can tell "reconnect the
    // account" from "try again later" without reading English.
    assert_eq!(answer["error"]["kind"], json!("auth"), "{answer}");
    let message = answer["error"]["message"].as_str().unwrap();
    assert!(message.contains(&harness.thread_id.to_string()), "{message}");
    assert!(message.contains("put back"), "{message}");

    // And the store agrees: the optimistic write was reverted exactly.
    assert!(
        harness.labels().contains(&"INBOX".to_string()),
        "a refused archive left the store disagreeing with Gmail"
    );
}

#[test]
fn yes_is_not_enough_to_send() {
    let harness = Harness::new("send");
    let runtime = runtime();
    let recipients = vec!["molly@example.com".to_string()];

    let answer = door::decide_and_run(
        harness.gate(),
        runtime.handle(),
        tools::SEND_TOOL,
        &json!({ "draftId": "d-1" }),
        &Consent {
            mutate: true,
            recipients: None,
        },
        Some(&recipients),
    );

    assert_eq!(answer["ok"], json!(false));
    assert_eq!(answer["error"]["kind"], json!("recipientsRequired"));
    assert_eq!(harness.google.count(), 0);
}

/// There is no environment variable that authorises a send, and the consent
/// shape is what makes that true: `MACH_CLI_YES` can only ever set `mutate`.
#[test]
fn the_environment_can_authorise_a_change_and_never_a_letter() {
    let from_env = Consent {
        mutate: true,
        recipients: None,
    };
    assert_eq!(protocol::decide("archive", &from_env, None), Decision::Run);
    let to = vec!["a@b.test".to_string()];
    assert!(matches!(
        protocol::decide(tools::SEND_TOOL, &from_env, Some(&to)),
        Decision::Refuse(_)
    ));
}

// ===========================================================================
// the read path
// ===========================================================================

#[test]
fn a_read_answers_from_a_store_nothing_can_write_to() {
    let harness = Harness::new("reads");
    let store = harness.dir.store();

    let db = Db::open_read_only(&store).expect("open the store read-only");

    // The same eight verbs the agent has, through the same function.
    let threads = tools::execute_read(&db, "list_threads", &json!({}))
        .expect("list_threads is a read")
        .expect("it runs");
    assert_eq!(threads.payload["threads"].as_array().unwrap().len(), 1);

    let found = tools::execute_read(&db, "search_threads", &json!({ "query": "data room" }))
        .expect("search_threads is a read")
        .expect("it runs");
    assert_eq!(
        found.payload["threads"][0]["subject"],
        json!("Series A data room")
    );

    let detail = tools::execute_read(
        &db,
        "get_thread",
        &json!({ "threadId": harness.thread_id }),
    )
    .expect("get_thread is a read")
    .expect("it runs");
    assert_eq!(detail.payload["subject"], json!("Series A data room"));

    let accounts = tools::execute_read(&db, "list_accounts", &json!({}))
        .expect("list_accounts is a read")
        .expect("it runs");
    assert_eq!(
        accounts.payload["accounts"][0]["email"],
        json!("alex@lumen.example")
    );

    // The property the whole read route rests on: this handle cannot write, and
    // the engine says so rather than a code review saying so.
    let write = db.write(|conn| {
        conn.execute("DELETE FROM threads", [])
            .map_err(mach_lib::db::DbError::from)
    });
    assert!(write.is_err(), "the read-only store accepted a write");
}

#[test]
fn a_missing_store_is_an_error_rather_than_a_new_empty_one() {
    let dir = TempDir::new("absent");
    let store = dir.store();
    assert!(Db::open_read_only(&store).is_err());
    assert!(
        !store.exists(),
        "opening for reading created a store, so an empty inbox would be a lie"
    );
}

#[test]
fn a_read_verb_needs_no_app_and_a_write_verb_does() {
    // The distinction the CLI routes on, asserted rather than assumed.
    assert!(protocol::is_local("search_threads"));
    assert!(protocol::is_local("get_thread"));
    // A Gmail filter lives at Google: a read, and still not answerable locally.
    assert!(!protocol::is_local(tools::LIST_FILTERS_TOOL));
    assert_eq!(protocol::classify(tools::LIST_FILTERS_TOOL), Class::Read);
    assert!(!protocol::is_local("archive"));
}

// ===========================================================================
// the vocabulary is generated
// ===========================================================================

/// The claim the whole design rests on: `mach` has no verb list of its own.
/// Every verb resolves against the tool surface, and every tool in the surface
/// resolves — so a command added to `Command::catalogue` is a `mach` verb on
/// the same day, and a hand-written parallel vocabulary would fail this.
#[test]
fn every_verb_comes_from_the_tool_surface() {
    let names: Vec<String> = tools::tools()
        .iter()
        .map(|t| t.definition.name.clone())
        .collect();
    assert!(names.len() > 15, "the surface looks truncated: {names:?}");

    for name in &names {
        assert_eq!(
            args::resolve_verb(name, &names).expect(name),
            *name,
            "{name} is in the surface but not reachable as a verb"
        );
    }

    // Every command in the catalogue is one of those names, spelled the same.
    for spec in mach_lib::commands::Command::catalogue() {
        assert!(
            names.iter().any(|n| n == spec.kind),
            "{} is in the catalogue and not in the surface",
            spec.kind
        );
    }
}

/// Each verb's flags are its own schema's properties, so `mach help archive`
/// and the model's tool definition cannot disagree.
#[test]
fn a_verbs_flags_are_its_schema() {
    let archive = tools::find("archive").expect("archive is a tool");
    let input = args::build_input(
        &["--threadIds".to_string(), "7,9".to_string()],
        &archive.definition.input_schema,
    )
    .expect("parse");
    assert_eq!(input, json!({ "threadIds": [7, 9] }));

    // And a parameter that is not in the schema is refused rather than sent:
    // every schema in the surface is `additionalProperties: false`, so a typo
    // passed through would be a call that did something else.
    assert!(args::build_input(
        &["--threadId".to_string(), "7".to_string(), "--colour".to_string(), "red".to_string()],
        &archive.definition.input_schema,
    )
    .is_err());
}

// ===========================================================================
// the endpoint file
// ===========================================================================

#[test]
fn the_endpoint_file_is_owner_only_and_round_trips() {
    let dir = TempDir::new("endpoint");
    let written = Endpoint {
        port: 51_234,
        token: "abc".into(),
        pid: std::process::id(),
        version: "0.0.0".into(),
    };
    let path = endpoint::write(&dir.0, &written).expect("write");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a bearer token that can send mail was {mode:o}");
    }

    let read = endpoint::read(&dir.0).expect("read");
    assert_eq!(read, written);
    assert!(read.writer_is_alive(), "this process is alive");
    assert!(read.url().starts_with("http://127.0.0.1:51234/"));
}

/// A hard kill skips `Drop`, so a stale file with a plausible port in it is a
/// normal thing to find. It must read as "not running" rather than as a socket
/// worth talking to.
#[test]
fn a_stale_endpoint_file_is_recognised_by_its_pid() {
    let dir = TempDir::new("stale");
    // A pid that cannot be running: `kill(0, 0)` addresses the process group,
    // so the smallest safely-dead number is one no process was ever given.
    let dead = 0x7FFF_FFFEu32;
    endpoint::write(
        &dir.0,
        &Endpoint {
            port: 51_234,
            token: "abc".into(),
            pid: dead,
            version: "0.0.0".into(),
        },
    )
    .expect("write");
    assert!(!endpoint::read(&dir.0).unwrap().writer_is_alive());
}

/// The identifier is in `tauri.conf.json`, which the bundler reads at build
/// time and nothing reads at run time. The CLI has to know it to find the
/// store, so the copy is checked against the original.
#[test]
fn the_bundle_identifier_still_matches_the_tauri_config() {
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json"))
            .expect("tauri.conf.json"),
    )
    .expect("valid json");
    assert_eq!(
        config["identifier"].as_str().unwrap(),
        endpoint::BUNDLE_IDENTIFIER
    );
}

// ===========================================================================
// output
// ===========================================================================

#[test]
fn a_read_renders_as_a_table_and_as_json() {
    let harness = Harness::new("render");
    let db = Db::open_read_only(harness.dir.store()).expect("open");
    let outcome = tools::execute_read(&db, "list_threads", &json!({}))
        .unwrap()
        .unwrap();

    let human = render::outcome(&outcome.summary, &outcome.payload);
    assert!(human.starts_with("1 conversations"), "{human}");
    assert!(human.contains("Series A data room"), "{human}");
    // The wire convention — a timestamp is unix milliseconds under a `Ms` or
    // `at` key — is what turns a column of integers into a column of dates.
    assert!(!human.contains("1754000000000"), "{human}");

    // And the same outcome, whole, for something that is not a person.
    let machine = json!({
        "ok": true,
        "tool": "list_threads",
        "summary": outcome.summary,
        "payload": outcome.payload,
        "mutated": outcome.mutated,
    });
    assert_eq!(machine["payload"]["threads"].as_array().unwrap().len(), 1);
}
