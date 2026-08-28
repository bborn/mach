//! Evicting `body_html`, and getting it back.
//!
//! Most of this file is not "does the sweep work". It is "what can the sweep
//! destroy", because the answer has to be nothing: `body_html` is a cache of
//! something Gmail holds *only* for a message Gmail knows about, and for the
//! rows where that is not true — a local draft, an outbox entry, a message
//! sitting in Trash waiting to be purged — dropping the column is permanent loss
//! of something nobody can fetch back.
//!
//! So the properties under test are:
//!
//!  1. **Nothing unrecoverable is evicted.** Every id Mach minted, every draft,
//!     and everything in Trash or Spam survives a sweep that is otherwise
//!     evicting everything.
//!  2. **Nothing is ever left with neither a body nor its text.** A message with
//!     no `body_text` has one derived from its HTML in the same statement that
//!     drops the HTML, and a derivation that fails leaves the HTML alone.
//!  3. **A sender's own text is never overwritten, and the index only gains.** A
//!     search that found a message by a phrase in its body finds it after the
//!     sweep; a message that had no indexed body at all becomes findable by one.
//!  4. **The read path never waits.** An evicted message renders its text with
//!     no request, and the request that upgrades it is a second call the reader
//!     is not blocked on.
//!  5. **A failed fetch is loud and lossless.** The text stays and the reason is
//!     returned rather than swallowed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mach_lib::commands::{CommandError, GoogleClients};
use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::Db;
use mach_lib::evict::{
    self, restore_html, EvictionPolicy, Keep, MessageFacts, Plan, RestoreError, Restored,
};
use mach_lib::google::calendar::CalendarClient;
use mach_lib::google::gmail::GmailClient;
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, StaticTokenProvider, TransportError,
};
use mach_lib::ipc::render::{render_message, BodyFormat};

// ===========================================================================
// Scaffolding
// ===========================================================================

static COUNTER: AtomicU64 = AtomicU64::new(0);

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// A fixed "now" so no test depends on the wall clock.
const NOW: i64 = 1_800_000_000_000;

struct TempDb {
    path: PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mach-evict-{}-{}-{}.sqlite3",
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
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }
}

/// One message, with everything the policy looks at spelled out.
#[derive(Clone)]
struct Sample {
    gmail_message_id: String,
    internal_date: i64,
    html: Option<String>,
    text: Option<String>,
    is_draft: bool,
    labels: Vec<String>,
    subject: String,
}

impl Default for Sample {
    fn default() -> Self {
        Sample {
            gmail_message_id: format!("18f0c0ffee{}", COUNTER.fetch_add(1, Ordering::SeqCst)),
            // Two years old: comfortably past any window under test.
            internal_date: NOW - 730 * DAY_MS,
            html: Some(big_html("quarterly")),
            text: Some("The quarterly numbers are attached.".into()),
            is_draft: false,
            labels: vec!["INBOX".into()],
            subject: "Quarterly review".into(),
        }
    }
}

/// HTML big enough to clear the policy's floor, so no test accidentally
/// measures `Keep::TooSmall` when it meant to measure something else.
fn big_html(word: &str) -> String {
    format!(
        "<html><body><p>{word}</p>{}</body></html>",
        "<div style=\"padding:0\">&nbsp;</div>".repeat(200)
    )
}

fn account(db: &Db) -> i64 {
    let conn = db.writer();
    q::upsert_account(
        &conn,
        &NewAccount {
            email: "alex@example.com".into(),
            display_name: None,
            token_ref: "com.mach.mail.oauth".into(),
            colour_index: 0,
        },
    )
    .expect("account")
}

fn store(db: &Db, account_id: i64, sample: &Sample) -> i64 {
    let conn = db.writer();
    let thread_id = q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: format!("t{}", COUNTER.fetch_add(1, Ordering::SeqCst)),
            participants: vec![Participant::new("tawny@example.com")],
            subject: sample.subject.clone(),
            snippet: "…".into(),
            last_message_at: sample.internal_date,
            is_unread: false,
            message_count: 1,
            has_attachments: false,
            label_ids: sample.labels.clone(),
        },
    )
    .expect("thread");
    q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: sample.gmail_message_id.clone(),
            from: Participant {
                name: Some("Tawny".into()),
                email: "tawny@example.com".into(),
            },
            to: vec![Participant::new("alex@example.com")],
            subject: sample.subject.clone(),
            body_html: sample.html.clone(),
            body_text: sample.text.clone(),
            snippet: "…".into(),
            internal_date: sample.internal_date,
            is_draft: sample.is_draft,
            ..Default::default()
        },
    )
    .expect("message")
}

fn html_of(db: &Db, message_id: i64) -> Option<String> {
    db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT body_html FROM messages WHERE id = ?1",
                [message_id],
                |r| r.get(0),
            )
            .expect("row"))
    })
    .expect("read")
}

fn text_of(db: &Db, message_id: i64) -> Option<String> {
    db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT body_text FROM messages WHERE id = ?1",
                [message_id],
                |r| r.get(0),
            )
            .expect("row"))
    })
    .expect("read")
}

fn policy() -> EvictionPolicy {
    EvictionPolicy::default()
}

// --- the stubbed transport -------------------------------------------------

/// Gmail, scripted. Nothing in this file reaches the network; the one place a
/// request could be made goes through here, and every test asserts on how many
/// were made as well as on what came back.
struct StubGmail {
    responses: Mutex<Vec<Result<HttpResponse, TransportError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl StubGmail {
    fn new(responses: Vec<Result<HttpResponse, TransportError>>) -> Arc<Self> {
        Arc::new(StubGmail {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for StubGmail {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let mut queue = self.responses.lock().unwrap();
        let next = if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue
                .first()
                .map(|r| match r {
                    Ok(response) => Ok(response.clone()),
                    Err(e) => Err(TransportError::new(e.to_string())),
                })
                .unwrap_or_else(|| Err(TransportError::new("script exhausted")))
        };
        Box::pin(async move { next })
    }
}

/// A `GoogleClients` over one stub, for one account.
struct StubClients {
    transport: Arc<StubGmail>,
}

impl GoogleClients for StubClients {
    fn gmail(&self, _account_id: i64) -> Result<GmailClient, CommandError> {
        Ok(
            GmailClient::new(
                Arc::clone(&self.transport) as Arc<dyn HttpTransport>,
                Arc::new(StaticTokenProvider::new("test-token")),
            )
            .with_base_url("https://gmail.test/gmail/v1"),
        )
    }

    fn calendar(&self, _account_id: i64) -> Result<CalendarClient, CommandError> {
        unreachable!("no test here touches the calendar")
    }
}

fn clients(transport: &Arc<StubGmail>) -> Arc<dyn GoogleClients> {
    Arc::new(StubClients {
        transport: Arc::clone(transport),
    })
}

fn ok_json(body: String) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        status: 200,
        headers: Vec::new(),
        body: body.into_bytes(),
    })
}

fn api_error(status: u16, message: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        status,
        headers: Vec::new(),
        body: format!(r#"{{"error":{{"code":{status},"message":"{message}"}}}}"#).into_bytes(),
    })
}

/// A `messages.get?format=full` response carrying one HTML part.
fn message_with_html(id: &str, html: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    format!(
        r#"{{"id":"{id}","threadId":"t1","internalDate":"1700000000000",
            "payload":{{"mimeType":"text/html","body":{{"size":{},"data":"{}"}}}}}}"#,
        html.len(),
        URL_SAFE_NO_PAD.encode(html)
    )
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

// ===========================================================================
// The guard — what can never be evicted
// ===========================================================================

/// The facts of a message that *is* evictable, so each test below changes one
/// thing and the change is what the assertion is about.
fn evictable() -> MessageFacts {
    MessageFacts {
        id: 1,
        account_id: 1,
        gmail_message_id: "18f0c0ffee01".into(),
        internal_date: NOW - 730 * DAY_MS,
        is_draft: false,
        html_bytes: Some(90_000),
        has_text: true,
        deleted: false,
        html_restored_at: None,
    }
}

#[test]
fn the_baseline_is_actually_evictable() {
    // Every case below is "the baseline, but for one field". If the baseline
    // were kept for some unrelated reason, all of them would pass vacuously.
    assert_eq!(evict::retention_reason(&evictable(), NOW, &policy()), None);
}

#[test]
fn a_message_id_mach_minted_is_never_evicted() {
    // The three spellings, all of them via `is_local_message_id`, which is the
    // codebase's single answer to "is this id ours or Google's".
    for id in [
        DRAFT_ID_PREFIX.to_string() + "17",
        OUTBOX_ID_PREFIX.to_string() + "3",
        LOCAL_ID_PREFIX.to_string() + "whatever",
        // The empty id counts: a row with no id is unaddressable for the same
        // reason, and `NOT LIKE 'mach-%'` in the candidate SQL does not catch it.
        String::new(),
    ] {
        let facts = MessageFacts {
            gmail_message_id: id.clone(),
            ..evictable()
        };
        assert_eq!(
            evict::retention_reason(&facts, NOW, &policy()),
            Some(Keep::Unrecoverable),
            "id {id:?} must be refused before anything else is considered"
        );
    }
}

#[test]
fn a_draft_is_never_evicted_even_with_a_real_gmail_id() {
    // A draft written on the web arrives through ordinary sync with a real
    // message id, so the id test does not cover it. It is unsent text the owner
    // is in the middle of.
    let facts = MessageFacts {
        is_draft: true,
        ..evictable()
    };
    assert_eq!(
        evict::retention_reason(&facts, NOW, &policy()),
        Some(Keep::Draft)
    );
}

#[test]
fn trash_and_spam_are_never_evicted() {
    // Gmail purges both after thirty days. The body is recoverable today and
    // not next month, and nobody is watching that date.
    let facts = MessageFacts {
        deleted: true,
        ..evictable()
    };
    assert_eq!(
        evict::retention_reason(&facts, NOW, &policy()),
        Some(Keep::Deleted)
    );
}

#[test]
fn a_message_with_no_plain_text_has_its_text_derived_first() {
    // The read path rests on `body_text` being there, and for most of the mail
    // on this store it is not — HTML-only is what the majority of senders ship.
    // So the plan for those rows is to write one, from the HTML, before the HTML
    // goes. It is not a refusal and it is not an unguarded eviction.
    let facts = MessageFacts {
        has_text: false,
        ..evictable()
    };
    assert_eq!(evict::plan(&facts, NOW, &policy()), Plan::Derive);
    assert_eq!(
        evict::retention_reason(&facts, NOW, &policy()),
        None,
        "a row waiting on a derivation is not being kept"
    );
}

#[test]
fn deriving_text_never_reaches_a_message_that_could_not_be_fetched_back() {
    // The order in `plan`: every unrecoverable reason is answered before the
    // question of text is asked. Otherwise a draft with no text part would be
    // offered for derivation, and the only thing standing between that and an
    // evicted draft would be the derivation failing.
    for (label, facts) in [
        (
            "a local id",
            MessageFacts {
                gmail_message_id: format!("{OUTBOX_ID_PREFIX}4"),
                has_text: false,
                ..evictable()
            },
        ),
        (
            "a draft",
            MessageFacts {
                is_draft: true,
                has_text: false,
                ..evictable()
            },
        ),
        (
            "a trashed message",
            MessageFacts {
                deleted: true,
                has_text: false,
                ..evictable()
            },
        ),
        (
            "a recent message",
            MessageFacts {
                internal_date: NOW - 3 * DAY_MS,
                has_text: false,
                ..evictable()
            },
        ),
        (
            "a small body",
            MessageFacts {
                html_bytes: Some(400),
                has_text: false,
                ..evictable()
            },
        ),
    ] {
        assert!(
            matches!(evict::plan(&facts, NOW, &policy()), Plan::Keep(_)),
            "{label} with no text must be kept, not derived"
        );
    }
}

#[test]
fn a_recent_message_is_untouched() {
    let facts = MessageFacts {
        internal_date: NOW - 3 * DAY_MS,
        ..evictable()
    };
    assert_eq!(
        evict::retention_reason(&facts, NOW, &policy()),
        Some(Keep::TooRecent)
    );
}

#[test]
fn a_message_opened_recently_stays_resident() {
    // The read signal, taken where it is free: on the re-fetch, which is
    // already a write, rather than on every open.
    let facts = MessageFacts {
        html_restored_at: Some(NOW - DAY_MS),
        ..evictable()
    };
    assert_eq!(
        evict::retention_reason(&facts, NOW, &policy()),
        Some(Keep::RecentlyRestored)
    );

    // And is eligible again once that has worn off.
    let stale = MessageFacts {
        html_restored_at: Some(NOW - 60 * DAY_MS),
        ..evictable()
    };
    assert_eq!(evict::retention_reason(&stale, NOW, &policy()), None);
}

#[test]
fn a_small_body_is_not_worth_a_round_trip() {
    let facts = MessageFacts {
        html_bytes: Some(400),
        ..evictable()
    };
    assert_eq!(
        evict::retention_reason(&facts, NOW, &policy()),
        Some(Keep::TooSmall)
    );
}

#[test]
fn unrecoverability_is_decided_before_economics() {
    // A local draft that is also small, also recent and also has no text must
    // still report the reason that matters. Otherwise a future change to the
    // floor or the window would silently move a draft into scope.
    let facts = MessageFacts {
        gmail_message_id: DRAFT_ID_PREFIX.to_string() + "9",
        html_bytes: Some(10),
        internal_date: NOW,
        has_text: false,
        ..evictable()
    };
    assert_eq!(
        evict::retention_reason(&facts, NOW, &policy()),
        Some(Keep::Unrecoverable)
    );
}

// ===========================================================================
// The sweep, against a real store
// ===========================================================================

#[test]
fn an_old_message_loses_its_html_and_keeps_its_text() {
    let db = TempDb::new("old");
    let account_id = account(&db);
    let id = store(&db, account_id, &Sample::default());

    let report = evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert_eq!(report.evicted, 1);
    assert!(report.bytes_freed > 2048, "bytes: {}", report.bytes_freed);
    assert_eq!(html_of(&db, id), None, "the HTML is gone");
    assert_eq!(
        text_of(&db, id).as_deref(),
        Some("The quarterly numbers are attached."),
        "a body the sender wrote is left exactly as it was"
    );
    assert_eq!(
        report.derived, 0,
        "and nothing was derived, because nothing needed to be"
    );
}

#[test]
fn the_sweep_leaves_everything_unrecoverable_where_it_is() {
    // The whole guard, through the real query rather than through
    // `retention_reason` directly: a clause that is in the function and missing
    // from the SQL — or the other way round — is exactly the mistake this
    // catches.
    let db = TempDb::new("guarded");
    let account_id = account(&db);

    let local_draft = store(
        &db,
        account_id,
        &Sample {
            gmail_message_id: format!("{DRAFT_ID_PREFIX}17"),
            is_draft: true,
            ..Default::default()
        },
    );
    let outbox = store(
        &db,
        account_id,
        &Sample {
            gmail_message_id: format!("{OUTBOX_ID_PREFIX}3"),
            ..Default::default()
        },
    );
    let no_id = store(
        &db,
        account_id,
        &Sample {
            gmail_message_id: String::new(),
            ..Default::default()
        },
    );
    let gmail_draft = store(
        &db,
        account_id,
        &Sample {
            is_draft: true,
            ..Default::default()
        },
    );
    let trashed = store(
        &db,
        account_id,
        &Sample {
            labels: vec!["TRASH".into()],
            ..Default::default()
        },
    );
    let spam = store(
        &db,
        account_id,
        &Sample {
            labels: vec!["SPAM".into()],
            ..Default::default()
        },
    );
    let recent = store(
        &db,
        account_id,
        &Sample {
            internal_date: NOW - 3 * DAY_MS,
            ..Default::default()
        },
    );
    let tiny = store(
        &db,
        account_id,
        &Sample {
            html: Some("<p>ok</p>".into()),
            ..Default::default()
        },
    );
    let ordinary = store(&db, account_id, &Sample::default());

    let report = evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert_eq!(report.evicted, 1, "only the ordinary old message");
    for (label, id) in [
        ("a local draft", local_draft),
        ("an outbox message", outbox),
        ("a row with no id", no_id),
        ("a Gmail-side draft", gmail_draft),
        ("a trashed message", trashed),
        ("a spam message", spam),
        ("a recent message", recent),
        ("a tiny body", tiny),
    ] {
        assert!(
            html_of(&db, id).is_some(),
            "{label} must keep its HTML, and did not"
        );
    }
    assert_eq!(html_of(&db, ordinary), None);
}

#[test]
fn a_sweep_never_rewrites_a_body_the_sender_wrote() {
    // The sweep writes `body_text`, and only ever into a row that has none — a
    // sender's own text is never replaced by a machine's reading of their
    // markup. When Mach wants that reading indexed as well it goes in
    // `search_text`, which is a different column and is nobody's body. Asserted
    // as a property of the whole store rather than of one row, so a future
    // clause that "fixes up" a body while it is there has to fail this.
    let db = TempDb::new("columns");
    let account_id = account(&db);
    for n in 0..5 {
        store(
            &db,
            account_id,
            &Sample {
                subject: format!("Subject {n}"),
                text: Some(format!("body number {n}")),
                ..Default::default()
            },
        );
    }
    let before: Vec<(String, Option<String>)> = db
        .read(|conn| {
            Ok(conn
                .prepare("SELECT subject, body_text FROM messages ORDER BY id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read");

    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let after: Vec<(String, Option<String>)> = db
        .read(|conn| {
            Ok(conn
                .prepare("SELECT subject, body_text FROM messages ORDER BY id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .expect("read");

    assert_eq!(before, after);
}

// ===========================================================================
// HTML-only mail
// ===========================================================================
//
// The rows the first version of this module refused, which on the owner's store
// was 12 481 of 12 494 candidates. They have no `body_text`, so there was
// nothing to render while a re-fetch was in flight — and, separately, nothing in
// `messages_fts` either: those messages were findable by subject alone.

/// A body with prose in it and no `text/plain` part, which is what most HTML
/// mail is: a table of spacer cells with some words somewhere inside.
fn html_only(prose: &str) -> Sample {
    Sample {
        html: Some(format!(
            "<html><head><style>.x {{ color: #ff0000 }}</style></head><body>\
             <table><tr><td><p>{prose}</p></td></tr></table>{}</body></html>",
            "<div style=\"padding:0\">&nbsp;</div>".repeat(200)
        )),
        text: None,
        ..Default::default()
    }
}

fn derived_at_of(db: &Db, message_id: i64) -> Option<i64> {
    db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT body_text_derived_at FROM messages WHERE id = ?1",
                [message_id],
                |r| r.get(0),
            )
            .expect("row"))
    })
    .expect("read")
}

#[test]
fn an_html_only_message_gains_text_and_then_loses_its_html() {
    let db = TempDb::new("derive");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &html_only("The pangolin invoice is overdue."),
    );
    assert_eq!(text_of(&db, id), None, "it starts with no text at all");
    let html_bytes = html_of(&db, id).expect("resident").len() as u64;

    let report = evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert_eq!(report.evicted, 1);
    assert_eq!(report.derived, 1);
    assert_eq!(html_of(&db, id), None, "the HTML is gone");
    assert_eq!(
        text_of(&db, id).as_deref(),
        Some("The pangolin invoice is overdue."),
        "and the text it never had is there"
    );
    assert_eq!(
        derived_at_of(&db, id),
        Some(NOW),
        "marked as text Mach wrote, not text the sender sent"
    );

    // The stylesheet was in the body and is not in the text.
    assert!(!text_of(&db, id).unwrap().contains("color"));

    // Net, because storing the text costs some of what dropping the HTML saved.
    // Reporting the gross is how a sweep that writes back nearly as much as it
    // drops would still look like a win.
    assert_eq!(report.bytes_written, text_of(&db, id).unwrap().len() as u64);
    assert_eq!(
        report.bytes_freed + report.bytes_written,
        html_bytes,
        "bytes_freed is the difference, not the gross"
    );
}

#[test]
fn a_derivation_that_produces_nothing_leaves_the_html_alone() {
    // The failure case, and the reason the write is conditional rather than
    // ordered: a body that is one image with no `alt` has no text in it, so
    // there is nothing to fall back to and the HTML has to stay.
    let db = TempDb::new("underivable");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &Sample {
            html: Some(format!(
                "<html><body>{}</body></html>",
                "<img src=\"https://cdn.test/pixel.png\">".repeat(200)
            )),
            text: None,
            ..Default::default()
        },
    );

    let report = evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert_eq!(report.evicted, 0);
    assert_eq!(report.kept_count(Keep::NoText), 1);
    assert!(
        html_of(&db, id).is_some(),
        "the body is still there to be read"
    );
    assert_eq!(text_of(&db, id), None);
    assert_eq!(derived_at_of(&db, id), None);
}

#[test]
fn a_body_that_is_almost_all_text_is_not_worth_evicting() {
    // Dropping 3 KB of markup to store 2.9 KB of text frees nothing and costs a
    // round trip on the next open.
    let db = TempDb::new("nogain");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &Sample {
            html: Some(format!("<p>{}</p>", "prose ".repeat(700))),
            text: None,
            ..Default::default()
        },
    );

    let report = evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert_eq!(report.evicted, 0);
    assert_eq!(report.kept_count(Keep::NoGain), 1);
    assert!(html_of(&db, id).is_some());
    assert_eq!(text_of(&db, id), None, "and nothing was written either");
}

#[test]
fn a_derived_body_has_a_ceiling() {
    // Derived text is written back into the store the sweep is shrinking, so it
    // cannot be allowed to depend on the sender's markup being sane.
    let huge = format!("<p>{}</p>", "pangolin ".repeat(200_000));
    let derived = evict::derive_text(&huge).expect("there is prose in it");

    assert!(
        derived.len() <= evict::MAX_DERIVED_BYTES,
        "{} bytes",
        derived.len()
    );
    assert!(derived.starts_with("pangolin pangolin"));
}

#[test]
fn no_message_is_ever_left_with_neither_a_body_nor_its_text() {
    // The ordering guarantee, enforced by SQLite rather than asserted after the
    // fact. This trigger fires once per UPDATE *statement*, inside the sweep's
    // own transaction, so it sees every intermediate state a crash could freeze
    // — and aborts on the one state that must never exist. Deriving in a second
    // statement after the eviction would trip it; so would evicting a row whose
    // derivation came back empty.
    let db = TempDb::new("atomic");
    let account_id = account(&db);
    {
        let conn = db.writer();
        conn.execute_batch(
            "CREATE TEMP TRIGGER bodyless AFTER UPDATE ON messages
             WHEN new.body_html IS NULL
              AND new.html_evicted_at IS NOT NULL
              AND (new.body_text IS NULL OR trim(new.body_text) = '')
             BEGIN
               SELECT RAISE(ABORT, 'a message was left with neither its HTML nor any text');
             END;",
        )
        .expect("trigger");
    }

    let with_text = store(&db, account_id, &Sample::default());
    let derived = store(&db, account_id, &html_only("Quarterly numbers attached."));
    let underivable = store(
        &db,
        account_id,
        &Sample {
            html: Some(format!(
                "<html><body>{}</body></html>",
                "<img src=\"https://cdn.test/pixel.png\">".repeat(200)
            )),
            text: None,
            ..Default::default()
        },
    );

    let report = evict::sweep(&db, NOW, &policy()).expect("the sweep must not abort");

    assert_eq!(report.evicted, 2);
    assert_eq!(html_of(&db, with_text), None);
    assert_eq!(html_of(&db, derived), None);
    assert!(text_of(&db, derived).is_some());
    assert!(
        html_of(&db, underivable).is_some(),
        "the one that could not be derived kept its body"
    );
}

#[test]
fn a_sender_who_sends_text_later_is_not_overwritten() {
    // The write's own `body_text IS NULL OR trim(body_text) = ''`, which is what
    // makes the derivation safe to do outside the transaction: a sync that fills
    // in a real text part between the read and the write keeps it, and the row
    // is simply not evicted this time round.
    let db = TempDb::new("race");
    let account_id = account(&db);
    let id = store(&db, account_id, &html_only("Derived words."));

    // Stand in for the sync that landed in between.
    db.write(|conn| {
        conn.execute(
            "UPDATE messages SET body_text = 'The sender said this' WHERE id = ?1",
            [id],
        )?;
        Ok(())
    })
    .expect("sync write");

    evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert_eq!(
        text_of(&db, id).as_deref(),
        Some("The sender said this"),
        "the sender's own text survived"
    );
}

#[test]
fn search_finds_a_message_by_text_derived_from_its_html() {
    // The second half of the fix, and the part that is not about disk. These
    // messages were in `messages_fts` under their subject and nothing else;
    // `body_text` is the only body column the index has ever had, and they had
    // none. Deriving one is the only way the phrase becomes findable.
    let db = TempDb::new("derived-search");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &Sample {
            subject: "Weekly digest".into(),
            ..html_only("The pangolin invoice is overdue.")
        },
    );

    let before = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
        .expect("search");
    assert!(
        before.is_empty(),
        "the phrase is in the HTML, and the HTML was never indexed"
    );

    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let after = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
        .expect("search");
    assert_eq!(after.len(), 1, "and now it is findable");
    assert_eq!(html_of(&db, id), None, "having also freed the markup");
}

#[test]
fn derived_text_renders_while_the_html_is_being_fetched_back() {
    let db = TempDb::new("derived-read");
    let account_id = account(&db);
    let id = store(&db, account_id, &html_only("Quarterly numbers attached."));
    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let rendered = render_message(&db, id, false).expect("render");

    assert_eq!(rendered.format, BodyFormat::Text);
    assert!(
        rendered.body.html.contains("Quarterly numbers attached."),
        "{}",
        rendered.body.html
    );
    assert!(rendered.html_evicted, "and the upgrade is still offered");
}

#[test]
fn a_second_sweep_does_not_re_derive_what_it_already_wrote() {
    let db = TempDb::new("derive-twice");
    let account_id = account(&db);
    let id = store(&db, account_id, &html_only("Once only."));

    assert_eq!(evict::sweep(&db, NOW, &policy()).expect("first").derived, 1);
    let second = evict::sweep(&db, NOW, &policy()).expect("second");

    assert_eq!(second.examined, 0);
    assert_eq!(second.derived, 0);
    assert_eq!(text_of(&db, id).as_deref(), Some("Once only."));
}

#[test]
fn search_still_finds_an_evicted_message_by_its_body() {
    // `messages_fts` is external-content over (subject, body_text). For a
    // message that came with its own text, eviction writes neither, so this is a
    // check that the trigger's delete-and-reinsert on the UPDATE leaves the
    // index able to answer.
    let db = TempDb::new("search");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &Sample {
            text: Some("The pangolin invoice is overdue".into()),
            ..Default::default()
        },
    );

    let before = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
        .expect("search");
    assert_eq!(before.len(), 1, "the phrase is findable to begin with");

    evict::sweep(&db, NOW, &policy()).expect("sweep");
    assert_eq!(html_of(&db, id), None, "the message really was evicted");

    let after = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
        .expect("search");
    assert_eq!(after.len(), 1, "and is still findable by the same phrase");
    assert_eq!(before[0].id, after[0].id);
}

#[test]
fn a_second_sweep_finds_nothing_left_to_do() {
    // Idempotence, and the thing that would break it: a row whose HTML is gone
    // must not be a candidate, or every sweep would re-stamp the whole mailbox
    // and re-churn the index for nothing.
    let db = TempDb::new("twice");
    let account_id = account(&db);
    for _ in 0..3 {
        store(&db, account_id, &Sample::default());
    }

    assert_eq!(evict::sweep(&db, NOW, &policy()).expect("first").evicted, 3);
    let second = evict::sweep(&db, NOW, &policy()).expect("second");
    assert_eq!(second.evicted, 0);
    assert_eq!(second.examined, 0);
}

#[test]
fn the_sweep_batches_without_skipping_rows() {
    // The cursor is a keyset over `id`, because the sweep mutates the predicate
    // it is paginating over — an OFFSET would step past a row on every batch
    // after the first.
    let db = TempDb::new("batched");
    let account_id = account(&db);
    for _ in 0..25 {
        store(&db, account_id, &Sample::default());
    }

    let report = evict::sweep(
        &db,
        NOW,
        &EvictionPolicy {
            batch: 4,
            ..policy()
        },
    )
    .expect("sweep");

    assert_eq!(report.evicted, 25);
    let left: i64 = db
        .read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT count(*) FROM messages WHERE body_html IS NOT NULL",
                    [],
                    |r| r.get(0),
                )
                .expect("count"))
        })
        .expect("read");
    assert_eq!(left, 0);
}

// ===========================================================================
// The read path
// ===========================================================================

#[test]
fn an_evicted_message_renders_its_text_with_no_request() {
    let db = TempDb::new("render");
    let account_id = account(&db);
    let id = store(&db, account_id, &Sample::default());
    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let rendered = render_message(&db, id, false).expect("render");

    assert_eq!(rendered.format, BodyFormat::Text);
    assert!(
        rendered.body.html.contains("The quarterly numbers are attached."),
        "the text is on screen: {}",
        rendered.body.html
    );
    assert!(
        rendered.html_evicted,
        "and the frontend is told an upgrade is coming"
    );
}

#[test]
fn a_message_that_never_had_html_is_not_marked_for_a_fetch() {
    // The distinction the `html_evicted_at` column exists for. Both render as
    // text; only one is worth a request, and getting this wrong would cost a
    // round trip on every open of every plain-text message forever.
    let db = TempDb::new("plain");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &Sample {
            html: None,
            ..Default::default()
        },
    );

    let rendered = render_message(&db, id, false).expect("render");
    assert_eq!(rendered.format, BodyFormat::Text);
    assert!(!rendered.html_evicted);
}

#[test]
fn the_refetch_upgrades_an_evicted_message() {
    let db = TempDb::new("refetch");
    let account_id = account(&db);
    let sample = Sample::default();
    let id = store(&db, account_id, &sample);
    evict::sweep(&db, NOW, &policy()).expect("sweep");
    assert_eq!(html_of(&db, id), None);

    let transport = StubGmail::new(vec![ok_json(message_with_html(
        &sample.gmail_message_id,
        "<p>The deck is attached.</p>",
    ))]);
    let clients = clients(&transport);

    let restored = block_on(restore_html(&db, &clients, id)).expect("restore");

    assert!(matches!(restored, Restored::Fetched { .. }));
    assert_eq!(transport.calls(), 1);
    assert!(html_of(&db, id)
        .expect("html is back")
        .contains("The deck is attached."));

    let rendered = render_message(&db, id, false).expect("render");
    assert_eq!(rendered.format, BodyFormat::Html);
    assert!(!rendered.html_evicted, "no second fetch is offered");
    assert!(rendered.body.html.contains("The deck is attached."));
}

#[test]
fn opening_an_evicted_message_twice_costs_one_fetch() {
    let db = TempDb::new("cached");
    let account_id = account(&db);
    let sample = Sample::default();
    let id = store(&db, account_id, &sample);
    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let transport = StubGmail::new(vec![ok_json(message_with_html(
        &sample.gmail_message_id,
        "<p>Once.</p>",
    ))]);
    let clients = clients(&transport);

    block_on(restore_html(&db, &clients, id)).expect("first");
    let second = block_on(restore_html(&db, &clients, id)).expect("second");

    assert_eq!(second, Restored::AlreadyResident);
    assert_eq!(transport.calls(), 1, "the second open made no request");
}

#[test]
fn a_restored_body_is_not_evicted_again_the_same_day() {
    // What `html_restored_at` is for. Without it a message opened this morning
    // is evicted this afternoon and re-fetched tomorrow, forever.
    let db = TempDb::new("residency");
    let account_id = account(&db);
    let sample = Sample::default();
    let id = store(&db, account_id, &sample);
    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let transport = StubGmail::new(vec![ok_json(message_with_html(
        &sample.gmail_message_id,
        &big_html("restored"),
    ))]);
    block_on(restore_html(&db, &clients(&transport), id)).expect("restore");

    // A sweep an hour later leaves it alone; one two months later does not.
    evict::sweep(&db, evict_now() + 60 * 60 * 1000, &policy()).expect("soon");
    assert!(html_of(&db, id).is_some(), "still resident");

    evict::sweep(&db, evict_now() + 60 * DAY_MS, &policy()).expect("later");
    assert_eq!(html_of(&db, id), None, "eligible again once it has aged");
}

/// `restore_html` stamps with the wall clock, so the sweeps above have to be
/// expressed relative to it rather than to the fixed `NOW`.
fn evict_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ===========================================================================
// Failure
// ===========================================================================

#[test]
fn a_message_gmail_no_longer_has_leaves_the_text_and_says_so() {
    let db = TempDb::new("gone");
    let account_id = account(&db);
    let id = store(&db, account_id, &Sample::default());
    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let transport = StubGmail::new(vec![api_error(404, "Requested entity was not found.")]);
    let error = block_on(restore_html(&db, &clients(&transport), id)).expect_err("gone");

    assert!(matches!(error, RestoreError::Gone), "got {error:?}");
    assert_eq!(error.kind(), "gone");
    assert!(
        error.to_string().contains("no longer in Gmail"),
        "the sentence has to be fit for the reading pane: {error}"
    );
    // And the message is still readable.
    assert_eq!(
        text_of(&db, id).as_deref(),
        Some("The quarterly numbers are attached.")
    );
    let rendered = render_message(&db, id, false).expect("render");
    assert_eq!(rendered.format, BodyFormat::Text);
    assert!(rendered.body.html.contains("quarterly numbers"));
}

#[test]
fn a_network_failure_leaves_the_text_and_says_so() {
    let db = TempDb::new("offline");
    let account_id = account(&db);
    let id = store(&db, account_id, &Sample::default());
    evict::sweep(&db, NOW, &policy()).expect("sweep");

    let transport = StubGmail::new(vec![Err(TransportError::new("connection refused"))]);
    let error = block_on(restore_html(&db, &clients(&transport), id)).expect_err("offline");

    assert_eq!(error.kind(), "google");
    assert!(error.to_string().contains("could not reach Gmail"));
    let rendered = render_message(&db, id, false).expect("render");
    assert_eq!(rendered.format, BodyFormat::Text);
    assert!(
        rendered.html_evicted,
        "still evicted, so the next open tries again"
    );
}

#[test]
fn a_local_id_is_refused_before_a_request_is_made() {
    // Belt and braces on the sweep's guard. Nothing local can be evicted, so
    // this state is unreachable — and if it is ever reached, the answer is a
    // sentence rather than a 404 from a request that could not work.
    let db = TempDb::new("local");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &Sample {
            gmail_message_id: format!("{OUTBOX_ID_PREFIX}9"),
            html: None,
            ..Default::default()
        },
    );
    db.write(|conn| {
        conn.execute(
            "UPDATE messages SET html_evicted_at = ?2 WHERE id = ?1",
            rusqlite::params![id, NOW],
        )?;
        Ok(())
    })
    .expect("forge an evicted local row");

    let transport = StubGmail::new(vec![]);
    let error = block_on(restore_html(&db, &clients(&transport), id)).expect_err("refused");

    assert!(matches!(error, RestoreError::Unrecoverable));
    assert_eq!(transport.calls(), 0, "no request was made");
}

// ===========================================================================
// Reclaiming the pages
// ===========================================================================

#[test]
fn eviction_frees_pages_and_only_a_vacuum_returns_them() {
    // The whole point of `reclaim` existing, as a test: NULLing a column moves
    // pages onto the free list and leaves the file exactly as large as it was.
    let db = TempDb::new("reclaim");
    let account_id = account(&db);
    for n in 0..200 {
        store(
            &db,
            account_id,
            &Sample {
                html: Some(big_html(&format!("filler {n}")).repeat(4)),
                ..Default::default()
            },
        );
    }

    let before = evict::free_space(&db).expect("space");
    evict::sweep(&db, NOW, &policy()).expect("sweep");
    let after_sweep = evict::free_space(&db).expect("space");

    assert!(
        after_sweep.freelist_count > before.freelist_count,
        "the sweep put pages on the free list: {} → {}",
        before.freelist_count,
        after_sweep.freelist_count
    );
    assert_eq!(
        after_sweep.page_count, before.page_count,
        "and the file is the same size"
    );

    let report = evict::reclaim(&db).expect("vacuum");

    assert_eq!(
        report.after.freelist_count, 0,
        "the vacuum consumed the free list"
    );
    assert!(
        report.bytes_returned() > 0,
        "and gave the bytes back: {} → {}",
        report.before.file_bytes(),
        report.after.file_bytes()
    );
}

#[test]
fn a_vacuum_does_not_lose_a_message_or_the_index() {
    let db = TempDb::new("vacuum-safe");
    let account_id = account(&db);
    let id = store(
        &db,
        account_id,
        &Sample {
            text: Some("The pangolin invoice is overdue".into()),
            ..Default::default()
        },
    );
    evict::sweep(&db, NOW, &policy()).expect("sweep");
    evict::reclaim(&db).expect("vacuum");

    assert_eq!(
        text_of(&db, id).as_deref(),
        Some("The pangolin invoice is overdue")
    );
    let found = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
        .expect("search");
    assert_eq!(found.len(), 1, "the index survived the rewrite");
}

// ===========================================================================
// The markup's text, and where it goes when the markup does not
// ===========================================================================
//
// `messages_fts` indexes `subject`, `body_text` and `search_text` (migration
// 23). The sweep does not fill that third column — sync does, when a message
// arrives — but it owns the one invariant between them: the two columns never
// hold the same text, because `messages_fts` would then carry every one of its
// terms twice.

fn search_text_of(db: &Db, message_id: i64) -> Option<String> {
    db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT search_text FROM messages WHERE id = ?1",
                [message_id],
                |r| r.get(0),
            )
            .expect("row"))
    })
    .expect("read")
}

fn set_search_text(db: &Db, message_id: i64, text: &str) {
    db.write(|conn| {
        conn.execute(
            "UPDATE messages SET search_text = ?2 WHERE id = ?1",
            rusqlite::params![message_id, text],
        )?;
        Ok(())
    })
    .expect("set search_text");
}

#[test]
fn deriving_a_body_moves_the_markups_text_out_of_search_text() {
    // An HTML-only message arrives, so sync puts the markup's text in
    // `search_text`. Ninety days later the sweep writes that same text into
    // `body_text` — a body to render while the re-fetch is in flight — and the
    // column it came from is cleared in the same statement.
    let db = TempDb::new("search-text-moves");
    let account_id = account(&db);
    let id = store(&db, account_id, &html_only("Quarterly pangolin numbers attached."));
    set_search_text(&db, id, "Quarterly pangolin numbers attached.");

    let before = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
        .expect("search");
    assert_eq!(before.len(), 1, "findable before the sweep");

    evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert!(text_of(&db, id).expect("text").contains("pangolin"));
    assert_eq!(search_text_of(&db, id), None, "not stored twice");
    assert_eq!(derived_at_of(&db, id), Some(NOW));

    let after = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
        .expect("search");
    assert_eq!(after.len(), 1, "and still findable after it");
}

#[test]
fn an_ordinary_eviction_leaves_search_text_where_it_is() {
    // The row has a body of its own, so the sweep only drops the markup. The
    // markup's text is still the only place some of its words are.
    let db = TempDb::new("search-text-stays");
    let account_id = account(&db);
    let id = store(&db, account_id, &Sample::default());
    set_search_text(&db, id, "Pangolin conservation update, from the footer.");

    evict::sweep(&db, NOW, &policy()).expect("sweep");

    assert_eq!(html_of(&db, id), None);
    assert_eq!(
        search_text_of(&db, id).as_deref(),
        Some("Pangolin conservation update, from the footer.")
    );
    assert_eq!(
        db.read(|conn| q::search_thread_summaries(conn, "pangolin", 10))
            .expect("search")
            .len(),
        1
    );
}

#[test]
fn a_refetch_fills_the_search_text_nothing_else_could_reach() {
    // A message evicted before any of this shipped: its markup was gone, so
    // neither sync nor the backfill could read it. Opening it brings the markup
    // back, and the derivation runs on the way past.
    let db = TempDb::new("refetch-search-text");
    let account_id = account(&db);
    let sample = Sample::default();
    let id = store(&db, account_id, &sample);
    evict::sweep(&db, NOW, &policy()).expect("sweep");
    assert_eq!(search_text_of(&db, id), None);
    assert!(db
        .read(|conn| q::search_thread_summaries(conn, "armadillo", 10))
        .expect("search")
        .is_empty());

    let transport = StubGmail::new(vec![ok_json(message_with_html(
        &sample.gmail_message_id,
        "<p>The armadillo brush ships Tuesday from the Leeds warehouse.</p>",
    ))]);
    let clients = clients(&transport);

    block_on(restore_html(&db, &clients, id)).expect("restore");

    assert!(search_text_of(&db, id).expect("filled").contains("armadillo"));
    assert_eq!(
        db.read(|conn| q::search_thread_summaries(conn, "armadillo", 10))
            .expect("search")
            .len(),
        1,
        "and now it is findable by what its markup said"
    );
    // The sender's own body is untouched, and the HTML it went for landed.
    assert_eq!(
        text_of(&db, id).as_deref(),
        Some("The quarterly numbers are attached.")
    );
    assert!(html_of(&db, id).expect("html is back").contains("armadillo"));
}

#[test]
fn a_refetch_adds_nothing_to_a_body_the_sweep_already_derived() {
    // The row's `body_text` *is* this markup's text. Storing it again in
    // `search_text` would put every one of its terms in the index twice.
    let db = TempDb::new("refetch-already-derived");
    let account_id = account(&db);
    let sample = html_only("Quarterly pangolin numbers attached.");
    let id = store(&db, account_id, &sample);
    evict::sweep(&db, NOW, &policy()).expect("sweep");
    assert!(derived_at_of(&db, id).is_some());

    let transport = StubGmail::new(vec![ok_json(message_with_html(
        &sample.gmail_message_id,
        "<p>Quarterly pangolin numbers attached.</p>",
    ))]);
    let clients = clients(&transport);

    block_on(restore_html(&db, &clients, id)).expect("restore");

    assert_eq!(search_text_of(&db, id), None);
}
