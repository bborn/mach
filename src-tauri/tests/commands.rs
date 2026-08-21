//! Behaviour tests for the typed command layer.
//!
//! No network: every test drives a scripted `HttpTransport` (the same injection
//! seam `tests/google.rs` uses) against a real in-memory SQLite database.
//!
//! The load-bearing tests are the ones that pin the *design claims* rather than
//! the mechanics:
//!
//!  * `local_change_lands_before_the_remote_call` — the transport reads the
//!    database at the moment the request goes out and finds the change already
//!    committed. This is the whole reason the command layer exists.
//!  * `remote_failure_rolls_the_local_change_back_exactly` — the store never
//!    keeps a write Google refused.
//!  * `undoing_an_archive_restores_every_label_not_just_inbox` — undo is
//!    correct, not approximate.
//!  * `fifty_threads_issue_one_batched_call` — bulk triage is one round trip.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use mach_lib::commands::{
    AccountClients, Command, CommandDispatcher, Conferencing, EventDraft, EventPatch, EventScope,
    FailureKind, Notify, ThreadLabelState,
};
use mach_lib::db::models::{
    Event, EventReminder, EventReminders, LabelType, NewAccount, NewEvent, NewLabel, NewMessage,
    NewThread, Participant, RsvpStatus,
};
use mach_lib::db::{queries, Db};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, RetryPolicy, StaticTokenProvider,
    TransportError,
};

// ============================================================== test doubles

/// Replays a scripted list of responses, records every request, and — crucially
/// — can snapshot the local database *at the moment the request is made*. That
/// snapshot is what proves the local write happened first.
struct FakeTransport {
    responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    default: Mutex<Option<Result<HttpResponse, TransportError>>>,
    requests: Mutex<Vec<HttpRequest>>,
    /// Database + thread id to observe on every request.
    observe: Mutex<Option<(Db, i64)>>,
    /// Labels seen on that thread, one entry per request.
    observed: Mutex<Vec<Vec<String>>>,
    /// Database to count event rows in on every request — the calendar half of
    /// the same local-first claim `observe` makes for mail.
    observe_events: Mutex<Option<Db>>,
    observed_events: Mutex<Vec<usize>>,
}

impl FakeTransport {
    fn always_ok() -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::new()),
            default: Mutex::new(Some(Ok(HttpResponse::json(200, "{}")))),
            requests: Mutex::new(Vec::new()),
            observe: Mutex::new(None),
            observed: Mutex::new(Vec::new()),
            observe_events: Mutex::new(None),
            observed_events: Mutex::new(Vec::new()),
        })
    }

    fn scripted(responses: Vec<Result<HttpResponse, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            default: Mutex::new(Some(Ok(HttpResponse::json(200, "{}")))),
            requests: Mutex::new(Vec::new()),
            observe: Mutex::new(None),
            observed: Mutex::new(Vec::new()),
            observe_events: Mutex::new(None),
            observed_events: Mutex::new(Vec::new()),
        })
    }

    fn always_failing(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::new()),
            default: Mutex::new(Some(Ok(HttpResponse::json(status, body.to_string())))),
            requests: Mutex::new(Vec::new()),
            observe: Mutex::new(None),
            observed: Mutex::new(Vec::new()),
            observe_events: Mutex::new(None),
            observed_events: Mutex::new(Vec::new()),
        })
    }

    fn observing(self: &Arc<Self>, db: &Db, thread_id: i64) {
        *self.observe.lock().unwrap() = Some((db.clone(), thread_id));
    }

    fn observing_events(self: &Arc<Self>, db: &Db) {
        *self.observe_events.lock().unwrap() = Some(db.clone());
    }

    fn observed_events(&self) -> Vec<usize> {
        self.observed_events.lock().unwrap().clone()
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn call_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn observed(&self) -> Vec<Vec<String>> {
        self.observed.lock().unwrap().clone()
    }
}

impl HttpTransport for FakeTransport {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        if let Some((db, thread_id)) = self.observe.lock().unwrap().as_ref() {
            let labels = thread_labels(db, *thread_id);
            self.observed.lock().unwrap().push(labels);
        }
        if let Some(db) = self.observe_events.lock().unwrap().as_ref() {
            let count = all_events(db).len();
            self.observed_events.lock().unwrap().push(count);
        }
        self.requests.lock().unwrap().push(request);
        let next = self.responses.lock().unwrap().pop_front();
        let out = match next {
            Some(r) => r,
            None => self
                .default
                .lock()
                .unwrap()
                .clone()
                .expect("transport script exhausted"),
        };
        Box::pin(async move { out })
    }
}

// ================================================================== fixtures

fn dispatcher(db: &Db, transport: Arc<FakeTransport>) -> CommandDispatcher {
    let clients = AccountClients::new(transport)
        .with_gmail_base_url("https://gmail.test/gmail/v1")
        .with_calendar_base_url("https://calendar.test/calendar/v3")
        .with_retry_policy(RetryPolicy::none())
        .with_account(1, Arc::new(StaticTokenProvider::new("token-1")))
        .with_account(2, Arc::new(StaticTokenProvider::new("token-2")));
    CommandDispatcher::new(db.clone(), Arc::new(clients)).expect("dispatcher")
}

fn seed_account(db: &Db, email: &str) -> i64 {
    db.write(|c| {
        queries::upsert_account(
            c,
            &NewAccount {
                email: email.to_string(),
                display_name: None,
                token_ref: String::new(),
                colour_index: 0,
            },
        )
    })
    .unwrap()
}

/// A thread with one message and the given labels.
fn seed_thread(db: &Db, account_id: i64, gmail_thread_id: &str, labels: &[&str]) -> i64 {
    seed_thread_with_messages(db, account_id, gmail_thread_id, labels, 1)
}

fn seed_thread_with_messages(
    db: &Db,
    account_id: i64,
    gmail_thread_id: &str,
    labels: &[&str],
    messages: usize,
) -> i64 {
    db.write(|c| {
        let thread_id = queries::upsert_thread(
            c,
            &NewThread {
                account_id,
                gmail_thread_id: gmail_thread_id.to_string(),
                participants: vec![Participant::new("someone@example.com")],
                subject: format!("subject {gmail_thread_id}"),
                snippet: "snippet".into(),
                last_message_at: 1_700_000_000_000,
                is_unread: labels.contains(&"UNREAD"),
                message_count: messages as i64,
                has_attachments: false,
                label_ids: labels.iter().map(|s| s.to_string()).collect(),
            },
        )?;
        for i in 0..messages {
            queries::upsert_message(
                c,
                &NewMessage {
                    thread_id,
                    account_id,
                    gmail_message_id: format!("{gmail_thread_id}-m{i}"),
                    from: Participant::new("someone@example.com"),
                    subject: "subject".into(),
                    snippet: "snippet".into(),
                    internal_date: 1_700_000_000_000 + i as i64,
                    is_unread: labels.contains(&"UNREAD"),
                    ..Default::default()
                },
            )?;
        }
        Ok(thread_id)
    })
    .unwrap()
}

fn seed_label(db: &Db, account_id: i64, gmail_label_id: &str, name: &str) {
    db.write(|c| {
        queries::upsert_label(
            c,
            &NewLabel {
                account_id,
                gmail_label_id: gmail_label_id.to_string(),
                name: name.to_string(),
                label_type: LabelType::User,
            },
        )
        .map(|_| ())
    })
    .unwrap();
}

fn thread_labels(db: &Db, thread_id: i64) -> Vec<String> {
    let mut labels = db
        .read(|c| queries::thread_summary(c, thread_id))
        .unwrap()
        .expect("thread")
        .label_ids;
    labels.sort();
    labels
}

/// Every event row, oldest first. `events_in_range` is the only list query the
/// store exposes, so "everything" is the widest window it will take.
fn all_events(db: &Db) -> Vec<Event> {
    db.read(|c| queries::events_in_range(c, i64::MIN, i64::MAX, None))
        .unwrap()
}

fn seed_event(db: &Db, account_id: i64, new: NewEvent) -> i64 {
    db.write(|c| {
        queries::upsert_event(
            c,
            &NewEvent {
                account_id,
                ..new
            },
        )
    })
    .unwrap()
}

fn thread_is_unread(db: &Db, thread_id: i64) -> bool {
    db.read(|c| queries::thread_summary(c, thread_id))
        .unwrap()
        .expect("thread")
        .is_unread
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

fn body_json(request: &HttpRequest) -> serde_json::Value {
    serde_json::from_slice(request.body.as_deref().unwrap_or(b"null")).expect("json body")
}

fn ids_of(value: &serde_json::Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ========================================================= local-then-remote

#[tokio::test]
async fn local_change_lands_before_the_remote_call() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "UNREAD"]);

    let transport = FakeTransport::always_ok();
    transport.observing(&db, thread);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    // One request went out, and at the instant it did the local store had
    // already dropped INBOX. This is the ordering claim, asserted directly.
    assert_eq!(transport.call_count(), 1);
    assert_eq!(transport.observed(), vec![vec!["UNREAD".to_string()]]);
}

#[tokio::test]
async fn archive_applies_the_local_change_and_issues_the_right_remote_call() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(thread_labels(&db, thread), sorted(&["Receipts"]));

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    // One message in the thread, so the single-message endpoint is used.
    assert!(
        requests[0].url.ends_with("/users/me/messages/t1-m0/modify"),
        "unexpected url {}",
        requests[0].url
    );
    let body = body_json(&requests[0]);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["INBOX"]);
    assert!(ids_of(&body, "addLabelIds").is_empty());
}

#[tokio::test]
async fn mark_read_clears_the_unread_label_and_the_unread_flag() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "UNREAD"]);
    assert!(thread_is_unread(&db, thread));

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MarkRead {
            thread_ids: vec![thread],
            read: true,
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX"]));
    assert!(!thread_is_unread(&db, thread));
    let body = body_json(&transport.requests()[0]);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["UNREAD"]);
}

#[tokio::test]
async fn star_adds_the_starred_label() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    d.execute(Command::Star {
        thread_ids: vec![thread],
        starred: true,
    })
    .await
    .unwrap();

    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX", "STARRED"]));
    let body = body_json(&transport.requests()[0]);
    assert_eq!(ids_of(&body, "addLabelIds"), vec!["STARRED"]);
}

#[tokio::test]
async fn adding_and_removing_a_label_are_inverses_of_each_other() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Label_7"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Label {
            thread_ids: vec![thread],
            label_id: "Label_7".into(),
            add: false,
        })
        .await
        .unwrap();

    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX"]));
    assert_eq!(
        result.undo,
        Some(Command::Label {
            thread_ids: vec![thread],
            label_id: "Label_7".into(),
            add: true,
        }),
        "removing a label must return add-that-label"
    );

    d.execute(result.undo.unwrap()).await.unwrap();
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX", "Label_7"]));
}

// ======================================================================= inbox
//
// Mach's Inbox is Gmail's Primary: INBOX minus the bulk tabs. GitHub mail
// arrives with CATEGORY_UPDATES (and often CATEGORY_FORUMS) still on, so it
// sits in All and not Inbox until those come off.

#[tokio::test]
async fn moving_to_inbox_strips_bulk_tabs_and_keeps_inbox() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(
        &db,
        account,
        "t1",
        &["INBOX", "CATEGORY_UPDATES", "CATEGORY_FORUMS", "UNREAD"],
    );

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveToInbox {
            thread_ids: vec![thread],
            restore: Vec::new(),
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(result.message, "Moved 1 conversation to Inbox");
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX", "UNREAD"]));

    let body = body_json(&transport.requests()[0]);
    assert_eq!(ids_of(&body, "addLabelIds"), Vec::<String>::new());
    assert_eq!(
        ids_of(&body, "removeLabelIds"),
        vec!["CATEGORY_FORUMS", "CATEGORY_UPDATES"]
    );
}

#[tokio::test]
async fn moving_to_inbox_from_archive_adds_inbox() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["CATEGORY_UPDATES"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    d.execute(Command::MoveToInbox {
        thread_ids: vec![thread],
        restore: Vec::new(),
    })
    .await
    .unwrap();

    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX"]));
    let body = body_json(&transport.requests()[0]);
    assert_eq!(ids_of(&body, "addLabelIds"), vec!["INBOX"]);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["CATEGORY_UPDATES"]);
}

#[tokio::test]
async fn undoing_move_to_inbox_restores_the_bulk_tabs() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(
        &db,
        account,
        "t1",
        &["INBOX", "CATEGORY_UPDATES", "CATEGORY_FORUMS"],
    );

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveToInbox {
            thread_ids: vec![thread],
            restore: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        result.undo,
        Some(Command::MoveToInbox {
            thread_ids: vec![thread],
            restore: vec![ThreadLabelState {
                thread_id: thread,
                label_ids: sorted(&["CATEGORY_FORUMS", "CATEGORY_UPDATES", "INBOX"]),
                is_unread: false,
            }],
        })
    );

    d.execute(result.undo.unwrap()).await.unwrap();
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["CATEGORY_FORUMS", "CATEGORY_UPDATES", "INBOX"]),
    );
}

#[tokio::test]
async fn moving_to_inbox_is_a_noop_when_already_there() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "STARRED"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveToInbox {
            thread_ids: vec![thread],
            restore: Vec::new(),
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.undo, None);
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX", "STARRED"]));
    assert!(transport.requests().is_empty());
}

#[tokio::test]
async fn trashing_one_message_uses_the_trash_endpoint() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(thread_labels(&db, thread), sorted(&["TRASH"]));
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].url.ends_with("/users/me/messages/t1-m0/trash"),
        "unexpected url {}",
        requests[0].url
    );
    // Undo restores the labels the thread actually had.
    assert_eq!(
        result.undo,
        Some(Command::Untrash {
            thread_ids: vec![thread],
            restore: vec![ThreadLabelState {
                thread_id: thread,
                label_ids: vec!["INBOX".to_string()],
                is_unread: false,
            }],
        })
    );
}

// ======================================================================= spam
//
// `!` is Gmail's Report spam key. The design claim being pinned here is the one
// the command's doc comment argues for: the inverse names the *exact prior
// state*, so a conversation that was starred, labelled, unread or already out
// of the inbox comes back as all of those — and specifically is not deposited
// in an inbox it was never in.

#[tokio::test]
async fn reporting_spam_adds_spam_and_removes_inbox() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::ReportSpam {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(result.message, "Reported 1 conversation as spam");
    // The other labels are untouched — this is a two-label delta, not a move.
    assert_eq!(thread_labels(&db, thread), sorted(&["Receipts", "SPAM"]));

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let body = body_json(&requests[0]);
    assert_eq!(ids_of(&body, "addLabelIds"), vec!["SPAM"]);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["INBOX"]);
}

#[tokio::test]
async fn undoing_a_spam_report_restores_every_label_and_the_unread_flag() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "STARRED", "Receipts", "UNREAD"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::ReportSpam {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["Receipts", "SPAM", "STARRED", "UNREAD"]),
    );

    // The inverse is the restore form, carrying the state as it stood.
    assert_eq!(
        result.undo,
        Some(Command::NotSpam {
            thread_ids: vec![thread],
            restore: vec![ThreadLabelState {
                thread_id: thread,
                label_ids: sorted(&["INBOX", "Receipts", "STARRED", "UNREAD"]),
                is_unread: true,
            }],
        })
    );

    d.execute(result.undo.unwrap()).await.unwrap();
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["INBOX", "Receipts", "STARRED", "UNREAD"]),
        "undo must restore the star and the label, not just the inbox"
    );
    assert!(thread_is_unread(&db, thread));
}

#[tokio::test]
async fn undoing_a_spam_report_does_not_hand_an_archived_thread_an_inbox() {
    // The trap `Command::Archive`'s doc comment describes, in its spam form. A
    // thread reported from a label — already out of the inbox — never lost an
    // INBOX, so putting one back would be undo making a second move nobody
    // asked for.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::ReportSpam {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();
    assert_eq!(thread_labels(&db, thread), sorted(&["Receipts", "SPAM"]));
    // One label moved, so exactly one moves back.
    let body = body_json(&transport.requests()[0]);
    assert_eq!(ids_of(&body, "addLabelIds"), vec!["SPAM"]);
    assert!(ids_of(&body, "removeLabelIds").is_empty());

    d.execute(result.undo.expect("report spam is undoable"))
        .await
        .unwrap();
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["Receipts"]),
        "undo must not put a thread in an inbox it was never in"
    );
}

#[tokio::test]
async fn not_spam_with_no_prior_state_means_the_inbox() {
    // What a user dispatches from the Spam mailbox, where there is nothing to
    // restore from.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["SPAM"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::NotSpam {
            thread_ids: vec![thread],
            restore: Vec::new(),
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.message, "Marked 1 conversation not spam");
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX"]));
    let body = body_json(&transport.requests()[0]);
    assert_eq!(ids_of(&body, "addLabelIds"), vec!["INBOX"]);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["SPAM"]);
}

#[tokio::test]
async fn a_selection_of_spam_is_one_batched_call() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let threads: Vec<i64> = (0..12)
        .map(|i| seed_thread(&db, account, &format!("s{i}"), &["INBOX", "UNREAD"]))
        .collect();

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::ReportSpam {
            thread_ids: threads.clone(),
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.applied.len(), 12);
    assert_eq!(result.message, "Reported 12 conversations as spam");
    assert_eq!(
        transport.call_count(),
        1,
        "a selection must be one batchModify, not one call per row"
    );
    let request = &transport.requests()[0];
    assert!(
        request.url.ends_with("/users/me/messages/batchModify"),
        "unexpected url {}",
        request.url
    );
    let body = body_json(request);
    assert_eq!(ids_of(&body, "ids").len(), 12);
    assert_eq!(ids_of(&body, "addLabelIds"), vec!["SPAM"]);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["INBOX"]);
    for thread in &threads {
        assert_eq!(thread_labels(&db, *thread), sorted(&["SPAM", "UNREAD"]));
    }
}

#[tokio::test]
async fn a_refused_spam_report_is_named_and_rolled_back() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "STARRED", "UNREAD"]);
    let before = thread_labels(&db, thread);

    let transport = FakeTransport::always_failing(403, r#"{"error":{"message":"nope"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::ReportSpam {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(!result.ok, "a refusal must not be reported as success");
    assert!(result.applied.is_empty());
    assert_eq!(result.failed.len(), 1);
    assert_eq!(result.failed[0].ids, vec![thread]);
    assert_eq!(result.failed[0].kind, FailureKind::Forbidden);
    assert!(result.failed[0].rolled_back);
    assert!(result.undo.is_none(), "nothing happened, nothing to undo");

    assert_eq!(thread_labels(&db, thread), before);
    assert!(thread_is_unread(&db, thread));
}

// =================================================================== rollback

#[tokio::test]
async fn remote_failure_rolls_the_local_change_back_exactly() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts", "UNREAD"]);
    let before = thread_labels(&db, thread);

    let transport = FakeTransport::always_failing(403, r#"{"error":{"message":"nope"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert!(result.applied.is_empty());
    assert_eq!(result.failed.len(), 1);
    assert_eq!(result.failed[0].kind, FailureKind::Forbidden);
    assert!(result.failed[0].rolled_back);
    assert!(!result.failed[0].retriable);
    assert!(result.undo.is_none(), "nothing happened, nothing to undo");

    // The exact prior state, not an approximation of it.
    assert_eq!(thread_labels(&db, thread), before);
    assert!(thread_is_unread(&db, thread));
}

#[tokio::test]
async fn a_rate_limit_is_reported_as_retriable_but_still_rolled_back() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX"]);

    let transport = FakeTransport::always_failing(429, r#"{"error":{"message":"slow down"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert_eq!(result.failed[0].kind, FailureKind::RateLimited);
    assert!(result.failed[0].retriable);
    assert!(result.failed[0].rolled_back);
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX"]));
}

// ======================================================================= undo

#[tokio::test]
async fn undoing_an_archive_restores_every_label_not_just_inbox() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts", "Family"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();
    assert_eq!(thread_labels(&db, thread), sorted(&["Family", "Receipts"]));

    let undo = result.undo.expect("archive is undoable");
    d.execute(undo).await.unwrap();

    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["Family", "INBOX", "Receipts"]),
        "undo must restore the labels the thread actually had"
    );
}

#[tokio::test]
async fn undo_of_mark_read_only_touches_threads_that_actually_changed() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let unread = seed_thread(&db, account, "t1", &["INBOX", "UNREAD"]);
    let already_read = seed_thread(&db, account, "t2", &["INBOX"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MarkRead {
            thread_ids: vec![unread, already_read],
            read: true,
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(
        result.undo,
        Some(Command::MarkRead {
            thread_ids: vec![unread],
            read: false,
        }),
        "undo must not mark an already-read thread unread"
    );

    d.execute(result.undo.unwrap()).await.unwrap();
    assert!(thread_is_unread(&db, unread));
    assert!(!thread_is_unread(&db, already_read));
}

// ============================================================== idempotence

#[tokio::test]
async fn archiving_an_already_archived_thread_is_a_no_op_not_an_error() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.applied, vec![thread]);
    assert_eq!(
        transport.call_count(),
        0,
        "a no-op must not spend a round trip"
    );
    assert_eq!(thread_labels(&db, thread), sorted(&["Receipts"]));
    assert!(
        result.undo.is_none(),
        "nothing changed, so the inverse must not claim to restore INBOX"
    );
}

#[tokio::test]
async fn archiving_a_mixed_selection_produces_an_inverse_for_only_the_changed_threads() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let in_inbox = seed_thread(&db, account, "t1", &["INBOX"]);
    let already_archived = seed_thread(&db, account, "t2", &["Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![in_inbox, already_archived],
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(
        result.undo,
        Some(Command::Unarchive {
            thread_ids: vec![in_inbox],
            restore: Vec::new(),
        })
    );

    d.execute(result.undo.unwrap()).await.unwrap();
    assert_eq!(thread_labels(&db, in_inbox), sorted(&["INBOX"]));
    assert_eq!(
        thread_labels(&db, already_archived),
        sorted(&["Receipts"]),
        "the untouched thread must not gain INBOX"
    );
}

// ====================================================================== batch

#[tokio::test]
async fn fifty_threads_issue_one_batched_call() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let threads: Vec<i64> = (0..50)
        .map(|i| seed_thread(&db, account, &format!("t{i}"), &["INBOX"]))
        .collect();

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: threads.clone(),
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.applied.len(), 50);
    assert_eq!(
        transport.call_count(),
        1,
        "50 threads must be one batchModify, not 50 modifies"
    );
    let request = &transport.requests()[0];
    assert!(
        request.url.ends_with("/users/me/messages/batchModify"),
        "unexpected url {}",
        request.url
    );
    let body = body_json(request);
    assert_eq!(ids_of(&body, "ids").len(), 50);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["INBOX"]);
    for thread in &threads {
        assert!(thread_labels(&db, *thread).is_empty());
    }
}

#[tokio::test]
async fn threads_from_two_accounts_are_batched_per_account() {
    let db = Db::open_in_memory().unwrap();
    let a = seed_account(&db, "a@example.com");
    let b = seed_account(&db, "b@example.com");
    let t1 = seed_thread(&db, a, "a1", &["INBOX"]);
    let t2 = seed_thread(&db, a, "a2", &["INBOX"]);
    let t3 = seed_thread(&db, b, "b1", &["INBOX"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![t1, t2, t3],
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(
        transport.call_count(),
        2,
        "batchModify is per-account, so two accounts means two calls"
    );
    let tokens: Vec<String> = transport
        .requests()
        .iter()
        .map(|r| r.header("authorization").unwrap_or_default().to_string())
        .collect();
    assert!(tokens.contains(&"Bearer token-1".to_string()));
    assert!(tokens.contains(&"Bearer token-2".to_string()));
}

#[tokio::test]
async fn a_failed_chunk_rolls_back_only_its_own_threads() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    // 50 threads, one message each, chunked 10 ids at a time => 5 chunks.
    let threads: Vec<i64> = (0..50)
        .map(|i| seed_thread(&db, account, &format!("t{i}"), &["INBOX"]))
        .collect();

    // Chunk 3 (0-indexed 2) fails; the rest succeed.
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(204, "")),
        Ok(HttpResponse::json(204, "")),
        Ok(HttpResponse::json(500, r#"{"error":{"message":"boom"}}"#)),
        Ok(HttpResponse::json(204, "")),
        Ok(HttpResponse::json(204, "")),
    ]);
    let d = dispatcher(&db, transport.clone()).with_max_batch_message_ids(10);

    let result = d
        .execute(Command::Archive {
            thread_ids: threads.clone(),
        })
        .await
        .unwrap();

    assert!(!result.ok, "a partial failure is not ok");
    assert_eq!(transport.call_count(), 5);
    assert_eq!(result.applied.len(), 40);
    assert_eq!(result.failed.len(), 1);
    assert_eq!(result.failed[0].ids.len(), 10);
    assert!(result.failed[0].rolled_back);
    assert_eq!(result.failed[0].kind, FailureKind::Server);

    // The failed chunk's threads are back in the inbox; the rest are archived.
    let failed: Vec<i64> = result.failed[0].ids.clone();
    for thread in &threads {
        if failed.contains(thread) {
            assert_eq!(
                thread_labels(&db, *thread),
                sorted(&["INBOX"]),
                "thread {thread} was in the failed chunk and must be rolled back"
            );
        } else {
            assert!(thread_labels(&db, *thread).is_empty());
        }
    }

    // Undo covers only the threads that really changed.
    match result.undo.expect("40 threads changed") {
        Command::Unarchive { thread_ids, .. } => assert_eq!(thread_ids.len(), 40),
        other => panic!("unexpected inverse {other:?}"),
    }
}

#[tokio::test]
async fn a_multi_message_thread_never_straddles_two_chunks() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let a = seed_thread_with_messages(&db, account, "t1", &["INBOX"], 3);
    let b = seed_thread_with_messages(&db, account, "t2", &["INBOX"], 3);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone()).with_max_batch_message_ids(4);

    d.execute(Command::Archive {
        thread_ids: vec![a, b],
    })
    .await
    .unwrap();

    // 6 ids with a cap of 4 would split 4/2 if chunking were id-wise; thread-wise
    // chunking splits 3/3 so a thread is never half-applied.
    let sizes: Vec<usize> = transport
        .requests()
        .iter()
        .map(|r| ids_of(&body_json(r), "ids").len())
        .collect();
    assert_eq!(sizes, vec![3, 3]);
}

// ======================================================== unsent mail in a thread

/// Put a row Google has not been told about into a conversation: the mirror
/// `compose::mirror` writes when a draft is saved (`mach-draft:…`), or the
/// optimistic copy `compose::outbox` writes when a reply is queued
/// (`mach-outbox:…`). Neither id exists at Gmail.
fn seed_unsent_message(
    db: &Db,
    account_id: i64,
    thread_id: i64,
    gmail_message_id: &str,
    is_draft: bool,
) {
    db.write(|c| {
        queries::upsert_message(
            c,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: gmail_message_id.to_string(),
                from: Participant::new("me@example.com"),
                subject: "subject".into(),
                snippet: "snippet".into(),
                internal_date: 1_700_000_500_000,
                is_unread: false,
                is_draft,
                ..Default::default()
            },
        )?;
        if is_draft {
            // What the Drafts mailbox reads, and what `mirror` writes alongside
            // the message row.
            c.execute(
                "INSERT OR IGNORE INTO thread_labels (thread_id, gmail_label_id) VALUES (?1, 'DRAFT')",
                [thread_id],
            )?;
        }
        Ok(())
    })
    .unwrap();
}

/// The reported bug, exactly: `E` on a conversation holding one real message and
/// one unsent draft answered `400 Invalid ids value` and archived nothing,
/// because the draft's `mach-draft:` placeholder rode along in the request.
#[tokio::test]
async fn archiving_a_conversation_with_an_unsent_draft_leaves_the_draft_out_of_the_request() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX"]);
    seed_unsent_message(&db, account, thread, "mach-draft:draft-19fe992a3f8", true);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(result.applied, vec![thread]);
    assert!(result.failed.is_empty());

    // One addressable id, so the single-message endpoint — the placeholder did
    // not even turn this into a batch.
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].url.ends_with("/users/me/messages/t1-m0/modify"),
        "unexpected url {}",
        requests[0].url
    );
    // Nothing Mach minted for itself appears anywhere on the wire.
    let sent = String::from_utf8_lossy(requests[0].body.as_deref().unwrap_or(b"")).to_string();
    assert!(
        !requests[0].url.contains("mach-draft") && !sent.contains("mach-draft"),
        "the draft placeholder reached Gmail: {} {sent}",
        requests[0].url
    );

    // The conversation left the inbox and stayed in Drafts, which is where
    // Gmail leaves it too. The draft row itself is untouched.
    assert_eq!(thread_labels(&db, thread), sorted(&["DRAFT"]));
    let messages = db
        .read(|c| queries::thread_with_messages(c, thread))
        .unwrap()
        .expect("thread")
        .messages;
    assert!(
        messages
            .iter()
            .any(|m| m.gmail_message_id == "mach-draft:draft-19fe992a3f8" && m.is_draft),
        "the draft was removed from the conversation"
    );
}

/// Exactly which ids leave, for a conversation holding two real messages, an
/// unsent draft and a reply still in the outbox.
async fn ids_named_to_gmail(make: &dyn Fn(i64) -> Command) -> (Vec<String>, bool) {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let thread = seed_thread_with_messages(
        &db,
        account,
        "t1",
        &["INBOX", "UNREAD", "CATEGORY_UPDATES"],
        2,
    );
    seed_unsent_message(&db, account, thread, "mach-draft:d1", true);
    seed_unsent_message(&db, account, thread, "mach-outbox:o1", false);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());
    let result = d.execute(make(thread)).await.unwrap();

    let ids = transport
        .requests()
        .iter()
        .flat_map(|r| ids_of(&body_json(r), "ids"))
        .collect();
    (ids, result.ok)
}

/// One case: a name, and the command to run against the seeded thread.
type Case = (&'static str, Box<dyn Fn(i64) -> Command>);

/// Archive was the one that was reported; every sibling gathers its ids the same
/// way and had the same defect.
#[tokio::test]
async fn no_mail_command_names_an_unsent_message_to_gmail() {
    let cases: Vec<Case> = vec![
        (
            "archive",
            Box::new(|t| Command::Archive { thread_ids: vec![t] }),
        ),
        (
            "trash",
            Box::new(|t| Command::Trash { thread_ids: vec![t] }),
        ),
        (
            "report spam",
            Box::new(|t| Command::ReportSpam { thread_ids: vec![t] }),
        ),
        (
            "star",
            Box::new(|t| Command::Star {
                thread_ids: vec![t],
                starred: true,
            }),
        ),
        (
            "mark read",
            Box::new(|t| Command::MarkRead {
                thread_ids: vec![t],
                read: true,
            }),
        ),
        (
            "snooze",
            Box::new(|t| Command::Snooze {
                thread_ids: vec![t],
                until: 1_800_000_000_000,
            }),
        ),
        (
            "label",
            Box::new(|t| Command::Label {
                thread_ids: vec![t],
                label_id: "Label_7".into(),
                add: true,
            }),
        ),
        (
            "move to inbox",
            Box::new(|t| Command::MoveToInbox {
                thread_ids: vec![t],
                restore: Vec::new(),
            }),
        ),
    ];

    for (name, make) in cases {
        let (ids, ok) = ids_named_to_gmail(&*make).await;
        assert!(ok, "{name} failed");
        assert_eq!(
            ids,
            vec!["t1-m0".to_string(), "t1-m1".to_string()],
            "{name} named the wrong ids"
        );
    }
}

// ===================================================== deleting a draft
//
// The reported bug: four drafts selected in the Drafts mailbox, delete pressed,
// nothing deleted and a red toast reading "4 failed — thread has no locally
// known Gmail message ids; sync it before acting on it". A draft has no message
// id `batchModify` will take; `commands::drafts` deletes it through
// `drafts.delete` instead.

/// The refusal that used to fire, quoted so a test failure names the regression
/// rather than a diff of strings.
const OLD_REFUSAL: &str = "no locally known Gmail message ids";

/// Give a draft message the Gmail draft id the sweep in `sync::mail` writes,
/// which is the id — and the *only* id — `drafts.delete` takes.
fn seed_draft_id(db: &Db, account_id: i64, gmail_message_id: &str, gmail_draft_id: &str) {
    db.write(|c| {
        queries::set_message_draft_id(c, account_id, gmail_message_id, gmail_draft_id).map(|_| ())
    })
    .unwrap();
}

fn thread_exists(db: &Db, thread_id: i64) -> bool {
    db.read(|c| queries::thread_summary(c, thread_id))
        .unwrap()
        .is_some()
}

fn message_ids(db: &Db, thread_id: i64) -> Vec<String> {
    db.read(|c| queries::thread_with_messages(c, thread_id))
        .unwrap()
        .map(|t| {
            t.messages
                .into_iter()
                .map(|m| m.gmail_message_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn drafts_list_body(entries: &[(&str, &str, &str)]) -> String {
    let drafts: Vec<String> = entries
        .iter()
        .map(|(draft_id, message_id, thread_id)| {
            format!(
                r#"{{"id":"{draft_id}","message":{{"id":"{message_id}","threadId":"{thread_id}"}}}}"#
            )
        })
        .collect();
    format!(r#"{{"drafts":[{}]}}"#, drafts.join(","))
}

/// A conversation that is only a draft, written here and never pushed. Nothing
/// exists at Gmail, so the whole thing is local and costs no request — and the
/// refusal is gone.
#[tokio::test]
async fn deleting_a_draft_only_conversation_discards_the_draft() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    // What `mirror` writes for a draft with no conversation behind it.
    let drafted = seed_thread_with_messages(&db, account, "mach-draft:d2", &["DRAFT"], 0);
    seed_unsent_message(&db, account, drafted, "mach-draft:d2", true);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![drafted],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(
        !format!("{result:?}").contains(OLD_REFUSAL),
        "the old refusal is still being reported: {result:?}"
    );
    assert!(result.failed.is_empty(), "{:?}", result.failed);
    assert_eq!(result.applied, vec![drafted]);
    assert_eq!(result.message, "Discarded 1 draft");
    // Gmail was never told about this draft, so there was nothing to tell it.
    assert_eq!(transport.call_count(), 0, "{:?}", transport.requests());
    // `drafts.delete` cannot be undone, so nothing is offered.
    assert_eq!(result.undo, None);
    // The row is gone from the Drafts mailbox because the row is gone.
    assert!(!thread_exists(&db, drafted));
}

/// The same conversation, with the draft pushed to Gmail. `drafts.delete` takes
/// the draft id, and no `batchModify` is attempted for it.
#[tokio::test]
async fn a_pushed_draft_is_deleted_through_the_drafts_api_and_never_batch_modified() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let drafted = seed_thread_with_messages(&db, account, "t7", &["DRAFT"], 0);
    seed_unsent_message(&db, account, drafted, "18f0c0ffee", true);
    seed_draft_id(&db, account, "18f0c0ffee", "r-99");

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![drafted],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(result.message, "Discarded 1 draft");
    assert_eq!(result.undo, None);

    let requests = transport.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0].url.ends_with("/users/me/drafts/r-99"),
        "unexpected url {}",
        requests[0].url
    );
    // The draft's message id was never named to `messages.batchModify` or to
    // `messages.trash` — deleting the draft is the whole of what happened.
    assert!(
        !requests
            .iter()
            .any(|r| r.url.contains("/messages") || r.url.contains("batchModify")),
        "a draft reached the message endpoints: {requests:?}"
    );
}

/// A reply in progress on a real conversation. Delete means the conversation
/// goes to Trash *and* the draft does not survive it, which is two different
/// calls to two different endpoints.
#[tokio::test]
async fn trashing_a_conversation_that_also_holds_a_draft_takes_both() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread_with_messages(&db, account, "t1", &["INBOX", "DRAFT"], 2);
    seed_unsent_message(&db, account, thread, "18f0beef", true);
    seed_draft_id(&db, account, "18f0beef", "r-12");

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(result.applied, vec![thread]);
    assert_eq!(result.message, "Trashed 1 conversation · discarded 1 draft");

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    // Drafts first, so the batch below is planned from a store the draft has
    // already left.
    assert!(
        requests[0].url.ends_with("/users/me/drafts/r-12"),
        "unexpected url {}",
        requests[0].url
    );
    // Exactly the two real messages, and not the draft's.
    let batched = ids_of(&body_json(&requests[1]), "ids");
    assert_eq!(batched, vec!["t1-m0".to_string(), "t1-m1".to_string()]);
    assert!(
        !requests[1].url.contains("18f0beef") && !batched.iter().any(|id| id == "18f0beef"),
        "the draft's message id was batch-modified: {:?}",
        requests[1]
    );

    // The conversation is in the trash and out of Drafts, and the draft row is
    // out of the conversation.
    assert_eq!(thread_labels(&db, thread), sorted(&["TRASH"]));
    assert_eq!(
        message_ids(&db, thread),
        vec!["t1-m0".to_string(), "t1-m1".to_string()]
    );

    // ⌘Z puts the conversation back. It cannot put the draft back, and the undo
    // entry says only what it can do.
    assert!(matches!(result.undo, Some(Command::Untrash { .. })));
    assert_eq!(result.undo_label.as_deref(), Some("Trashed 1 conversation"));
}

/// The owner's actual four rows: the Drafts mailbox says there is a draft here
/// and the store holds no message to prove it. The draft id comes from
/// `drafts.list`, which is the only endpoint that pairs one with a thread.
#[tokio::test]
async fn a_conversation_that_claims_a_draft_it_cannot_name_is_resolved_from_drafts_list() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let shell = seed_thread_with_messages(&db, account, "19fec64272d5a82f", &["DRAFT"], 0);

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        drafts_list_body(&[("r-4", "18ff11", "19fec64272d5a82f")]),
    ))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![shell],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(
        !format!("{result:?}").contains(OLD_REFUSAL),
        "the old refusal is still being reported: {result:?}"
    );
    assert_eq!(result.message, "Discarded 1 draft");

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests[0].url.contains("/users/me/drafts"));
    assert!(
        requests[1].url.ends_with("/users/me/drafts/r-4"),
        "unexpected url {}",
        requests[1].url
    );
    assert!(!thread_exists(&db, shell));
}

/// The same shape, with no such draft at Gmail: the row is a leftover claiming a
/// draft that stopped existing somewhere else, and clearing it is the job.
#[tokio::test]
async fn a_draft_row_gmail_no_longer_has_is_cleared_rather_than_refused() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let shell = seed_thread_with_messages(&db, account, "19fec0cbc69d10a7", &["DRAFT"], 0);

    // `{}` — no drafts at all.
    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![shell],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(result.message, "Discarded 1 draft");
    // One listing, and no delete: there was nothing there to delete.
    assert_eq!(transport.call_count(), 1);
    assert!(!thread_exists(&db, shell));
}

/// Forty rows, some of them drafts. One command, one round of calls, one
/// sentence — and the sentence counts the two things separately, because only
/// one of them can be taken back.
#[tokio::test]
async fn a_mixed_selection_discards_the_drafts_and_trashes_the_rest() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let one = seed_thread(&db, account, "t1", &["INBOX"]);
    let two = seed_thread(&db, account, "t2", &["INBOX"]);
    let drafted = seed_thread_with_messages(&db, account, "mach-draft:d5", &["DRAFT"], 0);
    seed_unsent_message(&db, account, drafted, "mach-draft:d5", true);
    let replying = seed_thread_with_messages(&db, account, "t3", &["INBOX", "DRAFT"], 1);
    seed_unsent_message(&db, account, replying, "mach-draft:d6", true);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![one, two, drafted, replying],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(result.failed.is_empty(), "{:?}", result.failed);
    assert_eq!(
        result.message,
        "Trashed 3 conversations · discarded 2 drafts"
    );
    assert_eq!(result.undo_label.as_deref(), Some("Trashed 3 conversations"));
    // Every id the user pointed at is accounted for.
    let mut applied = result.applied.clone();
    applied.sort();
    let mut expected = vec![one, two, drafted, replying];
    expected.sort();
    assert_eq!(applied, expected);

    // Neither draft was ever at Gmail, so the only request is the one batch
    // that trashes the three conversations.
    let requests = transport.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let batched = ids_of(&body_json(&requests[0]), "ids");
    assert_eq!(
        batched,
        vec!["t1-m0".to_string(), "t2-m0".to_string(), "t3-m0".to_string()]
    );

    // ⌘Z reaches the three conversations and names only them.
    match result.undo {
        Some(Command::Untrash { ref thread_ids, .. }) => {
            let mut ids = thread_ids.clone();
            ids.sort();
            let mut expected = vec![one, two, replying];
            expected.sort();
            assert_eq!(ids, expected);
        }
        other => panic!("expected an untrash, got {other:?}"),
    }
}

/// A refused `drafts.delete` says so, and leaves the draft exactly where it was
/// — including in the conversation, which is not trashed around a draft that is
/// still there.
#[tokio::test]
async fn a_refused_draft_delete_is_reported_and_changes_nothing() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread_with_messages(&db, account, "t1", &["INBOX", "DRAFT"], 1);
    seed_unsent_message(&db, account, thread, "18f0beef", true);
    seed_draft_id(&db, account, "18f0beef", "r-12");

    let transport = FakeTransport::always_failing(403, r#"{"error":{"message":"nope"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert!(result.applied.is_empty(), "{:?}", result.applied);
    assert_eq!(result.failed.len(), 1, "{:?}", result.failed);
    let failure = &result.failed[0];
    assert_eq!(failure.ids, vec![thread]);
    assert!(
        failure.message.contains("draft"),
        "a failure that does not name the thing: {:?}",
        failure.message
    );
    // Gmail was asked once and refused; the conversation was never trashed, so
    // there is nothing to have rolled back.
    assert!(!failure.rolled_back);
    assert_eq!(transport.call_count(), 1);
    assert_eq!(thread_labels(&db, thread), sorted(&["DRAFT", "INBOX"]));
    assert!(message_ids(&db, thread).iter().any(|id| id == "18f0beef"));
}

/// The other half of the original test, unchanged: a conversation nobody has
/// synced is still refused, and still told to sync. That is a different report
/// from a draft, and it is the one where the advice is worth giving.
#[tokio::test]
async fn a_conversation_that_has_never_been_synced_is_still_refused() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let unsynced = seed_thread_with_messages(&db, account, "t9", &["INBOX"], 0);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Trash {
            thread_ids: vec![unsynced],
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert_eq!(transport.call_count(), 0, "nothing was addressable");
    assert!(result.applied.is_empty());
    assert_eq!(result.failed.len(), 1, "{:?}", result.failed);
    assert_eq!(result.failed[0].kind, FailureKind::Invalid);
    assert!(
        result.failed[0].message.contains("sync it"),
        "unhelpful message {:?}",
        result.failed[0].message
    );
    assert_eq!(thread_labels(&db, unsynced), sorted(&["INBOX"]));
}

/// Archiving is not deleting. A conversation holding a draft keeps it, which is
/// what Gmail does and what `commands::mail` has always documented.
#[tokio::test]
async fn archiving_a_conversation_with_a_draft_leaves_the_draft_alone() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread_with_messages(&db, account, "t1", &["INBOX", "DRAFT"], 1);
    seed_unsent_message(&db, account, thread, "18f0beef", true);
    seed_draft_id(&db, account, "18f0beef", "r-12");

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Archive {
            thread_ids: vec![thread],
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.url.contains("/drafts")),
        "archive touched the drafts api: {:?}",
        transport.requests()
    );
    assert_eq!(thread_labels(&db, thread), sorted(&["DRAFT"]));
    assert!(message_ids(&db, thread).iter().any(|id| id == "18f0beef"));
}


// ===================================================================== snooze

#[tokio::test]
async fn snooze_moves_the_thread_out_of_the_inbox_and_records_a_wake_time() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    let result = d
        .execute(Command::Snooze {
            thread_ids: vec![thread],
            until: wake_at,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["Label_snooze", "Receipts"])
    );
    let body = body_json(&transport.requests()[0]);
    assert_eq!(ids_of(&body, "addLabelIds"), vec!["Label_snooze"]);
    assert_eq!(ids_of(&body, "removeLabelIds"), vec!["INBOX"]);

    let due = db
        .read(|c| mach_lib::db::command_queries::due_snoozes(c, wake_at))
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].thread_id, thread);
    assert_eq!(due[0].wake_at, wake_at);

    // Un-snooze restores exactly what was there before, INBOX included.
    let undo = result.undo.expect("snooze is undoable");
    assert_eq!(
        undo,
        Command::Unsnooze {
            thread_ids: vec![thread]
        }
    );
    d.execute(undo).await.unwrap();
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX", "Receipts"]));
    assert!(db
        .read(|c| mach_lib::db::command_queries::due_snoozes(c, wake_at))
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn snoozing_creates_the_label_when_it_does_not_exist_yet() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let thread = seed_thread(&db, account, "t1", &["INBOX"]);

    // Gmail has no snooze primitive, so Mach uses a real user label — the same
    // approach Superhuman and Boomerang take. On a fresh account that label
    // does not exist, and refusing to snooze because of it is a dead end the
    // user cannot resolve from inside the app. Create it instead.
    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Snooze {
            thread_ids: vec![thread],
            until: 1_800_000_000_000,
        })
        .await
        .expect("a missing snooze label must be created, not refused");

    assert!(result.ok, "snooze should succeed: {}", result.message);
    assert!(
        transport.call_count() >= 2,
        "expected a labels.create followed by the modify, saw {}",
        transport.call_count()
    );
    assert!(
        !thread_labels(&db, thread).contains(&"INBOX".to_string()),
        "a snoozed thread leaves the inbox"
    );
}

// ================================================================ waking them

// The sweep that brings a snoozed conversation back. `due_snoozes` had no
// caller outside these tests for the whole life of the feature, so every snooze
// was permanent: out of the inbox, labelled, and never returned.

fn snooze_row(db: &Db, thread_id: i64) -> Option<mach_lib::db::command_queries::SnoozeRow> {
    db.read(|c| mach_lib::db::command_queries::snooze_row(c, thread_id))
        .unwrap()
}

/// A wake time already behind the clock — what a snooze looks like on disk to
/// the process that opens the store after it came due.
const ALREADY_PAST: i64 = 1_700_000_000_000;

#[tokio::test]
async fn a_due_snooze_wakes_and_the_row_is_cleared() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    d.execute(Command::Snooze {
        thread_ids: vec![thread],
        until: wake_at,
    })
    .await
    .unwrap();
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["Label_snooze", "Receipts"]),
        "the thread is out of the inbox to begin with"
    );

    let report = mach_lib::snooze::wake_due(&d, wake_at).await.unwrap();

    assert_eq!(report.due, vec![thread]);
    assert_eq!(report.woken, vec![thread]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["INBOX", "Receipts"]),
        "waking restores the labels the thread was snoozed from"
    );
    assert!(
        snooze_row(&db, thread).is_none(),
        "a woken conversation is no longer snoozed"
    );

    // Sweeping again finds nothing, and costs no round trip.
    let calls = transport.call_count();
    let again = mach_lib::snooze::wake_due(&d, wake_at).await.unwrap();
    assert!(again.is_empty());
    assert_eq!(transport.call_count(), calls);
}

#[tokio::test]
async fn a_snooze_that_is_not_due_yet_is_left_alone() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let thread = seed_thread(&db, account, "t1", &["INBOX"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    d.execute(Command::Snooze {
        thread_ids: vec![thread],
        until: wake_at,
    })
    .await
    .unwrap();
    let calls = transport.call_count();

    let report = mach_lib::snooze::wake_due(&d, wake_at - 1).await.unwrap();

    assert!(report.is_empty(), "{report:?}");
    assert_eq!(transport.call_count(), calls, "nothing went to Gmail");
    assert_eq!(thread_labels(&db, thread), sorted(&["Label_snooze"]));
    assert_eq!(snooze_row(&db, thread).map(|r| r.wake_at), Some(wake_at));
}

#[tokio::test]
async fn a_wake_that_came_due_while_the_app_was_closed_fires_at_launch() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts"]);

    let transport = FakeTransport::always_ok();

    // The session that snoozed it, and then went away. Nothing is left running
    // to notice the wake time pass — which is the point: the wake time is a row.
    {
        let d = dispatcher(&db, transport.clone());
        d.execute(Command::Snooze {
            thread_ids: vec![thread],
            until: ALREADY_PAST,
        })
        .await
        .unwrap();
    }

    // The next launch: a new dispatcher over the same store, and the sweep that
    // starts with it. The tick is set an hour out, so only the *first* sweep —
    // the immediate one — can possibly have woken anything by the time this
    // test reads the result.
    let d = Arc::new(dispatcher(&db, transport.clone()));
    let cancel = mach_lib::sync::CancelToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(mach_lib::snooze::run(
        Arc::clone(&d),
        cancel.clone(),
        std::time::Duration::from_secs(3600),
        move |report| {
            let _ = tx.send(report);
        },
    ));

    let report = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("the sweep runs at launch rather than one tick later")
        .expect("a report");
    cancel.cancel();
    let _ = task.await;

    assert_eq!(report.woken, vec![thread]);
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX", "Receipts"]));
    assert!(snooze_row(&db, thread).is_none());
}

#[tokio::test]
async fn a_refused_wake_is_reported_and_stays_snoozed_for_the_next_sweep() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let thread = seed_thread(&db, account, "t1", &["INBOX", "Receipts"]);

    // The snooze lands; the first wake is refused; everything after succeeds.
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(200, "{}")),
        Ok(HttpResponse::json(503, r#"{"error":{"message":"backend error"}}"#)),
    ]);
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    d.execute(Command::Snooze {
        thread_ids: vec![thread],
        until: wake_at,
    })
    .await
    .unwrap();

    let refused = mach_lib::snooze::wake_due(&d, wake_at).await.unwrap();

    assert_eq!(refused.due, vec![thread]);
    assert!(refused.woken.is_empty(), "nothing was woken");
    assert_eq!(refused.failed.len(), 1, "{:?}", refused.failed);
    let failure = &refused.failed[0];
    assert_eq!(failure.ids, vec![thread]);
    assert_eq!(failure.kind, FailureKind::Server);
    assert!(failure.retriable, "a 503 is worth trying again");
    assert!(failure.rolled_back);

    // Rolled back completely: still out of the inbox, still labelled, and — the
    // half-woken case — still holding the row that makes the retry possible.
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["Label_snooze", "Receipts"])
    );
    assert_eq!(snooze_row(&db, thread).map(|r| r.wake_at), Some(wake_at));

    // The retry is the next sweep, and it needs no new information.
    let woken = mach_lib::snooze::wake_due(&d, wake_at).await.unwrap();
    assert_eq!(woken.woken, vec![thread]);
    assert!(woken.failed.is_empty());
    assert_eq!(thread_labels(&db, thread), sorted(&["INBOX", "Receipts"]));
    assert!(snooze_row(&db, thread).is_none());
}

#[tokio::test]
async fn a_wake_applies_exactly_the_labels_the_snooze_did_in_reverse() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let before = sorted(&["INBOX", "UNREAD", "IMPORTANT", "Receipts"]);
    let thread = seed_thread(&db, account, "t1", &["INBOX", "UNREAD", "IMPORTANT", "Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    d.execute(Command::Snooze {
        thread_ids: vec![thread],
        until: wake_at,
    })
    .await
    .unwrap();
    mach_lib::snooze::wake_due(&d, wake_at).await.unwrap();

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "one modify each way");
    let snoozed = body_json(&requests[0]);
    let woken = body_json(&requests[1]);

    // Not "roughly the opposite": the set added by one is the set removed by
    // the other, both ways round.
    assert_eq!(ids_of(&snoozed, "addLabelIds"), vec!["Label_snooze"]);
    assert_eq!(ids_of(&snoozed, "removeLabelIds"), vec!["INBOX"]);
    assert_eq!(
        ids_of(&woken, "addLabelIds"),
        ids_of(&snoozed, "removeLabelIds")
    );
    assert_eq!(
        ids_of(&woken, "removeLabelIds"),
        ids_of(&snoozed, "addLabelIds")
    );

    // And the store is back where it started, unread flag included.
    assert_eq!(thread_labels(&db, thread), before);
    assert!(thread_is_unread(&db, thread));
}

#[tokio::test]
async fn waking_an_archived_snooze_does_not_hand_it_an_inbox_it_never_had() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    // Snoozed from the archive — a reminder about a thread already dealt with.
    let thread = seed_thread(&db, account, "t1", &["Receipts"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    d.execute(Command::Snooze {
        thread_ids: vec![thread],
        until: wake_at,
    })
    .await
    .unwrap();
    let report = mach_lib::snooze::wake_due(&d, wake_at).await.unwrap();

    assert_eq!(report.woken, vec![thread]);
    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["Receipts"]),
        "the stored row is the authority on where it came from"
    );
}

#[tokio::test]
async fn several_due_snoozes_wake_together() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    seed_label(&db, account, "Label_snooze", "Mach/Snoozed");
    let first = seed_thread(&db, account, "t1", &["INBOX"]);
    let second = seed_thread(&db, account, "t2", &["INBOX"]);
    let later = seed_thread(&db, account, "t3", &["INBOX"]);

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    d.execute(Command::Snooze {
        thread_ids: vec![first, second],
        until: wake_at,
    })
    .await
    .unwrap();
    d.execute(Command::Snooze {
        thread_ids: vec![later],
        until: wake_at + 60_000,
    })
    .await
    .unwrap();

    let report = mach_lib::snooze::wake_due(&d, wake_at).await.unwrap();

    assert_eq!(report.woken, vec![first, second]);
    assert_eq!(thread_labels(&db, first), sorted(&["INBOX"]));
    assert_eq!(thread_labels(&db, second), sorted(&["INBOX"]));
    assert_eq!(
        thread_labels(&db, later),
        sorted(&["Label_snooze"]),
        "the one that is not due yet stays where it is"
    );
}

#[tokio::test]
async fn a_command_whose_account_has_no_client_rolls_back_rather_than_stranding_the_write() {
    // The one failure that never happens against a scripted transport and does
    // happen live: `ManagedClients` resolves an account id to a client through
    // the store and the token manager, and that resolution can fail — Google not
    // configured at all, an account row gone. The local write has already
    // committed by then. Returning at that point would leave the store saying
    // one thing and Gmail another, with nothing said and nothing to retry.
    let db = Db::open_in_memory().unwrap();
    seed_account(&db, "a@example.com");
    seed_account(&db, "b@example.com");
    let orphan = seed_account(&db, "c@example.com");
    seed_label(&db, orphan, "Label_snooze", "Mach/Snoozed");
    let thread = seed_thread(&db, orphan, "t1", &["INBOX", "Receipts"]);

    // `dispatcher` registers credentials for accounts 1 and 2 only.
    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let wake_at = 1_800_000_000_000;
    let result = d
        .execute(Command::Snooze {
            thread_ids: vec![thread],
            until: wake_at,
        })
        .await
        .expect("a missing client is a reported failure, not an error that hides the write");

    assert!(!result.ok);
    assert!(result.applied.is_empty());
    assert!(result.undo.is_none(), "nothing happened, so nothing to undo");
    assert_eq!(result.failed.len(), 1, "{:?}", result.failed);
    assert_eq!(result.failed[0].ids, vec![thread]);
    assert!(result.failed[0].rolled_back);
    assert_eq!(transport.call_count(), 0);

    assert_eq!(
        thread_labels(&db, thread),
        sorted(&["INBOX", "Receipts"]),
        "the thread never left the inbox"
    );
    assert!(
        snooze_row(&db, thread).is_none(),
        "and it is not recorded as snoozed either"
    );
}

// ======================================================================= rsvp

#[tokio::test]
async fn rsvp_updates_the_local_event_and_patches_the_right_endpoint() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = db
        .write(|c| {
            queries::upsert_event(
                c,
                &NewEvent {
                    account_id: account,
                    calendar_id: "primary".into(),
                    google_event_id: "evt-1".into(),
                    title: "Standup".into(),
                    start_ts: 1_700_000_000_000,
                    end_ts: 1_700_003_600_000,
                    attendees: vec![Participant::new("a@example.com")],
                    rsvp_status: Some(RsvpStatus::NeedsAction),
                    status: "confirmed".into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();

    // events_rsvp reads the event, then patches it.
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"evt-1","attendees":[{"email":"a@example.com","self":true,
                 "responseStatus":"needsAction"}]}"#,
        )),
        Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#)),
    ]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Rsvp {
            event_id,
            response: RsvpStatus::Accepted,
            comment: None,
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let events = db
        .read(|c| queries::events_in_range(c, 0, i64::MAX, None))
        .unwrap();
    assert_eq!(events[0].rsvp_status, Some(RsvpStatus::Accepted));

    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].url.ends_with("/calendars/primary/events/evt-1"),
        "the read half of an RSVP is a plain events.get: {}",
        requests[0].url
    );
    assert_eq!(requests[1].method, mach_lib::google::HttpMethod::Patch);
    assert!(requests[1]
        .url
        .contains("/calendars/primary/events/evt-1?"));
    let patched = body_json(&requests[1]);
    assert_eq!(patched["attendees"][0]["responseStatus"], "accepted");

    assert_eq!(
        result.undo,
        // Undoing an RSVP tells the organizer too — a retraction nobody hears
        // about is the same silence the response was.
        Some(Command::Rsvp {
            event_id,
            response: RsvpStatus::NeedsAction,
            comment: None,
            notify: Some(Notify::Guests),
        })
    );
}

#[tokio::test]
async fn a_failed_rsvp_restores_the_previous_response() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = db
        .write(|c| {
            queries::upsert_event(
                c,
                &NewEvent {
                    account_id: account,
                    calendar_id: "primary".into(),
                    google_event_id: "evt-1".into(),
                    title: "Standup".into(),
                    rsvp_status: Some(RsvpStatus::Tentative),
                    status: "confirmed".into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();

    let transport = FakeTransport::always_failing(404, r#"{"error":{"message":"gone"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Rsvp {
            event_id,
            response: RsvpStatus::Declined,
            comment: None,
            notify: None,
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert_eq!(result.failed[0].kind, FailureKind::NotFound);
    let events = db
        .read(|c| queries::events_in_range(c, i64::MIN, i64::MAX, None))
        .unwrap();
    assert_eq!(events[0].rsvp_status, Some(RsvpStatus::Tentative));
}

// ============================================================ self-describing

#[test]
fn every_command_variant_appears_in_the_catalogue() {
    let catalogue = Command::catalogue();
    let kinds: Vec<&str> = catalogue.iter().map(|spec| spec.kind).collect();
    for expected in [
        "archive", "unarchive", "markRead", "star", "label", "moveToInbox", "reportSpam",
        "notSpam", "trash", "untrash", "snooze", "unsnooze", "rsvp", "createEvent",
        "updateEvent", "deleteEvent", "moveEvent", "unsubscribe",
    ] {
        assert!(kinds.contains(&expected), "{expected} missing from catalogue");
    }
    assert_eq!(kinds.len(), 18);
    // Every spec is serialisable, which is what makes it an agent tool schema.
    let json = serde_json::to_value(catalogue).unwrap();
    assert!(json.is_array());
    assert_eq!(json[0]["params"][0]["name"], "threadIds");
}

#[test]
fn commands_round_trip_through_the_wire_shape_the_ui_uses() {
    let command = Command::MarkRead {
        thread_ids: vec![1, 2],
        read: true,
    };
    let json = serde_json::to_value(&command).unwrap();
    assert_eq!(json["kind"], "markRead");
    assert_eq!(json["threadIds"], serde_json::json!([1, 2]));
    assert_eq!(json["read"], true);
    assert_eq!(
        serde_json::from_value::<Command>(json).unwrap(),
        command,
        "the TypeScript Command union must deserialize back into Rust"
    );
}

#[tokio::test]
async fn an_unknown_thread_is_a_typed_error_before_anything_is_written() {
    let db = Db::open_in_memory().unwrap();
    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let err = d
        .execute(Command::Archive {
            thread_ids: vec![404],
        })
        .await
        .expect_err("no such thread");

    assert_eq!(err.kind(), "unknownThread");
    assert_eq!(transport.call_count(), 0);
}

// ================================================== calendar: the write path

const HOUR_MS: i64 = 3_600_000;
const NOON: i64 = 1_754_654_400_000; // 2025-08-08T12:00:00Z

fn timed_draft(title: &str, start: i64, hours: i64) -> EventDraft {
    EventDraft {
        title: title.to_string(),
        start_ts: start,
        end_ts: start + hours * HOUR_MS,
        ..Default::default()
    }
}

#[tokio::test]
async fn creating_an_event_writes_the_row_before_the_insert_goes_out() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"evt-new","htmlLink":"https://calendar.google.com/evt-new"}"#,
    ))]);
    transport.observing_events(&db);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: timed_draft("Standup", NOON, 1),
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    // The whole claim: at the instant the insert went out, the block was
    // already on the grid.
    assert_eq!(transport.observed_events(), vec![1]);

    let events = all_events(&db);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].title, "Standup");
    assert_eq!(events[0].start_ts, NOON);
    // The placeholder id is gone: the row now answers to what Google called it,
    // so the next sync updates this row rather than adding a twin.
    assert_eq!(events[0].google_event_id, "evt-new");
    assert_eq!(
        events[0].html_link.as_deref(),
        Some("https://calendar.google.com/evt-new")
    );

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, mach_lib::google::HttpMethod::Post);
    assert!(
        requests[0].url.contains("/calendars/primary/events?"),
        "unexpected url {}",
        requests[0].url
    );
    let body = body_json(&requests[0]);
    assert_eq!(body["summary"], "Standup");
    assert!(body["start"]["dateTime"].is_string());
    assert!(body["end"]["dateTime"].is_string());

    assert_eq!(
        result.undo,
        // The undo carries the choice the create made, so ⌘Z on an event that
        // invited three people cancels with the same three.
        Some(Command::DeleteEvent {
            event_id: events[0].id,
            scope: EventScope::This,
            notify: Some(Notify::Guests),
        })
    );
    assert_eq!(result.applied, vec![events[0].id]);
}

#[tokio::test]
async fn a_failed_create_leaves_no_row_behind() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");

    let transport = FakeTransport::always_failing(403, r#"{"error":{"message":"no"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: timed_draft("Standup", NOON, 1),
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert_eq!(result.failed[0].kind, FailureKind::Forbidden);
    assert!(result.undo.is_none());
    assert!(all_events(&db).is_empty(), "the optimistic row survived");
}

#[tokio::test]
async fn an_all_day_create_sends_dates_not_timestamps() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"e1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    // All-day rows are stored at UTC midnight, and the end date is exclusive.
    let midnight = 1_754_611_200_000; // 2025-08-08T00:00:00Z
    d.execute(Command::CreateEvent {
        account_id: account,
        calendar_id: "primary".into(),
        draft: EventDraft {
            title: "Offsite".into(),
            start_ts: midnight,
            end_ts: midnight + 2 * 86_400_000,
            is_all_day: true,
            ..Default::default()
        },
    })
    .await
    .unwrap();

    let body = body_json(&transport.requests()[0]);
    assert_eq!(body["start"]["date"], "2025-08-08");
    assert_eq!(body["end"]["date"], "2025-08-10");
    assert!(body["start"]["dateTime"].is_null());
}

#[tokio::test]
async fn a_recurring_create_adopts_the_id_its_first_occurrence_will_have() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"series"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: EventDraft {
                recurrence: vec!["RRULE:FREQ=WEEKLY".into()],
                ..timed_draft("Weekly", NOON, 1)
            },
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let events = all_events(&db);
    // `{master}_{original start in UTC}` — what events.instances will return,
    // so sync updates this row instead of adding a second one beside it.
    assert_eq!(events[0].google_event_id, "series_20250808T120000Z");
    assert_eq!(events[0].recurring_event_id.as_deref(), Some("series"));
    assert_eq!(body_json(&transport.requests()[0])["recurrence"][0], "RRULE:FREQ=WEEKLY");
    // Undoing the creation of a series takes the series, not one occurrence.
    assert_eq!(
        result.undo,
        Some(Command::DeleteEvent {
            event_id: events[0].id,
            scope: EventScope::All,
            notify: Some(Notify::Guests),
        })
    );
}

#[tokio::test]
async fn moving_an_event_in_time_patches_google_and_inverts_to_where_it_was() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                start_ts: Some(NOON + HOUR_MS),
                end_ts: Some(NOON + 2 * HOUR_MS),
                is_all_day: Some(false),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let events = all_events(&db);
    assert_eq!(events[0].start_ts, NOON + HOUR_MS);
    assert_eq!(events[0].end_ts, NOON + 2 * HOUR_MS);

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, mach_lib::google::HttpMethod::Patch);
    assert!(requests[0].url.contains("/calendars/primary/events/evt-1?"));
    // A time change always sends both halves: Google rejects a body whose start
    // is a `date` and whose end is a `dateTime`.
    let body = body_json(&requests[0]);
    assert!(body["start"]["dateTime"].is_string());
    assert!(body["end"]["dateTime"].is_string());
    assert!(body["summary"].is_null(), "an untouched field was sent");

    assert_eq!(
        result.undo,
        Some(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                start_ts: Some(NOON),
                end_ts: Some(NOON + HOUR_MS),
                is_all_day: Some(false),
                ..Default::default()
            },
            scope: EventScope::This,
        })
    );
}

#[tokio::test]
async fn a_failed_update_restores_every_field_exactly() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            location: Some("Room 2".into()),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_failing(500, r#"{"error":{"message":"boom"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                title: Some("Renamed".into()),
                location: Some(String::new()),
                start_ts: Some(NOON + HOUR_MS),
                end_ts: Some(NOON + 2 * HOUR_MS),
                is_all_day: Some(false),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert_eq!(result.failed[0].kind, FailureKind::Server);
    let events = all_events(&db);
    assert_eq!(events[0].title, "Standup");
    assert_eq!(events[0].location.as_deref(), Some("Room 2"));
    assert_eq!(events[0].start_ts, NOON);
    assert_eq!(events[0].end_ts, NOON + HOUR_MS);
}

#[tokio::test]
async fn clearing_a_text_field_sends_null_and_inverts_to_the_old_text() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            location: Some("Room 2".into()),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                location: Some(String::new()),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(all_events(&db)[0].location.is_none());
    assert!(body_json(&transport.requests()[0])["location"].is_null());
    assert_eq!(
        result.undo,
        Some(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                location: Some("Room 2".into()),
                ..Default::default()
            },
            scope: EventScope::This,
        })
    );
}

#[tokio::test]
async fn renaming_a_whole_series_patches_the_master_and_every_local_occurrence() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let mut ids = Vec::new();
    for week in 0..3i64 {
        ids.push(seed_event(
            &db,
            account,
            NewEvent {
                calendar_id: "primary".into(),
                google_event_id: format!("series_{week}"),
                title: "Weekly".into(),
                start_ts: NOON + week * 7 * 24 * HOUR_MS,
                end_ts: NOON + week * 7 * 24 * HOUR_MS + HOUR_MS,
                recurring_event_id: Some("series".into()),
                status: "confirmed".into(),
                ..Default::default()
            },
        ));
    }

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"series"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id: ids[1],
            patch: EventPatch {
                title: Some("Weekly sync".into()),
                ..Default::default()
            },
            scope: EventScope::All,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    // The series master is what Google was asked to change.
    assert!(
        transport.requests()[0]
            .url
            .contains("/calendars/primary/events/series?"),
        "unexpected url {}",
        transport.requests()[0].url
    );
    for event in all_events(&db) {
        assert_eq!(event.title, "Weekly sync");
    }
    assert_eq!(result.applied.len(), 3);
    assert_eq!(
        result.undo,
        Some(Command::UpdateEvent {
            event_id: ids[1],
            patch: EventPatch {
                title: Some("Weekly".into()),
                ..Default::default()
            },
            scope: EventScope::All,
        })
    );
}

#[tokio::test]
async fn re_timing_a_whole_series_is_refused_rather_than_guessed_at() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "series_0".into(),
            title: "Weekly".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            recurring_event_id: Some("series".into()),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let err = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                start_ts: Some(NOON + HOUR_MS),
                end_ts: Some(NOON + 2 * HOUR_MS),
                is_all_day: Some(false),
                ..Default::default()
            },
            scope: EventScope::All,
        })
        .await
        .expect_err("a series cannot be re-timed from one occurrence");

    assert_eq!(err.kind(), "invalid");
    assert_eq!(transport.call_count(), 0, "nothing should have been sent");
    assert_eq!(all_events(&db)[0].start_ts, NOON, "nothing should have moved");
}

#[tokio::test]
async fn deleting_an_event_removes_the_row_and_inverts_to_a_create() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            location: Some("Room 2".into()),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            attendees: vec![Participant::new("b@example.com")],
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(204, "{}"))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(all_events(&db).is_empty());
    let requests = transport.requests();
    assert_eq!(requests[0].method, mach_lib::google::HttpMethod::Delete);
    assert!(requests[0].url.contains("/calendars/primary/events/evt-1?"));

    match result.undo {
        Some(Command::CreateEvent {
            account_id,
            calendar_id,
            draft,
        }) => {
            assert_eq!(account_id, account);
            assert_eq!(calendar_id, "primary");
            assert_eq!(draft.title, "Standup");
            assert_eq!(draft.start_ts, NOON);
            assert_eq!(draft.location.as_deref(), Some("Room 2"));
            assert_eq!(draft.attendees.len(), 1);
        }
        other => panic!("expected a create as the inverse, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_delete_puts_the_event_back_with_its_id_intact() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_failing(500, r#"{"error":{"message":"boom"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .unwrap();

    assert!(!result.ok);
    let events = all_events(&db);
    assert_eq!(events.len(), 1);
    // The same row id, not a new one — anything holding a reference to it (the
    // open modal, the selection) still points at the right event.
    assert_eq!(events[0].id, event_id);
    assert_eq!(events[0].title, "Standup");
}

#[tokio::test]
async fn deleting_a_series_takes_every_occurrence_and_offers_no_undo() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let mut ids = Vec::new();
    for week in 0..3i64 {
        ids.push(seed_event(
            &db,
            account,
            NewEvent {
                calendar_id: "primary".into(),
                google_event_id: format!("series_{week}"),
                title: "Weekly".into(),
                start_ts: NOON + week * 7 * 24 * HOUR_MS,
                end_ts: NOON + week * 7 * 24 * HOUR_MS + HOUR_MS,
                recurring_event_id: Some("series".into()),
                status: "confirmed".into(),
                ..Default::default()
            },
        ));
    }

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(204, "{}"))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id: ids[2],
            scope: EventScope::All,
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(all_events(&db).is_empty());
    assert_eq!(result.applied.len(), 3);
    assert!(transport.requests()[0]
        .url
        .contains("/calendars/primary/events/series?"));
    // Google has no endpoint that puts a cancelled occurrence back into its
    // series, so claiming an inverse here would be a lie.
    assert!(result.undo.is_none());
}

#[tokio::test]
async fn deleting_one_occurrence_addresses_the_instance_not_the_series() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let keep = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "series_0".into(),
            title: "Weekly".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            recurring_event_id: Some("series".into()),
            status: "confirmed".into(),
            ..Default::default()
        },
    );
    let drop = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "series_1".into(),
            title: "Weekly".into(),
            start_ts: NOON + 7 * 24 * HOUR_MS,
            end_ts: NOON + 7 * 24 * HOUR_MS + HOUR_MS,
            recurring_event_id: Some("series".into()),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(204, "{}"))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id: drop,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let events = all_events(&db);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, keep);
    assert!(transport.requests()[0]
        .url
        .contains("/calendars/primary/events/series_1?"));
}

#[tokio::test]
async fn deleting_something_google_already_lost_still_clears_the_grid() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_failing(404, r#"{"error":{"message":"gone"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .unwrap();

    // Restoring the row would leave a block on screen that nothing can remove.
    assert!(result.ok, "{result:?}");
    assert!(all_events(&db).is_empty());
}

#[tokio::test]
async fn moving_an_event_to_another_account_inserts_there_and_deletes_here() {
    let db = Db::open_in_memory().unwrap();
    let from = seed_account(&db, "a@example.com");
    let to = seed_account(&db, "b@example.com");
    let event_id = seed_event(
        &db,
        from,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(200, r#"{"id":"evt-2"}"#)),
        Ok(HttpResponse::json(204, "{}")),
    ]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveEvent {
            event_id,
            account_id: to,
            calendar_id: "work".into(),
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let events = all_events(&db);
    assert_eq!(events[0].account_id, to);
    assert_eq!(events[0].calendar_id, "work");
    assert_eq!(events[0].google_event_id, "evt-2");

    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.contains("/calendars/work/events?"));
    assert!(requests[1].url.contains("/calendars/primary/events/evt-1?"));
    assert_eq!(requests[1].method, mach_lib::google::HttpMethod::Delete);

    assert_eq!(
        result.undo,
        Some(Command::MoveEvent {
            event_id,
            account_id: from,
            calendar_id: "primary".into(),
            notify: Some(Notify::Guests),
        })
    );
}

#[tokio::test]
async fn a_move_whose_copy_lands_but_whose_delete_fails_leaves_one_event_not_two() {
    let db = Db::open_in_memory().unwrap();
    let from = seed_account(&db, "a@example.com");
    let to = seed_account(&db, "b@example.com");
    let event_id = seed_event(
        &db,
        from,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(200, r#"{"id":"evt-2"}"#)),
        Ok(HttpResponse::json(500, r#"{"error":{"message":"boom"}}"#)),
        // The undo of the copy.
        Ok(HttpResponse::json(204, "{}")),
    ]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveEvent {
            event_id,
            account_id: to,
            calendar_id: "work".into(),
            notify: None,
        })
        .await
        .unwrap();

    assert!(!result.ok);
    let events = all_events(&db);
    assert_eq!(events[0].account_id, from);
    assert_eq!(events[0].calendar_id, "primary");
    assert_eq!(events[0].google_event_id, "evt-1");
    // Three calls: insert, the delete that failed, and the cleanup of the copy.
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].url.contains("/calendars/work/events/evt-2?"));
    assert_eq!(requests[2].method, mach_lib::google::HttpMethod::Delete);
}

#[tokio::test]
async fn an_edit_that_changes_nothing_costs_no_round_trip() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch::default(),
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(result.ok);
    assert!(result.undo.is_none());
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn an_unknown_event_is_a_typed_error_before_anything_is_written() {
    let db = Db::open_in_memory().unwrap();
    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let err = d
        .execute(Command::DeleteEvent {
            event_id: 404,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .expect_err("no such event");

    assert_eq!(err.kind(), "unknownEvent");
    assert_eq!(transport.call_count(), 0);
}

// ======================================= calendar: the fields that round trip
//
// Everything below pins the same claim from a different angle: what Mach sends
// to Google, and what Google says back, is *kept* — so reopening the modal
// shows the event that was saved rather than a lossy shadow of it. Each of
// these fields used to be write-only, and each one had a visible symptom:
// a weekly meeting that reopened as "does not repeat", an alert that vanished,
// the same invitation drawn twice because nothing tied the two copies together,
// and an edit offered on someone else's event that Google would only refuse.

#[tokio::test]
async fn a_recurring_create_stores_the_rule_it_sent() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"series","iCalUID":"uid-weekly@google.com"}"#,
    ))]);
    let d = dispatcher(&db, transport.clone());

    d.execute(Command::CreateEvent {
        account_id: account,
        calendar_id: "primary".into(),
        draft: EventDraft {
            recurrence: vec!["RRULE:FREQ=WEEKLY;BYDAY=FR".into()],
            reminder_minutes: Some(vec![10]),
            ..timed_draft("Weekly", NOON, 1)
        },
    })
    .await
    .unwrap();

    let event = &all_events(&db)[0];
    // Google will never tell us this again: `singleEvents=true` returns
    // occurrences, and an occurrence carries no rule. Create time is the only
    // moment it is knowable, so it is the moment it is written down.
    assert_eq!(event.recurrence, vec!["RRULE:FREQ=WEEKLY;BYDAY=FR"]);
    let reminders = event.reminders.as_ref().expect("reminders stored");
    assert!(!reminders.use_default);
    assert_eq!(reminders.overrides[0].minutes, 10);
    assert_eq!(reminders.overrides[0].method, "popup");
    // The uid is minted by Google and adopted from the insert's answer. It is
    // what lets the copy of this meeting on another account be recognised as
    // the same meeting rather than drawn beside it.
    assert_eq!(event.ical_uid.as_deref(), Some("uid-weekly@google.com"));
    // We made it, so we own it — and the UI reads that to decide whether to
    // offer an edit at all.
    assert_eq!(event.organizer_self, Some(true));
    assert_eq!(
        event.organizer.as_ref().map(|p| p.email.as_str()),
        Some("a@example.com")
    );
}

#[tokio::test]
async fn a_sync_that_expands_a_series_does_not_erase_the_rule_it_expanded() {
    // The exact shape of "I made it weekly and it came back as a one-off": the
    // create writes the rule, then the next sync overwrites that row with the
    // expanded instance — which carries no rule at all.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport =
        FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"series"}"#))]);
    let d = dispatcher(&db, transport.clone());

    d.execute(Command::CreateEvent {
        account_id: account,
        calendar_id: "primary".into(),
        draft: EventDraft {
            recurrence: vec!["RRULE:FREQ=WEEKLY".into()],
            ..timed_draft("Weekly", NOON, 1)
        },
    })
    .await
    .unwrap();
    let created = all_events(&db)[0].google_event_id.clone();

    // Sync, writing the same row back exactly as `events.list` describes it.
    db.write(|c| {
        queries::upsert_event(
            c,
            &NewEvent {
                account_id: account,
                calendar_id: "primary".into(),
                google_event_id: created.clone(),
                title: "Weekly".into(),
                start_ts: NOON,
                end_ts: NOON + HOUR_MS,
                recurring_event_id: Some("series".into()),
                status: "confirmed".into(),
                ..Default::default()
            },
        )
    })
    .unwrap();

    assert_eq!(all_events(&db)[0].recurrence, vec!["RRULE:FREQ=WEEKLY"]);

    // And the sibling occurrences Google expands out of the same series inherit
    // it, so every block of the series agrees about how it repeats.
    db.write(|c| {
        queries::upsert_event(
            c,
            &NewEvent {
                account_id: account,
                calendar_id: "primary".into(),
                google_event_id: "series_20250815T120000Z".into(),
                title: "Weekly".into(),
                start_ts: NOON + 7 * 24 * HOUR_MS,
                end_ts: NOON + 7 * 24 * HOUR_MS + HOUR_MS,
                recurring_event_id: Some("series".into()),
                status: "confirmed".into(),
                ..Default::default()
            },
        )
    })
    .unwrap();

    let events = all_events(&db);
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(
            event.recurrence,
            vec!["RRULE:FREQ=WEEKLY"],
            "occurrence {} lost the series rule",
            event.google_event_id
        );
    }
}

#[tokio::test]
async fn changing_the_rule_stores_it_and_inverts_to_the_old_one() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            recurrence: vec!["RRULE:FREQ=DAILY".into()],
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                recurrence: Some(vec!["RRULE:FREQ=WEEKLY".into()]),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(all_events(&db)[0].recurrence, vec!["RRULE:FREQ=WEEKLY"]);
    assert_eq!(
        body_json(&transport.requests()[0])["recurrence"][0],
        "RRULE:FREQ=WEEKLY"
    );
    // This is the undo that could not exist before: the prior rule is now a
    // thing the store knows, so `z` puts it back instead of doing nothing.
    assert_eq!(
        result.undo,
        Some(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                recurrence: Some(vec!["RRULE:FREQ=DAILY".into()]),
                ..Default::default()
            },
            scope: EventScope::This,
        })
    );
}

#[tokio::test]
async fn clearing_the_rule_is_stored_as_cleared_not_as_unknown() {
    // The one case the sync-side `COALESCE` must not swallow: an edit that says
    // "does not repeat" genuinely means it.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            recurrence: vec!["RRULE:FREQ=DAILY".into()],
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    d.execute(Command::UpdateEvent {
        event_id,
        patch: EventPatch {
            recurrence: Some(Vec::new()),
            ..Default::default()
        },
        scope: EventScope::This,
    })
    .await
    .unwrap();

    assert!(all_events(&db)[0].recurrence.is_empty());
    assert!(body_json(&transport.requests()[0])["recurrence"]
        .as_array()
        .expect("an explicit empty list, not an absent key")
        .is_empty());
}

#[tokio::test]
async fn changing_the_rule_of_one_occurrence_is_refused_before_google_can_400() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "series_0".into(),
            title: "Weekly".into(),
            recurring_event_id: Some("series".into()),
            recurrence: vec!["RRULE:FREQ=WEEKLY".into()],
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());

    let err = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                recurrence: Some(vec!["RRULE:FREQ=DAILY".into()]),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .expect_err("an instance has no rule of its own");

    assert_eq!(err.kind(), "invalid");
    assert_eq!(transport.call_count(), 0);
    assert_eq!(all_events(&db)[0].recurrence, vec!["RRULE:FREQ=WEEKLY"]);
}

#[tokio::test]
async fn a_reminder_edit_inverts_only_when_the_prior_state_can_be_spoken() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");

    // Prior state: an explicit ten-minute popup. That is expressible, so the
    // edit has an inverse.
    let explicit = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            reminders: Some(EventReminders {
                use_default: false,
                overrides: vec![EventReminder {
                    method: "popup".into(),
                    minutes: 10,
                }],
            }),
            status: "confirmed".into(),
            ..Default::default()
        },
    );
    // Prior state: the calendar's own default. `EventPatch` has no way to say
    // "go back to the default", so claiming an inverse would be a lie.
    let defaulted = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-2".into(),
            title: "Retro".into(),
            reminders: Some(EventReminders {
                use_default: true,
                overrides: Vec::new(),
            }),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());
    let patch = EventPatch {
        reminder_minutes: Some(vec![30]),
        ..Default::default()
    };

    let first = d
        .execute(Command::UpdateEvent {
            event_id: explicit,
            patch: patch.clone(),
            scope: EventScope::This,
        })
        .await
        .unwrap();
    assert_eq!(
        first.undo,
        Some(Command::UpdateEvent {
            event_id: explicit,
            patch: EventPatch {
                reminder_minutes: Some(vec![10]),
                ..Default::default()
            },
            scope: EventScope::This,
        })
    );

    let second = d
        .execute(Command::UpdateEvent {
            event_id: defaulted,
            patch,
            scope: EventScope::This,
        })
        .await
        .unwrap();
    assert!(second.ok);
    assert!(
        second.undo.is_none(),
        "the calendar default is not something a patch can restore"
    );
    // The write still landed — no inverse is not the same as no effect.
    let stored = all_events(&db)
        .into_iter()
        .find(|e| e.id == defaulted)
        .unwrap();
    assert_eq!(stored.reminders.unwrap().overrides[0].minutes, 30);
}

#[tokio::test]
async fn a_transparency_edit_writes_the_column_and_inverts() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_ok();
    let d = dispatcher(&db, transport.clone());
    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                transparency: Some("transparent".into()),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(
        all_events(&db)[0].transparency.as_deref(),
        Some("transparent")
    );

    let body = body_json(&transport.requests()[0]);
    assert_eq!(body["transparency"], "transparent");
    assert!(body["summary"].is_null(), "an untouched field was sent");

    // A missing column reads as busy, so the inverse of "make this free" is
    // an explicit opaque rather than a silent omit.
    assert_eq!(
        result.undo,
        Some(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                transparency: Some("opaque".into()),
                ..Default::default()
            },
            scope: EventScope::This,
        })
    );
}

#[tokio::test]
async fn creating_a_free_event_sends_transparency() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"evt-free"}"#,
    ))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: EventDraft {
                title: "Out of office".into(),
                start_ts: NOON,
                end_ts: NOON + HOUR_MS,
                transparency: Some("transparent".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert_eq!(
        all_events(&db)[0].transparency.as_deref(),
        Some("transparent")
    );
    assert_eq!(body_json(&transport.requests()[0])["transparency"], "transparent");
}

#[tokio::test]
async fn a_failed_update_restores_the_rule_and_the_alerts_too() {
    // `restore_event` is a full-row replacement, and a column left out of it
    // silently reverts to its default on the first rollback that touches the
    // row. This is the test that fails when a new column is forgotten there.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            recurrence: vec!["RRULE:FREQ=DAILY".into()],
            reminders: Some(EventReminders {
                use_default: false,
                overrides: vec![EventReminder {
                    method: "email".into(),
                    minutes: 60,
                }],
            }),
            ical_uid: Some("uid-1@google.com".into()),
            organizer: Some(Participant::new("boss@example.com")),
            organizer_self: Some(false),
            guests_can_modify: Some(true),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_failing(503, r#"{"error":{"message":"try later"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                title: Some("Renamed".into()),
                recurrence: Some(vec!["RRULE:FREQ=WEEKLY".into()]),
                reminder_minutes: Some(vec![5]),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert_eq!(result.failed[0].kind, FailureKind::Server);
    assert!(result.failed[0].rolled_back);

    let event = &all_events(&db)[0];
    assert_eq!(event.id, event_id);
    assert_eq!(event.title, "Standup");
    assert_eq!(event.recurrence, vec!["RRULE:FREQ=DAILY"]);
    let reminders = event.reminders.as_ref().expect("reminders survived");
    assert_eq!(reminders.overrides[0].minutes, 60);
    // Not rewritten to `popup` on the way back out — an alert someone set to
    // email on the web is theirs.
    assert_eq!(reminders.overrides[0].method, "email");
    assert_eq!(event.ical_uid.as_deref(), Some("uid-1@google.com"));
    assert_eq!(event.organizer_self, Some(false));
    assert_eq!(event.guests_can_modify, Some(true));
    assert_eq!(
        event.organizer.as_ref().map(|p| p.email.as_str()),
        Some("boss@example.com")
    );
}

#[tokio::test]
async fn a_failed_delete_puts_the_rule_and_the_uid_back_as_well() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            recurrence: vec!["RRULE:FREQ=MONTHLY".into()],
            ical_uid: Some("uid-9@google.com".into()),
            organizer_self: Some(true),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::always_failing(403, r#"{"error":{"message":"no"}}"#);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .unwrap();

    assert!(!result.ok);
    assert_eq!(result.failed[0].kind, FailureKind::Forbidden);
    let event = &all_events(&db)[0];
    assert_eq!(event.id, event_id);
    assert_eq!(event.recurrence, vec!["RRULE:FREQ=MONTHLY"]);
    assert_eq!(event.ical_uid.as_deref(), Some("uid-9@google.com"));
    assert_eq!(event.organizer_self, Some(true));
}

#[tokio::test]
async fn undoing_a_delete_re_creates_the_event_rather_than_a_lookalike() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seed_event(
        &db,
        account,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            recurrence: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO".into()],
            reminders: Some(EventReminders {
                use_default: false,
                overrides: vec![EventReminder {
                    method: "popup".into(),
                    minutes: 15,
                }],
            }),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(204, "{}"))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .unwrap();

    match result.undo {
        Some(Command::CreateEvent { draft, .. }) => {
            assert_eq!(draft.recurrence, vec!["RRULE:FREQ=WEEKLY;BYDAY=MO"]);
            assert_eq!(draft.reminder_minutes, Some(vec![15]));
        }
        other => panic!("expected a create as the inverse, got {other:?}"),
    }
}

#[tokio::test]
async fn a_moved_event_keeps_its_alerts_on_the_calendar_it_lands_on() {
    let db = Db::open_in_memory().unwrap();
    let from = seed_account(&db, "a@example.com");
    let to = seed_account(&db, "b@example.com");
    let event_id = seed_event(
        &db,
        from,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: NOON,
            end_ts: NOON + HOUR_MS,
            reminders: Some(EventReminders {
                use_default: false,
                overrides: vec![EventReminder {
                    method: "popup".into(),
                    minutes: 20,
                }],
            }),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"evt-2","iCalUID":"uid-moved@google.com"}"#,
        )),
        Ok(HttpResponse::json(204, "{}")),
    ]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveEvent {
            event_id,
            account_id: to,
            calendar_id: "work".into(),
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    // A move is insert-then-delete, so the alerts have to travel in the insert
    // body or they are simply gone from the copy that survives.
    let body = body_json(&transport.requests()[0]);
    assert_eq!(body["reminders"]["useDefault"], false);
    assert_eq!(body["reminders"]["overrides"][0]["minutes"], 20);
    // The copy is a different event with a different uid, and the row adopts it.
    assert_eq!(
        all_events(&db)[0].ical_uid.as_deref(),
        Some("uid-moved@google.com")
    );
}

#[tokio::test]
async fn a_failed_move_restores_the_identity_it_had_before() {
    let db = Db::open_in_memory().unwrap();
    let from = seed_account(&db, "a@example.com");
    let to = seed_account(&db, "b@example.com");
    let event_id = seed_event(
        &db,
        from,
        NewEvent {
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            ical_uid: Some("uid-original@google.com".into()),
            status: "confirmed".into(),
            ..Default::default()
        },
    );

    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"evt-2","iCalUID":"uid-copy@google.com"}"#,
        )),
        Ok(HttpResponse::json(500, r#"{"error":{"message":"boom"}}"#)),
        Ok(HttpResponse::json(204, "{}")),
    ]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveEvent {
            event_id,
            account_id: to,
            calendar_id: "work".into(),
            notify: None,
        })
        .await
        .unwrap();

    assert!(!result.ok);
    let event = &all_events(&db)[0];
    assert_eq!(event.account_id, from);
    assert_eq!(event.google_event_id, "evt-1");
    // The uid of the copy that was cleaned up must not be left on the row that
    // stayed behind — that would make the original look like the copy to every
    // cross-account merge from here on.
    assert_eq!(event.ical_uid.as_deref(), Some("uid-original@google.com"));
}

#[test]
fn the_event_commands_round_trip_through_the_wire_shape_the_ui_uses() {
    let command = Command::UpdateEvent {
        event_id: 7,
        patch: EventPatch {
            title: Some("Renamed".into()),
            start_ts: Some(NOON),
            end_ts: Some(NOON + HOUR_MS),
            is_all_day: Some(false),
            ..Default::default()
        },
        scope: EventScope::All,
    };
    let json = serde_json::to_value(&command).unwrap();
    assert_eq!(json["kind"], "updateEvent");
    assert_eq!(json["eventId"], 7);
    assert_eq!(json["patch"]["startTs"], NOON);
    assert_eq!(json["scope"], "all");
    assert_eq!(
        serde_json::from_value::<Command>(json).unwrap(),
        command,
        "the TypeScript Command union must deserialize back into Rust"
    );

    // `scope` is optional on the wire and defaults to this occurrence, so the
    // common case does not have to say so.
    let terse: Command =
        serde_json::from_value(serde_json::json!({ "kind": "deleteEvent", "eventId": 3 })).unwrap();
    assert_eq!(
        terse,
        Command::DeleteEvent {
            event_id: 3,
            scope: EventScope::This,
            notify: None,
        }
    );
}

// ================================================ calendar: telling the guests

/// The bug this whole section exists for.
///
/// Google's calendar API notifies nobody unless the request says so. Mach never
/// said so, so every event the owner ever created in it with a guest list went
/// onto his calendar, recorded the names, and mailed none of them. Nothing in
/// the response distinguishes that from the working case — which is why these
/// tests assert on the *request*, and why the local rows are barely mentioned.
fn guest_draft(title: &str, guests: &[&str]) -> EventDraft {
    EventDraft {
        attendees: guests.iter().map(|e| Participant::new(*e)).collect(),
        ..timed_draft(title, NOON, 1)
    }
}

fn seeded_event(db: &Db, account: i64, guests: &[&str]) -> i64 {
    db.write(|c| {
        queries::upsert_event(
            c,
            &NewEvent {
                account_id: account,
                calendar_id: "primary".into(),
                google_event_id: "evt-1".into(),
                title: "Board call".into(),
                start_ts: NOON,
                end_ts: NOON + HOUR_MS,
                attendees: guests.iter().map(|e| Participant::new(*e)).collect(),
                status: "confirmed".into(),
                ..Default::default()
            },
        )
    })
    .unwrap()
}

#[tokio::test]
async fn creating_an_event_with_guests_actually_invites_them() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport =
        FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-new"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: guest_draft("Board call", &["ada@example.com", "bob@example.com"]),
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let url = &transport.requests()[0].url;
    assert!(
        url.contains("sendUpdates=all"),
        "an insert with attendees and no sendUpdates invites nobody: {url}"
    );
    let body = body_json(&transport.requests()[0]);
    assert_eq!(body["attendees"][0]["email"], "ada@example.com");
    assert_eq!(body["attendees"][1]["email"], "bob@example.com");

    // What happened, and what ⌘Z can take back, are two different sentences.
    assert_eq!(result.message, "Created “Board call” · invited 2 guests");
    assert_eq!(result.undo_label.as_deref(), Some("Created “Board call”"));
}

#[tokio::test]
async fn a_silent_create_is_available_and_says_so_out_loud() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport =
        FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-new"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: EventDraft {
                notify: Some(Notify::Nobody),
                ..guest_draft("Board call", &["ada@example.com"])
            },
        })
        .await
        .unwrap();

    assert!(transport.requests()[0].url.contains("sendUpdates=none"));
    // An event carrying a guest list that nobody has been told about is the
    // exact shape of the original bug, so a quiet create names its own quiet —
    // this is the ⌘D path, where nobody was asked.
    assert_eq!(result.message, "Created “Board call” · nobody was invited");
    // Nothing irreversible happened, so the undo entry is the whole sentence.
    assert_eq!(result.undo_label, None);
}

#[tokio::test]
async fn an_event_with_no_guests_claims_no_invitations() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport =
        FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-new"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: timed_draft("Gym", NOON, 1),
        })
        .await
        .unwrap();

    assert_eq!(result.message, "Created “Gym”");
    assert_eq!(result.undo_label, None);
}

#[tokio::test]
async fn changing_the_time_of_a_meeting_tells_the_people_in_it() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seeded_event(&db, account, &["ada@example.com", "bob@example.com"]);
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                start_ts: Some(NOON + HOUR_MS),
                end_ts: Some(NOON + 2 * HOUR_MS),
                is_all_day: Some(false),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(transport.requests()[0].url.contains("sendUpdates=all"));
    assert_eq!(result.message, "Moved the event · told 2 guests");
    assert_eq!(result.undo_label.as_deref(), Some("Moved the event"));
}

#[tokio::test]
async fn adding_a_guest_tells_the_new_list_not_the_old_one() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seeded_event(&db, account, &["ada@example.com"]);
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                attendees: Some(vec![
                    Participant::new("ada@example.com"),
                    Participant::new("bob@example.com"),
                ]),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(transport.requests()[0].url.contains("sendUpdates=all"));
    // Two, not one: the person being added is the person most in need of the
    // mail, and counting the pre-patch list would leave them out of the sentence.
    assert_eq!(result.message, "Updated the guest list · told 2 guests");
}

#[tokio::test]
async fn a_change_only_the_organizer_can_see_does_not_mail_anybody() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seeded_event(&db, account, &["ada@example.com"]);
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            // An alert offset is between the owner and their own phone.
            patch: EventPatch {
                reminder_minutes: Some(vec![15]),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(
        transport.requests()[0].url.contains("sendUpdates=none"),
        "{}",
        transport.requests()[0].url
    );
    assert_eq!(result.message, "Saved the event");
}

#[tokio::test]
async fn a_quiet_edit_is_available_and_the_undo_stays_quiet_too() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seeded_event(&db, account, &["ada@example.com"]);
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                title: Some("Board call (short)".into()),
                notify: Some(Notify::Nobody),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(transport.requests()[0].url.contains("sendUpdates=none"));
    assert_eq!(result.message, "Renamed the event");
    assert_eq!(result.undo_label, None);
    // The correction is as quiet as the change was.
    match result.undo {
        Some(Command::UpdateEvent { patch, .. }) => {
            assert_eq!(patch.notify, Some(Notify::Nobody));
            assert_eq!(patch.title.as_deref(), Some("Board call"));
        }
        other => panic!("expected an update inverse, got {other:?}"),
    }
}

#[tokio::test]
async fn deleting_a_meeting_sends_the_cancellation() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seeded_event(&db, account, &["ada@example.com", "bob@example.com"]);
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::new(204, Vec::new()))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::DeleteEvent {
            event_id,
            scope: EventScope::This,
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(
        transport.requests()[0].url.contains("sendUpdates=all"),
        "a meeting that vanishes from one calendar and stays in everyone else's \
         is a missed cancellation: {}",
        transport.requests()[0].url
    );
    assert_eq!(
        result.message,
        "Deleted “Board call” · cancelled with 2 guests"
    );
    assert_eq!(result.undo_label.as_deref(), Some("Deleted “Board call”"));
    // Putting it back re-invites, which is the only honest reversal there is.
    match result.undo {
        Some(Command::CreateEvent { draft, .. }) => {
            assert_eq!(draft.notify, Some(Notify::Guests));
            assert_eq!(draft.attendees.len(), 2);
        }
        other => panic!("expected a create inverse, got {other:?}"),
    }
}

#[tokio::test]
async fn a_move_re_invites_once_rather_than_cancelling_and_inviting() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = seeded_event(&db, account, &["ada@example.com"]);
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(200, r#"{"id":"evt-2"}"#)),
        Ok(HttpResponse::new(204, Vec::new())),
    ]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::MoveEvent {
            event_id,
            account_id: account,
            calendar_id: "work".into(),
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let reqs = transport.requests();
    assert!(reqs[0].url.contains("sendUpdates=all"), "{}", reqs[0].url);
    // The source copy goes quietly. A cancellation arriving beside the fresh
    // invitation for the same meeting reads as "your meeting is off".
    assert!(reqs[1].url.contains("sendUpdates=none"), "{}", reqs[1].url);
    assert_eq!(result.message, "Moved “Board call” · re-invited 1 guest");
}

#[tokio::test]
async fn an_rsvp_tells_the_organizer_and_can_carry_a_note() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = db
        .write(|c| {
            queries::upsert_event(
                c,
                &NewEvent {
                    account_id: account,
                    calendar_id: "primary".into(),
                    google_event_id: "evt-1".into(),
                    title: "Standup".into(),
                    attendees: vec![Participant::new("a@example.com")],
                    rsvp_status: Some(RsvpStatus::NeedsAction),
                    status: "confirmed".into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();

    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"evt-1","attendees":[{"email":"a@example.com","self":true,
                 "responseStatus":"needsAction"}]}"#,
        )),
        Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#)),
    ]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::Rsvp {
            event_id,
            response: RsvpStatus::Tentative,
            comment: Some("Might be five minutes late".into()),
            notify: None,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let reqs = transport.requests();
    assert!(reqs[1].url.contains("sendUpdates=all"), "{}", reqs[1].url);
    let body = body_json(&reqs[1]);
    assert_eq!(body["attendees"][0]["responseStatus"], "tentative");
    assert_eq!(body["attendees"][0]["comment"], "Might be five minutes late");
}

#[tokio::test]
async fn answering_the_same_way_twice_is_still_free_unless_a_note_came_with_it() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = db
        .write(|c| {
            queries::upsert_event(
                c,
                &NewEvent {
                    account_id: account,
                    calendar_id: "primary".into(),
                    google_event_id: "evt-1".into(),
                    title: "Standup".into(),
                    attendees: vec![Participant::new("a@example.com")],
                    rsvp_status: Some(RsvpStatus::Accepted),
                    status: "confirmed".into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();

    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"evt-1","attendees":[{"email":"a@example.com","self":true,
                 "responseStatus":"accepted"}]}"#,
        )),
        Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#)),
    ]);
    let d = dispatcher(&db, transport.clone());

    d.execute(Command::Rsvp {
        event_id,
        response: RsvpStatus::Accepted,
        comment: None,
        notify: None,
    })
    .await
    .unwrap();
    assert_eq!(transport.call_count(), 0, "same answer, no round trip");

    d.execute(Command::Rsvp {
        event_id,
        response: RsvpStatus::Accepted,
        comment: Some("Bringing the deck".into()),
        notify: None,
    })
    .await
    .unwrap();
    assert_eq!(
        transport.call_count(),
        2,
        "a note is news even when the answer is not"
    );
}

// ============================================= calendar: adding the video call

#[tokio::test]
async fn asking_for_a_meet_link_sends_a_create_request_and_adopts_the_answer() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"evt-new","conferenceData":{"conferenceId":"abc-defg-hij",
             "conferenceSolution":{"name":"Google Meet"},
             "entryPoints":[{"entryPointType":"video",
                             "uri":"https://meet.google.com/abc-defg-hij"}]}}"#,
    ))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::CreateEvent {
            account_id: account,
            calendar_id: "primary".into(),
            draft: EventDraft {
                conferencing: Some(Conferencing::Meet),
                ..timed_draft("Board call", NOON, 1)
            },
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let req = &transport.requests()[0];
    // Version 0 makes Google read the block and do nothing with it.
    assert!(req.url.contains("conferenceDataVersion=1"), "{}", req.url);
    let body = body_json(req);
    assert_eq!(
        body["conferenceData"]["createRequest"]["conferenceSolutionKey"]["type"],
        "hangoutsMeet"
    );
    assert!(body["conferenceData"]["createRequest"]["requestId"].is_string());

    // The link Google minted is on the row now, not at the next sync.
    let events = all_events(&db);
    let conference = events[0].conference.as_ref().expect("the minted conference");
    assert_eq!(
        conference.video().map(|e| e.uri.as_str()),
        Some("https://meet.google.com/abc-defg-hij")
    );
}

#[tokio::test]
async fn taking_the_call_off_an_event_sends_an_explicit_null_and_offers_no_undo() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "a@example.com");
    let event_id = db
        .write(|c| {
            queries::upsert_event(
                c,
                &NewEvent {
                    account_id: account,
                    calendar_id: "primary".into(),
                    google_event_id: "evt-1".into(),
                    title: "Board call".into(),
                    start_ts: NOON,
                    end_ts: NOON + HOUR_MS,
                    status: "confirmed".into(),
                    conference: Some(mach_lib::db::models::EventConference {
                        id: Some("abc-defg-hij".into()),
                        name: Some("Google Meet".into()),
                        entry_points: vec![mach_lib::db::models::ConferenceEntry {
                            kind: "video".into(),
                            uri: "https://meet.google.com/abc-defg-hij".into(),
                            label: None,
                            pin: None,
                            region_code: None,
                        }],
                        notes: None,
                    }),
                    ..Default::default()
                },
            )
        })
        .unwrap();

    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(200, r#"{"id":"evt-1"}"#))]);
    let d = dispatcher(&db, transport.clone());

    let result = d
        .execute(Command::UpdateEvent {
            event_id,
            patch: EventPatch {
                conferencing: Some(Conferencing::None),
                ..Default::default()
            },
            scope: EventScope::This,
        })
        .await
        .unwrap();

    assert!(result.ok, "{result:?}");
    let body = body_json(&transport.requests()[0]);
    assert!(
        body.get("conferenceData").is_some() && body["conferenceData"].is_null(),
        "removing a call is an explicit null, not an absent key: {body}"
    );
    assert!(all_events(&db)[0].conference.is_none());
    // Google mints a new code every time, so there is no putting that meeting
    // back — offering an undo would hand back a different address.
    assert_eq!(result.undo, None);
}
