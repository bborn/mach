//! Behaviour tests for "tell him mail arrived".
//!
//! Two layers, because the feature has two halves that fail in different ways.
//!
//! The **engine** tests drive a real [`SyncEngine`] against a small fake Gmail
//! and a real SQLite file. They exist for one property above all others: a
//! backfill must not notify. That cannot be checked by reading the rule, only by
//! running a first sync and finding that nothing was said — which is what these
//! do, against the same code path the app runs.
//!
//! The **store** tests call [`notify::plan`] directly on a hand-seeded database.
//! `plan` is the whole decision — settings, rule, coalescing, and the memory of
//! what has already been said — and it takes a connection and returns a value,
//! so the interesting cases are cheap to write and impossible to get wrong by
//! accident of timing.
//!
//! What is *not* here is the platform. `notify::host` is the only file that
//! knows a banner is a banner, and it is behind a trait; one test installs a
//! recording implementation of it to prove the wire is connected, and nothing in
//! this suite can put anything on anybody's screen.

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
use mach_lib::ipc::prefs;
use mach_lib::notify::{self, host::Host, Banner, Delivery, PendingOpen, Permission};
use mach_lib::sync::{SyncConfig, SyncEngine, TransportClients};

const GMAIL_BASE: &str = "https://gmail.test/gmail/v1";
const CALENDAR_BASE: &str = "https://calendar.test/calendar/v3";

// ===========================================================================
// A fake Gmail, cut down to what notifications need
// ===========================================================================

#[derive(Debug, Clone)]
struct FakeMessage {
    id: String,
    thread_id: String,
    labels: Vec<String>,
    subject: String,
    from: String,
}

impl FakeMessage {
    fn new(id: &str, from_name: &str) -> Self {
        Self {
            id: id.into(),
            thread_id: format!("t-{id}"),
            labels: vec!["INBOX".into(), "UNREAD".into()],
            subject: format!("Subject {id}"),
            from: format!("{from_name} <{}@example.com>", from_name.to_lowercase()),
        }
    }

    fn to_json(&self) -> Value {
        let body = format!("body of {}", self.id);
        json!({
            "id": self.id,
            "threadId": self.thread_id,
            "labelIds": self.labels,
            "snippet": format!("snippet {}", self.id),
            "internalDate": "1700000000000",
            "payload": {
                "mimeType": "text/plain",
                "headers": [
                    { "name": "Subject", "value": self.subject },
                    { "name": "From", "value": self.from },
                    { "name": "To", "value": "Alex <alex@example.com>" },
                ],
                "body": { "size": body.len(), "data": encode_base64url(body.as_bytes()) },
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

    /// Already in the mailbox when Mach first looks — what a backfill finds.
    fn seed(&mut self, message: FakeMessage) {
        self.history_id += 1;
        self.messages.insert(message.id.clone(), message);
    }

    /// Arrives now, with the `messagesAdded` record Gmail would write.
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
    /// `(after N messages.get calls, token, message)` — the "mail landed while
    /// the first sync was still running" case.
    deliver_after_gets: Mutex<Vec<(usize, String, FakeMessage)>>,
    gets_served: Mutex<usize>,
}

impl FakeGmail {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            accounts: Mutex::new(HashMap::new()),
            deliver_after_gets: Mutex::new(Vec::new()),
            gets_served: Mutex::new(0),
        })
    }

    fn install(&self, token: &str, mailbox: Mailbox) {
        self.accounts.lock().unwrap().insert(token.into(), mailbox);
    }

    fn with<T>(&self, token: &str, f: impl FnOnce(&mut Mailbox) -> T) -> T {
        let mut accounts = self.accounts.lock().unwrap();
        f(accounts.get_mut(token).expect("unknown token"))
    }

    fn deliver_after(&self, gets: usize, token: &str, message: FakeMessage) {
        self.deliver_after_gets
            .lock()
            .unwrap()
            .push((gets, token.into(), message));
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

        // The calendar is switched off in every config here; answer anything
        // that is not Gmail with an empty list rather than a route error.
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

            ["messages", id] => {
                let served = {
                    let mut n = self.gets_served.lock().unwrap();
                    *n += 1;
                    *n
                };
                let due: Vec<FakeMessage> = self
                    .deliver_after_gets
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(at, tok, _)| *at == served && tok == &token)
                    .map(|(_, _, m)| m.clone())
                    .collect();
                for message in due {
                    mailbox.deliver(message);
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

fn new_engine(db: &Db, gmail: Arc<FakeGmail>) -> SyncEngine {
    let clients = TransportClients::new(gmail, |account| {
        Arc::new(StaticTokenProvider::new(format!("tok-{}", account.id))) as Arc<dyn TokenProvider>
    })
    .with_base_urls(GMAIL_BASE, CALENDAR_BASE)
    .with_retry_policy(RetryPolicy::none());
    SyncEngine::new(db.clone(), Arc::new(clients), mail_config()).expect("engine")
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

/// A database on disk, so a test can close it and open it again the way a
/// relaunch does.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mach-notify-test-{}-{}/mach.sqlite3",
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

/// Everything Mach has said, as it is stored: the ring of message keys behind
/// [`notify::NOTIFIED_KEY`]. This is the observable the engine tests assert on,
/// because it is written in the same transaction as the decision and is
/// therefore exactly "what was announced" — no platform, no timing, no globals.
fn spoken_about(db: &Db) -> Vec<String> {
    db.read(|conn| Ok(prefs::get(conn, notify::NOTIFIED_KEY)?))
        .expect("read")
        .as_ref()
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
    .expect("history id")
}

// ===========================================================================
// The engine: what a first sync must not do
// ===========================================================================

/// The failure this whole feature is arranged around. A new account's backfill
/// stores the year; if any of it counted as an arrival, adding a mailbox would
/// mean thirty thousand banners.
#[tokio::test]
async fn a_first_sync_of_a_full_mailbox_says_nothing() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, "alex@example.com");

    let gmail = FakeGmail::new();
    let mut mailbox = Mailbox::new("alex@example.com");
    for n in 0..40 {
        mailbox.seed(FakeMessage::new(&format!("m{n}"), &format!("Sender{n}")));
    }
    gmail.install(&format!("tok-{account_id}"), mailbox);

    let engine = new_engine(&db, Arc::clone(&gmail));
    let pass = engine.sync_once().await;

    assert!(pass.account(account_id).unwrap().is_ok(), "{pass:?}");
    assert_eq!(pass.messages_written(), 40, "the backfill must still store it all");
    assert!(
        spoken_about(&db).is_empty(),
        "a backfill announced something: {:?}",
        spoken_about(&db)
    );
}

/// The nastier half of the same failure. A backfill takes minutes or hours, and
/// the catch-up replay that follows it re-visits everything that moved while it
/// ran — which is history being *discovered*, not mail arriving.
#[tokio::test]
async fn mail_that_lands_during_the_first_sync_is_not_announced_either() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, "alex@example.com");
    let token = format!("tok-{account_id}");

    let gmail = FakeGmail::new();
    let mut mailbox = Mailbox::new("alex@example.com");
    for n in 0..10 {
        mailbox.seed(FakeMessage::new(&format!("m{n}"), &format!("Sender{n}")));
    }
    gmail.install(&token, mailbox);
    // Three messages land while the backfill is halfway through fetching.
    for (n, id) in [(3usize, "late-a"), (4, "late-b"), (5, "late-c")] {
        gmail.deliver_after(n, &token, FakeMessage::new(id, "Latecomer"));
    }

    let engine = new_engine(&db, Arc::clone(&gmail));
    let pass = engine.sync_once().await;

    assert!(pass.account(account_id).unwrap().is_ok(), "{pass:?}");
    assert!(
        spoken_about(&db).is_empty(),
        "the catch-up after a backfill announced something: {:?}",
        spoken_about(&db)
    );
}

/// And the other side of it: once the account is synced, mail arriving is news.
#[tokio::test]
async fn mail_arriving_after_the_first_sync_is_announced() {
    let temp = TempDb::new();
    let db = temp.open();
    let account_id = add_account(&db, "alex@example.com");
    let token = format!("tok-{account_id}");

    let gmail = FakeGmail::new();
    let mut mailbox = Mailbox::new("alex@example.com");
    mailbox.seed(FakeMessage::new("old", "Historic"));
    gmail.install(&token, mailbox);

    let engine = new_engine(&db, Arc::clone(&gmail));
    engine.sync_once().await;
    assert!(spoken_about(&db).is_empty(), "the backfill is still silent");

    gmail.with(&token, |m| m.deliver(FakeMessage::new("fresh", "Anna")));
    engine.sync_once().await;

    assert_eq!(spoken_about(&db), vec![format!("{account_id}:fresh")]);
}

/// A crash between announcing and committing the watermark replays the same
/// history window on the next launch. The ring is what stops that becoming a
/// second banner, and it has to survive the process to do it.
#[tokio::test]
async fn the_same_message_is_never_announced_twice_across_a_restart() {
    let temp = TempDb::new();
    let account_id;
    let token;
    let gmail = FakeGmail::new();

    let watermark_before;
    {
        let db = temp.open();
        account_id = add_account(&db, "alex@example.com");
        token = format!("tok-{account_id}");

        let mut mailbox = Mailbox::new("alex@example.com");
        mailbox.seed(FakeMessage::new("old", "Historic"));
        gmail.install(&token, mailbox);

        let engine = new_engine(&db, Arc::clone(&gmail));
        engine.sync_once().await;

        watermark_before = history_id(&db, account_id).expect("synced");
        gmail.with(&token, |m| m.deliver(FakeMessage::new("fresh", "Anna")));
        engine.sync_once().await;
        assert_eq!(spoken_about(&db), vec![format!("{account_id}:fresh")]);
    }

    // Relaunch: a new handle on the same file, and a watermark rewound to
    // before the delivery — which is precisely what a crash mid-pass leaves.
    let db = temp.open();
    db.write(|conn| queries::set_history_id(conn, account_id, Some(&watermark_before)))
        .expect("rewind");

    let engine = new_engine(&db, Arc::clone(&gmail));
    engine.sync_once().await;

    assert_eq!(
        spoken_about(&db),
        vec![format!("{account_id}:fresh")],
        "the replay announced the same message again"
    );
}

// ===========================================================================
// The store: the rule, the coalescing, and the settings
// ===========================================================================

/// A database with one account and a handful of messages already in it, ready
/// for `plan` to be asked about them by id.
struct Seeded {
    db: Db,
    account_id: i64,
}

impl Seeded {
    fn new() -> Self {
        let db = Db::open_in_memory().expect("db");
        // The per-message label list lives in the sync layer's own tables,
        // which the engine creates on construction and nothing here does.
        db.write(mach_lib::db::sync_queries::ensure_schema)
            .expect("sync schema");
        let account_id = add_account(&db, "alex@example.com");
        Seeded { db, account_id }
    }

    /// One stored message, in its own thread unless `thread` says otherwise.
    fn message(&self, id: &str, from: (&str, &str), subject: &str, labels: &[&str]) -> &Self {
        self.threaded(id, id, from, subject, labels)
    }

    fn threaded(
        &self,
        id: &str,
        thread: &str,
        from: (&str, &str),
        subject: &str,
        labels: &[&str],
    ) -> &Self {
        let account_id = self.account_id;
        self.db
            .write(|conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO threads (account_id, gmail_thread_id) VALUES (?1, ?2)",
                    rusqlite::params![account_id, thread],
                )?;
                let thread_id: i64 = conn.query_row(
                    "SELECT id FROM threads WHERE account_id = ?1 AND gmail_thread_id = ?2",
                    rusqlite::params![account_id, thread],
                    |row| row.get(0),
                )?;
                conn.execute(
                    "INSERT INTO messages (thread_id, account_id, gmail_message_id, from_name,
                                           from_email, subject, snippet, is_unread)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
                    rusqlite::params![
                        thread_id,
                        account_id,
                        id,
                        from.0,
                        from.1,
                        subject,
                        format!("preview of {id}"),
                    ],
                )?;
                mach_lib::db::sync_queries::set_message_labels(
                    conn,
                    account_id,
                    id,
                    &labels.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
            })
            .expect("seed message");
        self
    }

    fn set(&self, key: &str, value: Value) -> &Self {
        self.db
            .write(|conn| prefs::set(conn, key, &value, 0))
            .expect("write preference");
        self
    }

    fn plan(&self, ids: &[&str]) -> Option<Banner> {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        self.db
            .write(|conn| notify::plan(conn, self.account_id, &ids))
            .expect("plan")
            .map(|(banner, _, _)| banner)
    }
}

const INBOX_UNREAD: &[&str] = &["INBOX", "UNREAD"];

/// The three lines, read out of the store rather than composed by hand: the
/// preview comes from `messages.snippet`, which is the column the sync loop
/// writes and the only one of the three the rule does not already carry.
#[test]
fn a_plain_message_names_its_sender_its_subject_and_its_preview() {
    let seeded = Seeded::new();
    seeded.message("m1", ("Anna Lee", "anna@example.com"), "Lunch?", INBOX_UNREAD);

    let banner = seeded.plan(&["m1"]).expect("a banner");
    assert_eq!(banner.title, "Anna Lee");
    assert_eq!(banner.subtitle.as_deref(), Some("Lunch?"));
    assert_eq!(banner.body, "preview of m1");
}

#[test]
fn five_messages_arriving_together_are_one_banner() {
    let seeded = Seeded::new();
    for (id, name) in [
        ("m1", "Anna"),
        ("m2", "Bob"),
        ("m3", "Carol"),
        ("m4", "Dan"),
        ("m5", "Eve"),
    ] {
        seeded.message(
            id,
            (name, &format!("{}@example.com", name.to_lowercase())),
            "Hello",
            INBOX_UNREAD,
        );
    }

    let banner = seeded.plan(&["m1", "m2", "m3", "m4", "m5"]).expect("a banner");
    assert_eq!(banner.title, "5 new messages");
    assert_eq!(
        banner.subtitle.as_deref(),
        Some("Anna, Bob, Carol and 2 others")
    );
    assert_eq!(banner.body, "Hello", "the newest subject, not five of them");
}

#[test]
fn promotions_are_silent_and_a_thread_you_are_in_is_not() {
    let quiet = Seeded::new();
    quiet.message(
        "promo",
        ("A Shop", "deals@shop.example"),
        "50% off",
        &["INBOX", "UNREAD", "CATEGORY_PROMOTIONS"],
    );
    assert!(quiet.plan(&["promo"]).is_none());

    // The same category, but on a thread this account has written to.
    let loud = Seeded::new();
    loud.threaded(
        "mine",
        "shared",
        ("Alex", "alex@example.com"),
        "Where is my order?",
        &["SENT"],
    );
    loud.threaded(
        "reply",
        "shared",
        ("A Shop", "help@shop.example"),
        "Re: Where is my order?",
        &["INBOX", "UNREAD", "CATEGORY_UPDATES"],
    );
    assert_eq!(
        loud.plan(&["reply"]).expect("a banner").title,
        "A Shop",
        "a reply in a conversation you started is not bulk mail"
    );
}

#[test]
fn archived_read_and_self_sent_mail_all_stay_silent() {
    let seeded = Seeded::new();
    seeded.message("archived", ("Anna", "anna@example.com"), "Filed", &["UNREAD"]);
    seeded.message("read", ("Anna", "anna@example.com"), "Seen", &["INBOX"]);
    seeded.message(
        "mine",
        ("Alex", "alex@example.com"),
        "Sent",
        &["INBOX", "UNREAD", "SENT"],
    );
    seeded.message(
        "junk",
        ("Spammer", "x@spam.example"),
        "Hello friend",
        &["INBOX", "UNREAD", "SPAM"],
    );

    assert!(seeded.plan(&["archived", "read", "mine", "junk"]).is_none());
}

#[test]
fn the_same_message_planned_twice_only_speaks_once() {
    let seeded = Seeded::new();
    seeded.message("m1", ("Anna Lee", "anna@example.com"), "Lunch?", INBOX_UNREAD);

    assert!(seeded.plan(&["m1"]).is_some());
    assert!(
        seeded.plan(&["m1"]).is_none(),
        "the ring should have remembered it"
    );
}

#[test]
fn switching_notifications_off_silences_everything() {
    let seeded = Seeded::new();
    seeded
        .message("m1", ("Anna", "anna@example.com"), "Lunch?", INBOX_UNREAD)
        .set(notify::ENABLED_KEY, json!(false));

    assert!(seeded.plan(&["m1"]).is_none());
    assert!(
        seeded.plan(&["m1"]).is_none(),
        "and nothing was remembered, so turning it back on cannot replay it"
    );

    seeded.set(notify::ENABLED_KEY, json!(true));
    assert!(
        seeded.plan(&["m1"]).is_some(),
        "switching it back on lets the next arrival through"
    );
}

#[test]
fn one_account_can_be_muted_without_muting_the_other() {
    let seeded = Seeded::new();
    seeded
        .message("m1", ("Anna", "anna@example.com"), "Lunch?", INBOX_UNREAD)
        .set(
            notify::ACCOUNTS_KEY,
            json!({ seeded.account_id.to_string(): false, "999": true }),
        );

    assert!(seeded.plan(&["m1"]).is_none(), "this account is muted");

    seeded.set(notify::ACCOUNTS_KEY, json!({ "999": false }));
    assert!(
        seeded.plan(&["m1"]).is_some(),
        "an account that is not named is not muted"
    );
}

#[test]
fn a_message_that_is_not_in_the_store_is_skipped_rather_than_guessed_at() {
    let seeded = Seeded::new();
    seeded.message("m1", ("Anna", "anna@example.com"), "Lunch?", INBOX_UNREAD);

    let banner = seeded.plan(&["gone", "m1"]).expect("a banner");
    assert_eq!(banner.title, "Anna", "the one that exists still speaks");
}

// ===========================================================================
// The platform seam
// ===========================================================================

/// The host is process-wide, and so is the pending-open slot, so every test
/// that installs one takes this first. Without it two of them interleave and
/// each claims the other's conversation.
static SEAM: Mutex<()> = Mutex::new(());

struct Recorder {
    banners: Mutex<Vec<Banner>>,
    badges: Mutex<Vec<Option<i64>>>,
    /// What [`Host::show`] reports back — the thing that decides whether the
    /// caller arms the fallback.
    delivery: Delivery,
    /// Conversations the host was told to open, in order.
    opened: Mutex<Vec<PendingOpen>>,
    reopened: Mutex<usize>,
}

impl Recorder {
    fn new(delivery: Delivery) -> Self {
        Self {
            banners: Mutex::new(Vec::new()),
            badges: Mutex::new(Vec::new()),
            delivery,
            opened: Mutex::new(Vec::new()),
            reopened: Mutex::new(0),
        }
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new(Delivery::Unwatched)
    }
}

impl Host for Recorder {
    fn show(&self, banner: &Banner, _target: &PendingOpen) -> Delivery {
        self.banners.lock().unwrap().push(banner.clone());
        self.delivery
    }
    fn set_badge(&self, count: Option<i64>) {
        self.badges.lock().unwrap().push(count);
    }
    fn reopen(&self) {
        *self.reopened.lock().unwrap() += 1;
    }
    fn open_conversation(&self, target: &PendingOpen) {
        self.opened.lock().unwrap().push(target.clone());
    }
    fn db(&self) -> Option<Db> {
        None
    }
    fn permission(&self) -> Permission {
        Permission::Granted
    }
    fn request_permission(&self) -> Permission {
        Permission::Granted
    }
}

/// The wire from a decision to the platform, and the slot a click reads.
///
/// A host that cannot report its own clicks leaves the fallback armed, which is
/// the only thing an activation can then go through.
#[test]
fn a_banner_nothing_is_watching_leaves_a_conversation_to_open() {
    let _seam = SEAM.lock().unwrap_or_else(|p| p.into_inner());
    let recorder = Arc::new(Recorder::new(Delivery::Unwatched));
    notify::host::install(recorder.clone());
    let _ = notify::take_pending_open();

    let seeded = Seeded::new();
    seeded.message(
        "m1",
        ("Zephyr Quinnell", "zephyr@example.com"),
        "The only one of these",
        INBOX_UNREAD,
    );

    notify::announce(&seeded.db, seeded.account_id, &["m1".to_string()]);

    let spoken = recorder.banners.lock().unwrap().clone();
    assert!(
        spoken.iter().any(|b| b.title == "Zephyr Quinnell"),
        "the banner never reached the host: {spoken:?}"
    );

    let target = notify::take_pending_open().expect("a conversation to open");
    assert_eq!(target.account_id, seeded.account_id);
    assert_eq!(target.gmail_thread_id, "m1");
    assert!(
        notify::take_pending_open().is_none(),
        "claiming it twice would reopen a conversation nobody asked for"
    );
}

/// A watched banner carries its own click, so the guess must stay disarmed —
/// otherwise the next Dock click reopens what the banner already opened.
#[test]
fn a_watched_banner_does_not_also_arm_the_fallback() {
    let _seam = SEAM.lock().unwrap_or_else(|p| p.into_inner());
    let recorder = Arc::new(Recorder::new(Delivery::Watched));
    notify::host::install(recorder.clone());
    let _ = notify::take_pending_open();

    let seeded = Seeded::new();
    seeded.message(
        "m1",
        ("Wilhelmina Ashgrove", "wilhelmina@example.com"),
        "Watched",
        INBOX_UNREAD,
    );
    notify::announce(&seeded.db, seeded.account_id, &["m1".to_string()]);

    assert!(
        recorder
            .banners
            .lock()
            .unwrap()
            .iter()
            .any(|b| b.title == "Wilhelmina Ashgrove"),
        "the banner should still be shown"
    );
    assert!(
        notify::take_pending_open().is_none(),
        "the click is coming back to us; guessing as well would open it twice"
    );
}

/// A banner that never reached the screen must leave nothing to claim: a Dock
/// click should not open mail the owner was never told about.
#[test]
fn a_banner_that_was_never_shown_arms_nothing() {
    let _seam = SEAM.lock().unwrap_or_else(|p| p.into_inner());
    let recorder = Arc::new(Recorder::new(Delivery::Silent));
    notify::host::install(recorder.clone());
    let _ = notify::take_pending_open();

    let seeded = Seeded::new();
    seeded.message(
        "m1",
        ("Ptolemy Vance", "ptolemy@example.com"),
        "Unseen",
        INBOX_UNREAD,
    );
    notify::announce(&seeded.db, seeded.account_id, &["m1".to_string()]);

    assert!(notify::take_pending_open().is_none());
}

/// The click path itself: a response against one banner opens *that* banner's
/// conversation. No macOS in the loop — the assertion is on the routing.
#[cfg(target_os = "macos")]
#[test]
fn a_click_opens_the_conversation_its_own_banner_was_about() {
    let _seam = SEAM.lock().unwrap_or_else(|p| p.into_inner());
    let recorder = Arc::new(Recorder::new(Delivery::Watched));
    notify::host::install(recorder.clone());

    let target = PendingOpen {
        account_id: 7,
        thread_id: 4321,
        gmail_thread_id: "the-one-that-was-clicked".into(),
        at_ms: 0,
    };
    // Stands in for `Ok(NotificationResponse::Click)` arriving on the thread
    // that sent this banner.
    notify::mac::route_click(&target);

    let opened = recorder.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1, "exactly one conversation, not a guess");
    assert_eq!(opened[0].thread_id, 4321);
    assert_eq!(opened[0].gmail_thread_id, "the-one-that-was-clicked");
    assert_eq!(
        *recorder.reopened.lock().unwrap(),
        1,
        "the window has to come forward too"
    );
}
