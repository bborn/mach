//! Behaviour tests for "the agent has already written the reply".
//!
//! Two layers, and they fail in different ways.
//!
//! The **engine** tests drive a real [`SyncEngine`] against a small fake Gmail, a
//! real SQLite file and a **scripted model transport that records every request**
//! — so "a backfill must not write suggestions" and "the preference off means
//! nothing generates" are checked by running the code the app runs and finding
//! that the transport was never asked. Reading the gate is not the same claim.
//!
//! The **store** tests call [`suggest::plan`] and [`suggest::generate`] directly
//! on a hand-seeded database. Between them they cover the whole path from an
//! arrival to a row without a clock or a network anywhere near it.
//!
//! What is not here, and cannot be: anything that reaches Google. This module
//! has no Gmail client, and the fake one in this file answers `404` to every
//! route a draft or a send would use — see `fake gmail: no route`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use mach_lib::db::models::NewAccount;
use mach_lib::db::{queries, Db};
use mach_lib::google::types::encode_base64url;
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, RetryPolicy, StaticTokenProvider,
    TokenProvider, TransportError,
};
use mach_lib::ipc::agent::engine::config::{AgentConfig, Credential};
use mach_lib::ipc::agent::engine::error::AgentError;
use mach_lib::ipc::agent::engine::wire::{ChunkStream, ModelCall, ModelTransport};
use mach_lib::ipc::prefs;
use mach_lib::suggest::{self, store, Headers, Stance};
use mach_lib::sync::{SyncConfig, SyncEngine, TransportClients};

const GMAIL_BASE: &str = "https://gmail.test/gmail/v1";
const CALENDAR_BASE: &str = "https://calendar.test/calendar/v3";
const ME: &str = "bruno@example.com";

// ===========================================================================
// A scripted model
// ===========================================================================

/// Answers every request with the same body and remembers what it was asked.
///
/// This is the observable the engine tests turn on: a suggestion that was never
/// generated is a transport that was never called, and that is a fact about the
/// code rather than about a row that might have been deleted by something else.
struct ScriptedModel {
    body: String,
    calls: Mutex<Vec<ModelCall>>,
}

impl ScriptedModel {
    fn new(stances: &[(&str, &str)]) -> Arc<Self> {
        let items: Vec<Value> = stances
            .iter()
            .map(|(label, body)| json!({ "label": label, "body": body }))
            .collect();
        let text = serde_json::to_string(&items).unwrap();
        Arc::new(ScriptedModel {
            body: json!({ "content": [ { "type": "text", "text": text } ] }).to_string(),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<ModelCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl ModelTransport for ScriptedModel {
    fn send<'a>(&'a self, call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>> {
        self.calls.lock().unwrap().push(call);
        let body = self.body.clone();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tx.send(Ok(body.into_bytes())).await.ok();
            Ok(rx)
        })
    }
}

fn test_config() -> AgentConfig {
    AgentConfig {
        credential: Credential::ApiKey("test-key".into()),
        model: "claude-opus-5".into(),
        effort: "medium".into(),
        max_tokens: 32_000,
        base_url: "https://api.anthropic.test".into(),
        fallbacks: true,
    }
}

// ===========================================================================
// A fake Gmail, cut down to what suggestions need
// ===========================================================================

#[derive(Debug, Clone)]
struct FakeMessage {
    id: String,
    thread_id: String,
    labels: Vec<String>,
    subject: String,
    from: String,
    to: String,
    body: String,
    /// Extra headers, for the exclusion cases.
    headers: Vec<(String, String)>,
}

impl FakeMessage {
    /// A person, writing to him, about something.
    fn human(id: &str, from_name: &str) -> Self {
        Self {
            id: id.into(),
            thread_id: format!("t-{id}"),
            labels: vec!["INBOX".into(), "UNREAD".into()],
            subject: format!("Lunch on Tuesday ({id})"),
            from: format!(
                "{from_name} <{}@example.org>",
                from_name.to_ascii_lowercase()
            ),
            to: format!("Bruno <{ME}>"),
            body: "Are you free on Tuesday? I could do two o'clock at the usual place.".into(),
            headers: Vec::new(),
        }
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn to_json(&self) -> Value {
        let mut headers = vec![
            json!({ "name": "Subject", "value": self.subject }),
            json!({ "name": "From", "value": self.from }),
            json!({ "name": "To", "value": self.to }),
        ];
        for (name, value) in &self.headers {
            headers.push(json!({ "name": name, "value": value }));
        }
        json!({
            "id": self.id,
            "threadId": self.thread_id,
            "labelIds": self.labels,
            "snippet": "snippet",
            "internalDate": "1700000000000",
            "payload": {
                "mimeType": "text/plain",
                "headers": headers,
                "body": {
                    "size": self.body.len(),
                    "data": encode_base64url(self.body.as_bytes()),
                },
            }
        })
    }

    fn stub(&self) -> Value {
        json!({ "id": self.id, "threadId": self.thread_id, "labelIds": self.labels })
    }
}

#[derive(Default)]
struct Mailbox {
    email: String,
    messages: BTreeMap<String, FakeMessage>,
    history: Vec<(u64, Value)>,
    history_id: u64,
}

impl Mailbox {
    fn new(email: &str) -> Self {
        Self {
            email: email.into(),
            history_id: 1000,
            ..Default::default()
        }
    }

    fn seed(&mut self, message: FakeMessage) {
        self.history_id += 1;
        self.messages.insert(message.id.clone(), message);
    }

    fn deliver(&mut self, message: FakeMessage) {
        self.history_id += 1;
        self.history.push((
            self.history_id,
            json!({
                "id": self.history_id.to_string(),
                "messages": [ message.stub() ],
                "messagesAdded": [ { "message": message.to_json() } ],
            }),
        ));
        self.messages.insert(message.id.clone(), message);
    }
}

struct FakeGmail {
    accounts: Mutex<HashMap<String, Mailbox>>,
}

impl FakeGmail {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            accounts: Mutex::new(HashMap::new()),
        })
    }

    fn install(&self, token: &str, mailbox: Mailbox) {
        self.accounts.lock().unwrap().insert(token.into(), mailbox);
    }

    fn with<T>(&self, token: &str, f: impl FnOnce(&mut Mailbox) -> T) -> T {
        let mut accounts = self.accounts.lock().unwrap();
        f(accounts.get_mut(token).expect("unknown token"))
    }

    fn handle(&self, request: &HttpRequest) -> HttpResponse {
        let token = request
            .header("Authorization")
            .unwrap_or_default()
            .trim_start_matches("Bearer ")
            .to_string();

        let url = url::Url::parse(&request.url).expect("valid url");
        let query: HashMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let segments: Vec<String> = url
            .path_segments()
            .map(|s| s.map(str::to_string).collect())
            .unwrap_or_default();

        if !url.path().starts_with("/gmail/") {
            return HttpResponse::json(200, r#"{"items":[]}"#);
        }

        let mut accounts = self.accounts.lock().unwrap();
        let Some(mailbox) = accounts.get_mut(&token) else {
            return HttpResponse::json(401, r#"{"error":{"message":"no such token"}}"#);
        };

        let tail: Vec<&str> = segments.iter().skip(4).map(|s| s.as_str()).collect();
        match tail.as_slice() {
            ["profile"] => HttpResponse::json(
                200,
                json!({
                    "emailAddress": mailbox.email,
                    "historyId": mailbox.history_id.to_string(),
                })
                .to_string(),
            ),
            ["labels"] => HttpResponse::json(
                200,
                json!({ "labels": [
                    { "id": "INBOX", "name": "INBOX", "type": "system" },
                    { "id": "UNREAD", "name": "UNREAD", "type": "system" },
                ] })
                .to_string(),
            ),
            ["messages"] => {
                let page: Vec<Value> = mailbox
                    .messages
                    .values()
                    .map(|m| json!({ "id": m.id, "threadId": m.thread_id }))
                    .collect();
                HttpResponse::json(200, json!({ "messages": page }).to_string())
            }
            ["messages", id] => match mailbox.messages.get(*id) {
                Some(message) => HttpResponse::json(200, message.to_json().to_string()),
                None => HttpResponse::json(404, r#"{"error":{"message":"not found"}}"#),
            },
            ["history"] => {
                let start: u64 = query
                    .get("startHistoryId")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let records: Vec<Value> = mailbox
                    .history
                    .iter()
                    .filter(|(id, _)| *id > start)
                    .map(|(_, record)| record.clone())
                    .collect();
                HttpResponse::json(
                    200,
                    json!({
                        "history": records,
                        "historyId": mailbox.history_id.to_string(),
                    })
                    .to_string(),
                )
            }
            other => HttpResponse::json(
                404,
                json!({"error": {"message": format!("fake gmail: no route {other:?}")}})
                    .to_string(),
            ),
        }
    }
}

impl HttpTransport for FakeGmail {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        Box::pin(async move { Ok(self.handle(&request)) })
    }
}

// ===========================================================================
// harness
// ===========================================================================

fn mail_config() -> SyncConfig {
    SyncConfig {
        message_batch_size: 10,
        list_page_size: 100,
        history_page_size: 100,
        request_concurrency: 4,
        backfill_fetch_concurrency: 4,
        poll_interval: Duration::from_secs(3600),
        sync_mail: true,
        sync_calendar: false,
        ..Default::default()
    }
}

fn new_engine(db: &Db, gmail: Arc<FakeGmail>, model: Option<Arc<ScriptedModel>>) -> SyncEngine {
    let clients = TransportClients::new(gmail, |account| {
        Arc::new(StaticTokenProvider::new(format!("tok-{}", account.id))) as Arc<dyn TokenProvider>
    })
    .with_base_urls(GMAIL_BASE, CALENDAR_BASE)
    .with_retry_policy(RetryPolicy::none());
    let engine = SyncEngine::new(db.clone(), Arc::new(clients), mail_config()).expect("engine");
    if let Some(model) = model {
        engine.set_suggest_transport(model);
    }
    engine
}

/// The agent needs a credential to get as far as a request, and every request
/// in this file is intercepted by [`ScriptedModel`] — so this is what makes the
/// path reachable without a network. Set once for the whole binary.
fn configure_agent() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        }
        // Never the agent's own base URL. Nothing in this file reaches a
        // network — the transport is scripted — and a host that cannot resolve
        // is the belt to that braces.
        std::env::set_var("MACH_AGENT_BASE_URL", "https://api.anthropic.invalid");
    });
}

fn add_account(db: &Db, email: &str) -> i64 {
    configure_agent();
    db.write(|conn| {
        queries::upsert_account(
            conn,
            &NewAccount {
                email: email.into(),
                display_name: Some("Bruno".into()),
                token_ref: email.into(),
                colour_index: 0,
            },
        )
    })
    .expect("account")
}

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mach-suggest-test-{}-{}/mach.sqlite3",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        Self { path }
    }

    fn open(&self) -> Db {
        Db::open(&self.path).expect("open db")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Every suggestion row on disk, however stale.
fn rows(db: &Db) -> i64 {
    db.read(|conn| {
        Ok(conn
            .query_row("SELECT count(*) FROM reply_suggestions", [], |r| r.get(0))
            .unwrap_or(0))
    })
    .expect("count")
}

/// Not a Gmail draft anywhere, ever. Asserted by looking at the whole store
/// rather than at the feature's own tables: the rule is "nothing this feature
/// does creates a draft", and a draft would land in `messages` with `is_draft`.
fn draft_rows(db: &Db) -> i64 {
    db.read(|conn| {
        Ok(conn
            .query_row("SELECT count(*) FROM messages WHERE is_draft = 1", [], |r| {
                r.get(0)
            })
            .unwrap_or(0))
    })
    .expect("count")
}

fn set_pref(db: &Db, key: &str, value: Value) {
    db.write(|conn| prefs::set(conn, key, &value, 0)).expect("pref");
}

/// Let the task `consider` spawned finish. It is deliberately detached — the
/// sync pass does not wait on a model — so a test has to.
async fn settle() {
    for _ in 0..40 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ===========================================================================
// The engine: what a first sync must not do
// ===========================================================================

/// The failure the whole gate is arranged around. A new account's backfill
/// stores a year of mail, every message of it an "arrival" as far as the store
/// is concerned. If any of that reached the model, adding a mailbox would cost
/// tens of thousands of requests.
#[tokio::test]
async fn a_first_sync_of_a_full_mailbox_writes_nothing() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, ME);

    let gmail = FakeGmail::new();
    let mut mailbox = Mailbox::new(ME);
    for n in 0..30 {
        mailbox.seed(FakeMessage::human(&format!("m{n}"), &format!("Sender{n}")));
    }
    gmail.install(&format!("tok-{account_id}"), mailbox);

    let model = ScriptedModel::new(&[("Say you'll be there", "Tuesday works.")]);
    let engine = new_engine(&db, Arc::clone(&gmail), Some(Arc::clone(&model)));
    let pass = engine.sync_once().await;
    settle().await;

    assert!(pass.account(account_id).unwrap().is_ok(), "{pass:?}");
    assert_eq!(pass.messages_written(), 30, "the backfill must still store it all");
    assert!(
        model.calls().is_empty(),
        "a backfill asked a model {} times",
        model.calls().len()
    );
    assert_eq!(rows(&db), 0);
    assert_eq!(draft_rows(&db), 0);
}

/// The whole path, once: a synced account, a person writing to him, one call,
/// one row, and no draft anywhere.
#[tokio::test]
async fn a_human_message_on_a_synced_account_gets_stances() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, ME);
    let token = format!("tok-{account_id}");

    let gmail = FakeGmail::new();
    gmail.install(&token, Mailbox::new(ME));

    let model = ScriptedModel::new(&[
        ("Say you'll be there", "Tuesday works. Two o'clock."),
        ("Ask for a raincheck", "Can we push it to next week?"),
    ]);
    let engine = new_engine(&db, Arc::clone(&gmail), Some(Arc::clone(&model)));

    // First pass: establishes the watermark. Nothing has arrived yet.
    engine.sync_once().await;
    settle().await;
    assert!(model.calls().is_empty());

    gmail.with(&token, |m| m.deliver(FakeMessage::human("new1", "Kate")));
    engine.sync_once().await;
    settle().await;

    assert_eq!(model.calls().len(), 1, "one message, one call");
    assert_eq!(rows(&db), 1);
    assert_eq!(draft_rows(&db), 0, "a suggestion is never a draft");

    let thread_id = db
        .read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT thread_id FROM messages WHERE gmail_message_id = 'new1'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap())
        })
        .unwrap();
    let found = db
        .read(|conn| store::fresh_for_thread(conn, thread_id))
        .unwrap()
        .expect("stances");
    assert_eq!(found.stances.len(), 2);
    assert_eq!(found.stances[0].label, "Say you'll be there");

    // And the counter that makes the hit rate measurable moved with it.
    let counters = db.read(|conn| store::counters(conn)).unwrap();
    assert_eq!(counters.suggested, 1);
}

/// The preference means *nothing generates*, not *nothing displays*. Checked at
/// the transport, because the difference between the two is a bill.
#[tokio::test]
async fn the_preference_off_means_no_model_call_at_all() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, ME);
    let token = format!("tok-{account_id}");
    set_pref(&db, suggest::ENABLED_KEY, json!(false));

    let gmail = FakeGmail::new();
    gmail.install(&token, Mailbox::new(ME));
    let model = ScriptedModel::new(&[("Say yes", "Yes.")]);
    let engine = new_engine(&db, Arc::clone(&gmail), Some(Arc::clone(&model)));

    engine.sync_once().await;
    gmail.with(&token, |m| m.deliver(FakeMessage::human("new1", "Kate")));
    engine.sync_once().await;
    settle().await;

    assert!(
        model.calls().is_empty(),
        "the preference is off and the model was asked anyway"
    );
    assert_eq!(rows(&db), 0, "the preference is off and a row was written");
}

/// An engine that was never given a transport writes nothing — the default
/// everywhere except the running app.
#[tokio::test]
async fn an_engine_with_no_model_writes_nothing() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, ME);
    let token = format!("tok-{account_id}");

    let gmail = FakeGmail::new();
    gmail.install(&token, Mailbox::new(ME));
    let engine = new_engine(&db, Arc::clone(&gmail), None);

    engine.sync_once().await;
    gmail.with(&token, |m| m.deliver(FakeMessage::human("new1", "Kate")));
    engine.sync_once().await;
    settle().await;

    assert_eq!(rows(&db), 0);
}

/// The exclusions, through the engine rather than through the predicate: mail
/// that arrives on the live path and is passed over for each of the reasons the
/// owner's inbox is actually full of.
#[tokio::test]
async fn list_bulk_and_no_reply_mail_is_passed_over() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, ME);
    let token = format!("tok-{account_id}");

    let gmail = FakeGmail::new();
    gmail.install(&token, Mailbox::new(ME));
    let model = ScriptedModel::new(&[("Say yes", "Yes.")]);
    let engine = new_engine(&db, Arc::clone(&gmail), Some(Arc::clone(&model)));
    engine.sync_once().await;

    let mut newsletter = FakeMessage::human("bulk1", "Product");
    newsletter = newsletter.header("List-Unsubscribe", "<https://example.org/u/1>");

    let mut robot = FakeMessage::human("bulk2", "Robot");
    robot.from = format!("no-reply@linear.app");

    let mut vacation = FakeMessage::human("bulk3", "Away");
    vacation = vacation.header("Auto-Submitted", "auto-replied");

    let mut announcement = FakeMessage::human("bulk4", "Announce");
    announcement.to = "everyone@example.org".into();

    let mut updates = FakeMessage::human("bulk5", "Statements");
    updates.labels.push("CATEGORY_UPDATES".into());

    gmail.with(&token, |m| {
        for message in [newsletter, robot, vacation, announcement, updates] {
            m.deliver(message);
        }
    });
    engine.sync_once().await;
    settle().await;

    assert!(
        model.calls().is_empty(),
        "bulk mail reached the model: {} calls",
        model.calls().len()
    );
    assert_eq!(rows(&db), 0);
}

// ===========================================================================
// The store side: plan and generate, directly
// ===========================================================================

/// A hand-seeded store with one thread, one incoming message, and some of his
/// own Sent mail to the same person.
fn seeded() -> (TempDb, Db, i64) {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, ME);

    db.write_background(mach_lib::db::sync_queries::ensure_schema)
        .expect("sync schema");
    db.write(|conn| {
        conn.execute(
            "INSERT INTO threads (id, account_id, gmail_thread_id, subject)
             VALUES (1, ?1, 't1', 'Lunch on Tuesday')",
            [account_id],
        )?;
        // His own past reply, in the same thread — the voice example.
        conn.execute(
            "INSERT INTO messages
                 (id, thread_id, account_id, gmail_message_id, from_email, to_json,
                  subject, body_text, internal_date)
             VALUES (10, 1, ?1, 'old', ?2, '[]', 'Lunch',
                     'Tuesday''s fine by me. I''ll come to you — two o''clock, same as last time.',
                     1000)",
            rusqlite::params![account_id, ME],
        )?;
        conn.execute(
            "INSERT INTO messages
                 (id, thread_id, account_id, gmail_message_id, from_name, from_email, to_json,
                  subject, body_text, snippet, internal_date)
             VALUES (11, 1, ?1, 'incoming', 'Kate', 'kate@example.org', ?2,
                     'Lunch on Tuesday', 'Are you free on Tuesday?', 'Are you free', 2000)",
            rusqlite::params![
                account_id,
                serde_json::to_string(&json!([{ "email": ME }])).unwrap()
            ],
        )?;
        conn.execute(
            "INSERT INTO sync_message_labels (account_id, gmail_message_id, label_ids)
             VALUES (?1, 'incoming', ?2)",
            rusqlite::params![account_id, json!(["INBOX", "UNREAD"]).to_string()],
        )?;
        Ok(())
    })
    .expect("seed");

    (temp, db, account_id)
}

#[tokio::test]
async fn generating_writes_a_row_and_a_counter_and_nothing_else() {
    let (_temp, db, account_id) = seeded();

    let jobs = db
        .read(|conn| {
            suggest::plan(
                conn,
                account_id,
                &["incoming".to_string()],
                &HashMap::from([("incoming".to_string(), Headers::default())]),
            )
        })
        .unwrap();
    assert_eq!(jobs.len(), 1, "the message should have earned a suggestion");
    let job = &jobs[0];
    assert_eq!(job.correspondent, "Kate <kate@example.org>");

    let model = ScriptedModel::new(&[
        ("Say you'll be there", "Tuesday works — two o'clock."),
        ("Push it to next week", "Could we do the following Tuesday?"),
    ]);
    let stances = suggest::generate(
        &db,
        model.as_ref(),
        &test_config(),
        "claude-sonnet-5",
        job,
        7,
    )
    .await;

    assert_eq!(stances.len(), 2);
    assert_eq!(rows(&db), 1);
    assert_eq!(draft_rows(&db), 0);

    // His own past reply reached the prompt — the whole point of the feature.
    let call = &model.calls()[0];
    assert!(
        call.body.contains("Replies he has written before"),
        "no voice section in the prompt"
    );
    assert!(
        call.body.contains("same as last time"),
        "his own Sent mail did not reach the prompt"
    );
    // And it went out on the cheap model, not the agent's.
    assert!(
        call.body.contains("claude-sonnet-5"),
        "the request named the wrong model"
    );
    assert!(!call.body.contains("claude-opus-5"), "the agent's model was used");
}

#[tokio::test]
async fn a_model_that_answers_with_nothing_usable_writes_no_row() {
    let (_temp, db, account_id) = seeded();
    let jobs = db
        .read(|conn| {
            suggest::plan(
                conn,
                account_id,
                &["incoming".to_string()],
                &HashMap::new(),
            )
        })
        .unwrap();

    struct Refusing;
    impl ModelTransport for Refusing {
        fn send<'a>(&'a self, _call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>> {
            Box::pin(async move {
                let (tx, rx) = tokio::sync::mpsc::channel(4);
                tx.send(Ok(
                    br#"{"content":[{"type":"text","text":"I can't help with that."}]}"#.to_vec(),
                ))
                .await
                .ok();
                Ok(rx)
            })
        }
    }

    let stances =
        suggest::generate(&db, &Refusing, &test_config(), "claude-sonnet-5", &jobs[0], 7).await;
    assert!(stances.is_empty());
    assert_eq!(rows(&db), 0);
    let counters = db.read(|conn| store::counters(conn)).unwrap();
    assert_eq!(counters.suggested, 0, "nothing was suggested, so nothing counts");
}

#[tokio::test]
async fn the_same_message_is_not_paid_for_twice() {
    let (_temp, db, account_id) = seeded();
    let arrived = vec!["incoming".to_string()];

    let jobs = db
        .read(|conn| suggest::plan(conn, account_id, &arrived, &HashMap::new()))
        .unwrap();
    let model = ScriptedModel::new(&[("Say yes", "Yes.")]);
    suggest::generate(&db, model.as_ref(), &test_config(), "m", &jobs[0], 7).await;

    // A replayed history window reports the same id again.
    let again = db
        .read(|conn| suggest::plan(conn, account_id, &arrived, &HashMap::new()))
        .unwrap();
    assert!(again.is_empty(), "the same message was planned twice");
}

#[tokio::test]
async fn the_preference_off_plans_nothing() {
    let (_temp, db, account_id) = seeded();
    set_pref(&db, suggest::ENABLED_KEY, json!(false));
    let jobs = db
        .read(|conn| {
            suggest::plan(conn, account_id, &["incoming".to_string()], &HashMap::new())
        })
        .unwrap();
    assert!(jobs.is_empty());
}

#[tokio::test]
async fn a_pass_never_plans_more_than_the_cap() {
    let (_temp, db, account_id) = seeded();

    // Twenty more of the same shape, each in its own conversation.
    db.write(|conn| {
        for n in 0..20 {
            let thread = 100 + n;
            conn.execute(
                "INSERT INTO threads (id, account_id, gmail_thread_id) VALUES (?1, ?2, ?3)",
                rusqlite::params![thread, account_id, format!("t{thread}")],
            )?;
            conn.execute(
                "INSERT INTO messages
                     (id, thread_id, account_id, gmail_message_id, from_name, from_email,
                      to_json, subject, body_text, internal_date)
                 VALUES (?1, ?2, ?3, ?4, 'Kate', 'kate@example.org', ?5, 'Hello', 'Hello?', 3000)",
                rusqlite::params![
                    thread,
                    thread,
                    account_id,
                    format!("bulk{n}"),
                    serde_json::to_string(&json!([{ "email": ME }])).unwrap()
                ],
            )?;
            conn.execute(
                "INSERT INTO sync_message_labels (account_id, gmail_message_id, label_ids)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    account_id,
                    format!("bulk{n}"),
                    json!(["INBOX", "UNREAD"]).to_string()
                ],
            )?;
        }
        Ok(())
    })
    .expect("seed many");

    let arrived: Vec<String> = (0..20).map(|n| format!("bulk{n}")).collect();
    let jobs = db
        .read(|conn| suggest::plan(conn, account_id, &arrived, &HashMap::new()))
        .unwrap();
    assert_eq!(jobs.len(), suggest::MAX_PER_PASS);
}

/// Picking a stance reads only from local storage. Proved by taking the model
/// away entirely — no transport, no config, no network — and finding the whole
/// text still there.
#[tokio::test]
async fn picking_a_stance_needs_no_model() {
    let (_temp, db, account_id) = seeded();
    db.write(|conn| {
        store::save(
            conn,
            account_id,
            1,
            11,
            "incoming",
            &[Stance {
                label: "Say you'll be there".into(),
                body: "Tuesday works — two o'clock.".into(),
            }],
            "claude-sonnet-5",
            1,
        )
    })
    .unwrap();

    let found = db
        .read(|conn| store::fresh_for_thread(conn, 1))
        .unwrap()
        .expect("stances");
    assert_eq!(found.stances[0].body, "Tuesday works — two o'clock.");
}

/// The promotion: a picked stance becomes an ordinary draft on the ordinary
/// path, and the suggestion row goes with it.
#[tokio::test]
async fn a_sent_stance_takes_its_suggestion_with_it() {
    let (_temp, db, account_id) = seeded();
    db.write(|conn| {
        store::save(
            conn,
            account_id,
            1,
            11,
            "incoming",
            &[Stance {
                label: "Say you'll be there".into(),
                body: "Tuesday works.".into(),
            }],
            "m",
            1,
        )
    })
    .unwrap();

    db.write(|conn| {
        store::record(
            conn,
            store::Outcome::SentAsWritten,
            Some(0),
            "Say you'll be there",
            2,
        )?;
        store::forget(conn, 1)
    })
    .unwrap();

    assert_eq!(rows(&db), 0);
    let counters = db.read(|conn| store::counters(conn)).unwrap();
    assert_eq!(counters.sent_as_written, 1);
}
