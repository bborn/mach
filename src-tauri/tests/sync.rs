//! Behaviour tests for the sync engine.
//!
//! No network. Instead of scripting a queue of canned responses, these drive a
//! small **fake Gmail/Calendar server** that holds real mailbox state and answers
//! from it. That matters here in a way it did not for `tests/google.rs`: the
//! properties under test — resumability, the watermark ordering, idempotence —
//! are about what happens when the same endpoints are called *again* after
//! something changed, which a fixed response script cannot express.
//!
//! The store is a real SQLite database (in memory, or a temp file where a test
//! needs to prove that progress survives reopening it).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use mach_lib::db::models::{NewAccount, ThreadQuery};
use mach_lib::db::{queries, sync_queries, Db};
use mach_lib::google::types::encode_base64url;
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, RetryPolicy, StaticTokenProvider,
    TokenProvider, TransportError,
};
use mach_lib::sync::{CancelToken, SyncConfig, SyncEngine, TransportClients};

const GMAIL_BASE: &str = "https://gmail.test/gmail/v1";
const CALENDAR_BASE: &str = "https://calendar.test/calendar/v3";

// ===========================================================================
// the fake Google
// ===========================================================================

#[derive(Debug, Clone)]
struct FakeMessage {
    id: String,
    thread_id: String,
    labels: Vec<String>,
    internal_date: i64,
    subject: String,
    from: String,
    to: String,
    snippet: String,
    body: String,
    /// (filename, mime, size)
    attachment: Option<(String, String, i64)>,
}

impl FakeMessage {
    fn new(id: &str, thread_id: &str) -> Self {
        Self {
            id: id.into(),
            thread_id: thread_id.into(),
            labels: vec!["INBOX".into(), "UNREAD".into()],
            internal_date: 1_700_000_000_000,
            subject: format!("Subject {id}"),
            from: format!("Sender {id} <{id}@example.com>"),
            to: "Alex <alex@example.com>".into(),
            snippet: format!("snippet {id}"),
            body: format!("body of {id}"),
            attachment: None,
        }
    }

    fn labels(mut self, labels: &[&str]) -> Self {
        self.labels = labels.iter().map(|s| s.to_string()).collect();
        self
    }

    fn at(mut self, ms: i64) -> Self {
        self.internal_date = ms;
        self
    }

    fn subject(mut self, s: &str) -> Self {
        self.subject = s.into();
        self
    }

    fn with_attachment(mut self, filename: &str) -> Self {
        self.attachment = Some((filename.into(), "application/pdf".into(), 4096));
        self
    }

    fn to_json(&self) -> Value {
        let mut parts = vec![json!({
            "partId": "0",
            "mimeType": "text/plain",
            "body": { "size": self.body.len(), "data": encode_base64url(self.body.as_bytes()) }
        })];
        if let Some((filename, mime, size)) = &self.attachment {
            parts.push(json!({
                "partId": "1",
                "mimeType": mime,
                "filename": filename,
                "body": { "attachmentId": format!("att-{}", self.id), "size": size }
            }));
        }
        json!({
            "id": self.id,
            "threadId": self.thread_id,
            "labelIds": self.labels,
            "snippet": self.snippet,
            "internalDate": self.internal_date.to_string(),
            "payload": {
                "mimeType": "multipart/mixed",
                "headers": [
                    { "name": "Subject", "value": self.subject },
                    { "name": "From", "value": self.from },
                    { "name": "To", "value": self.to },
                    { "name": "Message-ID", "value": format!("<{}@mach.test>", self.id) },
                ],
                "parts": parts,
            }
        })
    }

    fn stub(&self) -> Value {
        json!({ "id": self.id, "threadId": self.thread_id, "labelIds": self.labels })
    }
}

/// One account's server-side state.
struct Mailbox {
    email: String,
    messages: BTreeMap<String, FakeMessage>,
    /// `(history_id, record)` pairs, in order.
    history: Vec<(u64, Value)>,
    history_id: u64,
    labels: Vec<(String, String, String)>,
    /// `(calendar id, is primary)`
    calendars: Vec<(String, bool)>,
    /// Everything in the window, answered by a full `events.list`.
    events: BTreeMap<String, Vec<Value>>,
    /// What an incremental (`syncToken`) call will report next, then cleared.
    pending: BTreeMap<String, Vec<Value>>,
    sync_token_seq: u64,
    /// Gmail prunes history records after a while. Any `startHistoryId` below
    /// this answers 404, exactly as the real API does.
    history_floor: u64,
    calendar_token_expired: bool,
    /// When set, every request for this account fails with this status.
    fail_with: Option<(u16, String)>,
    page_size: usize,
}

impl Mailbox {
    fn new(email: &str) -> Self {
        Self {
            email: email.into(),
            messages: BTreeMap::new(),
            history: Vec::new(),
            history_id: 1000,
            labels: vec![
                ("INBOX".into(), "INBOX".into(), "system".into()),
                ("UNREAD".into(), "UNREAD".into(), "system".into()),
                ("STARRED".into(), "STARRED".into(), "system".into()),
                ("Label_7".into(), "Receipts".into(), "user".into()),
            ],
            calendars: vec![("primary".into(), true)],
            events: BTreeMap::new(),
            pending: BTreeMap::new(),
            sync_token_seq: 0,
            history_floor: 0,
            calendar_token_expired: false,
            fail_with: None,
            page_size: 100,
        }
    }

    /// Seed a message with no history record — the state a backfill discovers.
    /// The watermark still moves, because something did change server-side;
    /// only the record explaining it is absent.
    fn seed(&mut self, message: FakeMessage) {
        self.history_id += 1;
        self.messages.insert(message.id.clone(), message);
    }

    /// Drop every history record older than the current watermark. Anything
    /// asking to resume from before it gets a 404, which is precisely how
    /// Gmail expresses "your historyId aged out".
    fn expire_history(&mut self) {
        self.history_floor = self.history_id;
        self.history.clear();
    }

    /// Deliver a message *now*, the way Gmail would: it appears in the mailbox
    /// and a `messagesAdded` record appears in the history.
    fn deliver(&mut self, message: FakeMessage) {
        self.history_id += 1;
        let record = json!({
            "id": self.history_id.to_string(),
            "messages": [ message.stub() ],
            "messagesAdded": [ { "message": message.to_json() } ],
        });
        self.history.push((self.history_id, record));
        self.messages.insert(message.id.clone(), message);
    }

    fn add_labels(&mut self, message_id: &str, labels: &[&str]) {
        let Some(message) = self.messages.get_mut(message_id) else {
            return;
        };
        for label in labels {
            if !message.labels.iter().any(|l| l == label) {
                message.labels.push(label.to_string());
            }
        }
        let stub = message.stub();
        self.history_id += 1;
        self.history.push((
            self.history_id,
            json!({
                "id": self.history_id.to_string(),
                "labelsAdded": [ { "message": stub, "labelIds": labels } ],
            }),
        ));
    }

    fn remove_labels(&mut self, message_id: &str, labels: &[&str]) {
        let Some(message) = self.messages.get_mut(message_id) else {
            return;
        };
        message.labels.retain(|l| !labels.contains(&l.as_str()));
        let stub = message.stub();
        self.history_id += 1;
        self.history.push((
            self.history_id,
            json!({
                "id": self.history_id.to_string(),
                "labelsRemoved": [ { "message": stub, "labelIds": labels } ],
            }),
        ));
    }

    fn delete(&mut self, message_id: &str) {
        let Some(message) = self.messages.remove(message_id) else {
            return;
        };
        self.history_id += 1;
        self.history.push((
            self.history_id,
            json!({
                "id": self.history_id.to_string(),
                "messagesDeleted": [ { "message": message.stub() } ],
            }),
        ));
    }

    /// The history id an incremental sync would move to.
    fn current_history_id(&self) -> u64 {
        self.history_id
    }
}

/// Side effects the fake performs as calls arrive, so a test can express "a
/// message lands while the backfill is running".
#[derive(Default)]
struct Hooks {
    gets_served: usize,
    /// Fail every `messages.get` from the Nth onwards, simulating a crash.
    fail_gets_after: Option<usize>,
    /// `(after N gets, token, message)` — deliver into a mailbox mid-backfill.
    deliver_after_gets: Vec<(usize, String, FakeMessage)>,
    /// Cancel the engine once N gets have been served.
    cancel_after_gets: Option<(usize, CancelToken)>,
}

struct FakeGoogle {
    /// Keyed by bearer token, so five accounts are five mailboxes.
    accounts: Mutex<HashMap<String, Mailbox>>,
    hooks: Mutex<Hooks>,
    requests: Mutex<Vec<String>>,
    /// `messages.get` calls inside the transport right now, and the high-water
    /// mark across the run. How wide the engine actually runs is not something
    /// to take on trust — this is how a test measures it.
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    /// When non-zero, every `messages.get` is held until this many are in the
    /// transport *at the same time*. An engine that fetches serially therefore
    /// cannot pass by being quick; it stalls and the high-water mark indicts it.
    concurrency_gate: AtomicUsize,
}

impl FakeGoogle {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            accounts: Mutex::new(HashMap::new()),
            hooks: Mutex::new(Hooks::default()),
            requests: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            concurrency_gate: AtomicUsize::new(0),
        })
    }

    /// Refuse to answer any `messages.get` until `n` of them are in flight
    /// together.
    fn require_concurrency(&self, n: usize) {
        self.concurrency_gate.store(n, Ordering::SeqCst);
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// The rendezvous. Returns as soon as the required number of requests have
    /// met here, and thereafter immediately — the gate opens once and stays
    /// open, so the tail of a backfill is not made to wait for peers that no
    /// longer exist.
    async fn enter_get(&self) {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        let target = self.concurrency_gate.load(Ordering::SeqCst);
        if target == 0 {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.max_in_flight() < target {
            if Instant::now() >= deadline {
                // The engine cannot reach the width it was configured with.
                // Stop holding requests and let the assertion report it, rather
                // than hanging the suite.
                self.concurrency_gate.store(0, Ordering::SeqCst);
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    fn install(&self, token: &str, mailbox: Mailbox) {
        self.accounts.lock().unwrap().insert(token.into(), mailbox);
    }

    fn with<T>(&self, token: &str, f: impl FnOnce(&mut Mailbox) -> T) -> T {
        let mut accounts = self.accounts.lock().unwrap();
        f(accounts.get_mut(token).expect("unknown token"))
    }

    fn hooks<T>(&self, f: impl FnOnce(&mut Hooks) -> T) -> T {
        f(&mut self.hooks.lock().unwrap())
    }

    fn gets_served(&self) -> usize {
        self.hooks.lock().unwrap().gets_served
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn requests_matching(&self, needle: &str) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|r| r.contains(needle))
            .collect()
    }

    fn handle(&self, request: &HttpRequest) -> HttpResponse {
        self.requests.lock().unwrap().push(request.url.clone());

        let token = request
            .header("Authorization")
            .unwrap_or_default()
            .trim_start_matches("Bearer ")
            .to_string();

        let url = url::Url::parse(&request.url).expect("valid url");
        let path = url.path().to_string();
        let query: HashMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let segments: Vec<String> = url
            .path_segments()
            .map(|s| s.map(percent_decode).collect())
            .unwrap_or_default();

        let mut accounts = self.accounts.lock().unwrap();
        let Some(mailbox) = accounts.get_mut(&token) else {
            return HttpResponse::json(401, r#"{"error":{"message":"no such token"}}"#);
        };
        if let Some((status, message)) = mailbox.fail_with.clone() {
            return HttpResponse::json(status, json!({"error": {"message": message}}).to_string());
        }

        if path.starts_with("/gmail/") {
            self.gmail(mailbox, &segments, &query, &token)
        } else {
            self.calendar(mailbox, &segments, &query)
        }
    }

    fn gmail(
        &self,
        mailbox: &mut Mailbox,
        segments: &[String],
        query: &HashMap<String, String>,
        token: &str,
    ) -> HttpResponse {
        // /gmail/v1/users/me/<resource>[/<id>]
        let tail: Vec<&str> = segments.iter().skip(4).map(|s| s.as_str()).collect();
        match tail.as_slice() {
            ["profile"] => HttpResponse::json(
                200,
                json!({
                    "emailAddress": mailbox.email,
                    "historyId": mailbox.current_history_id().to_string(),
                })
                .to_string(),
            ),

            ["labels"] => {
                let labels: Vec<Value> = mailbox
                    .labels
                    .iter()
                    .map(|(id, name, kind)| json!({ "id": id, "name": name, "type": kind }))
                    .collect();
                HttpResponse::json(200, json!({ "labels": labels }).to_string())
            }

            ["messages"] => {
                let ids: Vec<&FakeMessage> = mailbox.messages.values().collect();
                let start: usize = query
                    .get("pageToken")
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0);
                let end = (start + mailbox.page_size).min(ids.len());
                let page: Vec<Value> = ids[start..end]
                    .iter()
                    .map(|m| json!({ "id": m.id, "threadId": m.thread_id }))
                    .collect();
                let mut body = json!({ "messages": page });
                if end < ids.len() {
                    body["nextPageToken"] = json!(end.to_string());
                }
                HttpResponse::json(200, body.to_string())
            }

            ["messages", id] => {
                let effect = self.hooks(|h| {
                    h.gets_served += 1;
                    let n = h.gets_served;
                    let fail = h.fail_gets_after.is_some_and(|after| n > after);
                    let deliver: Vec<(String, FakeMessage)> = h
                        .deliver_after_gets
                        .iter()
                        .filter(|(at, _, _)| *at == n)
                        .map(|(_, tok, m)| (tok.clone(), m.clone()))
                        .collect();
                    let cancel = h
                        .cancel_after_gets
                        .as_ref()
                        .filter(|(at, _)| *at == n)
                        .map(|(_, c)| c.clone());
                    (fail, deliver, cancel)
                });
                let (fail, deliver, cancel) = effect;

                for (target, message) in deliver {
                    if target == token {
                        mailbox.deliver(message);
                    } else {
                        // Delivering into another mailbox from here would need
                        // the outer lock; tests only ever target the caller.
                        panic!("deliver_after_gets targets a different account");
                    }
                }
                if let Some(cancel) = cancel {
                    cancel.cancel();
                }
                if fail {
                    return HttpResponse::json(
                        503,
                        r#"{"error":{"message":"simulated interruption"}}"#,
                    );
                }

                match mailbox.messages.get(*id) {
                    Some(message) => HttpResponse::json(200, message.to_json().to_string()),
                    None => HttpResponse::json(404, r#"{"error":{"message":"not found"}}"#),
                }
            }

            ["history"] => {
                let start: u64 = query
                    .get("startHistoryId")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if start < mailbox.history_floor {
                    return HttpResponse::json(
                        404,
                        r#"{"error":{"message":"startHistoryId is too old"}}"#,
                    );
                }
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
                        "historyId": mailbox.current_history_id().to_string(),
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

    fn calendar(
        &self,
        mailbox: &mut Mailbox,
        segments: &[String],
        query: &HashMap<String, String>,
    ) -> HttpResponse {
        let tail: Vec<&str> = segments.iter().skip(2).map(|s| s.as_str()).collect();
        match tail.as_slice() {
            ["users", "me", "calendarList"] => {
                let items: Vec<Value> = mailbox
                    .calendars
                    .iter()
                    .map(|(id, primary)| json!({ "id": id, "primary": primary }))
                    .collect();
                HttpResponse::json(200, json!({ "items": items }).to_string())
            }

            ["calendars", calendar_id, "events"] => {
                let incremental = query.contains_key("syncToken");
                if incremental && mailbox.calendar_token_expired {
                    return HttpResponse::json(
                        410,
                        r#"{"error":{"message":"sync token expired","errors":[{"reason":"fullSyncRequired"}]}}"#,
                    );
                }
                let items: Vec<Value> = if incremental {
                    mailbox
                        .pending
                        .remove(*calendar_id)
                        .unwrap_or_default()
                } else {
                    mailbox
                        .events
                        .get(*calendar_id)
                        .cloned()
                        .unwrap_or_default()
                };
                mailbox.sync_token_seq += 1;
                HttpResponse::json(
                    200,
                    json!({
                        "items": items,
                        "nextSyncToken": format!("sync-{}", mailbox.sync_token_seq),
                    })
                    .to_string(),
                )
            }

            other => HttpResponse::json(
                404,
                json!({"error": {"message": format!("fake calendar: no route {other:?}")}})
                    .to_string(),
            ),
        }
    }
}

impl HttpTransport for FakeGoogle {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            // `messages.get` is `/messages/<id>`; `messages.list` is `/messages?`.
            let is_get = request.url.contains("/messages/");
            if is_get {
                self.enter_get().await;
            }
            let response = self.handle(&request);
            if is_get {
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
            }
            Ok(response)
        })
    }
}

fn percent_decode(input: &str) -> String {
    // url::Url::path_segments already yields percent-encoded segments; the only
    // encoded characters the fake ever sees are in calendar ids.
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

fn calendar_config() -> SyncConfig {
    SyncConfig {
        sync_mail: false,
        sync_calendar: true,
        poll_interval: Duration::from_secs(3600),
        ..Default::default()
    }
}

fn new_engine(db: &Db, google: Arc<FakeGoogle>, config: SyncConfig) -> SyncEngine {
    let clients = TransportClients::new(google, |account| {
        Arc::new(StaticTokenProvider::new(format!("tok-{}", account.id))) as Arc<dyn TokenProvider>
    })
    .with_base_urls(GMAIL_BASE, CALENDAR_BASE)
    // The retry loop has its own tests; here a failure should surface at once
    // rather than being smoothed over.
    .with_retry_policy(RetryPolicy::none());
    SyncEngine::new(db.clone(), Arc::new(clients), config).expect("engine")
}

fn add_account(db: &Db, email: &str) -> i64 {
    db.write(|conn| {
        queries::upsert_account(
            conn,
            &NewAccount {
                email: email.into(),
                display_name: None,
                token_ref: email.into(),
                colour_index: 0,
            },
        )
    })
    .expect("account")
}

/// A database backed by a real file, so a test can close it and reopen it the
/// way an app restart does.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mach-sync-test-{}-{}/mach.sqlite3",
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

fn history_id(db: &Db, account_id: i64) -> Option<String> {
    db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT history_id FROM accounts WHERE id = ?1",
                [account_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten())
    })
    .unwrap()
}

fn message_count(db: &Db) -> i64 {
    db.read(|conn| Ok(conn.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?))
        .unwrap()
}

fn thread_count(db: &Db) -> i64 {
    db.read(|conn| Ok(conn.query_row("SELECT count(*) FROM threads", [], |r| r.get(0))?))
        .unwrap()
}

/// Invariants that must hold whatever the sync did or did not finish.
fn assert_store_is_consistent(db: &Db) {
    db.read(|conn| {
        let orphan_messages: i64 = conn.query_row(
            "SELECT count(*) FROM messages m
             LEFT JOIN threads t ON t.id = m.thread_id WHERE t.id IS NULL",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(orphan_messages, 0, "messages pointing at a missing thread");

        let empty_threads: i64 = conn.query_row(
            "SELECT count(*) FROM threads t
             WHERE NOT EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id)",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(empty_threads, 0, "threads with no messages");

        let bad_counts: i64 = conn.query_row(
            "SELECT count(*) FROM threads t
             WHERE t.message_count <> (SELECT count(*) FROM messages m WHERE m.thread_id = t.id)",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(bad_counts, 0, "threads whose message_count is stale");

        let bad_times: i64 = conn.query_row(
            "SELECT count(*) FROM threads t
             WHERE t.last_message_at <> (SELECT max(m.internal_date) FROM messages m
                                         WHERE m.thread_id = t.id)",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(bad_times, 0, "threads whose last_message_at is stale");

        let orphan_attachments: i64 = conn.query_row(
            "SELECT count(*) FROM attachments a
             LEFT JOIN messages m ON m.id = a.message_id WHERE m.id IS NULL",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(orphan_attachments, 0, "attachments with no message");
        Ok(())
    })
    .unwrap();
}

fn labels_of(db: &Db, account_id: i64, gmail_thread_id: &str) -> Vec<String> {
    db.read(|conn| {
        let thread = queries::thread_by_gmail_id(conn, account_id, gmail_thread_id)?;
        Ok(thread.map(|t| t.label_ids).unwrap_or_default())
    })
    .unwrap()
}

// ===========================================================================
// backfill
// ===========================================================================

#[tokio::test]
async fn a_full_backfill_populates_threads_messages_and_labels() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    mailbox.seed(FakeMessage::new("m1", "t1").at(1_000).subject("Hello"));
    mailbox.seed(FakeMessage::new("m2", "t1").at(2_000).subject("Re: Hello"));
    mailbox.seed(
        FakeMessage::new("m3", "t2")
            .at(3_000)
            .labels(&["INBOX", "Label_7"])
            .with_attachment("invoice.pdf"),
    );
    google.install(&format!("tok-{account_id}"), mailbox);

    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    let pass = engine.sync_once().await;

    assert!(
        pass.account(account_id).unwrap().is_ok(),
        "backfill failed: {:?}",
        pass.account(account_id).unwrap().error
    );
    assert_eq!(pass.messages_written(), 3);

    // Everything is reachable through the normal read surface.
    let threads = db
        .read(|conn| queries::list_threads(conn, &ThreadQuery::default()))
        .unwrap();
    assert_eq!(threads.len(), 2);
    // Newest first.
    assert_eq!(threads[0].gmail_thread_id, "t2");
    assert_eq!(threads[0].last_message_at, 3_000);
    assert!(threads[0].has_attachments, "denormalised flag must be set");
    assert!(!threads[0].is_unread, "m3 carries no UNREAD label");
    assert_eq!(threads[0].label_ids, vec!["INBOX", "Label_7"]);

    let conversation = &threads[1];
    assert_eq!(conversation.gmail_thread_id, "t1");
    assert_eq!(conversation.message_count, 2);
    assert_eq!(conversation.subject, "Hello", "subject comes from the first message");
    assert_eq!(conversation.snippet, "snippet m2", "snippet from the last");
    assert!(conversation.is_unread);
    assert!(!conversation.has_attachments);
    assert_eq!(conversation.participants.len(), 2);
    assert_eq!(conversation.participants[0].email, "m1@example.com");
    assert_eq!(
        conversation.participants[0].name.as_deref(),
        Some("Sender m1")
    );

    let full = db
        .read(|conn| queries::thread_with_messages(conn, conversation.id))
        .unwrap()
        .unwrap();
    assert_eq!(full.messages.len(), 2);
    assert_eq!(full.messages[0].body_text.as_deref(), Some("body of m1"));
    assert_eq!(full.messages[0].to[0].email, "alex@example.com");
    assert_eq!(
        full.messages[0].rfc822_message_id.as_deref(),
        Some("<m1@mach.test>")
    );

    // Attachment metadata landed too.
    let attachment_thread = db
        .read(|conn| queries::thread_with_messages(conn, threads[0].id))
        .unwrap()
        .unwrap();
    assert_eq!(attachment_thread.messages[0].attachments.len(), 1);
    assert_eq!(
        attachment_thread.messages[0].attachments[0].filename,
        "invoice.pdf"
    );

    // The label list was synced as well.
    let labels = db.read(|conn| queries::list_labels(conn, Some(account_id))).unwrap();
    assert!(labels.iter().any(|l| l.gmail_label_id == "Label_7" && l.name == "Receipts"));

    // Local search works over what the backfill wrote — the point of the store.
    let hits = db
        .read(|conn| queries::search_thread_summaries(conn, "invoice", 10))
        .unwrap();
    assert!(hits.is_empty(), "the body, not the filename, is indexed");
    let hits = db
        .read(|conn| queries::search_thread_summaries(conn, "body of m3", 10))
        .unwrap();
    assert_eq!(hits.len(), 1);

    assert_store_is_consistent(&db);
}

#[tokio::test]
async fn a_backfill_that_dies_midway_resumes_instead_of_restarting() {
    let temp = TempDb::new();
    let account_id;
    let google = FakeGoogle::new();

    {
        let db = temp.open();
        account_id = add_account(&db, "one@example.com");
        let mut mailbox = Mailbox::new("one@example.com");
        for n in 1..=5 {
            mailbox.seed(FakeMessage::new(&format!("m{n}"), &format!("t{n}")).at(n * 1000));
        }
        google.install(&format!("tok-{account_id}"), mailbox);

        // Die partway through the second batch.
        google.hooks(|h| h.fail_gets_after = Some(2));

        let mut config = mail_config();
        config.message_batch_size = 2;
        config.request_concurrency = 1;
        config.backfill_fetch_concurrency = 1;
        let engine = new_engine(&db, Arc::clone(&google), config);
        let pass = engine.sync_once().await;

        let outcome = pass.account(account_id).unwrap();
        assert!(outcome.error.is_some(), "the interrupted pass must report failure");

        let stored = message_count(&db);
        assert!(stored > 0 && stored < 5, "expected partial progress, got {stored}");
        assert_eq!(
            history_id(&db, account_id),
            None,
            "a half-finished backfill must never look like a synced account"
        );
        let remaining = db
            .read(|conn| sync_queries::backfill_remaining(conn, account_id))
            .unwrap();
        assert_eq!(remaining, 5 - stored, "the rest is still queued");
        assert_store_is_consistent(&db);
    }

    // --- restart: new process, new engine, same file ------------------------
    let gets_before = google.gets_served();
    google.hooks(|h| h.fail_gets_after = None);

    let db = temp.open();
    let stored_before = message_count(&db);
    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    let pass = engine.sync_once().await;

    assert!(pass.account(account_id).unwrap().is_ok());
    assert_eq!(message_count(&db), 5, "the complete set, with no duplicates");
    assert_eq!(thread_count(&db), 5);
    assert!(history_id(&db, account_id).is_some(), "now it is synced");
    assert_eq!(
        db.read(|conn| sync_queries::backfill_remaining(conn, account_id))
            .unwrap(),
        0
    );

    let refetched = google.gets_served() - gets_before;
    assert_eq!(
        refetched as i64,
        5 - stored_before,
        "the restart must fetch only what was still queued"
    );

    // Nothing was enumerated twice either.
    let list_calls = google.requests_matching("/messages?").len();
    assert_eq!(list_calls, 1, "enumeration was already complete; do not redo it");

    assert_store_is_consistent(&db);
}

// ===========================================================================
// backfill throughput
// ===========================================================================

/// The reason this unit exists. A backfill that fetches one message at a time —
/// or one *batch* at a time, waiting for each transaction before asking for more
/// — uses a fifth of the quota Gmail grants. The engine must keep the wire full.
///
/// The fake refuses to answer any `messages.get` until `width` of them are in
/// flight together, so an engine that does not actually run wide cannot pass
/// this by being fast: it stalls at the rendezvous and the high-water mark
/// convicts it.
#[tokio::test]
async fn the_backfill_keeps_the_configured_number_of_requests_in_flight() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    for n in 1..=40 {
        mailbox.seed(FakeMessage::new(&format!("m{n:02}"), &format!("t{n:02}")).at(n * 1000));
    }
    google.install(&format!("tok-{account_id}"), mailbox);
    google.require_concurrency(8);

    let mut config = mail_config();
    config.request_concurrency = 16;
    config.backfill_fetch_concurrency = 8;
    // Deliberately smaller than the fetch width: writing must not be a barrier
    // the fetcher has to wait behind.
    config.message_batch_size = 5;
    let engine = new_engine(&db, Arc::clone(&google), config);

    let pass = engine.sync_once().await;
    let outcome = pass.account(account_id).unwrap();
    assert!(outcome.is_ok(), "backfill failed: {:?}", outcome.error);

    assert_eq!(
        google.max_in_flight(),
        8,
        "the engine must saturate the configured width, not approach it"
    );
    assert_eq!(
        google.gets_served(),
        40,
        "each message is fetched exactly once, however wide the engine runs"
    );
    assert_eq!(message_count(&db), 40, "and written exactly once");
    assert_eq!(thread_count(&db), 40);
    assert_eq!(pass.messages_written(), 40);
    assert_eq!(
        db.read(|conn| sync_queries::backfill_remaining(conn, account_id))
            .unwrap(),
        0
    );

    // The indicator the user watches has to survive concurrency too.
    let status = engine.status_snapshot();
    let account = status.account(account_id).unwrap();
    assert_eq!(account.messages_written, 40);
    assert_eq!((account.backfill_done, account.backfill_total), (40, 40));

    assert_store_is_consistent(&db);
}

/// Resumability is the property most likely to be traded away for throughput:
/// with many requests in flight there are many ids leased but not yet written
/// when the failure lands. Every one of them must still be in the queue.
#[tokio::test]
async fn a_wide_backfill_that_dies_midway_still_resumes_exactly() {
    let temp = TempDb::new();
    let account_id;
    let google = FakeGoogle::new();

    {
        let db = temp.open();
        account_id = add_account(&db, "one@example.com");
        let mut mailbox = Mailbox::new("one@example.com");
        for n in 1..=12 {
            mailbox.seed(FakeMessage::new(&format!("m{n:02}"), &format!("t{n:02}")).at(n * 1000));
        }
        google.install(&format!("tok-{account_id}"), mailbox);

        // Four succeed; everything after that is a 503, with several requests
        // already on the wire when it happens.
        google.hooks(|h| h.fail_gets_after = Some(4));
        google.require_concurrency(4);

        let mut config = mail_config();
        config.request_concurrency = 8;
        config.backfill_fetch_concurrency = 6;
        config.message_batch_size = 3;
        let engine = new_engine(&db, Arc::clone(&google), config);
        let pass = engine.sync_once().await;

        let outcome = pass.account(account_id).unwrap();
        assert!(outcome.error.is_some(), "the interrupted pass must report failure");
        assert!(
            google.max_in_flight() >= 4,
            "this test only means something if the engine was running wide: {}",
            google.max_in_flight()
        );

        let stored = message_count(&db);
        assert_eq!(
            stored, 4,
            "every message fetched before the failure is written, and nothing else"
        );
        let remaining = db
            .read(|conn| sync_queries::backfill_remaining(conn, account_id))
            .unwrap();
        assert_eq!(
            stored + remaining,
            12,
            "an id in flight when the pass died must still be queued"
        );
        assert_eq!(
            history_id(&db, account_id),
            None,
            "a half-finished backfill must never look like a synced account"
        );
        assert_store_is_consistent(&db);
    }

    // --- restart: new process, new engine, same file ------------------------
    let gets_before = google.gets_served();
    google.hooks(|h| h.fail_gets_after = None);

    let db = temp.open();
    let mut config = mail_config();
    config.request_concurrency = 8;
    config.backfill_fetch_concurrency = 6;
    let engine = new_engine(&db, Arc::clone(&google), config);
    let pass = engine.sync_once().await;

    assert!(pass.account(account_id).unwrap().is_ok());
    assert_eq!(message_count(&db), 12, "the complete set, with no duplicates");
    assert_eq!(thread_count(&db), 12);
    assert!(history_id(&db, account_id).is_some(), "now it is synced");
    assert_eq!(
        google.gets_served() - gets_before,
        8,
        "the restart fetches what was queued and not one message more"
    );
    assert_eq!(
        db.read(|conn| sync_queries::backfill_remaining(conn, account_id))
            .unwrap(),
        0
    );
    assert_store_is_consistent(&db);
}

/// The batch is a transaction, and the queue rows go inside it. If a write dies
/// partway, the ids it covered must come back — otherwise a batch that failed
/// after deleting some rows would leave those messages permanently unfetched,
/// which is the one failure mode this design exists to make impossible.
///
/// A trigger stands in for whatever might actually make a write fail; the
/// engine cannot tell the difference.
#[tokio::test]
async fn a_write_that_dies_partway_leaves_every_queue_row_it_covered() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    for n in 1..=8 {
        mailbox.seed(FakeMessage::new(&format!("m{n:02}"), &format!("t{n:02}")).at(n * 1000));
    }
    google.install(&format!("tok-{account_id}"), mailbox);

    // m08 is fetched first (the queue drains newest-first), so the batch dies
    // with the other seven already inserted inside the same transaction.
    db.write(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER poison BEFORE INSERT ON messages
             WHEN NEW.gmail_message_id = 'm08'
             BEGIN SELECT RAISE(ABORT, 'poisoned write'); END;",
        )?;
        Ok(())
    })
    .unwrap();

    let mut config = mail_config();
    // One batch for all eight, so the failure is guaranteed to be in it.
    config.message_batch_size = 8;
    let engine = new_engine(&db, Arc::clone(&google), config);
    let pass = engine.sync_once().await;

    assert!(
        pass.account(account_id).unwrap().error.is_some(),
        "a failed write must be reported, not swallowed"
    );
    assert_eq!(
        message_count(&db),
        0,
        "the whole transaction rolls back, including the seven that did insert"
    );
    assert_eq!(
        db.read(|conn| sync_queries::backfill_remaining(conn, account_id))
            .unwrap(),
        8,
        "no queue row may be deleted for a message that was not written"
    );
    assert_eq!(history_id(&db, account_id), None);
    assert_store_is_consistent(&db);

    // And because the queue survived intact, the retry completes the set.
    db.write(|conn| {
        conn.execute_batch("DROP TRIGGER poison;")?;
        Ok(())
    })
    .unwrap();

    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    let pass = engine.sync_once().await;
    assert!(pass.account(account_id).unwrap().is_ok());
    assert_eq!(message_count(&db), 8);
    assert!(history_id(&db, account_id).is_some());
    assert_store_is_consistent(&db);
}

#[tokio::test]
async fn a_message_that_arrives_during_the_backfill_is_not_lost() {
    // The classic bug: read the watermark *after* the backfill and every change
    // that happened while it ran falls into an unrecoverable gap.
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");
    let token = format!("tok-{account_id}");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    mailbox.seed(FakeMessage::new("m1", "t1").at(1_000));
    mailbox.seed(FakeMessage::new("m2", "t2").at(2_000));
    let watermark_before = mailbox.current_history_id();
    google.install(&token, mailbox);

    // Mail lands after the first message has been fetched — i.e. after
    // enumeration finished, so the backfill itself can never see it.
    google.hooks(|h| {
        h.deliver_after_gets = vec![(
            1,
            token.clone(),
            FakeMessage::new("m3", "t3").at(9_000).subject("Landed mid-backfill"),
        )]
    });

    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    let pass = engine.sync_once().await;
    assert!(pass.account(account_id).unwrap().is_ok());

    let threads = db
        .read(|conn| queries::list_threads(conn, &ThreadQuery::default()))
        .unwrap();
    let subjects: Vec<&str> = threads.iter().map(|t| t.subject.as_str()).collect();
    assert!(
        subjects.contains(&"Landed mid-backfill"),
        "a message delivered during the backfill was dropped; got {subjects:?}"
    );
    assert_eq!(message_count(&db), 3);

    // The proof of *why* it was not lost: history was replayed from the
    // watermark captured before enumeration, not from a fresher one.
    let history_calls = google.requests_matching("/history?");
    assert_eq!(history_calls.len(), 1);
    assert!(
        history_calls[0].contains(&format!("startHistoryId={watermark_before}")),
        "history must resume from the pre-backfill watermark, got {}",
        history_calls[0]
    );

    // And the stored watermark has moved past the new message.
    let stored: u64 = history_id(&db, account_id).unwrap().parse().unwrap();
    assert!(stored > watermark_before);

    assert_store_is_consistent(&db);
}

// ===========================================================================
// incremental
// ===========================================================================

/// Backfill an account and hand back its id plus the fake, ready for history.
async fn synced_account(db: &Db, google: &Arc<FakeGoogle>, seed: Vec<FakeMessage>) -> i64 {
    let account_id = add_account(db, "one@example.com");
    let mut mailbox = Mailbox::new("one@example.com");
    for message in seed {
        mailbox.seed(message);
    }
    google.install(&format!("tok-{account_id}"), mailbox);

    let engine = new_engine(db, Arc::clone(google), mail_config());
    let pass = engine.sync_once().await;
    assert!(pass.account(account_id).unwrap().is_ok());
    account_id
}

#[tokio::test]
async fn incremental_sync_applies_every_history_record_type() {
    let db = Db::open_in_memory().unwrap();
    let google = FakeGoogle::new();
    let account_id = synced_account(
        &db,
        &google,
        vec![
            FakeMessage::new("m1", "t1").at(1_000),
            FakeMessage::new("m2", "t2").at(2_000),
            FakeMessage::new("m3", "t3").at(3_000),
        ],
    )
    .await;
    let token = format!("tok-{account_id}");

    let watermark_before = history_id(&db, account_id).unwrap();

    google.with(&token, |mailbox| {
        // messagesAdded
        mailbox.deliver(FakeMessage::new("m4", "t4").at(4_000).subject("Fresh"));
        // labelsAdded
        mailbox.add_labels("m1", &["STARRED"]);
        // labelsRemoved — the read/unread path
        mailbox.remove_labels("m2", &["UNREAD"]);
        // messagesDeleted
        mailbox.delete("m3");
    });

    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    let pass = engine.sync_once().await;
    assert!(pass.account(account_id).unwrap().is_ok());

    // added
    let added = db
        .read(|conn| queries::thread_by_gmail_id(conn, account_id, "t4"))
        .unwrap()
        .expect("messagesAdded must create the thread");
    assert_eq!(added.subject, "Fresh");

    // labelsAdded
    let starred = labels_of(&db, account_id, "t1");
    assert!(starred.contains(&"STARRED".to_string()), "got {starred:?}");
    assert!(starred.contains(&"INBOX".to_string()), "existing labels survive");

    // labelsRemoved
    let read = db
        .read(|conn| queries::thread_by_gmail_id(conn, account_id, "t2"))
        .unwrap()
        .unwrap();
    assert!(!read.is_unread, "removing UNREAD must clear the thread badge");
    assert!(!labels_of(&db, account_id, "t2").contains(&"UNREAD".to_string()));

    // messagesDeleted
    assert!(
        db.read(|conn| queries::thread_by_gmail_id(conn, account_id, "t3"))
            .unwrap()
            .is_none(),
        "deleting the only message must drop the thread"
    );
    assert_eq!(message_count(&db), 3);

    // The watermark advanced, and only after the batch landed.
    let watermark_after = history_id(&db, account_id).unwrap();
    assert_ne!(watermark_before, watermark_after);

    assert_store_is_consistent(&db);
}

#[tokio::test]
async fn applying_the_same_history_batch_twice_changes_nothing() {
    let db = Db::open_in_memory().unwrap();
    let google = FakeGoogle::new();
    let account_id = synced_account(
        &db,
        &google,
        vec![
            FakeMessage::new("m1", "t1").at(1_000),
            FakeMessage::new("m2", "t1").at(1_500),
            FakeMessage::new("m3", "t2").at(2_000),
        ],
    )
    .await;
    let token = format!("tok-{account_id}");
    let watermark_before = history_id(&db, account_id).unwrap();

    google.with(&token, |mailbox| {
        mailbox.deliver(FakeMessage::new("m4", "t1").at(4_000));
        mailbox.add_labels("m1", &["STARRED"]);
        mailbox.remove_labels("m3", &["UNREAD"]);
    });

    let snapshot_after_first = {
        let engine = new_engine(&db, Arc::clone(&google), mail_config());
        assert!(engine.sync_once().await.account(account_id).unwrap().is_ok());
        store_fingerprint(&db)
    };

    // Rewind the watermark so the very same records are replayed.
    db.write(|conn| queries::set_history_id(conn, account_id, Some(&watermark_before)))
        .unwrap();

    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    assert!(engine.sync_once().await.account(account_id).unwrap().is_ok());

    assert_eq!(
        store_fingerprint(&db),
        snapshot_after_first,
        "replaying a history batch must be a no-op"
    );
    assert_eq!(message_count(&db), 4, "no duplicate messages");
    assert!(
        labels_of(&db, account_id, "t1").contains(&"STARRED".to_string()),
        "a replayed labelsAdded must not be undone"
    );
    assert!(
        !labels_of(&db, account_id, "t2").contains(&"UNREAD".to_string()),
        "a replayed labelsRemoved must not resurrect the label"
    );
    assert_store_is_consistent(&db);
}

/// Everything that should be identical after a replay, as one comparable value.
fn store_fingerprint(db: &Db) -> String {
    db.read(|conn| {
        let mut out = String::new();
        let mut stmt = conn.prepare(
            "SELECT account_id, gmail_thread_id, subject, snippet, last_message_at,
                    is_unread, message_count, has_attachments
             FROM threads ORDER BY account_id, gmail_thread_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(format!(
                "T {} {} {:?} {:?} {} {} {} {}\n",
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, bool>(7)?,
            ))
        })?;
        for row in rows {
            out.push_str(&row?);
        }

        let mut stmt = conn.prepare(
            "SELECT t.gmail_thread_id, tl.gmail_label_id FROM thread_labels tl
             JOIN threads t ON t.id = tl.thread_id
             ORDER BY t.gmail_thread_id, tl.gmail_label_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(format!(
                "L {} {}\n",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            ))
        })?;
        for row in rows {
            out.push_str(&row?);
        }

        let mut stmt = conn.prepare(
            "SELECT account_id, gmail_message_id, subject, is_unread, internal_date
             FROM messages ORDER BY account_id, gmail_message_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(format!(
                "M {} {} {:?} {} {}\n",
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            out.push_str(&row?);
        }
        Ok(out)
    })
    .unwrap()
}

#[tokio::test]
async fn an_expired_history_id_triggers_a_full_resync_rather_than_an_error() {
    let db = Db::open_in_memory().unwrap();
    let google = FakeGoogle::new();
    let account_id = synced_account(
        &db,
        &google,
        vec![FakeMessage::new("m1", "t1").at(1_000)],
    )
    .await;
    let token = format!("tok-{account_id}");

    // While we were away: the watermark aged out, and mail moved.
    google.with(&token, |mailbox| {
        mailbox.seed(FakeMessage::new("m2", "t2").at(2_000).subject("Missed"));
        mailbox.expire_history();
    });

    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    let pass = engine.sync_once().await;

    let outcome = pass.account(account_id).unwrap();
    assert!(
        outcome.is_ok(),
        "HistoryExpired is an expected path, not a failure: {:?}",
        outcome.error
    );
    assert_eq!(
        engine.status_snapshot().account(account_id).unwrap().last_error,
        None
    );

    assert_eq!(message_count(&db), 2, "the resync picked up what history could not");
    assert!(db
        .read(|conn| queries::thread_by_gmail_id(conn, account_id, "t2"))
        .unwrap()
        .is_some());
    assert!(history_id(&db, account_id).is_some(), "a fresh watermark was stored");
    assert_store_is_consistent(&db);
}

// ===========================================================================
// independence
// ===========================================================================

#[tokio::test]
async fn one_account_failing_does_not_stop_the_others() {
    let db = Db::open_in_memory().unwrap();
    let google = FakeGoogle::new();

    let healthy = add_account(&db, "healthy@example.com");
    let broken = add_account(&db, "revoked@example.com");
    let limited = add_account(&db, "limited@example.com");

    let mut mailbox = Mailbox::new("healthy@example.com");
    mailbox.seed(FakeMessage::new("m1", "t1").at(1_000));
    mailbox.seed(FakeMessage::new("m2", "t2").at(2_000));
    google.install(&format!("tok-{healthy}"), mailbox);

    let mut revoked = Mailbox::new("revoked@example.com");
    revoked.fail_with = Some((401, "invalid_grant".into()));
    google.install(&format!("tok-{broken}"), revoked);

    let mut throttled = Mailbox::new("limited@example.com");
    throttled.fail_with = Some((429, "rate limit exceeded".into()));
    google.install(&format!("tok-{limited}"), throttled);

    let engine = new_engine(&db, Arc::clone(&google), mail_config());
    let pass = engine.sync_once().await;

    assert!(
        pass.account(healthy).unwrap().is_ok(),
        "a healthy account must complete: {:?}",
        pass.account(healthy).unwrap().error
    );
    assert_eq!(pass.account(healthy).unwrap().messages_written, 2);
    assert!(pass.account(broken).unwrap().error.is_some());
    assert!(pass.account(limited).unwrap().error.is_some());
    assert_eq!(pass.failures().count(), 2);

    assert_eq!(message_count(&db), 2);

    // The status the UI renders isolates the failures too.
    let status = engine.status_snapshot();
    assert_eq!(status.account(healthy).unwrap().last_error, None);
    assert!(status.account(broken).unwrap().last_error.is_some());
    assert_eq!(status.errors().count(), 2);

    // And the healthy account is genuinely synced, not just error-free.
    assert!(history_id(&db, healthy).is_some());
    assert_eq!(history_id(&db, broken), None);
    assert_store_is_consistent(&db);
}

// ===========================================================================
// calendar
// ===========================================================================

fn event_json(id: &str, summary: &str, start: &str, end: &str) -> Value {
    json!({
        "id": id,
        "status": "confirmed",
        "summary": summary,
        "updated": "2026-08-01T12:00:00Z",
        "start": { "dateTime": start },
        "end": { "dateTime": end },
        "attendees": [
            { "email": "alex@example.com", "self": true, "responseStatus": "accepted" },
            { "email": "tawny@example.com", "displayName": "Tawny" },
        ],
    })
}

#[tokio::test]
async fn calendar_syncs_a_window_then_rides_the_sync_token() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");
    let token = format!("tok-{account_id}");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    mailbox.events.insert(
        "primary".into(),
        vec![
            event_json("e1", "Standup", "2026-08-10T09:00:00Z", "2026-08-10T09:15:00Z"),
            event_json("e2", "Review", "2026-08-11T14:00:00Z", "2026-08-11T15:00:00Z"),
        ],
    );
    google.install(&token, mailbox);

    let engine = new_engine(&db, Arc::clone(&google), calendar_config());
    let pass = engine.sync_once().await;
    assert!(
        pass.account(account_id).unwrap().is_ok(),
        "{:?}",
        pass.account(account_id).unwrap().error
    );
    assert_eq!(pass.events_written(), 2);

    let events = db
        .read(|conn| queries::events_in_range(conn, 0, i64::MAX, None))
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].title, "Standup");
    assert_eq!(events[0].calendar_id, "primary");
    assert_eq!(events[0].attendees.len(), 2);
    assert_eq!(
        events[0].rsvp_status,
        Some(mach_lib::db::models::RsvpStatus::Accepted)
    );

    // The first sweep asked for a window; it did not send a syncToken.
    let first = google.requests_matching("/events?");
    assert_eq!(first.len(), 1);
    assert!(first[0].contains("singleEvents=true"), "{}", first[0]);
    assert!(first[0].contains("timeMin="), "{}", first[0]);
    assert!(!first[0].contains("syncToken="), "{}", first[0]);

    // The token from the final page was stored, including on the account row.
    let stored = db
        .read(|conn| Ok(queries::list_accounts(conn)?[0].calendar_sync_token.clone()))
        .unwrap();
    assert_eq!(stored.as_deref(), Some("sync-1"));

    // --- second pass: incremental ------------------------------------------
    google.with(&token, |mailbox| {
        mailbox.pending.insert(
            "primary".into(),
            vec![
                event_json("e2", "Review (moved)", "2026-08-11T16:00:00Z", "2026-08-11T17:00:00Z"),
                json!({ "id": "e1", "status": "cancelled" }),
                event_json("e3", "New thing", "2026-08-12T10:00:00Z", "2026-08-12T11:00:00Z"),
            ],
        );
    });

    let engine = new_engine(&db, Arc::clone(&google), calendar_config());
    assert!(engine.sync_once().await.account(account_id).unwrap().is_ok());

    let second = google.requests_matching("/events?");
    assert_eq!(second.len(), 2);
    assert!(second[1].contains("syncToken=sync-1"), "{}", second[1]);
    assert!(
        !second[1].contains("timeMin="),
        "an incremental call must not also send a window: {}",
        second[1]
    );

    let events = db
        .read(|conn| queries::events_in_range(conn, 0, i64::MAX, None))
        .unwrap();
    let titles: Vec<&str> = events.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["Review (moved)", "New thing"]);
    assert!(
        !titles.contains(&"Standup"),
        "a cancelled instance must be deleted locally"
    );
}

#[tokio::test]
async fn an_expired_calendar_sync_token_falls_back_to_a_full_window() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");
    let token = format!("tok-{account_id}");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    mailbox.events.insert(
        "primary".into(),
        vec![event_json(
            "e1",
            "Standup",
            "2026-08-10T09:00:00Z",
            "2026-08-10T09:15:00Z",
        )],
    );
    google.install(&token, mailbox);

    let engine = new_engine(&db, Arc::clone(&google), calendar_config());
    assert!(engine.sync_once().await.account(account_id).unwrap().is_ok());

    // The token ages out, and the calendar changes while it is dead.
    google.with(&token, |mailbox| {
        mailbox.calendar_token_expired = true;
        mailbox.events.insert(
            "primary".into(),
            vec![
                event_json("e1", "Standup", "2026-08-10T09:00:00Z", "2026-08-10T09:15:00Z"),
                event_json("e9", "Added while away", "2026-08-13T09:00:00Z", "2026-08-13T10:00:00Z"),
            ],
        );
    });

    let engine = new_engine(&db, Arc::clone(&google), calendar_config());
    let pass = engine.sync_once().await;
    let outcome = pass.account(account_id).unwrap();
    assert!(
        outcome.is_ok(),
        "an expired syncToken is an expected path: {:?}",
        outcome.error
    );

    let events = db
        .read(|conn| queries::events_in_range(conn, 0, i64::MAX, None))
        .unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|e| e.title == "Added while away"));

    // The failed incremental was followed by a windowed call, and a new token
    // replaced the dead one.
    let calls = google.requests_matching("/events?");
    assert_eq!(calls.len(), 3);
    assert!(calls[1].contains("syncToken="));
    assert!(calls[2].contains("timeMin="), "{}", calls[2]);
    let stored = db
        .read(|conn| Ok(queries::list_accounts(conn)?[0].calendar_sync_token.clone()))
        .unwrap();
    assert_eq!(stored.as_deref(), Some("sync-2"));
}

// ===========================================================================
// cancellation
// ===========================================================================

#[tokio::test]
async fn cancelling_mid_sync_stops_promptly_and_leaves_the_store_consistent() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");
    let token = format!("tok-{account_id}");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    for n in 1..=12 {
        mailbox.seed(FakeMessage::new(&format!("m{n:02}"), &format!("t{n}")).at(n * 1000));
    }
    google.install(&token, mailbox);

    let mut config = mail_config();
    config.message_batch_size = 2;
    config.request_concurrency = 1;
    config.backfill_fetch_concurrency = 1;
    let engine = new_engine(&db, Arc::clone(&google), config);

    // Shutdown lands in the middle of the third batch.
    google.hooks(|h| h.cancel_after_gets = Some((5, engine.cancel_token())));

    let pass = engine.sync_once().await;
    let outcome = pass.account(account_id).unwrap();
    assert!(outcome.cancelled, "the pass must report cancellation");
    assert!(outcome.error.is_none(), "cancellation is not an error");

    // It stopped promptly: nowhere near all twelve were fetched.
    assert!(
        google.gets_served() < 12,
        "kept working after cancellation ({} gets)",
        google.gets_served()
    );

    // Whatever landed is coherent, and the account is not falsely marked synced.
    assert_store_is_consistent(&db);
    assert_eq!(history_id(&db, account_id), None);
    let stored = message_count(&db);
    let remaining = db
        .read(|conn| sync_queries::backfill_remaining(conn, account_id))
        .unwrap();
    assert_eq!(stored + remaining, 12, "no work was lost or invented");

    assert_eq!(
        engine.status_snapshot().account(account_id).unwrap().phase,
        mach_lib::sync::SyncPhase::Cancelled
    );

    // Shutting the engine down joins the loop rather than detaching it.
    engine.shutdown().await;
    assert!(!engine.status_snapshot().running);
}

/// The same promise, but with a wide backfill: shutdown now lands with a dozen
/// requests on the wire rather than one.
#[tokio::test]
async fn cancelling_a_wide_backfill_abandons_the_in_flight_requests_cleanly() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");
    let token = format!("tok-{account_id}");

    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    for n in 1..=30 {
        mailbox.seed(FakeMessage::new(&format!("m{n:02}"), &format!("t{n:02}")).at(n * 1000));
    }
    google.install(&token, mailbox);
    google.require_concurrency(8);

    let mut config = mail_config();
    config.request_concurrency = 12;
    config.backfill_fetch_concurrency = 8;
    config.message_batch_size = 4;
    let engine = new_engine(&db, Arc::clone(&google), config);

    google.hooks(|h| h.cancel_after_gets = Some((6, engine.cancel_token())));

    let pass = engine.sync_once().await;
    let outcome = pass.account(account_id).unwrap();
    assert!(outcome.cancelled, "the pass must report cancellation");
    assert!(outcome.error.is_none(), "cancellation is not an error");

    assert!(
        google.max_in_flight() >= 8,
        "the point of this test is a shutdown with many requests in flight: {}",
        google.max_in_flight()
    );
    assert!(
        google.gets_served() < 30,
        "kept working after cancellation ({} gets)",
        google.gets_served()
    );

    assert_store_is_consistent(&db);
    assert_eq!(history_id(&db, account_id), None);
    let stored = message_count(&db);
    let remaining = db
        .read(|conn| sync_queries::backfill_remaining(conn, account_id))
        .unwrap();
    assert_eq!(
        stored + remaining,
        30,
        "a request abandoned in flight leaves its queue row behind: no work lost or invented"
    );
    assert_eq!(
        engine.status_snapshot().account(account_id).unwrap().phase,
        mach_lib::sync::SyncPhase::Cancelled
    );

    engine.shutdown().await;
    let after = google.requests().len();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        google.requests().len(),
        after,
        "no fetch may outlive the shutdown"
    );
}

#[tokio::test]
async fn the_background_loop_starts_syncs_and_shuts_down() {
    let db = Db::open_in_memory().unwrap();
    let account_id = add_account(&db, "one@example.com");
    let google = FakeGoogle::new();
    let mut mailbox = Mailbox::new("one@example.com");
    mailbox.seed(FakeMessage::new("m1", "t1").at(1_000));
    google.install(&format!("tok-{account_id}"), mailbox);

    let mut config = mail_config();
    config.poll_interval = Duration::from_millis(50);
    let engine = new_engine(&db, Arc::clone(&google), config);

    let mut status = engine.status();
    engine.start();

    // Wait for a pass to complete rather than sleeping a guessed amount.
    let done = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if status
                .borrow_and_update()
                .account(account_id)
                .is_some_and(|a| a.phase == mach_lib::sync::SyncPhase::Done)
            {
                return;
            }
            status.changed().await.unwrap();
        }
    })
    .await;
    assert!(done.is_ok(), "the loop never finished a pass");

    assert_eq!(message_count(&db), 1);
    engine.shutdown().await;
    let after = google.requests().len();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        google.requests().len(),
        after,
        "no work may outlive shutdown"
    );
}
