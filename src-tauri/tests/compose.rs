//! Behaviour tests for the composer.
//!
//! This unit is the only one whose output other people see. A sync bug shows up
//! as a missing row in your own list; a threading bug shows up as a reply
//! that starts a new conversation in a *client Mach cannot observe*. So these
//! tests assert on the generated RFC822 bytes rather than on the shape of a
//! struct, and the load-bearing ones are:
//!
//!  * `a_reply_carries_the_whole_reference_chain` — the header that decides
//!    whether Apple Mail and Thunderbird group the reply with its thread.
//!  * `re_is_never_stacked` / `a_reply_to_a_forward_keeps_the_fwd` — subject
//!    hygiene, which is the visible half of the same problem.
//!  * `a_spanish_subject_and_body_survive_the_round_trip` — plenty of mailboxes carry
//!    Spanish-language correspondence; mojibake here is silent and permanent.
//!  * `cancelling_inside_the_undo_window_makes_no_request_at_all` — the undo
//!    guarantee, asserted as *zero* HTTP calls, not as a cancelled promise.
//!  * `a_message_queued_before_a_crash_still_leaves` — the outbox is durable,
//!    which is the whole reason it is a table.
//!
//! Nothing here touches the network: the Gmail client's injected
//! `HttpTransport` is a scripted fake, exactly as in `tests/google.rs`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use mail_parser::MessageParser;

use mach_lib::commands::{AccountClients, GoogleClients};
use mach_lib::db::models::{
    NewAccount, NewMessage, NewThread, Participant,
};
use mach_lib::db::{queries, Db};
use mach_lib::google::types::{decode_base64url, encode_base64url};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, RetryPolicy, StaticTokenProvider,
    TransportError,
};
use mach_lib::ipc::compose::engine as compose;

use compose::address::reply_recipients;
use compose::draft::{self, Draft, DraftKind};
use compose::markdown;
use compose::mime::{
    build_rfc822, forward_subject, references_for_reply, reply_subject, Mailbox, Outgoing,
};
use compose::outbox::{Outbox, OutboxState, UNDO_WINDOW_MS};

// ============================================================== test doubles

struct FakeTransport {
    responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    default: Mutex<Result<HttpResponse, TransportError>>,
    requests: Mutex<Vec<HttpRequest>>,
    /// Runs as a request goes out, before its response is handed back.
    ///
    /// Some races can only be set up from here. A push holds a `Draft` it
    /// loaded before the request; whether the row still exists when the
    /// response lands is the whole question, and there is no other seam
    /// between those two moments.
    #[allow(clippy::type_complexity)]
    on_request: Mutex<Option<Box<dyn Fn(&HttpRequest) + Send>>>,
}

impl FakeTransport {
    fn always_ok() -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::new()),
            default: Mutex::new(Ok(HttpResponse::json(
                200,
                r#"{"id":"sent-1","threadId":"t-1"}"#,
            ))),
            requests: Mutex::new(Vec::new()),
            on_request: Mutex::new(None),
        })
    }

    fn always_failing(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::new()),
            default: Mutex::new(Ok(HttpResponse::json(status, body.to_string()))),
            requests: Mutex::new(Vec::new()),
            on_request: Mutex::new(None),
        })
    }

    fn scripted(responses: Vec<Result<HttpResponse, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            default: Mutex::new(Ok(HttpResponse::json(
                200,
                r#"{"id":"sent-1","threadId":"t-1"}"#,
            ))),
            requests: Mutex::new(Vec::new()),
            on_request: Mutex::new(None),
        })
    }

    fn call_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// Requests to `messages.send` only.
    ///
    /// `call_count` used to be a fine proxy for "did a message leave", and it
    /// stopped being one when saving a draft started pushing it to Gmail: the
    /// undo guarantee is about *sending*, and a draft write is not a send.
    fn send_count(&self) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.url.contains("/messages/send"))
            .count()
    }

    fn draft_requests(&self) -> Vec<HttpRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.url.contains("/drafts"))
            .collect()
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// The RFC822 bytes Gmail was actually handed, decoded back out of `raw`.
    fn sent_rfc822(&self) -> Vec<Vec<u8>> {
        self.requests()
            .iter()
            .filter_map(|r| r.body.clone())
            .filter_map(|body| {
                let value: serde_json::Value = serde_json::from_slice(&body).ok()?;
                let raw = value.get("raw")?.as_str()?.to_string();
                decode_base64url(&raw).ok()
            })
            .collect()
    }
}

impl HttpTransport for FakeTransport {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        if let Some(hook) = self.on_request.lock().unwrap().as_ref() {
            hook(&request);
        }
        self.requests.lock().unwrap().push(request);
        let next = self.responses.lock().unwrap().pop_front();
        let out = next.unwrap_or_else(|| self.default.lock().unwrap().clone());
        Box::pin(async move { out })
    }
}

// ================================================================== fixtures

const NOW: i64 = 1_775_000_000_000; // a fixed instant, so the bytes are stable

fn clients(transport: Arc<FakeTransport>) -> Arc<dyn GoogleClients> {
    Arc::new(
        AccountClients::new(transport)
            .with_gmail_base_url("https://gmail.test/gmail/v1")
            .with_retry_policy(RetryPolicy::none())
            .with_account(1, Arc::new(StaticTokenProvider::new("token-1"))),
    )
}

fn outbox(db: &Db, transport: Arc<FakeTransport>) -> Outbox {
    Outbox::new(db.clone(), clients(transport)).expect("outbox")
}

fn person(name: &str, email: &str) -> Participant {
    Participant {
        name: Some(name.to_string()),
        email: email.to_string(),
    }
}

fn seed_account(db: &Db, email: &str, display: Option<&str>) -> i64 {
    db.write(|c| {
        queries::upsert_account(
            c,
            &NewAccount {
                email: email.to_string(),
                display_name: display.map(str::to_string),
                token_ref: String::new(),
                colour_index: 0,
            },
        )
    })
    .unwrap()
}

fn seed_thread(db: &Db, account_id: i64, subject: &str) -> i64 {
    db.write(|c| {
        queries::upsert_thread(
            c,
            &NewThread {
                account_id,
                gmail_thread_id: "gthread-1".to_string(),
                participants: vec![],
                subject: subject.to_string(),
                snippet: String::new(),
                last_message_at: NOW - 60_000,
                is_unread: false,
                message_count: 1,
                has_attachments: false,
                label_ids: vec!["INBOX".to_string()],
            },
        )
    })
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn seed_message(
    db: &Db,
    thread_id: i64,
    account_id: i64,
    gmail_id: &str,
    from: Participant,
    to: Vec<Participant>,
    cc: Vec<Participant>,
    subject: &str,
    message_id: &str,
    references: Option<&str>,
    body: &str,
) -> i64 {
    seed_message_with_reply_to(
        db, thread_id, account_id, gmail_id, from, to, cc, subject, message_id, references, body,
        &[],
    )
}

/// Same, but the sender set `Reply-To` — the shape mailing lists produce.
#[allow(clippy::too_many_arguments)]
fn seed_message_with_reply_to(
    db: &Db,
    thread_id: i64,
    account_id: i64,
    gmail_id: &str,
    from: Participant,
    to: Vec<Participant>,
    cc: Vec<Participant>,
    subject: &str,
    message_id: &str,
    references: Option<&str>,
    body: &str,
    reply_to: &[Participant],
) -> i64 {
    db.write(|c| {
        queries::upsert_message(
            c,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: gmail_id.to_string(),
                rfc822_message_id: Some(message_id.to_string()),
                reply_to: reply_to.to_vec(),
                in_reply_to: references.and_then(|r| r.split_whitespace().last()).map(str::to_string),
                references: references.map(str::to_string),
                from,
                to,
                cc,
                bcc: vec![],
                subject: subject.to_string(),
                body_html: None,
                body_text: Some(body.to_string()),
                snippet: body.chars().take(60).collect(),
                internal_date: NOW - 60_000,
                is_unread: false,
                is_draft: false,
                ..Default::default()
            },
        )
    })
    .unwrap()
}

/// One account, one thread, one inbound message from Tawny to Alex with Sam
/// on Cc. The shape almost every test below starts from.
fn seeded() -> (Db, i64, i64, i64) {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("Alex Rivera"));
    let thread = seed_thread(&db, account, "Series A data room");
    let message = seed_message(
        &db,
        thread,
        account,
        "gmsg-1",
        person("Tawny Rivers", "tawny@partner.com"),
        vec![
            person("Alex Rivera", "alex@example.com"),
            person("Sam Patel", "sam@partner.com"),
        ],
        vec![person("Dana Wu", "dana@partner.com")],
        "Series A data room",
        "<parent-1@mail.partner.com>",
        None,
        "Can you send the data room link?\nThanks.",
    );
    (db, account, thread, message)
}

/// A conversation of three, each message with its own people and its own place
/// in the chain. What replying "from any point in a thread" has to get right.
///
/// ```text
/// 1  Tawny  → Alex, Sam        cc Dana    <one@x>
/// 2  Priya  → Alex             cc Rex     <two@x>    refs: one
/// 3  Tawny  → Alex             cc Dana    <three@x>  refs: one two
/// ```
///
/// Returns `(db, account, thread, [m1, m2, m3])`.
fn seeded_long_thread() -> (Db, i64, i64, Vec<i64>) {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("Alex Rivera"));
    let thread = seed_thread(&db, account, "Series A data room");

    let one = seed_message_at(
        &db,
        thread,
        account,
        "gmsg-1",
        person("Tawny Rivers", "tawny@partner.com"),
        vec![
            person("Alex Rivera", "alex@example.com"),
            person("Sam Patel", "sam@partner.com"),
        ],
        vec![person("Dana Wu", "dana@partner.com")],
        "<one@x>",
        None,
        "Can you send the data room link?",
        NOW - 300_000,
    );
    let two = seed_message_at(
        &db,
        thread,
        account,
        "gmsg-2",
        person("Priya Raman", "priya@partner.com"),
        vec![person("Alex Rivera", "alex@example.com")],
        vec![person("Rex Oduya", "rex@partner.com")],
        "<two@x>",
        Some("<one@x>"),
        "The diligence checklist is attached.",
        NOW - 200_000,
    );
    let three = seed_message_at(
        &db,
        thread,
        account,
        "gmsg-3",
        person("Tawny Rivers", "tawny@partner.com"),
        vec![person("Alex Rivera", "alex@example.com")],
        vec![person("Dana Wu", "dana@partner.com")],
        "<three@x>",
        Some("<one@x> <two@x>"),
        "Any movement on this?",
        NOW - 100_000,
    );

    (db, account, thread, vec![one, two, three])
}

/// [`seed_message`] with the clock as an argument, so a thread can have an
/// order. The conversation is read oldest first, and every message in the older
/// helpers shares one instant.
#[allow(clippy::too_many_arguments)]
fn seed_message_at(
    db: &Db,
    thread_id: i64,
    account_id: i64,
    gmail_id: &str,
    from: Participant,
    to: Vec<Participant>,
    cc: Vec<Participant>,
    message_id: &str,
    references: Option<&str>,
    body: &str,
    at: i64,
) -> i64 {
    db.write(|c| {
        queries::upsert_message(
            c,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: gmail_id.to_string(),
                rfc822_message_id: Some(message_id.to_string()),
                reply_to: vec![],
                in_reply_to: references
                    .and_then(|r| r.split_whitespace().last())
                    .map(str::to_string),
                references: references.map(str::to_string),
                from,
                to,
                cc,
                bcc: vec![],
                subject: "Series A data room".to_string(),
                body_html: None,
                body_text: Some(body.to_string()),
                snippet: body.chars().take(60).collect(),
                internal_date: at,
                is_unread: false,
                is_draft: false,
                ..Default::default()
            },
        )
    })
    .unwrap()
}

fn headers_of(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.split("\r\n\r\n").next().unwrap_or_default().to_string()
}

fn built_bytes(db: &Db, draft: &Draft) -> Vec<u8> {
    let built = draft::build(db, draft, NOW, 0x2b2b).expect("build");
    build_rfc822(&built.outgoing).expect("rfc822")
}

fn reply_draft(db: &Db, thread: i64, kind: DraftKind, body: &str) -> Draft {
    let mut d = draft::prepare(db, thread, kind, "d1".to_string()).expect("prepare");
    d.body = body.to_string();
    d
}

// ========================================================= threading headers

#[test]
fn a_reply_names_its_parent_in_in_reply_to_and_references() {
    let (db, _account, thread, _message) = seeded();
    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, "On it."));
    let headers = headers_of(&bytes);

    assert!(
        headers.contains("In-Reply-To: <parent-1@mail.partner.com>"),
        "missing In-Reply-To in:\n{headers}"
    );
    assert!(
        headers.contains("References: <parent-1@mail.partner.com>"),
        "missing References in:\n{headers}"
    );
}

#[test]
fn a_reply_carries_the_whole_reference_chain() {
    // Six messages deep. A reply that names only its parent looks right in
    // Gmail (which threads on its own ids) and splits the conversation in every
    // client that groups on References — which is the failure nobody sending
    // the mail can see.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "Long thread");
    let chain = "<a@x> <b@x> <c@x> <d@x> <e@x>";
    seed_message(
        &db,
        thread,
        account,
        "gmsg-deep",
        person("Tawny", "tawny@partner.com"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "Long thread",
        "<f@x>",
        Some(chain),
        "still going",
    );

    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, "yes"));
    let headers = headers_of(&bytes).replace("\r\n\t", " ").replace("\r\n ", " ");

    assert!(headers.contains("In-Reply-To: <f@x>"), "{headers}");
    let references = headers
        .lines()
        .find(|l| l.starts_with("References:"))
        .expect("a References header");
    assert_eq!(
        references,
        "References: <a@x> <b@x> <c@x> <d@x> <e@x> <f@x>",
        "the parent's chain must be preserved and extended, not replaced"
    );
}

#[test]
fn the_reference_chain_falls_back_to_in_reply_to_when_there_is_no_references() {
    // Some clients send In-Reply-To without References. Dropping the parent's
    // ancestry there is the same thread split by another route.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "Old client");
    let mut message = db
        .read(|c| queries::messages_for_thread(c, thread))
        .unwrap();
    assert!(message.is_empty());

    seed_message(
        &db,
        thread,
        account,
        "gmsg-old",
        person("Tawny", "tawny@partner.com"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "Old client",
        "<second@x>",
        None,
        "hi",
    );
    // Give it an In-Reply-To but no References, the way an old client would.
    db.write(|c| {
        c.execute(
            "UPDATE messages SET in_reply_to = '<first@x>', references_header = NULL",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    message = db
        .read(|c| queries::messages_for_thread(c, thread))
        .unwrap();
    let (in_reply_to, references) = references_for_reply(&message[0]);
    assert_eq!(in_reply_to.as_deref(), Some("second@x"));
    assert_eq!(references, vec!["first@x", "second@x"]);
}

#[test]
fn a_fresh_message_has_no_threading_headers_at_all() {
    let (db, account, _thread, _message) = seeded();
    let fresh = Draft {
        id: "d-new".into(),
        account_id: account,
        thread_id: None,
        reply_to_id: None,
        kind: DraftKind::New,
        to: vec![Mailbox::named("Tawny", "tawny@partner.com")],
        cc: vec![],
        bcc: vec![],
        subject: "Hello".into(),
        body: "Hi there.".into(),
        body_format: Default::default(),
        updated_at: 0,
        remote: Default::default(),
        attachments: Vec::new(),
    };
    let headers = headers_of(&built_bytes(&db, &fresh));
    assert!(!headers.contains("In-Reply-To"), "{headers}");
    assert!(!headers.contains("References"), "{headers}");
}

#[test]
fn a_forward_does_not_thread_onto_the_original() {
    // Threading a forward onto its source is how a message you took out of a
    // conversation reappears inside it, for you and for the recipient.
    let (db, _account, thread, _message) = seeded();
    let mut d = draft::prepare(&db, thread, DraftKind::Forward, "d1".into()).unwrap();
    d.to = vec![Mailbox::new("someone@else.com")];
    d.body = "fyi".into();

    let headers = headers_of(&built_bytes(&db, &d));
    assert!(!headers.contains("In-Reply-To"), "{headers}");
    assert!(!headers.contains("References"), "{headers}");
    assert!(headers.contains("Subject: Fwd: Series A data room"), "{headers}");
}

// ============================================= replying from any point in it

#[test]
fn a_reply_to_a_middle_message_threads_onto_that_message() {
    // Eleven messages in, "reply" cannot mean "answer the last one". The chain
    // has to stop at the message being answered: naming a later id makes the
    // reply hang off something the recipient may not have, and carrying the
    // whole thread's References puts it in the wrong place in every client that
    // groups on them.
    let (db, _account, _thread, ids) = seeded_long_thread();
    let middle = ids[1];

    let mut d = draft::prepare_reply_to(&db, middle, DraftKind::Reply, "d1".into()).unwrap();
    assert_eq!(d.reply_to_id, Some(middle), "the draft answers this message");
    d.body = "On it.".into();

    let headers = headers_of(&built_bytes(&db, &d))
        .replace("\r\n\t", " ")
        .replace("\r\n ", " ");

    assert!(headers.contains("In-Reply-To: <two@x>"), "{headers}");
    let references = headers
        .lines()
        .find(|l| l.starts_with("References:"))
        .expect("a References header");
    assert_eq!(
        references, "References: <one@x> <two@x>",
        "the ancestry of the message being answered, and nothing after it"
    );
    assert!(
        !headers.contains("three@x"),
        "the newest message is not this reply's parent:\n{headers}"
    );
}

#[test]
fn with_no_message_named_a_reply_still_answers_the_newest() {
    // The strip under the conversation has always meant "answer the last
    // message", and it still does.
    let (db, _account, thread, ids) = seeded_long_thread();
    let d = draft::prepare(&db, thread, DraftKind::Reply, "d1".into()).unwrap();

    assert_eq!(d.reply_to_id, Some(ids[2]));
    let headers = headers_of(&built_bytes(&db, &{
        let mut d = d;
        d.body = "ok".into();
        d
    }));
    assert!(headers.contains("In-Reply-To: <three@x>"), "{headers}");
}

#[test]
fn reply_all_to_a_middle_message_addresses_that_messages_people() {
    // Not the thread's participants: the second message is from Priya with Rex
    // on Cc, and answering it must not mail Tawny, Sam and Dana — who are on
    // the other two messages and were never on this one.
    let (db, _account, _thread, ids) = seeded_long_thread();
    let d = draft::prepare_reply_to(&db, ids[1], DraftKind::ReplyAll, "d1".into()).unwrap();

    assert_eq!(
        d.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["priya@partner.com"]
    );
    assert_eq!(
        d.cc.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["rex@partner.com"]
    );

    // And a plain reply to the same message goes to its author alone.
    let plain = draft::prepare_reply_to(&db, ids[1], DraftKind::Reply, "d2".into()).unwrap();
    assert_eq!(
        plain.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["priya@partner.com"]
    );
    assert!(plain.cc.is_empty());
}

#[test]
fn a_reply_to_a_middle_message_quotes_that_message() {
    let (db, _account, _thread, ids) = seeded_long_thread();
    let mut d = draft::prepare_reply_to(&db, ids[1], DraftKind::Reply, "d1".into()).unwrap();
    d.body = "Looking now.".into();
    let bytes = built_bytes(&db, &d);
    let parsed = MessageParser::new().parse(&bytes).unwrap();

    let plain = parsed.body_text(0).unwrap();
    assert!(plain.contains("Priya Raman"), "the attribution is theirs:\n{plain}");
    assert!(plain.contains("> The diligence checklist is attached."), "{plain}");
    assert!(
        !plain.contains("Any movement on this?"),
        "the newest message must not be the quoted one:\n{plain}"
    );
}

#[test]
fn a_forward_of_a_middle_message_carries_that_message() {
    let (db, _account, _thread, ids) = seeded_long_thread();
    let mut d = draft::prepare_reply_to(&db, ids[1], DraftKind::Forward, "d1".into()).unwrap();
    d.to = vec![Mailbox::new("someone@else.com")];
    d.body = "fyi".into();
    let bytes = built_bytes(&db, &d);

    let plain = MessageParser::new().parse(&bytes).unwrap().body_text(0).unwrap().into_owned();
    assert!(plain.contains("---------- Forwarded message ---------"), "{plain}");
    assert!(plain.contains("The diligence checklist is attached."), "{plain}");
    assert!(plain.contains("priya@partner.com"), "{plain}");
    assert!(
        !plain.contains("Any movement on this?"),
        "a forward of the middle message must not reproduce the last one:\n{plain}"
    );

    // A forward still starts its own conversation, whichever message it took.
    let headers = headers_of(&bytes);
    assert!(!headers.contains("In-Reply-To"), "{headers}");
    assert!(!headers.contains("References"), "{headers}");
}

#[test]
fn an_unsent_draft_row_is_not_something_to_answer() {
    // A draft is mirrored into the conversation as a message row, so it is a
    // row the pointer can reach. Threading a reply onto your own unsent text is
    // the mistake `context_for_thread` skips drafts to avoid.
    let (db, account, thread, _ids) = seeded_long_thread();
    let draft_row = seed_message_at(
        &db,
        thread,
        account,
        "mach-draft:d-local",
        person("Alex Rivera", "alex@example.com"),
        vec![person("Tawny Rivers", "tawny@partner.com")],
        vec![],
        "<draft@x>",
        Some("<one@x> <two@x> <three@x>"),
        "half a sentence",
        NOW - 50_000,
    );
    db.write(|c| {
        c.execute("UPDATE messages SET is_draft = 1 WHERE id = ?1", [draft_row])?;
        Ok(())
    })
    .unwrap();

    let refused = draft::prepare_reply_to(&db, draft_row, DraftKind::Reply, "d1".into());
    assert!(refused.is_err(), "a draft row must not be a reply parent");
}

// =================================================================== subject

#[test]
fn re_is_never_stacked() {
    assert_eq!(reply_subject("Invoice"), "Re: Invoice");
    assert_eq!(reply_subject("Re: Invoice"), "Re: Invoice");
    assert_eq!(reply_subject("RE: Invoice"), "Re: Invoice");
    assert_eq!(reply_subject("re: re: Invoice"), "Re: Invoice");
    assert_eq!(reply_subject("Re[2]: Invoice"), "Re: Invoice");
    assert_eq!(reply_subject("RE : Invoice"), "Re: Invoice");
    // A word that merely starts with "re" is not a prefix.
    assert_eq!(reply_subject("Rethinking pricing"), "Re: Rethinking pricing");
    assert_eq!(reply_subject("Regarding: pricing"), "Re: Regarding: pricing");
}

#[test]
fn a_reply_to_a_forward_keeps_the_fwd() {
    // Gmail's own answer to "Fwd: Invoice" is "Re: Fwd: Invoice". Dropping the
    // Fwd renames somebody else's thread.
    assert_eq!(reply_subject("Fwd: Invoice"), "Re: Fwd: Invoice");
    assert_eq!(forward_subject("Fwd: Invoice"), "Fwd: Invoice");
    assert_eq!(forward_subject("FW: Invoice"), "Fwd: Invoice");
    assert_eq!(forward_subject("Re: Invoice"), "Fwd: Re: Invoice");
}

#[test]
fn the_subject_line_in_the_bytes_is_not_stacked_either() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "Re: Series A data room");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-1",
        person("Tawny", "tawny@partner.com"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "Re: Series A data room",
        "<p@x>",
        None,
        "hi",
    );
    let headers = headers_of(&built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, "ok")));
    assert!(
        headers.contains("Subject: Re: Series A data room"),
        "{headers}"
    );
    assert!(!headers.contains("Re: Re:"), "{headers}");
}

// ================================================================== encoding

#[test]
fn a_spanish_subject_and_body_survive_the_round_trip() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("Alex Rivera"));
    let thread = seed_thread(&db, account, "Reunión de mañana");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-1",
        person("José García", "jose@socio.es"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "Reunión de mañana",
        "<es-1@socio.es>",
        None,
        "¿Nos vemos a las diez?",
    );

    let body = "Sí, perfecto — nos vemos a las diez. ¡Un abrazo!";
    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, body));

    // The header must not carry raw UTF-8: RFC 5322 headers are US-ASCII, and a
    // raw eight-bit header is what produces "ReuniÃ³n" in the recipient's list.
    let headers = headers_of(&bytes);
    assert!(headers.is_ascii(), "headers are not 7-bit clean:\n{headers}");
    assert!(
        headers.contains("=?utf-8?Q?") || headers.contains("=?utf-8?B?"),
        "the subject was not RFC 2047 encoded:\n{headers}"
    );

    let parsed = MessageParser::new().parse(&bytes).expect("parses");
    assert_eq!(parsed.subject().unwrap(), "Re: Reunión de mañana");

    let text = parsed.body_text(0).expect("a text part");
    assert!(text.contains(body), "text part lost its accents: {text}");
    let html = parsed.body_html(0).expect("an html part");
    assert!(html.contains("¡Un abrazo!"), "html part lost its accents: {html}");

    // And the quoted original keeps its own accents.
    assert!(text.contains("> ¿Nos vemos a las diez?"), "{text}");
}

#[test]
fn a_non_ascii_display_name_is_a_bare_encoded_word_not_a_quoted_one() {
    // `mail-builder` writes `"=?utf-8?B?…?=" <a@b>`. RFC 2047 §5 forbids an
    // encoded-word inside a quoted-string, and a client that obeys the spec
    // shows the literal `=?utf-8?B?…?=` as the sender's name.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("José García"));
    let bytes = build_rfc822(&Outgoing {
        from: Mailbox::named("José García", "alex@example.com"),
        to: vec![Mailbox::named("Ana Ruiz", "ana@socio.es")],
        cc: vec![],
        bcc: vec![],
        subject: "Hola".into(),
        text: "Hola".into(),
        html: "<p>Hola</p>".into(),
        attachments: vec![],
        in_reply_to: None,
        references: vec![],
        message_id: "m1@example.com".into(),
        date_ms: NOW,
    })
    .unwrap();
    let _ = account;

    let headers = headers_of(&bytes);
    assert!(
        headers.contains("From: =?UTF-8?B?Sm9zw6kgR2FyY8OtYQ==?= <alex@example.com>"),
        "{headers}"
    );
    assert!(
        !headers.contains("\"=?"),
        "an encoded-word ended up inside a quoted-string:\n{headers}"
    );

    let parsed = MessageParser::new().parse(&bytes).unwrap();
    let from = parsed.from().unwrap().first().unwrap();
    assert_eq!(from.name(), Some("José García"));
}

#[test]
fn an_ascii_name_that_needs_quoting_gets_quoted_and_one_that_does_not_stays_bare() {
    let plain = build_rfc822(&Outgoing {
        from: Mailbox::named("Alex Rivera", "alex@example.com"),
        to: vec![
            Mailbox::named("Patel, Sam", "sam@partner.com"),
            Mailbox::new("bare@partner.com"),
        ],
        cc: vec![],
        bcc: vec![],
        subject: "x".into(),
        text: "x".into(),
        html: "<p>x</p>".into(),
        attachments: vec![],
        in_reply_to: None,
        references: vec![],
        message_id: "m@x".into(),
        date_ms: NOW,
    })
    .unwrap();
    let headers = headers_of(&plain);
    assert!(headers.contains("From: Alex Rivera <alex@example.com>"), "{headers}");
    assert!(
        headers.contains("To: \"Patel, Sam\" <sam@partner.com>, <bare@partner.com>"),
        "{headers}"
    );
}

#[test]
fn the_raw_field_is_base64url_and_round_trips() {
    let (db, _a, thread, _m) = seeded();
    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, "ok"));
    let encoded = encode_base64url(&bytes);
    assert!(
        !encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='),
        "not URL-safe, unpadded base64"
    );
    assert_eq!(decode_base64url(&encoded).unwrap(), bytes);
}

// ===================================================================== parts

#[test]
fn every_message_is_multipart_alternative_with_both_parts() {
    let (db, _a, thread, _m) = seeded();
    let bytes = built_bytes(
        &db,
        &reply_draft(
            &db,
            thread,
            DraftKind::Reply,
            "<div><b>Yes</b> — sending now.</div>",
        ),
    );
    let text = String::from_utf8_lossy(&bytes).into_owned();

    assert!(text.contains("multipart/alternative"), "{text}");
    assert!(text.contains("Content-Type: text/plain; charset=\"utf-8\""), "{text}");
    assert!(text.contains("Content-Type: text/html; charset=\"utf-8\""), "{text}");

    let parsed = MessageParser::new().parse(&bytes).unwrap();
    let plain = parsed.body_text(0).expect("text part");
    let html = parsed.body_html(0).expect("html part");
    // The editor's HTML *is* the html part; the plain part is read back out of
    // it, which is why the emphasis survives in one and not the other.
    assert!(html.contains("<b>Yes</b>"), "{html}");
    assert!(plain.contains("Yes — sending now."), "{plain}");
    assert!(!plain.contains("<b>"), "{plain}");
}

#[test]
fn a_draft_written_before_the_editor_was_rich_text_still_renders_as_markdown() {
    // The column default is `markdown` and this is why: a row written by the
    // old `<textarea>` composer is still in the store, and reading its
    // asterisks as HTML would send a message with none of the emphasis in it.
    let (db, _a, thread, _m) = seeded();
    let mut legacy = reply_draft(&db, thread, DraftKind::Reply, "**Yes** — sending now.");
    legacy.body_format = mach_lib::ipc::compose::engine::BodyFormat::Markdown;
    let bytes = built_bytes(&db, &legacy);

    let parsed = MessageParser::new().parse(&bytes).unwrap();
    let plain = parsed.body_text(0).expect("text part");
    let html = parsed.body_html(0).expect("html part");
    assert!(html.contains("<strong>Yes</strong>"), "{html}");
    assert!(plain.contains("**Yes** — sending now."), "{plain}");
}

#[test]
fn the_quoted_original_is_in_the_shape_other_clients_collapse() {
    let (db, _a, thread, _m) = seeded();
    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, "Sending now."));
    let parsed = MessageParser::new().parse(&bytes).unwrap();

    let plain = parsed.body_text(0).unwrap();
    assert!(plain.contains(" wrote:"), "no attribution line:\n{plain}");
    assert!(plain.contains("> Can you send the data room link?"), "{plain}");
    assert!(plain.contains("> Thanks."), "{plain}");

    let html = parsed.body_html(0).unwrap();
    assert!(html.contains("class=\"gmail_quote\""), "{html}");
    assert!(html.contains("<blockquote"), "{html}");
    assert!(html.contains("wrote:"), "{html}");
}

// ================================================================ recipients

#[test]
fn reply_all_excludes_you_dedupes_and_keeps_cc() {
    let (db, _a, thread, _m) = seeded();
    let d = reply_draft(&db, thread, DraftKind::ReplyAll, "ok");

    assert_eq!(
        d.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["tawny@partner.com"]
    );
    assert_eq!(
        d.cc.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["sam@partner.com", "dana@partner.com"],
        "Cc must be preserved, and your own address removed from To *and* Cc"
    );

    let headers = headers_of(&built_bytes(&db, &d));
    assert!(
        !headers.contains("alex@example.com>,") && !headers.contains(", <alex@example.com>"),
        "the sender is a recipient of their own reply:\n{headers}"
    );
}

#[test]
fn a_plain_reply_goes_only_to_the_author() {
    let (db, _a, thread, _m) = seeded();
    let d = reply_draft(&db, thread, DraftKind::Reply, "ok");
    assert_eq!(d.to.len(), 1);
    assert_eq!(d.to[0].email, "tawny@partner.com");
    assert!(d.cc.is_empty());
}

#[test]
fn recipients_are_deduped_case_insensitively() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "Dupes");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-1",
        person("Tawny", "tawny@partner.com"),
        vec![
            person("Alex", "Alex@Example.com"),
            person("Sam", "SAM@partner.com"),
            person("Sam again", "sam@partner.com"),
        ],
        vec![person("Tawny", "TAWNY@partner.com")],
        "Dupes",
        "<p@x>",
        None,
        "hi",
    );

    let d = reply_draft(&db, thread, DraftKind::ReplyAll, "ok");
    assert_eq!(
        d.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["tawny@partner.com"]
    );
    assert_eq!(
        d.cc.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["SAM@partner.com"],
        "the author must not reappear in Cc under a different case, and Sam must appear once"
    );
}

#[test]
fn replying_to_your_own_message_continues_it_rather_than_answering_yourself() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "Mine");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-1",
        person("Alex", "alex@example.com"),
        vec![person("Tawny", "tawny@partner.com")],
        vec![person("Sam", "sam@partner.com")],
        "Mine",
        "<p@x>",
        None,
        "sent this",
    );

    let d = reply_draft(&db, thread, DraftKind::ReplyAll, "one more thing");
    assert_eq!(
        d.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["tawny@partner.com"],
        "a reply to your own message goes to the people you wrote to"
    );
    assert_eq!(
        d.cc.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["sam@partner.com"]
    );
}

#[test]
fn a_forward_is_addressed_by_hand() {
    // Pre-filling a forward from the thread is how a private thread gets
    // forwarded back to the person who wrote it.
    let (db, _a, thread, _m) = seeded();
    let d = draft::prepare(&db, thread, DraftKind::Forward, "d1".into()).unwrap();
    assert!(d.to.is_empty());
    assert!(d.cc.is_empty());
}

// ------------------------------------------------------ a reply is addressed
//
// The defect these pin: `r` on a note the owner had mailed to himself opened a
// composer with an empty To showing its placeholder, and `Re:` with nothing
// after it. Both halves came from the same message — `from`, `to` and the
// account were one address, so removing "yourself" removed everybody, and the
// message row's `Subject` was empty while the conversation's was not.

/// Rule 5. Taking your own address out must not leave the field empty.
#[test]
fn replying_to_a_note_you_mailed_yourself_still_addresses_it() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "a note");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-self",
        person("Alex", "alex@example.com"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "a note",
        "<self-1@x>",
        None,
        "test",
    );

    let d = draft::prepare(&db, thread, DraftKind::Reply, "d1".into()).unwrap();
    assert_eq!(
        d.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["alex@example.com"],
        "a reply that addresses nobody is not a reply"
    );
    assert_eq!(d.subject, "Re: a note");
}

/// The same message, reply-all. Nobody else was on it, so there is nothing to
/// promote out of Cc — and the To must still name somebody.
#[test]
fn reply_all_to_a_note_you_mailed_yourself_still_addresses_it() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "a note");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-self",
        person("Alex", "alex@example.com"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "a note",
        "<self-1@x>",
        None,
        "test",
    );

    let d = draft::prepare(&db, thread, DraftKind::ReplyAll, "d1".into()).unwrap();
    assert_eq!(
        d.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["alex@example.com"]
    );
    assert!(d.cc.is_empty());
}

/// A message with no `Subject` header at all still belongs to a conversation
/// that has one, and that is the subject the reply carries.
#[test]
fn a_reply_falls_back_to_the_conversations_subject() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", None);
    let thread = seed_thread(&db, account, "Series A data room");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-nosubject",
        person("Tawny", "tawny@partner.com"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "",
        "<nosubject-1@x>",
        None,
        "test",
    );

    let reply = draft::prepare(&db, thread, DraftKind::Reply, "d1".into()).unwrap();
    assert_eq!(reply.subject, "Re: Series A data room");
    assert_eq!(
        reply.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["tawny@partner.com"]
    );

    let forward = draft::prepare(&db, thread, DraftKind::Forward, "d2".into()).unwrap();
    assert_eq!(forward.subject, "Fwd: Series A data room");
    assert!(forward.to.is_empty(), "a forward is addressed by hand");
}

/// Rule 2 across accounts: a reply-all must not Cc you at your other address.
#[test]
fn reply_all_removes_every_account_you_hold_not_just_the_sending_one() {
    let db = Db::open_in_memory().unwrap();
    let work = seed_account(&db, "alex@example.com", None);
    seed_account(&db, "alex@personal.example", None);
    let thread = seed_thread(&db, work, "Both of me");
    seed_message(
        &db,
        thread,
        work,
        "gmsg-both",
        person("Tawny", "tawny@partner.com"),
        vec![
            person("Alex", "alex@example.com"),
            person("Alex at home", "alex@personal.example"),
        ],
        vec![person("Sam", "sam@partner.com")],
        "Both of me",
        "<both-1@x>",
        None,
        "hello",
    );

    let d = draft::prepare(&db, thread, DraftKind::ReplyAll, "d1".into()).unwrap();
    assert_eq!(
        d.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["tawny@partner.com"]
    );
    assert_eq!(
        d.cc.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["sam@partner.com"],
        "your other account is still you; a reply-all must not mail you a copy"
    );
}

/// A forward carries the original, whoever it is eventually addressed to.
#[test]
fn a_forward_reproduces_the_original_and_says_so_in_the_subject() {
    let (db, _a, thread, _m) = seeded();
    let mut d = draft::prepare(&db, thread, DraftKind::Forward, "d1".into()).unwrap();
    assert_eq!(d.subject, "Fwd: Series A data room");
    d.to = vec![Mailbox::new("newperson@partner.com")];
    d.body = "See below.".into();

    let parsed_bytes = built_bytes(&db, &d);
    let parsed = MessageParser::new().parse(&parsed_bytes).unwrap();
    let plain = parsed.body_text(0).expect("text part");
    assert!(plain.contains("Forwarded message"), "{plain}");
    assert!(plain.contains("Can you send the data room link?"), "{plain}");
}

#[test]
fn a_message_with_no_recipients_is_refused_before_it_is_built() {
    let result = build_rfc822(&Outgoing {
        from: Mailbox::new("alex@example.com"),
        to: vec![],
        cc: vec![],
        bcc: vec![],
        subject: "x".into(),
        text: "x".into(),
        html: "<p>x</p>".into(),
        attachments: vec![],
        in_reply_to: None,
        references: vec![],
        message_id: "m@x".into(),
        date_ms: NOW,
    });
    assert!(result.is_err());
}

#[test]
fn bcc_rides_in_the_headers_because_that_is_all_gmail_gives_us() {
    let bytes = build_rfc822(&Outgoing {
        from: Mailbox::new("alex@example.com"),
        to: vec![Mailbox::new("tawny@partner.com")],
        cc: vec![],
        bcc: vec![Mailbox::new("quiet@partner.com")],
        subject: "x".into(),
        text: "x".into(),
        html: "<p>x</p>".into(),
        attachments: vec![],
        in_reply_to: None,
        references: vec![],
        message_id: "m@x".into(),
        date_ms: NOW,
    })
    .unwrap();
    assert!(headers_of(&bytes).contains("Bcc: <quiet@partner.com>"));
}

// ============================================================ the undo window

#[tokio::test]
async fn cancelling_inside_the_undo_window_makes_no_request_at_all() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "oops"), NOW, 1)
        .unwrap();
    let entry = out.queue(&built, NOW, NOW + UNDO_WINDOW_MS).unwrap();
    assert_eq!(transport.call_count(), 0, "queueing must not send");

    // Five seconds in: still recallable, still nothing on the wire.
    assert!(out.flush_due(NOW + 5_000).await.unwrap().is_empty());
    assert_eq!(transport.call_count(), 0);

    assert!(out.cancel(&entry.id, NOW + 5_000).unwrap());

    // And well past the window, there is nothing left to send.
    assert!(out.flush_due(NOW + 60_000).await.unwrap().is_empty());
    assert_eq!(
        transport.call_count(),
        0,
        "undo must cancel before anything leaves — not after"
    );
    assert!(out.get(&entry.id).unwrap().is_none());
}

#[tokio::test]
async fn letting_the_window_lapse_sends_exactly_once() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "on it"), NOW, 1)
        .unwrap();
    let entry = out.queue(&built, NOW, NOW + UNDO_WINDOW_MS).unwrap();

    let first = out.flush_due(NOW + UNDO_WINDOW_MS).await.unwrap();
    assert_eq!(first.len(), 1);
    assert!(first[0].sent);
    assert_eq!(transport.call_count(), 1);

    // Two more flushes — a timer and a manual one — must not resend.
    out.flush_due(NOW + 30_000).await.unwrap();
    out.flush_due(NOW + 60_000).await.unwrap();
    assert_eq!(transport.call_count(), 1, "exactly once");

    let stored = out.get(&entry.id).unwrap().unwrap();
    assert_eq!(stored.state, OutboxState::Sent);
    assert_eq!(stored.sent_message_id.as_deref(), Some("sent-1"));
}

#[tokio::test]
async fn the_request_carries_the_thread_id_so_the_reply_lands_in_the_conversation() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "ok"), NOW, 1).unwrap();
    out.queue(&built, NOW, NOW).unwrap();
    out.flush_due(NOW).await.unwrap();

    let request = transport.requests().into_iter().next().unwrap();
    assert!(request.url.ends_with("/users/me/messages/send"), "{}", request.url);
    let body: serde_json::Value =
        serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["threadId"], "gthread-1");

    // And the bytes on the wire are the bytes we built.
    let sent = transport.sent_rfc822();
    assert_eq!(sent.len(), 1);
    assert!(headers_of(&sent[0]).contains("In-Reply-To: <parent-1@mail.partner.com>"));
}

#[tokio::test]
async fn a_message_queued_before_a_crash_still_leaves() {
    // The window is a row, not a timer: nothing in memory has to survive for
    // the message to be sent, so a restart delays it rather than losing it.
    let (db, _a, thread, _m) = seeded();
    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "queued"), NOW, 1)
        .unwrap();

    let entry = {
        let dying = outbox(&db, FakeTransport::always_ok());
        dying.queue(&built, NOW, NOW + UNDO_WINDOW_MS).unwrap()
        // `dying` is dropped here, exactly as the process would be.
    };

    let transport = FakeTransport::always_ok();
    let reborn = outbox(&db, Arc::clone(&transport));
    let outcomes = reborn.flush_due(NOW + UNDO_WINDOW_MS).await.unwrap();

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].sent);
    assert_eq!(outcomes[0].id, entry.id);
    assert_eq!(transport.call_count(), 1);
}

#[tokio::test]
async fn a_retriable_failure_stays_in_the_queue() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(429, r#"{"error":{"message":"slow down"}}"#)),
        Ok(HttpResponse::json(200, r#"{"id":"sent-2","threadId":"gthread-1"}"#)),
    ]);
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "hi"), NOW, 1).unwrap();
    let entry = out.queue(&built, NOW, NOW).unwrap();

    let first = out.flush_due(NOW).await.unwrap();
    assert!(!first[0].sent);
    assert!(first[0].will_retry);
    assert_eq!(out.get(&entry.id).unwrap().unwrap().state, OutboxState::Holding);

    // The local copy is still in the thread, because it is still going to be
    // sent.
    assert_eq!(messages_in(&db, thread).len(), 2);

    let second = out.flush_due(NOW + 120_000).await.unwrap();
    assert!(second[0].sent);
    assert_eq!(out.get(&entry.id).unwrap().unwrap().state, OutboxState::Sent);
}

#[tokio::test]
async fn a_permanent_failure_keeps_the_bytes_and_takes_the_reply_out_of_the_thread() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_failing(403, r#"{"error":{"message":"no scope"}}"#);
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "hi"), NOW, 1).unwrap();
    let entry = out.queue(&built, NOW, NOW).unwrap();
    assert_eq!(messages_in(&db, thread).len(), 2, "the optimistic copy is written first");

    let outcomes = out.flush_due(NOW).await.unwrap();
    assert!(!outcomes[0].sent);
    assert!(!outcomes[0].will_retry);

    let stored = out.get(&entry.id).unwrap().unwrap();
    assert_eq!(stored.state, OutboxState::Failed);
    assert!(stored.last_error.is_some());
    assert_eq!(
        messages_in(&db, thread).len(),
        1,
        "a message that will not be sent must stop looking sent"
    );

    // …but it is not lost: retrying is a state change, not a re-compose.
    assert!(out.retry(&entry.id, NOW + 1).unwrap());
    assert_eq!(out.get(&entry.id).unwrap().unwrap().state, OutboxState::Holding);
}

#[tokio::test]
async fn the_local_copy_lands_before_the_request_goes_out() {
    // The speed thesis, for sending: the thread repaints from SQLite, then the
    // network happens.
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "Sending now."), NOW, 1)
        .unwrap();
    out.queue(&built, NOW, NOW + UNDO_WINDOW_MS).unwrap();

    let messages = messages_in(&db, thread);
    assert_eq!(messages.len(), 2);
    let mine = messages.last().unwrap();
    assert_eq!(mine.from.email, "alex@example.com");
    assert!(mine.body_text.as_deref().unwrap().contains("Sending now."));
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn a_sent_message_adopts_its_real_gmail_id() {
    // Otherwise the next sync pass inserts the same reply a second time,
    // underneath the optimistic one.
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "ok"), NOW, 1).unwrap();
    out.queue(&built, NOW, NOW).unwrap();
    out.flush_due(NOW).await.unwrap();

    let ids: Vec<String> = messages_in(&db, thread)
        .into_iter()
        .map(|m| m.gmail_message_id)
        .collect();
    assert!(ids.contains(&"sent-1".to_string()), "{ids:?}");
    assert!(!ids.iter().any(|id| id.starts_with("mach-outbox:")), "{ids:?}");
}

#[tokio::test]
async fn a_scheduled_send_waits_for_its_time() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(&db, &reply_draft(&db, thread, DraftKind::Reply, "monday"), NOW, 1)
        .unwrap();
    let monday = NOW + 3 * 86_400_000;
    out.queue(&built, NOW, monday).unwrap();

    out.flush_due(NOW + UNDO_WINDOW_MS).await.unwrap();
    assert_eq!(transport.call_count(), 0, "a scheduled send is not an undo window");

    out.flush_due(monday).await.unwrap();
    assert_eq!(transport.call_count(), 1);
}

fn messages_in(db: &Db, thread_id: i64) -> Vec<mach_lib::db::models::Message> {
    db.read(|c| queries::messages_for_thread(c, thread_id)).unwrap()
}

// ==================================================================== drafts

#[test]
fn a_draft_survives_being_reopened() {
    let (db, account, thread, message) = seeded();
    let mut d = reply_draft(&db, thread, DraftKind::ReplyAll, "half a thought");
    d.subject = "Re: Series A data room".into();

    let saved = draft::save_draft(&db, &d, NOW).unwrap();
    assert_eq!(saved.updated_at, NOW);

    let loaded = draft::load_draft_for_thread(&db, thread).unwrap().unwrap();
    assert_eq!(loaded.body, "half a thought");
    assert_eq!(loaded.kind, DraftKind::ReplyAll);
    assert_eq!(loaded.account_id, account);
    assert_eq!(loaded.reply_to_id, Some(message));
    assert_eq!(
        loaded.cc.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["sam@partner.com", "dana@partner.com"],
        "a reopened draft must keep the recipients, not recompute them"
    );

    draft::delete_draft(&db, &loaded.id, NOW).unwrap();
    assert!(draft::load_draft_for_thread(&db, thread).unwrap().is_none());
}

#[test]
fn autosaving_the_same_draft_twice_updates_one_row() {
    let (db, _a, thread, _m) = seeded();
    let mut d = reply_draft(&db, thread, DraftKind::Reply, "one");
    draft::save_draft(&db, &d, NOW).unwrap();
    d.body = "one two".into();
    draft::save_draft(&db, &d, NOW + 500).unwrap();

    let loaded = draft::load_draft(&db, &d.id).unwrap().unwrap();
    assert_eq!(loaded.body, "one two");
    assert_eq!(loaded.updated_at, NOW + 500);
}

// ================================================================== markdown
//
// This table is duplicated verbatim in `src/lib/compose.test.ts`. Two
// implementations of one grammar exist because the composer renders an
// optimistic local copy before Rust has seen the send; pinning both to the same
// cases is the only thing that keeps them from drifting.

const MARKDOWN_CASES: &[(&str, &str)] = &[
    ("plain", "<p>plain</p>"),
    ("**bold**", "<p><strong>bold</strong></p>"),
    ("*italic*", "<p><em>italic</em></p>"),
    ("_italic_", "<p><em>italic</em></p>"),
    ("`code`", "<p><code>code</code></p>"),
    ("a\nb", "<p>a<br>b</p>"),
    ("a\n\nb", "<p>a</p><p>b</p>"),
    ("- one\n- two", "<ul><li>one</li><li>two</li></ul>"),
    ("1. one\n2. two", "<ol><li>one</li><li>two</li></ol>"),
    ("> quoted", "<blockquote><p>quoted</p></blockquote>"),
    ("# Title", "<h1>Title</h1>"),
    ("### Small", "<h3>Small</h3>"),
    ("<script>x</script>", "<p>&lt;script&gt;x&lt;/script&gt;</p>"),
    ("5 * 3 * 2", "<p>5 * 3 * 2</p>"),
    ("`**not bold**`", "<p><code>**not bold**</code></p>"),
    (
        "see https://example.com/a_b_c now",
        "<p>see <a href=\"https://example.com/a_b_c\">https://example.com/a_b_c</a> now</p>",
    ),
    ("a & b", "<p>a &amp; b</p>"),
    ("¿Sí?", "<p>¿Sí?</p>"),
];

#[test]
fn the_editor_grammar_renders_exactly_these_cases() {
    for (source, expected) in MARKDOWN_CASES {
        assert_eq!(markdown::to_html(source), *expected, "input: {source:?}");
    }
}

#[test]
fn the_plain_text_part_is_the_source_the_user_typed() {
    let source = "**Yes** — see https://example.com\n\n- one\n- two";
    assert_eq!(markdown::to_text(source), source);
}

#[test]
fn the_editor_never_emits_markup_the_user_did_not_ask_for() {
    let hostile = "<img src=x onerror=alert(1)> <a href=\"javascript:x\">hi</a>";
    let html = markdown::to_html(hostile);
    // Every angle bracket the user typed is text, so no tag but `<p>` exists.
    assert!(!html.contains("<img"), "{html}");
    assert!(!html.contains("<a "), "{html}");
    assert_eq!(
        html,
        "<p>&lt;img src=x onerror=alert(1)&gt; &lt;a href=&quot;javascript:x&quot;&gt;hi&lt;/a&gt;</p>"
    );
}

// ======================================================================= ipc
//
// `send_message` is one Tauri command routing several operations, because
// `lib.rs` (where a second command would be registered) belongs to another unit
// while these are being built in parallel. `dispatch` is that router as a plain
// function, which is what makes it testable at all.

use mach_lib::ipc::compose::dispatch;
use serde_json::json;

#[tokio::test]
async fn the_ipc_round_trip_prepares_saves_sends_and_undoes() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let prepared = dispatch(
        &db,
        &out,
        json!({ "op": "prepare", "threadId": thread, "kind": "replyAll", "now": NOW }),
    )
    .await
    .unwrap();
    let mut draft_value = prepared["draft"].clone();
    assert_eq!(draft_value["subject"], "Re: Series A data room");
    draft_value["body"] = json!("On it — link below.");

    let saved = dispatch(
        &db,
        &out,
        json!({ "op": "saveDraft", "draft": draft_value, "now": NOW + 1 }),
    )
    .await
    .unwrap();
    assert_eq!(saved["draft"]["updatedAt"], NOW + 1);

    let sent = dispatch(
        &db,
        &out,
        json!({ "op": "send", "draft": draft_value, "now": NOW + 2 }),
    )
    .await
    .unwrap();
    assert_eq!(sent["undoUntil"], NOW + 2 + UNDO_WINDOW_MS);
    assert_eq!(sent["scheduled"], false);
    let outbox_id = sent["entry"]["id"].as_str().unwrap().to_string();

    // Sending clears the draft: reopening the thread must not offer an empty
    // composer on top of the reply that is already in it.
    let reopened = dispatch(&db, &out, json!({ "op": "loadDraft", "threadId": thread }))
        .await
        .unwrap();
    assert!(reopened["draft"].is_null());

    let undone = dispatch(
        &db,
        &out,
        json!({ "op": "undo", "outboxId": outbox_id, "now": NOW + 3 }),
    )
    .await
    .unwrap();
    assert_eq!(undone["cancelled"], true);

    let flushed = dispatch(&db, &out, json!({ "op": "flush", "now": NOW + 60_000 }))
        .await
        .unwrap();
    assert_eq!(flushed["outcomes"].as_array().unwrap().len(), 0);
    // Nothing was *sent*. Saving the draft may well have talked to Gmail — it
    // is supposed to — but the recalled message never left.
    assert_eq!(transport.send_count(), 0);
}

#[tokio::test]
async fn scheduling_is_the_same_mechanism_with_a_later_number() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let prepared = dispatch(
        &db,
        &out,
        json!({ "op": "prepare", "threadId": thread, "kind": "reply", "now": NOW }),
    )
    .await
    .unwrap();
    let mut d = prepared["draft"].clone();
    d["body"] = json!("Monday then.");

    let monday = NOW + 3 * 86_400_000;
    let sent = dispatch(
        &db,
        &out,
        json!({ "op": "send", "draft": d, "scheduleAt": monday, "now": NOW }),
    )
    .await
    .unwrap();
    assert_eq!(sent["undoUntil"], monday);
    assert_eq!(sent["scheduled"], true);

    dispatch(&db, &out, json!({ "op": "flush", "now": NOW + UNDO_WINDOW_MS }))
        .await
        .unwrap();
    assert_eq!(transport.call_count(), 0);

    dispatch(&db, &out, json!({ "op": "flush", "now": monday }))
        .await
        .unwrap();
    assert_eq!(transport.call_count(), 1);
}

#[tokio::test]
async fn preview_hands_back_the_headers_that_decide_threading() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let prepared = dispatch(
        &db,
        &out,
        json!({ "op": "prepare", "threadId": thread, "kind": "reply", "now": NOW }),
    )
    .await
    .unwrap();
    let preview = dispatch(
        &db,
        &out,
        json!({ "op": "preview", "draft": prepared["draft"], "now": NOW }),
    )
    .await
    .unwrap();

    let headers = preview["headers"].as_str().unwrap();
    assert!(headers.contains("In-Reply-To: <parent-1@mail.partner.com>"), "{headers}");
    assert_eq!(preview["gmailThreadId"], "gthread-1");
}

#[tokio::test]
async fn sending_with_no_recipients_is_refused_rather_than_queued() {
    let (db, account, _thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let result = dispatch(
        &db,
        &out,
        json!({
            "op": "send",
            "now": NOW,
            "draft": {
                "id": "d-empty",
                "accountId": account,
                "kind": "new",
                "subject": "nobody",
                "body": "hello?"
            }
        }),
    )
    .await;

    assert!(result.is_err());
    assert_eq!(out.list().unwrap().len(), 0);
    assert_eq!(transport.call_count(), 0);
}

#[test]
#[ignore = "prints the generated headers; run with --ignored --nocapture"]
fn dump_representative_reply() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("Alex Rivera"));
    let thread = seed_thread(&db, account, "Reunión de mañana");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-1",
        person("José García", "jose@socio.es"),
        vec![person("Alex Rivera", "alex@example.com"), person("Sam Patel", "sam@partner.com")],
        vec![person("Dana Wu", "dana@partner.com")],
        "Reunión de mañana",
        "<CAF=abc123@mail.gmail.com>",
        Some("<root@socio.es> <second@socio.es>"),
        "¿Nos vemos a las diez?",
    );
    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::ReplyAll, "Sí — a las diez."));
    println!("{}", headers_of(&bytes));
}

// ---------------------------------------------------------------------------
// Reply-To
// ---------------------------------------------------------------------------

/// A mailing list sets `Reply-To` to the list address. A reply must go back to
/// the list, not to whichever member happened to post — getting this wrong
/// answers a person in private when they asked the room.
#[test]
fn reply_to_wins_over_from() {
    let db = Db::open_in_memory().unwrap();
    let account_id = seed_account(&db, "alex@example.com", Some("Alex"));
    let thread_id = seed_thread(&db, account_id, "[list] a question");
    seed_message_with_reply_to(
        &db,
        thread_id,
        account_id,
        "gm-list",
        person("Dana Wu", "dana@member.example"),
        vec![person("The List", "list@lists.example")],
        vec![],
        "[list] a question",
        "<list-1@lists.example>",
        None,
        "anyone seen this?",
        &[person("The List", "list@lists.example")],
    );

    let message = db
        .read(|c| queries::messages_for_thread(c, thread_id))
        .unwrap()
        .pop()
        .unwrap();

    let r = reply_recipients(&message, "alex@example.com", false);
    assert_eq!(
        r.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["list@lists.example"],
        "a reply must go back to the list, not to the individual who posted"
    );
    assert!(
        !r.to.iter().any(|m| m.email == "dana@member.example"),
        "the poster's own address must not be the reply target when Reply-To is set"
    );
}

/// Without `Reply-To`, nothing changes: the author is still the target.
#[test]
fn absent_reply_to_still_replies_to_the_author() {
    let db = Db::open_in_memory().unwrap();
    let account_id = seed_account(&db, "alex@example.com", Some("Alex"));
    let thread_id = seed_thread(&db, account_id, "just us");
    seed_message(
        &db,
        thread_id,
        account_id,
        "gm-plain",
        person("Dana Wu", "dana@member.example"),
        vec![person("Alex", "alex@example.com")],
        vec![],
        "just us",
        "<plain-1@member.example>",
        None,
        "hello",
    );

    let message = db
        .read(|c| queries::messages_for_thread(c, thread_id))
        .unwrap()
        .pop()
        .unwrap();

    let r = reply_recipients(&message, "alex@example.com", false);
    assert_eq!(
        r.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["dana@member.example"]
    );
}

// ===========================================================================
// A draft you can find
// ===========================================================================
//
// The bug these pin: the agent drafted a reply, said so, and the Drafts
// mailbox read "Nothing in DRAFT." A composer draft lived in `compose_drafts`
// and the Drafts mailbox lists threads carrying Gmail's `DRAFT` label, so the
// one surface a person opens had never heard of the other. See
// `compose::mirror`.

use mach_lib::ipc::reads;
use mach_lib::ipc::types::ThreadQuery;

/// The Drafts mailbox, exactly as the rail asks for it.
fn drafts_mailbox(db: &Db) -> Vec<mach_lib::db::models::ThreadSummary> {
    reads::list_threads(
        db,
        &ThreadQuery {
            account_id: None,
            label_id: Some("DRAFT".to_string()),
            unread_only: false,
            limit: Some(50),
            cursor: None,
        },
    )
    .unwrap()
    .items
}

async fn save_body(db: &Db, out: &Outbox, thread: i64, body: &str, now: i64) -> serde_json::Value {
    let prepared = dispatch(
        db,
        out,
        json!({ "op": "prepare", "threadId": thread, "kind": "reply", "now": now }),
    )
    .await
    .unwrap();
    let mut draft = prepared["draft"].clone();
    draft["body"] = json!(body);
    dispatch(db, out, json!({ "op": "saveDraft", "draft": draft, "now": now }))
        .await
        .unwrap()["draft"]
        .clone()
}

#[tokio::test]
async fn a_saved_draft_is_in_the_drafts_mailbox() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    assert!(drafts_mailbox(&db).is_empty(), "nothing drafted yet");

    save_body(&db, &out, thread, "Both tax items are handled.", NOW).await;

    let rows = drafts_mailbox(&db);
    assert_eq!(rows.len(), 1, "the draft has to be where a person looks");
    assert_eq!(rows[0].id, thread);
    assert_eq!(rows[0].subject, "Series A data room");
}

/// Autosave fires every few hundred milliseconds. The mailbox must show one
/// draft, not a transcript of the typing.
#[tokio::test]
async fn saving_the_same_draft_repeatedly_leaves_one_row() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let mut draft = save_body(&db, &out, thread, "B", NOW).await;
    for (i, body) in ["Bo", "Bot", "Both"].iter().enumerate() {
        draft["body"] = json!(body);
        draft = dispatch(
            &db,
            &out,
            json!({ "op": "saveDraft", "draft": draft, "now": NOW + 1 + i as i64 }),
        )
        .await
        .unwrap()["draft"]
            .clone();
    }

    assert_eq!(drafts_mailbox(&db).len(), 1);
    let drafts: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND is_draft = 1",
                [thread],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(drafts, 1, "one draft, however many keystrokes");
}

/// A message written from nothing has no conversation, so it is given one —
/// otherwise it could be listed but never opened.
#[tokio::test]
async fn a_draft_with_no_thread_still_gets_a_conversation_to_live_in() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("Alex Rivera"));
    let out = outbox(&db, FakeTransport::always_ok());

    let saved = dispatch(
        &db,
        &out,
        json!({
            "op": "saveDraft",
            "draft": {
                "id": "d-fresh",
                "accountId": account,
                "kind": "new",
                "to": [{ "email": "tawny@partner.com" }],
                "subject": "Coffee?",
                "body": "Thursday?",
            },
            "now": NOW,
        }),
    )
    .await
    .unwrap()["draft"]
        .clone();

    let rows = drafts_mailbox(&db);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, "Coffee?");
    assert_eq!(
        saved["threadId"].as_i64(),
        Some(rows[0].id),
        "the row has to know its conversation, or reopening it finds nothing"
    );
}

#[tokio::test]
async fn discarding_a_draft_takes_it_out_of_the_mailbox() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let saved = save_body(&db, &out, thread, "never mind", NOW).await;
    assert_eq!(drafts_mailbox(&db).len(), 1);

    dispatch(
        &db,
        &out,
        json!({ "op": "discardDraft", "draftId": saved["id"] }),
    )
    .await
    .unwrap();

    assert!(drafts_mailbox(&db).is_empty(), "and the label goes with it");
    let left: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND is_draft = 1",
                [thread],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(left, 0);
}

/// Sending is the other way a draft stops existing. Leaving the mirror behind
/// would put the reply in the thread *and* leave its own draft above it.
#[tokio::test]
async fn sending_a_draft_takes_it_out_of_the_mailbox() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let saved = save_body(&db, &out, thread, "On it — link below.", NOW).await;
    assert_eq!(drafts_mailbox(&db).len(), 1);

    dispatch(&db, &out, json!({ "op": "send", "draft": saved, "now": NOW + 1 }))
        .await
        .unwrap();

    assert!(drafts_mailbox(&db).is_empty());
}

/// The reply that follows a draft must answer the last message *somebody else*
/// sent. Mach now writes drafts into the conversation, so "the last message"
/// is no longer a safe way to find the parent.
#[tokio::test]
async fn a_reply_prepared_after_a_draft_still_answers_the_real_message() {
    let (db, _a, thread, message) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    save_body(&db, &out, thread, "half a thought", NOW).await;

    let prepared = dispatch(
        &db,
        &out,
        json!({ "op": "prepare", "threadId": thread, "kind": "reply", "now": NOW + 5 }),
    )
    .await
    .unwrap();
    assert_eq!(prepared["draft"]["replyToId"].as_i64(), Some(message));
    assert_eq!(prepared["draft"]["subject"], "Re: Series A data room");
}

// ===========================================================================
// One draft, not two
// ===========================================================================

/// Saving pushes to Gmail — `drafts.create` the first time, `drafts.update`
/// after that. Two saves must not leave the owner with two drafts on his phone.
#[tokio::test]
async fn a_draft_is_created_once_and_updated_after_that() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"r-draft-1","message":{"id":"gmsg-draft-1","threadId":"gthread-1"}}"#,
    ));
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let saved = save_body(&db, &out, thread, "first", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();

    sync.push(&id, NOW).await.unwrap();
    save_body(&db, &out, thread, "second", NOW + 1).await;
    sync.push(&id, NOW + 1).await.unwrap();

    let calls = transport.draft_requests();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].method.as_str(), "POST", "created once");
    assert_eq!(calls[1].method.as_str(), "PUT", "updated after that");
    assert!(
        calls[1].url.ends_with("/drafts/r-draft-1"),
        "the update has to address the draft Gmail gave us: {}",
        calls[1].url
    );

    let stored = draft::load_draft(&db, &id).unwrap().unwrap();
    assert_eq!(stored.remote.state, compose::draft::RemoteState::Synced);
    assert_eq!(stored.remote.draft_id.as_deref(), Some("r-draft-1"));
}

/// The duplicate that matters is the one the *sync* would make. Once the push
/// lands, the local mirror carries Gmail's own message id, so the pass that
/// later brings the draft down upserts onto that row instead of beside it.
#[tokio::test]
async fn a_pushed_draft_is_adopted_so_the_sync_lands_on_the_same_row() {
    let (db, account, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"r-draft-9","message":{"id":"gmsg-draft-9","threadId":"gthread-1"}}"#,
    ));
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let saved = save_body(&db, &out, thread, "adopt me", NOW).await;
    sync.push(saved["id"].as_str().unwrap(), NOW).await.unwrap();

    // What a sync pass does when it meets this draft coming the other way.
    seed_message(
        &db,
        thread,
        account,
        "gmsg-draft-9",
        person("Alex Rivera", "alex@example.com"),
        vec![person("Tawny Rivers", "tawny@partner.com")],
        vec![],
        "Re: Series A data room",
        "<whatever@mail.gmail.com>",
        None,
        "adopt me",
    );

    let drafts: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND gmail_message_id LIKE 'gmsg-draft-9'",
                [thread],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(drafts, 1, "one draft, whichever end learned about it first");
    assert_eq!(drafts_mailbox(&db).len(), 1);
}

/// A push Google refuses leaves the draft local — and says so. Silence here is
/// the failure mode: he would believe it was on his phone.
#[tokio::test]
async fn a_refused_push_is_recorded_rather_than_swallowed() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_failing(403, r#"{"error":{"message":"nope"}}"#);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let saved = save_body(&db, &out, thread, "into the void", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();
    let state = sync.push(&id, NOW).await.unwrap();

    assert_eq!(state, compose::draft::RemoteState::Failed);
    let stored = draft::load_draft(&db, &id).unwrap().unwrap();
    assert!(stored.remote.error.is_some(), "the reason has to survive");
    // And it is still in the Drafts mailbox: local-only is not lost.
    assert_eq!(drafts_mailbox(&db).len(), 1);
}

/// Discarding must reach Gmail too, or the copy on his phone outlives the one
/// he threw away here.
#[tokio::test]
async fn discarding_a_pushed_draft_deletes_it_on_gmail() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"r-draft-3","message":{"id":"gmsg-draft-3","threadId":"gthread-1"}}"#,
    ));
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let saved = save_body(&db, &out, thread, "gone in a moment", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();
    sync.push(&id, NOW).await.unwrap();

    let remote_id = draft::load_draft(&db, &id)
        .unwrap()
        .unwrap()
        .remote
        .draft_id
        .unwrap();
    sync.delete(&remote_id, 1).await.unwrap();

    let deletes = transport
        .draft_requests()
        .into_iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .collect::<Vec<_>>();
    assert_eq!(deletes.len(), 1);
    assert!(deletes[0].url.ends_with("/drafts/r-draft-3"), "{:?}", deletes[0].url);
}

/// The defect a live run found: a sync pass rebuilds `thread_labels` from the
/// per-message label union, so the `DRAFT` row written here for a draft Google
/// has not heard of yet is dropped — and the draft left the mailbox again,
/// silently. The mailbox reads `messages.is_draft` too, which survives it.
#[tokio::test]
async fn a_sync_rebuilding_the_labels_does_not_hide_the_draft() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    save_body(&db, &out, thread, "still here?", NOW).await;
    assert_eq!(drafts_mailbox(&db).len(), 1);

    // Exactly what `recompute_thread` does: the label set, rewritten from what
    // Google last said about the messages in this conversation.
    db.write(|c| {
        queries::set_thread_labels(c, thread, &["INBOX".to_string()])?;
        Ok(())
    })
    .unwrap();

    assert_eq!(
        drafts_mailbox(&db).len(),
        1,
        "the draft is still unsent, so it is still in Drafts"
    );
}

/// The other defect a live run found: `drafts.update` does not edit a message,
/// it replaces it — so every push hands back a *new* message id. A mirror that
/// only answered to its own placeholder was adopted once and went stale after
/// that, which is a duplicate waiting for the next sync.
#[tokio::test]
async fn a_second_push_moves_the_mirror_onto_the_new_message_id() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"r-draft-7","message":{"id":"gmsg-first","threadId":"gthread-1"}}"#,
        )),
        Ok(HttpResponse::json(
            200,
            r#"{"id":"r-draft-7","message":{"id":"gmsg-second","threadId":"gthread-1"}}"#,
        )),
    ]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let saved = save_body(&db, &out, thread, "one", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();
    sync.push(&id, NOW).await.unwrap();

    let mut draft = draft::load_draft(&db, &id).unwrap().unwrap();
    draft.body = "two".into();
    draft::save_draft(&db, &draft, NOW + 1).unwrap();
    sync.push(&id, NOW + 1).await.unwrap();

    let ids: Vec<String> = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT gmail_message_id FROM messages WHERE thread_id = ?1 AND is_draft = 1",
            )?;
            let rows = stmt.query_map([thread], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap();
    assert_eq!(
        ids,
        vec!["gmsg-second".to_string()],
        "one mirror, following the message Gmail actually holds"
    );
}

/// The third defect a live run found, and the nastiest: once a draft has been
/// pushed, its mirror row is filed under Gmail's message id, not under the
/// `mach-draft:` placeholder it started with. Writing the mirror by the
/// placeholder alone therefore found nothing and *inserted a second row* on the
/// very next keystroke — two copies of one unsent reply in the same
/// conversation.
#[tokio::test]
async fn editing_a_pushed_draft_does_not_grow_a_second_mirror() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"r-draft-8","message":{"id":"gmsg-a","threadId":"gthread-1"}}"#,
        )),
        Ok(HttpResponse::json(
            200,
            r#"{"id":"r-draft-8","message":{"id":"gmsg-b","threadId":"gthread-1"}}"#,
        )),
    ]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let saved = save_body(&db, &out, thread, "first pass", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();
    sync.push(&id, NOW).await.unwrap();

    // The composer saving again, exactly as autosave does — through the router,
    // with the draft it was handed back.
    let mut again = draft::load_draft(&db, &id).unwrap().unwrap();
    again.body = "second pass".into();
    dispatch(
        &db,
        &out,
        json!({ "op": "saveDraft", "draft": again, "now": NOW + 1 }),
    )
    .await
    .unwrap();

    let drafts: i64 = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND is_draft = 1",
                [thread],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(drafts, 1, "one mirror, however many saves follow a push");

    // And discarding it still finds the row under the id it now has.
    dispatch(&db, &out, json!({ "op": "discardDraft", "draftId": id }))
        .await
        .unwrap();
    assert!(drafts_mailbox(&db).is_empty());
}

// ============================================ opening a draft from its thread
//
// The mirror made a draft *visible* in the conversation. These cover the other
// half: a row in the reading pane is a message id, and the thing behind it that
// can actually be typed into is a `compose_drafts` row keyed by draft id.
// Without a way across, the owner saw a message he had not sent and had no way
// to finish it — "don't know how to edit it if it is a draft".

/// The placeholder case: written locally, Gmail not told yet.
#[tokio::test]
async fn a_draft_row_in_a_thread_resolves_to_the_draft_it_is() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let saved = save_body(&db, &out, thread, "Both tax items are handled.", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();

    let mirror = messages_in(&db, thread)
        .into_iter()
        .find(|m| m.is_draft)
        .expect("the draft is in the conversation");
    assert!(
        mirror.gmail_message_id.starts_with("mach-draft:"),
        "not pushed yet, so it is still under the placeholder"
    );

    let found = dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": mirror.id }),
    )
    .await
    .unwrap();
    assert_eq!(found["draft"]["id"].as_str(), Some(id.as_str()));
    assert_eq!(
        found["draft"]["body"].as_str(),
        Some("Both tax items are handled."),
        "the body has to come back intact, or the composer opens empty over it"
    );

    // A message somebody actually sent has no draft behind it. Answering with
    // the thread's nearest one would put another person's mail in the editor.
    let sent = messages_in(&db, thread)
        .into_iter()
        .find(|m| !m.is_draft)
        .unwrap();
    let none = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": sent.id }))
        .await
        .unwrap();
    assert!(none["draft"].is_null());
}

/// The adopted case, and the reason this is resolved in Rust rather than by
/// parsing the placeholder in the UI. `adopt` renames the mirror to Gmail's own
/// message id the moment a push lands — within a second of the first keystroke
/// — and after that there is no `mach-draft:` prefix left to read.
#[tokio::test]
async fn a_pushed_draft_row_still_resolves_after_gmail_renames_it() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"r-draft-8","message":{"id":"gmsg-a","threadId":"gthread-1"}}"#,
    ))]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let saved = save_body(&db, &out, thread, "first pass", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();
    sync.push(&id, NOW).await.unwrap();

    let mirror = messages_in(&db, thread)
        .into_iter()
        .find(|m| m.is_draft)
        .expect("still one draft in the conversation");
    assert_eq!(mirror.gmail_message_id, "gmsg-a", "renamed by adopt");

    let found = dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": mirror.id }),
    )
    .await
    .unwrap();
    assert_eq!(found["draft"]["id"].as_str(), Some(id.as_str()));
}

/// Why the row is asked about by message rather than by thread: a conversation
/// can hold two drafts — the agent leaves one, the owner starts another — and
/// the thread-keyed lookup hands back whichever was typed in last, which is not
/// necessarily the one that was activated.
#[tokio::test]
async fn two_drafts_on_one_thread_open_the_one_that_was_activated() {
    let (db, account, thread, message) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    for (id, body, now) in [("d-older", "older text", NOW), ("d-newer", "newer text", NOW + 5_000)] {
        dispatch(
            &db,
            &out,
            json!({
                "op": "saveDraft",
                "draft": {
                    "id": id,
                    "accountId": account,
                    "threadId": thread,
                    "replyToId": message,
                    "kind": "reply",
                    "to": [{ "email": "dana@partner.com" }],
                    "subject": "Re: Series A data room",
                    "body": body,
                },
                "now": now,
            }),
        )
        .await
        .unwrap();
    }

    let mirrors: Vec<_> = messages_in(&db, thread)
        .into_iter()
        .filter(|m| m.is_draft)
        .collect();
    assert_eq!(mirrors.len(), 2, "two drafts, two rows in the conversation");

    for mirror in mirrors {
        let expected = mirror
            .gmail_message_id
            .strip_prefix("mach-draft:")
            .unwrap()
            .to_string();
        let found = dispatch(
            &db,
            &out,
            json!({ "op": "loadDraft", "messageId": mirror.id }),
        )
        .await
        .unwrap();
        assert_eq!(found["draft"]["id"].as_str(), Some(expected.as_str()));
    }

    // The thread-keyed lookup is still what reopening a conversation uses, and
    // still answers with the most recent — the two are different questions.
    let by_thread = dispatch(&db, &out, json!({ "op": "loadDraft", "threadId": thread }))
        .await
        .unwrap();
    assert_eq!(by_thread["draft"]["id"].as_str(), Some("d-newer"));
}

/// An empty payload is a bug in the caller, not an empty composer.
#[tokio::test]
async fn load_draft_still_refuses_a_payload_that_names_nothing() {
    let (db, _a, _t, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let error = dispatch(&db, &out, json!({ "op": "loadDraft" })).await;
    assert!(error.is_err());
}

// ==================================== drafts written in some other client

/// A draft exactly as Gmail hands one down: an ordinary message carrying the
/// `DRAFT` label, plus the draft id the `drafts.list` sweep learned for it.
///
/// Note what is *not* here — no `compose_drafts` row. That is the whole of the
/// problem this section covers: Mach has the text and no way to address the
/// draft it belongs to.
fn seed_remote_draft(
    db: &Db,
    thread_id: i64,
    account_id: i64,
    gmail_message_id: &str,
    gmail_draft_id: &str,
    body: &str,
) -> i64 {
    let message_id = db
        .write(|c| {
            queries::upsert_message(
                c,
                &NewMessage {
                    thread_id,
                    account_id,
                    gmail_message_id: gmail_message_id.to_string(),
                    rfc822_message_id: None,
                    in_reply_to: None,
                    references: None,
                    reply_to: vec![],
                    from: person("Alex Rivera", "alex@example.com"),
                    to: vec![person("Tawny Rivers", "tawny@partner.com")],
                    cc: vec![],
                    bcc: vec![],
                    subject: "Re: Series A data room".to_string(),
                    body_html: None,
                    body_text: Some(body.to_string()),
                    snippet: body.chars().take(60).collect(),
                    internal_date: NOW,
                    is_unread: false,
                    is_draft: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    db.write(|c| {
        queries::set_message_draft_id(c, account_id, gmail_message_id, gmail_draft_id)?;
        Ok(())
    })
    .unwrap();
    message_id
}

/// The complaint this whole section answers: "wtf is 'draft from another
/// client'? aren't drafts in the API?"
#[tokio::test]
async fn a_draft_written_on_the_phone_opens_in_the_composer() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let row = seed_remote_draft(
        &db,
        thread,
        account,
        "gmsg-remote-1",
        "r-9999",
        "Sending the link this afternoon.",
    );

    let found = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();

    assert!(!found["draft"].is_null(), "it has to open");
    assert_eq!(
        found["draft"]["body"].as_str(),
        Some("<div>Sending the link this afternoon.</div>"),
        "the text comes from the message Mach already has, not from a request"
    );
    assert_eq!(
        found["draft"]["remote"]["draftId"].as_str(),
        Some("r-9999"),
        "carrying the id, or the first save would create a second draft"
    );
    assert_eq!(found["draft"]["kind"].as_str(), Some("adopted"));
    assert_eq!(
        found["draft"]["remote"]["state"].as_str(),
        Some("synced"),
        "opening his phone's draft must not queue a push that rewrites it"
    );
}

/// The duplicate hazard, from the new direction. Saving an adopted draft has to
/// be `drafts.update` on the id Gmail already gave it.
#[tokio::test]
async fn saving_an_adopted_draft_updates_gmails_draft_rather_than_making_a_second() {
    let (db, account, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"r-9999","message":{"id":"gmsg-remote-2","threadId":"gthread-1"}}"#,
    ))]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "First pass.");

    let found = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();
    let mut draft = found["draft"].clone();
    draft["body"] = json!("First pass, edited here.");
    let saved = dispatch(&db, &out, json!({ "op": "saveDraft", "draft": draft, "now": NOW }))
        .await
        .unwrap();
    sync.push(saved["draft"]["id"].as_str().unwrap(), NOW)
        .await
        .unwrap();

    let drafts = transport.draft_requests();
    assert_eq!(drafts.len(), 1, "one call, not two");
    assert!(
        drafts[0].url.ends_with("/drafts/r-9999"),
        "addressed by the id Gmail gave it, url {}",
        drafts[0].url
    );
    assert_eq!(drafts[0].method, mach_lib::google::HttpMethod::Put);

    // And one draft row in the conversation, not two.
    let in_thread = messages_in(&db, thread)
        .into_iter()
        .filter(|m| m.is_draft)
        .count();
    assert_eq!(in_thread, 1);
}

/// The quiet corruption an adopted draft could suffer, and the reason `adopted`
/// is its own kind: the body already holds whatever the phone quoted, so quoting
/// the parent again would send the original twice.
#[tokio::test]
async fn adopting_does_not_quote_the_original_a_second_time() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let body = "On it.\n\nOn Thu, 7 Aug 2026 at 12:00, Tawny Rivers <tawny@partner.com> wrote:\n> Can you send the data room link?";
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", body);

    let found = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();
    let parsed: Draft = serde_json::from_value(found["draft"].clone()).unwrap();
    let bytes = built_bytes(&db, &parsed);
    let plain = MessageParser::new()
        .parse(&bytes)
        .and_then(|m| m.body_text(0).map(|t| t.into_owned()))
        .expect("a text part");

    assert_eq!(
        plain.matches("Can you send the data room link?").count(),
        1,
        "the original appears once, in the quote the phone wrote:\n{plain}"
    );

    // The headers that make it thread elsewhere are still there, taken from the
    // message it answers rather than from the body.
    let headers = headers_of(&bytes);
    assert!(
        headers.contains("In-Reply-To: <parent-1@mail.partner.com>"),
        "headers:\n{headers}"
    );
}

/// Two activations of the same row, or two windows. The local id is derived from
/// the Gmail draft id, so they converge instead of racing.
#[tokio::test]
async fn adopting_the_same_draft_twice_leaves_one_row() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "Once.");

    let first = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();
    let second = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();

    assert_eq!(first["draft"]["id"], second["draft"]["id"]);
    let rows: i64 = db
        .read(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM compose_drafts", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap();
    assert_eq!(rows, 1);
}

/// A fresh message written on the phone — no conversation behind it — still has
/// to open. It has no parent to thread onto, and that is not a failure.
#[tokio::test]
async fn a_draft_with_no_conversation_behind_it_still_opens() {
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("Alex Rivera"));
    let thread = seed_thread(&db, account, "Lunch?");
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-3", "r-4242", "Thursday?");

    let out = outbox(&db, FakeTransport::always_ok());
    let found = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();

    assert_eq!(
        found["draft"]["body"].as_str(),
        Some("<div>Thursday?</div>"),
        "a phone's plain text arrives wrapped, because the editor renders HTML"
    );
    assert!(
        found["draft"]["replyToId"].is_null(),
        "nothing to answer, so nothing is claimed as a parent"
    );

    let parsed: Draft = serde_json::from_value(found["draft"].clone()).unwrap();
    let headers = headers_of(&built_bytes(&db, &parsed));
    assert!(!headers.contains("In-Reply-To:"), "headers:\n{headers}");
}

/// Deleted on the phone stays deleted. The local copy is the only thing that
/// could put it back, so the sweep takes it out — row and mirror together.
#[tokio::test]
async fn a_draft_deleted_elsewhere_is_not_resurrected() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "Half a thought.");
    dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": row, "now": NOW }),
    )
    .await
    .unwrap();

    // Gmail's answer no longer mentions it.
    let removed = draft::forget_drafts_missing_from(&db, account, &[], NOW + 60_000).unwrap();
    assert_eq!(removed, vec!["r-9999".to_string()]);

    let left: i64 = db
        .read(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM compose_drafts", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap();
    assert_eq!(left, 0, "nothing left here to push back");
    assert!(
        !messages_in(&db, thread).iter().any(|m| m.is_draft),
        "and it is out of the conversation too"
    );
}

/// A draft *sent* from the phone also vanishes from `drafts.list`, and its
/// message is now an ordinary sent message. Reaping the row must not take the
/// mail with it.
#[tokio::test]
async fn a_draft_sent_elsewhere_keeps_the_message_it_became() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "Sent from bed.");
    dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": row, "now": NOW }),
    )
    .await
    .unwrap();

    // History got there first: the DRAFT label is gone, so this is now mail.
    db.write(|c| {
        c.execute("UPDATE messages SET is_draft = 0 WHERE id = ?1", [row])?;
        Ok(())
    })
    .unwrap();

    draft::forget_drafts_missing_from(&db, account, &[], NOW + 60_000).unwrap();

    assert!(
        messages_in(&db, thread).iter().any(|m| m.id == row),
        "the message he actually sent has to survive"
    );
}

/// A mirror with no draft row behind it.
///
/// His mailbox holds two of these on one conversation — same words, two message
/// ids, no `compose_drafts` row for either, left behind by earlier duplicate
/// bugs. They render as a `Draft` that cannot be opened or discarded, so the
/// sweep has to reach them from the messages rather than from the draft table.
#[tokio::test]
async fn a_mirror_with_no_draft_row_behind_it_is_swept() {
    let (db, account, thread, _m) = seeded();
    let row = seed_remote_draft(&db, thread, account, "gmsg-stale", "r-stale", "Half a thought.");

    let removed =
        compose::mirror::forget_orphan_mirrors(&db, account, &[], NOW + 60_000).unwrap();

    assert_eq!(removed, vec!["gmsg-stale".to_string()]);
    assert!(
        !messages_in(&db, thread).iter().any(|m| m.id == row),
        "the row that stood for nothing is gone"
    );
    assert!(drafts_mailbox(&db).is_empty(), "and so is the DRAFT label");
}

/// The direction to be careful in, from the mirror side: a draft *sent*
/// elsewhere is also absent from `drafts.list`, and the message it became is
/// mail. `sync::mail` clears `is_draft` from the label change before this runs,
/// and that is what this asserts on.
#[tokio::test]
async fn the_sweep_leaves_a_draft_that_was_sent_elsewhere_alone() {
    let (db, account, thread, _m) = seeded();
    let row = seed_remote_draft(&db, thread, account, "gmsg-sent", "r-sent", "Sent from bed.");
    db.write(|c| {
        c.execute("UPDATE messages SET is_draft = 0 WHERE id = ?1", [row])?;
        Ok(())
    })
    .unwrap();

    let removed =
        compose::mirror::forget_orphan_mirrors(&db, account, &[], NOW + 60_000).unwrap();

    assert!(removed.is_empty(), "removed {removed:?}");
    assert!(
        messages_in(&db, thread).iter().any(|m| m.id == row),
        "the message he actually sent has to survive"
    );
}

/// And a draft that is alive on Gmail, or was written here a moment ago, is not
/// litter either.
#[tokio::test]
async fn the_sweep_leaves_live_and_just_written_drafts_alone() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let live = seed_remote_draft(&db, thread, account, "gmsg-live", "r-live", "Still writing.");
    let saved = save_body(&db, &out, thread, "typed a moment ago", NOW + 1_000).await;

    let removed = compose::mirror::forget_orphan_mirrors(
        &db,
        account,
        &["gmsg-live".to_string()],
        NOW + 500,
    )
    .unwrap();

    assert!(removed.is_empty(), "removed {removed:?}");
    assert!(messages_in(&db, thread).iter().any(|m| m.id == live));
    assert!(draft::load_draft(&db, saved["id"].as_str().unwrap())
        .unwrap()
        .is_some());
}

/// The race the sweep would otherwise lose: a draft written here while
/// `drafts.list` was in flight is newer than the answer, and reaping it would
/// delete something that had just been typed.
#[tokio::test]
async fn a_draft_written_while_the_list_was_in_flight_survives_the_sweep() {
    let (db, account, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"r-newer","message":{"id":"gmsg-newer","threadId":"gthread-1"}}"#,
    ))]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let listed_at = NOW;
    let saved = save_body(&db, &out, thread, "typed a moment ago", NOW + 1_000).await;
    sync.push(saved["id"].as_str().unwrap(), NOW + 1_000)
        .await
        .unwrap();

    let removed = draft::forget_drafts_missing_from(&db, account, &[], listed_at).unwrap();
    assert!(removed.is_empty(), "removed {removed:?}");
    assert!(
        draft::load_draft(&db, saved["id"].as_str().unwrap())
            .unwrap()
            .is_some(),
        "the draft the owner just wrote is still here"
    );
}

/// Two writers, one draft. Mach adopted it; then it was edited on the phone,
/// which replaced the message behind it — so the row Mach holds points at a
/// message that no longer exists, and only the draft id says the two are the
/// same draft.
///
/// Last write wins, by time. Here the phone wrote last.
#[tokio::test]
async fn a_draft_edited_elsewhere_after_adoption_brings_the_newer_text_back() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "First thought.");
    let opened = dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": row, "now": NOW }),
    )
    .await
    .unwrap();
    let draft_id = opened["draft"]["id"].as_str().unwrap().to_string();

    // He edits it on the phone. `drafts.update` mints a new message id, so the
    // sync sees the old one deleted and a new one added.
    db.write(|c| {
        c.execute("DELETE FROM messages WHERE id = ?1", [row])?;
        Ok(())
    })
    .unwrap();
    let newer = seed_remote_draft(
        &db,
        thread,
        account,
        "gmsg-remote-2",
        "r-9999",
        "First thought, rewritten on the train.",
    );
    db.write(|c| {
        c.execute(
            "UPDATE messages SET internal_date = ?2 WHERE id = ?1",
            rusqlite::params![newer, NOW + 60_000],
        )?;
        Ok(())
    })
    .unwrap();

    let reopened = dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": newer, "now": NOW + 120_000 }),
    )
    .await
    .unwrap();

    assert_eq!(
        reopened["draft"]["id"].as_str(),
        Some(draft_id.as_str()),
        "still the same draft, not a second one"
    );
    assert_eq!(
        reopened["draft"]["body"].as_str(),
        Some("<div>First thought, rewritten on the train.</div>"),
        "the phone wrote last, so the phone's words are the ones to show"
    );
    assert_eq!(
        reopened["draft"]["remote"]["messageId"].as_str(),
        Some("gmsg-remote-2"),
        "re-pointed, or the next save mirrors onto a message Gmail deleted"
    );
}

/// The other side of the same rule: Mach wrote last, so what he typed here is
/// what he sees — and the row is still re-pointed at the message that exists.
#[tokio::test]
async fn a_local_edit_newer_than_the_remote_one_is_not_overwritten() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "First thought.");
    let opened = dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": row, "now": NOW }),
    )
    .await
    .unwrap();

    let mut draft = opened["draft"].clone();
    draft["body"] = json!("Typed here, and later.");
    dispatch(
        &db,
        &out,
        json!({ "op": "saveDraft", "draft": draft, "now": NOW + 120_000 }),
    )
    .await
    .unwrap();

    // Gmail's copy is older than that save.
    db.write(|c| {
        c.execute("DELETE FROM messages WHERE id = ?1", [row])?;
        Ok(())
    })
    .unwrap();
    let older = seed_remote_draft(&db, thread, account, "gmsg-remote-2", "r-9999", "Stale copy.");
    db.write(|c| {
        c.execute(
            "UPDATE messages SET internal_date = ?2 WHERE id = ?1",
            rusqlite::params![older, NOW + 60_000],
        )?;
        Ok(())
    })
    .unwrap();

    let reopened = dispatch(
        &db,
        &out,
        json!({ "op": "loadDraft", "messageId": older, "now": NOW + 180_000 }),
    )
    .await
    .unwrap();

    assert_eq!(
        reopened["draft"]["body"].as_str(),
        Some("Typed here, and later.")
    );
    assert_eq!(
        reopened["draft"]["remote"]["messageId"].as_str(),
        Some("gmsg-remote-2")
    );
}

// ===================================================== the editor's own HTML
//
// The composer emits HTML now, so two derivations replaced the markdown one:
// `html::sanitize`, which decides what may leave, and `html::to_text`, which
// produces the `text/plain` half of the message. `src/lib/email-html.ts` mirrors
// both for the editor; the table below is the one both answer to.

use compose::{attach, html};

/// The same cases as `TEXT_CASES` in `src/lib/email-html.test.ts`. Two
/// implementations of one derivation exist because the editor needs the answer
/// without a round trip and this side needs it to put on the wire; if they
/// drift, both suites fail.
const TEXT_CASES: &[(&str, &str)] = &[
    ("<div>Hello.</div>", "Hello."),
    ("<div>one</div><div>two</div>", "one\ntwo"),
    ("<p>one</p><p>two</p>", "one\n\ntwo"),
    ("<div>one<br>two</div>", "one\ntwo"),
    ("<div><b>bold</b> and <i>italic</i></div>", "bold and italic"),
    ("<ul><li>one</li><li>two</li></ul>", "- one\n- two"),
    ("<ol><li>one</li><li>two</li></ol>", "1. one\n2. two"),
    ("<blockquote><div>quoted</div></blockquote>", "> quoted"),
    (
        "<div>see <a href=\"https://example.com/a\">the page</a></div>",
        "see the page <https://example.com/a>",
    ),
    (
        "<div><a href=\"https://example.com\">https://example.com</a></div>",
        "https://example.com",
    ),
    ("<div>a &amp; b &nbsp;c</div>", "a & b c"),
    ("<div><br></div>", ""),
];

#[test]
fn the_plain_text_twin_renders_exactly_these_cases() {
    for (html_source, expected) in TEXT_CASES {
        assert_eq!(html::to_text(html_source), *expected, "input: {html_source:?}");
    }
}

#[test]
fn outgoing_html_carries_no_classes_no_ids_and_no_modern_css() {
    let pasted = "<p class=\"MsoNormal\" id=\"x\" style=\"color:var(--brand);font-weight:700\">\
                  <o:p></o:p>Quarterly <b>numbers</b></p>\
                  <style>p{color:red}</style><script>alert(1)</script>";
    let cleaned = html::sanitize(pasted);

    assert!(!cleaned.contains("class="), "{cleaned}");
    assert!(!cleaned.contains("id="), "{cleaned}");
    assert!(!cleaned.contains("var("), "{cleaned}");
    assert!(!cleaned.contains("<style"), "{cleaned}");
    assert!(!cleaned.contains("alert"), "{cleaned}");
    // The declaration that survives is the one Outlook honours.
    assert!(cleaned.contains("font-weight: 700"), "{cleaned}");
    assert!(cleaned.contains("<b>numbers</b>"), "{cleaned}");
}

#[test]
fn sanitizing_is_idempotent_because_every_autosave_does_it_again() {
    let once = html::sanitize("<div style=\"color:#333\">Hi <a href=\"https://x/y\">there</a></div>");
    assert_eq!(html::sanitize(&once), once);
}

#[test]
fn a_link_with_a_scheme_no_mail_client_should_follow_is_not_a_link() {
    let cleaned = html::sanitize("<a href=\"javascript:alert(1)\">click</a>");
    assert!(!cleaned.contains("javascript:"), "{cleaned}");
    assert!(cleaned.contains("click"), "the words are not the problem: {cleaned}");
}

#[test]
fn the_two_parts_of_an_html_draft_come_from_the_same_html() {
    let (db, _a, thread, _m) = seeded();
    let draft = reply_draft(
        &db,
        thread,
        DraftKind::Reply,
        "<div>Numbers:</div><ul><li>one</li><li>two</li></ul>",
    );
    let (text, rendered) = draft::body_parts(&draft);
    assert_eq!(text, "Numbers:\n\n- one\n- two");
    assert!(rendered.contains("<ul>"), "{rendered}");
}

// ================================================================ attachments

#[test]
fn an_attached_file_makes_the_message_multipart_mixed_around_the_alternative() {
    let (db, _a, thread, _m) = seeded();
    let mut draft = reply_draft(&db, thread, DraftKind::Reply, "<div>Numbers attached.</div>");
    draft = draft::save_draft(&db, &draft, NOW).unwrap();
    attach::add_bytes(&db, &draft.id, "q3 numbers.csv", b"a,b\n1,2\n", false, NOW).unwrap();
    let draft = draft::load_draft(&db, &draft.id).unwrap().unwrap();

    let bytes = built_bytes(&db, &draft);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(text.contains("multipart/mixed"), "{text}");
    assert!(text.contains("multipart/alternative"), "{text}");

    let parsed = MessageParser::new().parse(&bytes).unwrap();
    let attachment = parsed.attachments().next().expect("one attachment");
    assert_eq!(
        mail_parser::MimeHeaders::attachment_name(attachment),
        Some("q3 numbers.csv")
    );
    assert_eq!(attachment.contents(), b"a,b\n1,2\n");
    // The alternative is still intact underneath.
    assert!(parsed.body_text(0).is_some());
    assert!(parsed.body_html(0).is_some());
}

#[test]
fn a_file_larger_than_gmail_will_send_is_refused_when_it_is_chosen() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let huge = vec![0u8; (attach::MAX_ATTACHMENT_BYTES + 1) as usize];
    let refused = attach::add_bytes(&db, &draft.id, "huge.bin", &huge, false, NOW);
    assert!(refused.is_err(), "a 25 MB ceiling is Gmail's, not ours to ignore");
    assert!(attach::list(&db, &draft.id).unwrap().is_empty());
}

#[test]
fn several_files_are_refused_as_a_total_rather_than_one_at_a_time() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let chunk = vec![0u8; 10 * 1024 * 1024];
    attach::add_bytes(&db, &draft.id, "one.bin", &chunk, false, NOW).unwrap();
    attach::add_bytes(&db, &draft.id, "two.bin", &chunk, false, NOW).unwrap();
    let third = attach::add_bytes(&db, &draft.id, "three.bin", &chunk, false, NOW);
    assert!(third.is_err(), "30 MB is past what Gmail will take");
    assert_eq!(attach::list(&db, &draft.id).unwrap().len(), 2);
}

#[test]
fn a_senders_filename_is_sanitized_before_it_is_ever_stored() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let stored = attach::add_bytes(&db, &draft.id, "../../etc/passwd", b"x", false, NOW).unwrap();
    assert!(!stored.filename.contains('/'), "{}", stored.filename);
    assert!(!stored.filename.contains(".."), "{}", stored.filename);
}

#[test]
fn forgetting_a_draft_takes_its_files_with_it() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    attach::add_bytes(&db, &draft.id, "note.txt", b"hello", false, NOW).unwrap();
    draft::delete_draft(&db, &draft.id, NOW).unwrap();
    assert!(attach::list(&db, &draft.id).unwrap().is_empty());
}

/// The whole point of the draft row and the `compose_attachments` table: a
/// message written on Tuesday and sent on Thursday still has its files.
#[test]
fn a_file_survives_the_draft_being_saved_closed_and_reopened() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>Attached.</div>"),
        NOW,
    )
    .unwrap();
    let stored = attach::add_bytes(&db, &draft.id, "terms.pdf", b"%PDF-1.4", false, NOW).unwrap();

    // Closing a composer keeps the row; only a discard or a send takes it out.
    // Reopening is a fresh read, which is what this asserts on.
    let reopened = draft::load_draft(&db, &draft.id).unwrap().expect("the draft");
    assert_eq!(reopened.attachments.len(), 1);
    assert_eq!(reopened.attachments[0].filename, "terms.pdf");
    assert_eq!(reopened.attachments[0].id, stored.id);

    // And it is still there in the bytes, not merely in the list.
    let bytes = built_bytes(&db, &reopened);
    let parsed = MessageParser::new().parse(&bytes).unwrap();
    let file = parsed.attachments().next().expect("one attachment");
    assert_eq!(file.contents(), b"%PDF-1.4");
}

// ============================================================ inline images

/// `Content-ID`, `Content-Disposition: inline`, and a body that addresses the
/// part by `cid:` — the three halves of an image that draws where it sits.
#[test]
fn an_inline_image_is_a_related_part_the_body_addresses_by_cid() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let image =
        attach::add_bytes(&db, &draft.id, "chart.png", b"\x89PNG\r\n\x1a\n", true, NOW).unwrap();
    assert!(image.inline);
    assert!(!image.content_id.is_empty());

    // The body points at it, the way the composer writes it.
    let mut draft = draft::load_draft(&db, &draft.id).unwrap().unwrap();
    draft.body = format!(
        "<div>Here it is:</div><div><img src=\"cid:{}\" alt=\"chart.png\"></div>",
        image.content_id
    );

    let bytes = built_bytes(&db, &draft);
    let text = String::from_utf8_lossy(&bytes).into_owned();

    // RFC 2387: a `cid:` resolves inside a `multipart/related`, and the `type`
    // parameter names which part is the one to display.
    assert!(text.contains("multipart/related"), "{text}");
    assert!(text.contains("type=\"multipart/alternative\""), "{text}");
    assert!(
        text.contains(&format!("Content-ID: <{}>", image.content_id)),
        "{text}"
    );
    assert!(text.contains("Content-Disposition: inline"), "{text}");

    let parsed = MessageParser::new().parse(&bytes).unwrap();
    let html = parsed.body_html(0).expect("an html part").into_owned();
    assert!(
        html.contains(&format!("cid:{}", image.content_id)),
        "the body must address the part it ships: {html}"
    );

    // The image is a part of the message, not a file offered beside it. The
    // disposition is the whole difference, and it is what a recipient's client
    // reads to decide whether to draw it or list it.
    let dispositions = dispositions_of(&bytes);
    assert_eq!(dispositions, vec!["inline"], "{dispositions:?}");
}

/// Every `Content-Disposition` in a message, in order, by type alone.
fn dispositions_of(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("Content-Disposition:"))
        .map(|rest| {
            rest.trim()
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .collect()
}

/// One of each. The nesting is what Outlook needs: an inline image flattened
/// alongside the attachments renders a second time at the bottom, unnamed.
#[test]
fn an_image_in_the_body_and_a_file_beside_it_nest_related_inside_mixed() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let image =
        attach::add_bytes(&db, &draft.id, "chart.png", b"\x89PNG\r\n\x1a\n", true, NOW).unwrap();
    attach::add_bytes(&db, &draft.id, "terms.pdf", b"%PDF-1.4", false, NOW).unwrap();

    let mut draft = draft::load_draft(&db, &draft.id).unwrap().unwrap();
    draft.body = format!("<div><img src=\"cid:{}\"></div>", image.content_id);
    let bytes = built_bytes(&db, &draft);
    let text = String::from_utf8_lossy(&bytes).into_owned();

    let mixed = text.find("multipart/mixed").expect("a mixed wrapper");
    let related = text.find("multipart/related").expect("a related part");
    assert!(mixed < related, "related belongs inside mixed:\n{text}");

    // The image is drawn where it sits; only the PDF is offered as a file. The
    // order is the nesting: the inline part is inside the related, which comes
    // before the attachment inside the mixed.
    assert_eq!(dispositions_of(&bytes), vec!["inline", "attachment"]);

    let parsed = MessageParser::new().parse(&bytes).unwrap();
    assert!(parsed.body_text(0).is_some(), "the alternative survives");
    assert!(parsed.body_html(0).is_some(), "the alternative survives");
}

/// A message with nothing inline must come out byte-identical in structure to
/// the one this codebase has always produced.
#[test]
fn a_message_with_nothing_inline_keeps_the_shape_it_always_had() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>Numbers.</div>"),
        NOW,
    )
    .unwrap();
    attach::add_bytes(&db, &draft.id, "q3.csv", b"a,b\n", false, NOW).unwrap();
    let draft = draft::load_draft(&db, &draft.id).unwrap().unwrap();
    let text = String::from_utf8_lossy(&built_bytes(&db, &draft)).into_owned();
    assert!(!text.contains("multipart/related"), "{text}");
    assert!(text.contains("multipart/mixed"), "{text}");
}

#[test]
fn an_image_moves_between_the_body_and_the_list_without_being_re_read() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let image =
        attach::add_bytes(&db, &draft.id, "chart.png", b"\x89PNG\r\n\x1a\n", false, NOW).unwrap();
    assert!(!image.inline, "attached is the default");

    let inlined = attach::set_inline(&db, &image.id, true).unwrap().unwrap();
    assert!(inlined.inline);
    // The id it is addressed by does not change with the flag, which is why
    // moving it back and forth cannot break a body that already points at it.
    assert_eq!(inlined.content_id, image.content_id);

    let back = attach::set_inline(&db, &image.id, false).unwrap().unwrap();
    assert!(!back.inline);
    assert_eq!(back.content_id, image.content_id);
}

/// The choice is only offered on images, and Rust does not take somebody's word
/// for it: `attachAdd` carries one `inline` flag for every path in the drop.
#[test]
fn a_file_that_is_not_an_image_is_attached_however_it_was_asked_for() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let pdf = attach::add_bytes(&db, &draft.id, "terms.pdf", b"%PDF", true, NOW).unwrap();
    assert!(!pdf.inline, "a PDF has nowhere to render inside a body");
    assert!(attach::set_inline(&db, &pdf.id, true).is_err());
}

/// An image can be too large to *draw* long before it is too large to *send*.
/// The ceiling is the receive side's, and being past it costs the picture in
/// the body, not the file.
#[test]
fn an_image_too_large_to_draw_is_attached_rather_than_placed_in_the_body() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    // Real PNG magic, so the *size* is the only thing that can be refusing it.
    // These were zero bytes, and once `inline_mime` started sniffing they would
    // have been refused for not being an image at all — the assertion would
    // still have passed and stopped meaning anything.
    let mut huge = b"\x89PNG\r\n\x1a\n".to_vec();
    huge.resize((attach::MAX_INLINE_IMAGE_BYTES + 1) as usize, 0);
    let stored = attach::add_bytes(&db, &draft.id, "raw.png", &huge, true, NOW).unwrap();
    assert!(!stored.inline, "too big for a data URL, small enough to send");
    assert_eq!(attach::list(&db, &draft.id).unwrap().len(), 1);

    // And asking for it again says why rather than doing it quietly.
    let refused = attach::set_inline(&db, &stored.id, true);
    assert!(refused.is_err());
    assert!(refused.unwrap_err().to_string().contains("raw.png"));
}

/// An SVG is an image to a browser and a script host to everything that draws
/// it. It can be attached; it cannot go in a body.
#[test]
fn an_svg_is_attachable_but_never_inline() {
    let (db, _a, thread, _m) = seeded();
    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();
    let svg = attach::add_bytes(&db, &draft.id, "logo.svg", b"<svg/>", true, NOW).unwrap();
    assert!(!svg.inline);
    assert_eq!(attach::list(&db, &draft.id).unwrap().len(), 1);
}

#[tokio::test]
async fn a_message_over_the_json_limit_goes_to_the_upload_host_instead() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>Photos.</div>"),
        NOW,
    )
    .unwrap();
    let big = vec![7u8; 6 * 1024 * 1024];
    attach::add_bytes(&db, &draft.id, "photo.jpg", &big, false, NOW).unwrap();
    let draft = draft::load_draft(&db, &draft.id).unwrap().unwrap();

    let built = draft::build(&db, &draft, NOW, 1).unwrap();
    out.queue(&built, NOW, NOW).unwrap();
    out.flush_due(NOW + 1).await.unwrap();

    let requests = transport.requests();
    let send = requests.last().expect("one send");
    assert!(send.url.contains("/upload/"), "{}", send.url);
    assert!(send.url.contains("uploadType=multipart"), "{}", send.url);
    let content_type = send
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    assert!(content_type.starts_with("multipart/related"), "{content_type}");
    // The message rides as bytes, not as base64 inside a JSON string.
    let body = String::from_utf8_lossy(send.body.as_deref().unwrap_or_default()).into_owned();
    assert!(body.contains("message/rfc822"), "the RFC822 part is missing");
    assert!(body.contains("\"threadId\""), "the conversation is named in the metadata part");
}

#[tokio::test]
async fn an_ordinary_reply_still_takes_the_ordinary_endpoint() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));
    let built = draft::build(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>On it.</div>"),
        NOW,
        1,
    )
    .unwrap();
    out.queue(&built, NOW, NOW).unwrap();
    out.flush_due(NOW + 1).await.unwrap();

    let requests = transport.requests();
    let send = requests.last().expect("one send");
    assert!(!send.url.contains("/upload/"), "{}", send.url);
}

// ================================================== sending a draft-backed reply

/// Push a draft to the fake Gmail and hand back its local id, with the
/// transport left answering as `messages.send` rather than `drafts.create`.
async fn pushed_draft(
    db: &Db,
    out: &Outbox,
    transport: &Arc<FakeTransport>,
    thread: i64,
    body: &str,
) -> serde_json::Value {
    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"r-draft-1","message":{"id":"gmsg-draft-1","threadId":"gthread-1"}}"#,
    ));
    let saved = save_body(db, out, thread, body, NOW).await;
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(transport)));
    sync.push(saved["id"].as_str().unwrap(), NOW).await.unwrap();
    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"gmsg-draft-1","threadId":"gthread-1"}"#,
    ));
    saved
}

/// The bug, end to end.
///
/// He replied, pressed `⌘⏎`, and the conversation showed the sent message *and*
/// a draft of the same words. A reply that already exists as a Gmail draft has
/// to be sent **as that draft**: one request, which sends it and removes it, so
/// there is no moment in which both exist and nothing left over if the app dies
/// immediately afterwards.
#[tokio::test]
async fn sending_a_pushed_draft_sends_the_draft_itself_and_leaves_one_message() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = pushed_draft(&db, &out, &transport, thread, "Both items are handled.").await;
    let draft_id = saved["id"].as_str().unwrap().to_string();

    dispatch(&db, &out, json!({ "op": "send", "draft": saved, "now": NOW + 1 }))
        .await
        .unwrap();
    out.flush_due(NOW + UNDO_WINDOW_MS + 2).await.unwrap();

    // The one request that went out was `drafts.send`, addressed to the draft.
    let sends: Vec<_> = transport
        .requests()
        .into_iter()
        .filter(|r| r.url.contains("/drafts/send") || r.url.contains("/messages/send"))
        .collect();
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert!(sends[0].url.ends_with("/drafts/send"), "{}", sends[0].url);
    let body: serde_json::Value =
        serde_json::from_slice(sends[0].body.as_deref().unwrap_or_default()).unwrap();
    assert_eq!(body["id"].as_str(), Some("r-draft-1"), "it names the draft");
    assert!(body["message"]["raw"].is_string(), "and carries the queued bytes");

    // Nothing else was asked of Gmail: no `drafts.delete` chasing the send.
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.method.as_str() == "DELETE"),
        "a draft that was sent is already gone; deleting it is the second call \
         whose failure is this bug"
    );

    // And the conversation holds the reply, once, with no draft beside it.
    let messages = messages_in(&db, thread);
    assert_eq!(
        messages.iter().filter(|m| m.is_draft).count(),
        0,
        "no draft left in the conversation"
    );
    assert_eq!(
        messages.iter().filter(|m| !m.is_draft && m.subject.starts_with("Re:")).count(),
        1,
        "exactly one sent reply"
    );
    assert!(draft::load_draft(&db, &draft_id).unwrap().is_none(), "the row");
    assert!(drafts_mailbox(&db).is_empty(), "the Drafts mailbox");
}

/// The other half of the routing: a reply Gmail has never seen as a draft still
/// goes out as a new message. Losing that would strand every reply written
/// while the network was down.
#[tokio::test]
async fn a_reply_gmail_never_saw_as_a_draft_still_goes_to_messages_send() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let built = draft::build(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>On it.</div>"),
        NOW,
        1,
    )
    .unwrap();
    out.queue(&built, NOW, NOW).unwrap();
    out.flush_due(NOW + 1).await.unwrap();

    let send = transport.requests().pop().expect("one send");
    assert!(send.url.ends_with("/messages/send"), "{}", send.url);
}

/// The upload host is chosen by size, and the draft id must not cost a reply
/// its attachment.
#[tokio::test]
async fn a_draft_backed_send_with_a_file_on_it_still_takes_the_upload_host() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = pushed_draft(&db, &out, &transport, thread, "<div>Photos.</div>").await;
    let draft_id = saved["id"].as_str().unwrap().to_string();
    let big = vec![7u8; 6 * 1024 * 1024];
    attach::add_bytes(&db, &draft_id, "photo.jpg", &big, false, NOW).unwrap();

    let stored = draft::load_draft(&db, &draft_id).unwrap().unwrap();
    let built = draft::build(&db, &stored, NOW, 1).unwrap();
    out.queue(&built, NOW, NOW).unwrap();
    out.flush_due(NOW + 1).await.unwrap();

    let send = transport.requests().pop().expect("one send");
    assert!(send.url.contains("/upload/"), "{}", send.url);
    assert!(send.url.ends_with("/drafts/send?uploadType=multipart"), "{}", send.url);
    let body = String::from_utf8_lossy(send.body.as_deref().unwrap_or_default()).into_owned();
    assert!(body.contains("\"id\":\"r-draft-1\""), "the draft is named in the metadata");
    assert!(body.contains("message/rfc822"), "the RFC822 part is missing");
}

/// The autosave that lands *after* `⌘⏎`.
///
/// This is what actually happened in his mailbox: an outbox row at one
/// millisecond, a draft of the same words thirteen milliseconds later, and a
/// Gmail draft created from it while the reply was still inside its undo
/// window. The save has to be refused, and no second draft may be created.
#[tokio::test]
async fn an_autosave_arriving_after_the_send_does_not_write_the_draft_back() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = pushed_draft(&db, &out, &transport, thread, "Looks good. Thanks!").await;
    let before = transport.draft_requests().len();

    dispatch(&db, &out, json!({ "op": "send", "draft": saved, "now": NOW + 1 }))
        .await
        .unwrap();
    // The composer's debounce firing a moment too late.
    dispatch(&db, &out, json!({ "op": "saveDraft", "draft": saved, "now": NOW + 14 }))
        .await
        .unwrap();
    out.flush_due(NOW + UNDO_WINDOW_MS + 2).await.unwrap();

    assert!(
        draft::load_draft(&db, saved["id"].as_str().unwrap())
            .unwrap()
            .is_none(),
        "the row stays gone"
    );
    assert_eq!(
        messages_in(&db, thread).iter().filter(|m| m.is_draft).count(),
        0,
        "and no draft is put back in the conversation"
    );
    let created: Vec<_> = transport
        .draft_requests()
        .into_iter()
        .skip(before)
        .filter(|r| r.method.as_str() == "POST" && r.url.ends_with("/drafts"))
        .collect();
    assert!(created.is_empty(), "no second Gmail draft: {created:?}");
}

/// Undo inside the window puts the draft back on the Gmail draft it came from.
///
/// The draft on Google was deliberately left alone at queue time — `drafts.send`
/// was going to consume it — so recalling the send must hand the row its id
/// back. Otherwise the next save is a stranger and `drafts.create` makes the
/// second copy this whole path exists to prevent.
#[tokio::test]
async fn recalling_a_send_puts_the_draft_back_on_the_draft_it_came_from() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = pushed_draft(&db, &out, &transport, thread, "Looks good. Thanks!").await;
    let draft_id = saved["id"].as_str().unwrap().to_string();

    let queued = dispatch(&db, &out, json!({ "op": "send", "draft": saved, "now": NOW + 1 }))
        .await
        .unwrap();
    let outbox_id = queued["entry"]["id"].as_str().unwrap().to_string();
    let undone = dispatch(&db, &out, json!({ "op": "undo", "outboxId": outbox_id }))
        .await
        .unwrap();
    assert_eq!(undone["cancelled"].as_bool(), Some(true));

    let restored = dispatch(
        &db,
        &out,
        json!({ "op": "saveDraft", "draft": saved, "now": NOW + 20 }),
    )
    .await
    .unwrap();
    assert_eq!(
        restored["draft"]["remote"]["draftId"].as_str(),
        Some("r-draft-1"),
        "the recalled draft still knows which Gmail draft it is"
    );

    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));
    sync.push(&draft_id, NOW + 21).await.unwrap();
    let last = transport.draft_requests().pop().expect("a push");
    assert_eq!(last.method.as_str(), "PUT", "an update, not a second draft");
    assert!(last.url.ends_with("/drafts/r-draft-1"), "{}", last.url);
}

// =================================================================== discard

#[tokio::test]
async fn discarding_removes_the_row_the_mirror_and_the_gmail_draft() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = dispatch(
        &db,
        &out,
        json!({
            "op": "saveDraft",
            "draft": reply_draft(&db, thread, DraftKind::Reply, "<div>Half a thought.</div>"),
            "now": NOW,
        }),
    )
    .await
    .unwrap();
    let draft_id = saved["draft"]["id"].as_str().unwrap().to_string();
    // Give the row a Gmail identity, as a successful push would.
    draft::set_remote(
        &db,
        &draft_id,
        &compose::draft::DraftRemote {
            state: compose::draft::RemoteState::Synced,
            draft_id: Some("r-1234".into()),
            message_id: Some("gmsg-1234".into()),
            thread_id: Some("t-1".into()),
            error: None,
            synced_at: NOW,
        },
    )
    .unwrap();
    assert!(messages_in(&db, thread).iter().any(|m| m.is_draft));

    let result = dispatch(
        &db,
        &out,
        json!({ "op": "discardDraft", "draftId": draft_id, "now": NOW + 1000 }),
    )
    .await
    .unwrap();

    assert_eq!(result["remote"].as_str(), Some("deleted"));
    assert!(draft::load_draft(&db, &draft_id).unwrap().is_none(), "the row");
    assert!(
        !messages_in(&db, thread).iter().any(|m| m.is_draft),
        "the mirror in the conversation"
    );
    let deletes: Vec<_> = transport
        .requests()
        .into_iter()
        .filter(|r| r.url.contains("/drafts/r-1234"))
        .collect();
    assert_eq!(deletes.len(), 1, "exactly one drafts.delete");
}

#[tokio::test]
async fn a_discard_gmail_refuses_says_so_rather_than_pretending() {
    let (db, _a, thread, _m) = seeded();
    // Nothing reaches Google: the push fails, and so does the delete. The row's
    // Gmail identity is written by hand below, which is the state a draft is in
    // after a push that landed and a network that has since gone away.
    let transport = FakeTransport::always_failing(500, r#"{"error":{"message":"nope"}}"#);
    let out = outbox(&db, Arc::clone(&transport));

    let saved = dispatch(
        &db,
        &out,
        json!({
            "op": "saveDraft",
            "draft": reply_draft(&db, thread, DraftKind::Reply, "<div>Half a thought.</div>"),
            "now": NOW,
        }),
    )
    .await
    .unwrap();
    let draft_id = saved["draft"]["id"].as_str().unwrap().to_string();
    draft::set_remote(
        &db,
        &draft_id,
        &compose::draft::DraftRemote {
            state: compose::draft::RemoteState::Synced,
            draft_id: Some("r-1".into()),
            message_id: Some("gmsg-1".into()),
            thread_id: Some("t-1".into()),
            error: None,
            synced_at: NOW,
        },
    )
    .unwrap();

    let result = dispatch(
        &db,
        &out,
        json!({ "op": "discardDraft", "draftId": draft_id, "now": NOW + 1000 }),
    )
    .await
    .unwrap();

    // Local rows are gone either way — the UI never waits on Google — but the
    // sentence about the copy that is still on his phone is the whole point.
    assert!(draft::load_draft(&db, &draft_id).unwrap().is_none());
    assert_eq!(result["remote"].as_str(), Some("failed"));
    assert!(result["error"].as_str().is_some());
}

#[tokio::test]
async fn saving_a_discarded_draft_id_again_does_not_bring_it_back() {
    // The duplicate hazard, from the discard side: the composer's autosave can
    // be in flight when the draft is thrown away, and it arrives after the row
    // has gone.
    //
    // This used to assert *one* draft, on the reasoning that one is better than
    // two. One is still wrong: the owner discarded it. The row that came back
    // was a draft with nothing behind it — "I clicked discard but the draft
    // still shows" — and the push behind it created a Gmail draft holding text
    // he had thrown away. A retired draft stays retired; see `draft::retire`.
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));
    let draft = reply_draft(&db, thread, DraftKind::Reply, "<div>Half a thought.</div>");

    for _ in 0..2 {
        dispatch(
            &db,
            &out,
            json!({ "op": "saveDraft", "draft": draft, "now": NOW }),
        )
        .await
        .unwrap();
    }
    dispatch(
        &db,
        &out,
        json!({ "op": "discardDraft", "draftId": draft.id, "now": NOW + 10 }),
    )
    .await
    .unwrap();
    dispatch(
        &db,
        &out,
        json!({ "op": "saveDraft", "draft": draft, "now": NOW + 20 }),
    )
    .await
    .unwrap();

    assert_eq!(
        messages_in(&db, thread).iter().filter(|m| m.is_draft).count(),
        0,
        "a discarded draft stays discarded, whatever order the writes arrived in"
    );
    assert!(draft::load_draft(&db, &draft.id).unwrap().is_none());
}

/// The bug the owner hit within the hour, twice: every draft-backed send came
/// back "google resource not found" and nothing was delivered.
///
/// `⌘⏎` deletes the `compose_drafts` row and tombstones it, then the outbox
/// waits out the undo window. A push that was already in flight lands during
/// that wait, sees a retired draft, and — before this — deleted the Gmail draft
/// on the grounds that nothing would ever address it again. Something was:
/// `drafts.send`, ten seconds later, at an id that no longer existed. Both of
/// the owner's Gmail drafts were gone from `drafts.list` when it was checked.
///
/// The delete lands *during* the create, from the transport hook, because that
/// is where it lands in life: `push` reads the row, goes out, and the row is
/// taken from under it while the request is in flight.
#[tokio::test]
async fn a_push_landing_after_send_leaves_the_draft_for_the_outbox() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = pushed_draft(&db, &out, &transport, thread, "Sending this one.").await;
    let draft_id = saved["id"].as_str().unwrap().to_string();

    // Queue the send. This is what takes the row out and puts the Gmail id on
    // the outbox row.
    snapshot_draft_row(&db, &draft_id);
    dispatch(&db, &out, json!({ "op": "send", "draft": saved.clone(), "now": NOW + 1 }))
        .await
        .unwrap();

    let held = out.list().unwrap().into_iter().next().unwrap();
    assert_eq!(
        held.draft_id.as_deref(),
        Some(draft_id.as_str()),
        "the outbox row has to name the draft, or nothing can tell a send from a discard"
    );
    assert!(held.gmail_draft_id.is_some(), "and carry its Gmail id");

    // Now replay a push that was in flight across that moment: the draft row
    // is restored just long enough for `push` to load it, and the hook removes
    // it again as the request goes out.
    restore_draft_row(&db, &draft_id);
    let gone = {
        let db = db.clone();
        let id = draft_id.clone();
        move |_: &HttpRequest| delete_draft_row(&db, &id)
    };
    *transport.on_request.lock().unwrap() = Some(Box::new(gone));
    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"r-draft-2","message":{"id":"gmsg-draft-2","threadId":"gthread-1"}}"#,
    ));

    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));
    let _ = sync.push(&draft_id, NOW + 2).await;
    *transport.on_request.lock().unwrap() = None;

    assert!(
        !transport.requests().iter().any(|r| r.method.as_str() == "DELETE"),
        "the outbox is holding this draft in order to send it; deleting it is the 404"
    );

    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"gmsg-sent-1","threadId":"gthread-1"}"#,
    ));
    out.flush_due(NOW + UNDO_WINDOW_MS + 3).await.unwrap();

    let sends: Vec<_> = transport
        .requests()
        .into_iter()
        .filter(|r| r.url.contains("/drafts/send") || r.url.contains("/messages/send"))
        .collect();
    assert_eq!(sends.len(), 1, "{sends:?}");
    let body: serde_json::Value =
        serde_json::from_slice(sends[0].body.as_deref().unwrap_or_default()).unwrap();
    assert_eq!(
        body["id"].as_str(),
        Some("r-draft-2"),
        "it sends the draft that exists, not the id captured before the push landed"
    );

    let entry = out.list().unwrap().into_iter().next().unwrap();
    assert_eq!(
        entry.state,
        compose::outbox::OutboxState::Sent,
        "{:?}",
        entry.last_error
    );
}

/// The other half, so the fix above cannot be "never delete anything": a
/// discard leaves no outbox row, and the draft the push just made is litter.
#[tokio::test]
async fn a_push_landing_after_discard_still_deletes_the_gmail_draft() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = pushed_draft(&db, &out, &transport, thread, "Discarding this one.").await;
    let draft_id = saved["id"].as_str().unwrap().to_string();

    snapshot_draft_row(&db, &draft_id);
    dispatch(
        &db,
        &out,
        json!({ "op": "discardDraft", "draftId": draft_id, "now": NOW + 1 }),
    )
    .await
    .unwrap();

    restore_draft_row(&db, &draft_id);
    let gone = {
        let db = db.clone();
        let id = draft_id.clone();
        move |_: &HttpRequest| delete_draft_row(&db, &id)
    };
    *transport.on_request.lock().unwrap() = Some(Box::new(gone));
    *transport.default.lock().unwrap() = Ok(HttpResponse::json(
        200,
        r#"{"id":"r-draft-2","message":{"id":"gmsg-draft-2","threadId":"gthread-1"}}"#,
    ));

    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));
    let _ = sync.push(&draft_id, NOW + 2).await;
    *transport.on_request.lock().unwrap() = None;

    assert!(
        transport
            .requests()
            .iter()
            .any(|r| r.method.as_str() == "DELETE" && r.url.contains("r-draft-2")),
        "nothing is holding this draft, so the one the push just made has to go"
    );
}

// -------------------------------------------------- in-flight race helpers
//
// A push loads its `Draft`, goes out, and reads the world again when the
// response lands. Reproducing what the owner hit means taking the row away
// between those two reads, which no public API does — a send removes it before
// any push could start. So the row is copied aside, put back for the load, and
// removed again from the transport hook.

/// Copy a draft row aside before something deletes it.
fn snapshot_draft_row(db: &Db, id: &str) {
    db.write(|conn| {
        conn.execute("DROP TABLE IF EXISTS zz_draft_snapshot", [])?;
        conn.execute(
            "CREATE TABLE zz_draft_snapshot AS SELECT * FROM compose_drafts WHERE id = ?1",
            [id],
        )?;
        Ok(())
    })
    .unwrap();
}

/// Put it back, so `push` can load it the way it did before the send.
fn restore_draft_row(db: &Db, id: &str) {
    db.write(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO compose_drafts SELECT * FROM zz_draft_snapshot WHERE id = ?1",
            [id],
        )?;
        Ok(())
    })
    .unwrap();
}

/// Take it away again, from inside the request.
fn delete_draft_row(db: &Db, id: &str) {
    db.write(|conn| {
        conn.execute("DELETE FROM compose_drafts WHERE id = ?1", [id])?;
        Ok(())
    })
    .unwrap();
}

// ============================================== attaching, through the router

/// A drop hands the webview's paths straight to `attachAdd`.
///
/// The same call the file panel makes — the panel route only differs in who
/// produced the list — so this covers both. Driven through `dispatch` rather
/// than `attach::add_bytes` because the part being tested is the one in
/// between: reading the file off disk, and naming it from its path.
#[tokio::test]
async fn a_dropped_path_is_read_off_disk_and_attached() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let dir = std::env::temp_dir().join(format!("mach-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("board deck.pdf");
    std::fs::write(&path, b"%PDF-1.7 deck").unwrap();

    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>Deck attached.</div>"),
        NOW,
    )
    .unwrap();

    let result = dispatch(
        &db,
        &out,
        json!({
            "op": "attachAdd",
            "draftId": draft.id,
            "paths": [path.to_string_lossy()],
            "now": NOW,
        }),
    )
    .await
    .unwrap();

    assert_eq!(result["refused"].as_array().unwrap().len(), 0);
    let attachments = result["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["filename"], "board deck.pdf");
    assert_eq!(attachments[0]["sizeBytes"], 13);
    assert_eq!(attachments[0]["inline"], false);

    // And it is on the draft the composer will reload, not only in the answer.
    let reopened = draft::load_draft(&db, &draft.id).unwrap().unwrap();
    assert_eq!(reopened.attachments.len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

/// A file that cannot be read says so by name, and does not stop the ones that
/// can. Silently attaching three of four is the failure this project has paid
/// most for.
#[tokio::test]
async fn a_file_that_cannot_be_read_is_named_and_the_rest_still_attach() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let dir = std::env::temp_dir().join(format!("mach-drop-half-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("notes.txt");
    std::fs::write(&good, b"there").unwrap();
    let gone = dir.join("deleted.txt");

    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();

    let result = dispatch(
        &db,
        &out,
        json!({
            "op": "attachAdd",
            "draftId": draft.id,
            "paths": [gone.to_string_lossy(), good.to_string_lossy()],
            "now": NOW,
        }),
    )
    .await
    .unwrap();

    let refused = result["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 1);
    assert!(
        refused[0].as_str().unwrap().contains("deleted.txt"),
        "the refusal has to name the file: {refused:?}"
    );
    assert_eq!(result["attachments"].as_array().unwrap().len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

/// The 25 MB ceiling is reported at the moment the file is chosen, which is the
/// only moment anything can be done about it — not after `⌘⏎`, from the outbox.
#[tokio::test]
async fn a_file_past_the_ceiling_is_refused_where_it_was_chosen() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let dir = std::env::temp_dir().join(format!("mach-drop-big-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("raw.dng");
    std::fs::write(&path, vec![0u8; (attach::MAX_ATTACHMENT_BYTES + 1) as usize]).unwrap();

    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();

    let result = dispatch(
        &db,
        &out,
        json!({
            "op": "attachAdd",
            "draftId": draft.id,
            "paths": [path.to_string_lossy()],
            "now": NOW,
        }),
    )
    .await
    .unwrap();

    let refused = result["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 1);
    let said = refused[0].as_str().unwrap();
    assert!(said.contains("raw.dng"), "{said}");
    assert!(said.contains("25 MB"), "{said}");
    assert!(result["attachments"].as_array().unwrap().is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

/// Dropping an image with `inline` set puts it in the body rather than beside
/// it, and hands back the `Content-ID` the composer writes into the `<img>`.
#[tokio::test]
async fn an_image_dropped_for_the_body_comes_back_with_the_cid_to_point_at() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());

    let dir = std::env::temp_dir().join(format!("mach-drop-inline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("chart.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();

    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>hi</div>"),
        NOW,
    )
    .unwrap();

    let added = dispatch(
        &db,
        &out,
        json!({
            "op": "attachAdd",
            "draftId": draft.id,
            "paths": [path.to_string_lossy()],
            "inline": true,
            "now": NOW,
        }),
    )
    .await
    .unwrap();

    let file = &added["added"][0];
    assert_eq!(file["inline"], true);
    let content_id = file["contentId"].as_str().unwrap().to_string();
    assert!(!content_id.is_empty());

    // The bytes come back for drawing it, and only for the inline ones.
    let images = dispatch(&db, &out, json!({ "op": "attachImages", "draftId": draft.id }))
        .await
        .unwrap();
    let images = images["images"].as_array().unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["contentId"].as_str().unwrap(), content_id);
    assert_eq!(images[0]["mimeType"], "image/png");
    assert!(!images[0]["base64"].as_str().unwrap().is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

/// The draft Gmail holds is rebuilt from the same `draft::build`, so it carries
/// the files. A draft that syncs to his phone without them is a draft he cannot
/// finish anywhere else, which is the whole reason the push exists.
#[tokio::test]
async fn the_draft_pushed_to_gmail_carries_its_attachments() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"gdraft-1","message":{"id":"gm-1","threadId":"gt-1"}}"#,
    ))]);
    let out = outbox(&db, Arc::clone(&transport));

    let draft = draft::save_draft(
        &db,
        &reply_draft(&db, thread, DraftKind::Reply, "<div>Deck attached.</div>"),
        NOW,
    )
    .unwrap();
    attach::add_bytes(&db, &draft.id, "deck.pdf", b"%PDF-1.7 deck", false, NOW).unwrap();

    compose::remote::DraftRemoteSync::new(db.clone(), out.clients())
        .push(&draft.id, NOW)
        .await
        .unwrap();

    // `drafts.create` nests the message, so the `raw` is one level in.
    let create = transport
        .draft_requests()
        .pop()
        .expect("a drafts.create went out");
    let body: serde_json::Value =
        serde_json::from_slice(&create.body.expect("a body")).expect("json");
    let raw = body["message"]["raw"].as_str().expect("a raw message");
    let rfc822 = decode_base64url(raw).expect("base64url");

    let parsed = MessageParser::new().parse(&rfc822).unwrap();
    let file = parsed.attachments().next().expect("the file went too");
    assert_eq!(
        mail_parser::MimeHeaders::attachment_name(file),
        Some("deck.pdf")
    );
    assert_eq!(file.contents(), b"%PDF-1.7 deck");
}

// ===================================================== one draft, one mirror row
//
// "this UX still fucks me up. was it sent? was it not?"
//
// He sent a reply and the conversation showed the sent message *and* a red
// `DRAFT` row of the same words above it, same sender, same time, for several
// seconds. Two copies of one message with one of them labelled DRAFT is the
// exact confusion the pill exists to prevent, pointing the other way: he could
// not tell whether he had answered.
//
// The draft row was a *second* mirror of the same draft, stranded under a
// message id nothing addressed any more, and every removal path names a draft by
// at most two ids. Nothing could reach it until a sync pass swept it. So the
// tests below are in two halves: one mirror per draft however the ids move, and
// the queue taking that mirror out in the same write that puts the reply in.

/// The draft rows a conversation is showing.
fn draft_rows(db: &Db, thread_id: i64) -> Vec<mach_lib::db::models::Message> {
    messages_in(db, thread_id)
        .into_iter()
        .filter(|m| m.is_draft)
        .collect()
}

/// `⌘⏎` on an ordinary reply. The conversation holds the outgoing message and
/// nothing that says DRAFT — before the ten seconds, before any flush, before
/// Gmail has been told anything at all.
#[tokio::test]
async fn queueing_puts_the_reply_in_the_conversation_and_takes_the_draft_out() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = save_body(&db, &out, thread, "Both items are handled.", NOW).await;
    assert_eq!(draft_rows(&db, thread).len(), 1, "the draft is in the thread");

    dispatch(&db, &out, json!({ "op": "send", "draft": saved, "now": NOW + 1 }))
        .await
        .unwrap();

    assert!(draft_rows(&db, thread).is_empty(), "and it goes at queue time");
    let sent: Vec<_> = messages_in(&db, thread)
        .into_iter()
        .filter(|m| !m.is_draft && m.subject.starts_with("Re:"))
        .collect();
    assert_eq!(sent.len(), 1, "with the reply in its place");
    assert_eq!(transport.call_count(), 0, "and nothing waited on Google");
    assert!(drafts_mailbox(&db).is_empty(), "the Drafts mailbox agrees");
}

/// The same, for a draft written in Gmail and adopted here. Its local id is
/// `gmail-draft-<gmail id>` rather than `draft-<millis>`, and it reaches the
/// outbox already carrying a Gmail message id — the shape the owner actually
/// sent when he saw the two rows.
#[tokio::test]
async fn queueing_an_adopted_gmail_draft_takes_its_mirror_out_too() {
    let (db, account, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"r-9999","message":{"id":"gmsg-remote-2","threadId":"gthread-1"}}"#,
    ))]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "First pass.");

    let found = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();
    let draft_id = found["draft"]["id"].as_str().unwrap().to_string();
    assert!(draft_id.starts_with("gmail-draft-"), "{draft_id}");
    let mut draft = found["draft"].clone();
    draft["body"] = json!("First pass, edited here.");
    dispatch(&db, &out, json!({ "op": "saveDraft", "draft": draft, "now": NOW + 1 }))
        .await
        .unwrap();
    // The push moves the draft onto a new Gmail message id, as every save does.
    sync.push(&draft_id, NOW + 1).await.unwrap();
    assert_eq!(draft_rows(&db, thread).len(), 1, "still one row");

    let stored = draft::load_draft(&db, &draft_id).unwrap().unwrap();
    dispatch(
        &db,
        &out,
        json!({ "op": "send", "draft": serde_json::to_value(&stored).unwrap(), "now": NOW + 2 }),
    )
    .await
    .unwrap();

    assert!(
        draft_rows(&db, thread).is_empty(),
        "the adopted draft leaves the conversation at queue time as well"
    );
    assert!(drafts_mailbox(&db).is_empty());
}

/// The collision that produced the two rows.
///
/// `drafts.update` mints a new message id on every save. A sync pass that
/// imports the draft under that new id before `adopt` renames the mirror onto it
/// used to leave both rows: `adopt` renamed with `UPDATE OR IGNORE`, and the
/// `IGNORE` was the duplicate.
#[tokio::test]
async fn a_sync_landing_before_the_adoption_still_leaves_one_draft_row() {
    let (db, account, thread, _m) = seeded();
    let transport = FakeTransport::scripted(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"r-9999","message":{"id":"gmsg-remote-2","threadId":"gthread-1"}}"#,
    ))]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "First pass.");

    let found = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();
    let draft_id = found["draft"]["id"].as_str().unwrap().to_string();
    let mut draft = found["draft"].clone();
    draft["body"] = json!("First pass, edited here.");
    dispatch(&db, &out, json!({ "op": "saveDraft", "draft": draft, "now": NOW + 1 }))
        .await
        .unwrap();

    // Gmail's copy of the same draft, under the id the push is about to report.
    seed_remote_draft(
        &db,
        thread,
        account,
        "gmsg-remote-2",
        "r-9999",
        "First pass, edited here.",
    );
    sync.push(&draft_id, NOW + 1).await.unwrap();

    let rows = draft_rows(&db, thread);
    assert_eq!(rows.len(), 1, "one draft, one row: {rows:?}");
    assert_eq!(rows[0].gmail_message_id, "gmsg-remote-2", "the live id");

    // And it is still the draft the composer owns, so discard can reach it.
    let discarded = dispatch(&db, &out, json!({ "op": "discardDraft", "draftId": draft_id }))
        .await
        .unwrap();
    assert_eq!(discarded["ok"].as_bool(), Some(true));
    assert!(
        draft_rows(&db, thread).is_empty(),
        "the thread and the composer cannot disagree about whether a draft exists"
    );
}

/// However many times it is saved, pushed and re-synced.
#[tokio::test]
async fn a_draft_saved_and_pushed_over_and_over_is_one_row_throughout() {
    let (db, account, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    let mut saved = save_body(&db, &out, thread, "One.", NOW).await;
    let draft_id = saved["id"].as_str().unwrap().to_string();
    for (n, message_id) in ["gmsg-a", "gmsg-b", "gmsg-c"].iter().enumerate() {
        *transport.default.lock().unwrap() = Ok(HttpResponse::json(
            200,
            &format!(r#"{{"id":"r-1","message":{{"id":"{message_id}","threadId":"gthread-1"}}}}"#),
        ));
        saved["body"] = json!(format!("Pass {n}."));
        saved = dispatch(
            &db,
            &out,
            json!({ "op": "saveDraft", "draft": saved, "now": NOW + n as i64 + 1 }),
        )
        .await
        .unwrap()["draft"]
            .clone();
        sync.push(&draft_id, NOW + n as i64 + 1).await.unwrap();
        // And a sync pass re-importing what Gmail now holds.
        seed_remote_draft(&db, thread, account, message_id, "r-1", &format!("Pass {n}."));
        assert_eq!(
            draft_rows(&db, thread).len(),
            1,
            "one mirror after save {n}"
        );
    }
    assert_eq!(drafts_mailbox(&db).len(), 1, "and one row in the mailbox");
}

/// ⌘Z inside the window. The reply goes back to being a draft — in the
/// conversation, in the Drafts mailbox, and in the composer — without the
/// window that sent it having to still exist.
#[tokio::test]
async fn recalling_a_send_puts_the_draft_row_back_in_the_conversation() {
    let (db, _a, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));

    let saved = save_body(&db, &out, thread, "Both items are handled.", NOW).await;
    let draft_id = saved["id"].as_str().unwrap().to_string();
    let sent = dispatch(&db, &out, json!({ "op": "send", "draft": saved, "now": NOW + 1 }))
        .await
        .unwrap();
    let entry = sent["entry"]["id"].as_str().unwrap().to_string();
    assert!(draft_rows(&db, thread).is_empty());

    let undone = dispatch(
        &db,
        &out,
        json!({ "op": "undo", "outboxId": entry, "now": NOW + 2 }),
    )
    .await
    .unwrap();
    assert_eq!(undone["cancelled"].as_bool(), Some(true));

    let rows = draft_rows(&db, thread);
    assert_eq!(rows.len(), 1, "the draft is back, once: {rows:?}");
    assert_eq!(
        rows[0].body_text.as_deref().map(str::trim),
        Some("Both items are handled."),
        "with the words in it, out of the tombstone rather than out of the UI"
    );
    assert!(
        !messages_in(&db, thread)
            .iter()
            .any(|m| m.gmail_message_id.starts_with("mach-outbox:")),
        "and the optimistic copy of the reply is gone"
    );
    assert_eq!(drafts_mailbox(&db).len(), 1, "the Drafts mailbox has it back");
    let reopened = draft::load_draft(&db, &draft_id).unwrap();
    assert!(reopened.is_some(), "and so does the composer");
}

/// The same recall, for the adopted Gmail draft — whose row has to come back
/// still pointing at the Gmail draft it was written as, or the next save creates
/// a second one beside it.
#[tokio::test]
async fn recalling_an_adopted_draft_puts_it_back_pointing_at_the_same_gmail_draft() {
    let (db, account, thread, _m) = seeded();
    let transport = FakeTransport::always_ok();
    let out = outbox(&db, Arc::clone(&transport));
    let row = seed_remote_draft(&db, thread, account, "gmsg-remote-1", "r-9999", "First pass.");

    let found = dispatch(&db, &out, json!({ "op": "loadDraft", "messageId": row }))
        .await
        .unwrap();
    let draft_id = found["draft"]["id"].as_str().unwrap().to_string();
    let sent = dispatch(
        &db,
        &out,
        json!({ "op": "send", "draft": found["draft"], "now": NOW + 1 }),
    )
    .await
    .unwrap();
    assert!(draft_rows(&db, thread).is_empty());

    dispatch(
        &db,
        &out,
        json!({ "op": "undo", "outboxId": sent["entry"]["id"], "now": NOW + 2 }),
    )
    .await
    .unwrap();

    assert_eq!(draft_rows(&db, thread).len(), 1, "back in the conversation");
    let restored = draft::load_draft(&db, &draft_id).unwrap().expect("the row");
    assert_eq!(
        restored.remote.draft_id.as_deref(),
        Some("r-9999"),
        "still the same Gmail draft"
    );
}

/// The rows already on disk. A store carrying the duplicates and the orphans
/// this bug wrote is repaired once, on the first compose call after the upgrade.
#[test]
fn the_repair_collapses_duplicate_mirrors_and_drops_the_ones_nothing_owns() {
    let (db, account, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let _ = out;

    // A draft with a row under the id Gmail last named *and* a stranded one
    // under the id before it, which is what `adopt`'s ignored rename left.
    seed_remote_draft(&db, thread, account, "gmsg-old", "r-1", "Two items.");
    seed_remote_draft(&db, thread, account, "gmsg-new", "r-1", "Two items.");
    // And a local mirror whose draft row is gone: the one that answered ⌘⇧⌫
    // with "There is no draft here to throw away".
    seed_remote_draft(
        &db,
        thread,
        account,
        "mach-draft:draft-vanished",
        "r-2",
        "Nothing owns this.",
    );
    db.write(|conn| {
        conn.execute(
            "INSERT INTO compose_drafts (id, account_id, thread_id, kind, subject, body,
                                         updated_at, gmail_draft_id, gmail_message_id,
                                         remote_state, remote_synced_at)
             VALUES ('draft-live', ?1, ?2, 'reply', 'Re: Series A data room', 'Two items.',
                     ?3, 'r-1', 'gmsg-new', 'synced', ?3)",
            rusqlite::params![account, thread, NOW],
        )?;
        // The state a repaired store must not still be in.
        conn.execute("UPDATE messages SET mach_draft_id = NULL", [])?;
        conn.execute("DROP INDEX IF EXISTS idx_messages_mach_draft", [])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(draft_rows(&db, thread).len(), 3, "the store as it stands");

    db.write(compose::create_compose_schema).unwrap();

    let rows = draft_rows(&db, thread);
    assert_eq!(rows.len(), 1, "one draft, one row: {rows:?}");
    assert_eq!(rows[0].gmail_message_id, "gmsg-new", "the id Gmail named");

    // Idempotent: running it again is a no-op, not a second opinion.
    db.write(compose::create_compose_schema).unwrap();
    assert_eq!(draft_rows(&db, thread).len(), 1);
}

// ================================================= header injection

// A header ends at a CRLF, so any CR or LF in a value that becomes one ends it
// early and starts another — chosen by whoever supplied the value. Against the
// pinned `mail-builder 0.4.4` this worked through the subject, the address
// headers, `In-Reply-To`, an attachment's `Content-Type` and its `Content-ID`.
//
// These tests assert on the *emitted message*, not on the guard. A test that a
// filter removes `\r\n` proves the filter works; only a test that no second
// header appears in the bytes proves the hole is shut, and that is the one that
// keeps working if the defence is ever rewritten.
//
// The subject has a length-dependent edge that makes it easy to miss.
// `mail-builder` writes a *short* all-ASCII subject byte for byte and RFC 2047
// encodes a long one, because `get_encoding_type(text, is_inline = true, ..)`
// matches its `is_inline && ch == b'\n'` arm before the arm that would force
// encoding, and a long subject escapes only by tripping a separate fold check.
// So the short subject is the exploitable one, and it is covered by name.

/// Every header name in the block, ignoring folded continuation lines.
fn header_names(bytes: &[u8]) -> Vec<String> {
    headers_of(bytes)
        .lines()
        .filter(|line| !line.starts_with(' ') && !line.starts_with('\t'))
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.trim().to_ascii_lowercase())
        .collect()
}

/// A message with nothing wrong with it, for one field at a time to be spoiled.
fn clean_outgoing() -> Outgoing {
    Outgoing {
        from: Mailbox::named("Alex Rivera", "alex@example.com"),
        to: vec![Mailbox::named("Sam Patel", "sam@partner.com")],
        cc: vec![],
        bcc: vec![],
        subject: "Lunch".into(),
        text: "ok".into(),
        html: "<p>ok</p>".into(),
        attachments: vec![],
        in_reply_to: None,
        references: vec![],
        message_id: "m1@example.com".into(),
        date_ms: NOW,
    }
}

/// The property under test: `build_rfc822` either refuses, or emits a header
/// block carrying no header the caller did not ask for.
///
/// Refusing is what it does — the choice is argued in `mime.rs` — but the
/// assertion is written on the output so it survives that choice changing.
fn assert_cannot_smuggle_a_header(label: &str, msg: &Outgoing) {
    let bytes = match build_rfc822(msg) {
        Err(e) => {
            assert!(
                !e.to_string().is_empty(),
                "{label}: refused with nothing to tell the sender"
            );
            return;
        }
        Ok(bytes) => bytes,
    };

    let names = header_names(&bytes);
    for smuggled in ["bcc", "x-mach-injected"] {
        assert!(
            !names.iter().any(|n| n == smuggled),
            "{label}: a {smuggled:?} header appeared that nobody asked for\n\
             headers: {names:?}\n{}",
            headers_of(&bytes)
        );
    }
    assert!(
        !headers_of(&bytes).contains("attacker@evil.example"),
        "{label}: the attacker's address reached the header block\n{}",
        headers_of(&bytes)
    );
}

/// The three spellings of a line break, plus the NUL that ends a string for
/// whatever parses it next.
const BREAKS: &[(&str, &str)] = &[
    ("CRLF", "\r\n"),
    ("bare LF", "\n"),
    ("bare CR", "\r"),
    ("LF CR", "\n\r"),
    ("NUL then CRLF", "\0\r\n"),
];

#[test]
fn a_short_subject_cannot_carry_a_second_header() {
    // The exploitable case. Before the guard, `mail-builder` wrote this one
    // verbatim and the message left with a `Bcc` the owner never typed.
    for (name, brk) in BREAKS {
        let mut msg = clean_outgoing();
        msg.subject = format!("Re: hi{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: yes");
        assert_cannot_smuggle_a_header(&format!("short subject, {name}"), &msg);
    }
}

#[test]
fn a_long_subject_cannot_carry_a_second_header_either() {
    // Safe before the guard, but only by accident: long enough to trip
    // `mail-builder`'s fold check and get RFC 2047 encoded. A fix that leans on
    // that accident is not a fix, so the long case sits next to the short one.
    for (name, brk) in BREAKS {
        let mut msg = clean_outgoing();
        msg.subject = format!(
            "{}{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: yes",
            "A".repeat(120)
        );
        assert_cannot_smuggle_a_header(&format!("long subject, {name}"), &msg);
    }
}

#[test]
fn a_subject_that_decodes_to_a_line_break_stays_one_header() {
    // An encoded-word the *receiver* decodes back into a CRLF, and the raw
    // `=0D=0A` spelling of the same thing. These are legal subject text, so they
    // send — what they may not do is reach the wire as bytes a second parse
    // could split on.
    for spelling in [
        "=?us-ascii?Q?hi=0D=0ABcc:_attacker@evil.example?=",
        "=?utf-8?B?aGkNCkJjYzogYXR0YWNrZXJAZXZpbC5leGFtcGxl?=",
        "hi=0D=0ABcc: attacker@evil.example",
    ] {
        let mut msg = clean_outgoing();
        msg.subject = spelling.to_string();
        let bytes = build_rfc822(&msg).expect("an encoded-word subject is still sendable");
        assert!(
            !header_names(&bytes).iter().any(|n| n == "bcc"),
            "{spelling:?} produced a Bcc header:\n{}",
            headers_of(&bytes)
        );
        let head = headers_of(&bytes);
        for line in head.lines() {
            assert!(
                !line.starts_with("Bcc:"),
                "the subject folded into something that reads as a header:\n{head}"
            );
        }
    }
}

#[test]
fn a_recipient_address_cannot_carry_a_second_header() {
    for (name, brk) in BREAKS {
        for field in ["to", "cc", "bcc"] {
            let mut msg = clean_outgoing();
            let hostile = Mailbox::new(format!(
                "sam@partner.com{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: yes"
            ));
            match field {
                "to" => msg.to = vec![hostile],
                "cc" => msg.cc = vec![hostile],
                _ => msg.bcc = vec![hostile],
            }
            assert_cannot_smuggle_a_header(&format!("{field} address, {name}"), &msg);
        }
    }
}

#[test]
fn a_recipient_address_cannot_smuggle_a_second_mailbox() {
    // No line break needed: `render_mailbox` wraps the address in `<…>`, so a
    // `>` closes it early and what follows is read as another recipient.
    for hostile in [
        "sam@partner.com>, <attacker@evil.example",
        "sam@partner.com>,<attacker@evil.example",
        "sam@partner.com>; <attacker@evil.example",
    ] {
        let mut msg = clean_outgoing();
        msg.to = vec![Mailbox::new(hostile)];
        let built = build_rfc822(&msg);
        assert!(
            built.is_err(),
            "{hostile:?} was accepted:\n{}",
            headers_of(&built.unwrap())
        );
    }
}

#[test]
fn a_display_name_cannot_carry_a_second_header() {
    for (name, brk) in BREAKS {
        let mut msg = clean_outgoing();
        msg.to = vec![Mailbox::named(
            format!("Sam Patel{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: yes"),
            "sam@partner.com",
        )];
        assert_cannot_smuggle_a_header(&format!("To display name, {name}"), &msg);

        let mut msg = clean_outgoing();
        msg.from = Mailbox::named(
            format!("Alex Rivera{brk}Bcc: attacker@evil.example"),
            "alex@example.com",
        );
        assert_cannot_smuggle_a_header(&format!("From display name, {name}"), &msg);
    }
}

#[test]
fn a_from_address_cannot_carry_a_second_header() {
    for (name, brk) in BREAKS {
        let mut msg = clean_outgoing();
        msg.from = Mailbox::new(format!(
            "alex@example.com{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: yes"
        ));
        assert_cannot_smuggle_a_header(&format!("From address, {name}"), &msg);
    }
}

#[test]
fn a_threading_id_cannot_carry_a_second_header() {
    for (name, brk) in BREAKS {
        let mut msg = clean_outgoing();
        msg.in_reply_to = Some(format!(
            "a@b>{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: <c@d"
        ));
        assert_cannot_smuggle_a_header(&format!("In-Reply-To, {name}"), &msg);

        let mut msg = clean_outgoing();
        msg.references = vec![format!(
            "a@b>{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: <c@d"
        )];
        assert_cannot_smuggle_a_header(&format!("References, {name}"), &msg);

        let mut msg = clean_outgoing();
        msg.message_id = format!("a@b>{brk}Bcc: attacker@evil.example{brk}X-Mach-Injected: <c@d");
        assert_cannot_smuggle_a_header(&format!("Message-ID, {name}"), &msg);
    }
}

#[test]
fn attachment_metadata_cannot_carry_a_second_header() {
    use compose::mime::OutgoingAttachment;

    let hostile = "\r\nBcc: attacker@evil.example\r\nX-Mach-Injected: yes";
    let base = OutgoingAttachment {
        filename: "notes.pdf".into(),
        mime_type: "application/pdf".into(),
        bytes: vec![1, 2, 3],
        inline: false,
        content_id: "cid1".into(),
    };

    for spoil in ["filename", "content type", "content id"] {
        for inline in [false, true] {
            let mut file = base.clone();
            file.inline = inline;
            match spoil {
                "filename" => file.filename = format!("notes{hostile}.pdf"),
                "content type" => file.mime_type = format!("application/pdf{hostile}"),
                _ => file.content_id = format!("cid1{hostile}"),
            }
            let mut msg = clean_outgoing();
            msg.attachments = vec![file];

            // The part headers sit below the message header block, so the whole
            // message is searched rather than just `headers_of`.
            if let Ok(bytes) = build_rfc822(&msg) {
                let text = String::from_utf8_lossy(&bytes);
                assert!(
                    !text.contains("attacker@evil.example"),
                    "attachment {spoil} (inline={inline}) smuggled a header:\n{text}"
                );
            }
        }
    }
}

#[test]
fn refusing_a_message_says_which_field_was_wrong() {
    // Failure has to be visible, and actionable with it: a message he cannot
    // send and cannot diagnose is its own kind of broken.
    let mut msg = clean_outgoing();
    msg.subject = "hi\r\nBcc: attacker@evil.example".into();
    let error = build_rfc822(&msg).expect_err("a subject with a CRLF is refused");
    let text = error.to_string();
    assert!(text.contains("subject"), "does not name the field: {text}");
    assert_eq!(error.kind(), "invalid", "{text}");

    let mut msg = clean_outgoing();
    msg.to = vec![Mailbox::new("sam@partner.com\r\nBcc: attacker@evil.example")];
    let text = build_rfc822(&msg)
        .expect_err("a To address with a CRLF is refused")
        .to_string();
    assert!(text.contains("To"), "does not name the field: {text}");
}

#[test]
fn a_clean_message_is_not_caught_by_any_of_this() {
    // The guard is worthless if ordinary mail trips it. Accents, a comma in a
    // name, a long subject, a real threading chain, a file with punctuation.
    use compose::mime::OutgoingAttachment;

    let bytes = build_rfc822(&Outgoing {
        from: Mailbox::named("José García", "jose@example.com"),
        to: vec![
            Mailbox::named("Patel, Sam", "sam@partner.com"),
            Mailbox::new("bare@partner.com"),
        ],
        cc: vec![Mailbox::named("Ana Ruiz", "ana@socio.es")],
        bcc: vec![Mailbox::new("archive@example.com")],
        subject: format!("Re: {} — año, ¿sí?", "a long subject line ".repeat(6)),
        text: "hola".into(),
        html: "<p>hola</p>".into(),
        attachments: vec![OutgoingAttachment {
            filename: "año, informe (final).pdf".into(),
            mime_type: "application/pdf".into(),
            bytes: vec![1, 2, 3],
            inline: false,
            content_id: "cid-1.abc@example.com".into(),
        }],
        in_reply_to: Some("<parent@example.com>".into()),
        references: vec!["<root@example.com>".into(), "parent@example.com".into()],
        message_id: "new@example.com".into(),
        date_ms: NOW,
    })
    .expect("ordinary mail still builds");

    let names = header_names(&bytes);
    for expected in ["from", "to", "cc", "bcc", "subject", "in-reply-to", "references"] {
        assert!(names.iter().any(|n| n == expected), "missing {expected}: {names:?}");
    }
}

#[test]
fn a_hostile_parent_does_not_break_the_reply_button() {
    // The other half of "refuse rather than repair": a value Mach lifts out of
    // somebody else's message is cleaned where it is derived, so a sender cannot
    // make the reply unsendable by putting a CRLF in a header the owner has no
    // control over. The injected text survives as text, in the subject field,
    // where he can see it.
    let db = Db::open_in_memory().unwrap();
    let account = seed_account(&db, "alex@example.com", Some("Alex Rivera"));
    let thread = seed_thread(&db, account, "Invoice");
    seed_message(
        &db,
        thread,
        account,
        "gmsg-1",
        person("Mallory", "mallory@evil.example"),
        vec![person("Alex Rivera", "alex@example.com")],
        vec![],
        "Invoice\r\nBcc: attacker@evil.example",
        "<parent@evil.example>\r\nBcc: attacker@evil.example",
        None,
        "pay me",
    );

    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, "no"));
    let head = headers_of(&bytes);
    assert!(
        !header_names(&bytes).iter().any(|n| n == "bcc"),
        "the reply carried a Bcc:\n{head}"
    );
    // The parent's Message-ID was malformed, so it is dropped rather than
    // written: a threading hint lost, which beats a header invented.
    assert!(
        !header_names(&bytes).iter().any(|n| n == "in-reply-to"),
        "a malformed parent id was written anyway:\n{head}"
    );
    // Not merely refused — still sendable. The injected text is on the Subject
    // line, as text, which is where he can see it and delete it.
    assert!(
        head.lines()
            .any(|l| l.starts_with("Subject:") && l.contains("Bcc: attacker@evil.example")),
        "the injected text should stay visible in the subject:\n{head}"
    );
    assert!(
        !head.lines().any(|l| l.starts_with("Bcc:")),
        "it became a header instead:\n{head}"
    );
}

#[test]
fn a_hostile_parent_subject_flattens_rather_than_failing() {
    let replied = reply_subject("Invoice\r\nBcc: attacker@evil.example");
    assert!(!replied.contains('\r') && !replied.contains('\n'), "{replied:?}");
    assert!(replied.starts_with("Re: Invoice"), "{replied:?}");
    assert!(replied.contains("Bcc: attacker@evil.example"), "{replied:?}");

    let forwarded = forward_subject("Invoice\nBcc: attacker@evil.example");
    assert!(!forwarded.contains('\n'), "{forwarded:?}");

    let mut msg = clean_outgoing();
    msg.subject = replied;
    let bytes = build_rfc822(&msg).expect("the reply is still sendable");
    assert!(
        !header_names(&bytes).iter().any(|n| n == "bcc"),
        "{}",
        headers_of(&bytes)
    );
}

// ===================================================== the composer's latency

/// Opening a composer must not queue behind the sync engine's writer.
///
/// The two calls `r`, `a` and `f` await before anything is drawn are
/// `load_draft_for_thread` — is there already a draft here? — and `prepare`.
/// Both only read. Both used to begin with `db.write(ensure_compose_schema)`,
/// which takes the single writer mutex, and the sync engine holds that for a
/// whole batch of messages at a time; so pressing `a` while a backfill batch
/// was in flight left the window with nothing on it until Google's pace
/// allowed the batch to finish. Measured at 276ms behind a 300ms batch, and a
/// batch is not bounded by 300ms.
///
/// The assertion is not a stopwatch. A background writer is held for the whole
/// test, and the composer's reads have to *finish while it is held* — which
/// they can only do by never asking for it.
#[test]
fn opening_a_composer_does_not_wait_for_the_sync_writer() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    let (db, _account, thread, _message) = seeded();
    // The one call per store that does have work to do. Every call after it is
    // a read, and that is the state a running app is in.
    draft::load_draft_for_thread(&db, thread).unwrap();

    let holding = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = Arc::new(AtomicBool::new(false));

    let batch = std::thread::spawn({
        let db = db.clone();
        let holding = Arc::clone(&holding);
        let release = Arc::clone(&release);
        move || {
            db.write_background(|_conn| {
                let (lock, announced) = &*holding;
                *lock.lock().unwrap() = true;
                announced.notify_all();
                while !release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            })
            .unwrap();
        }
    });

    {
        let (lock, announced) = &*holding;
        let mut held = lock.lock().unwrap();
        while !*held {
            held = announced.wait(held).unwrap();
        }
    }

    let (tx, rx) = mpsc::channel();
    let opener = std::thread::spawn({
        let db = db.clone();
        move || {
            let existing = draft::load_draft_for_thread(&db, thread).unwrap();
            let prepared =
                draft::prepare(&db, thread, DraftKind::ReplyAll, "d-open".into()).unwrap();
            let _ = tx.send((existing, prepared));
        }
    });

    let opened = rx.recv_timeout(Duration::from_secs(5));
    release.store(true, Ordering::SeqCst);
    opener.join().unwrap();
    batch.join().unwrap();

    let (existing, prepared) = opened.expect(
        "the composer's reads waited on the writer the sync engine was holding — \
         something on the open path is asking for `Db::write` again",
    );
    assert!(existing.is_none());
    // And it is a real reply-all, not an empty draft that happened to return.
    assert_eq!(
        prepared.to.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["tawny@partner.com"]
    );
    assert_eq!(
        prepared.cc.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(),
        vec!["sam@partner.com", "dana@partner.com"]
    );
}


// ===========================================================================
// A file let go on the writing area
// ===========================================================================
//
// `attachAdd` with `inline` set is what a drop on the message body sends, and
// the flag is a *request*. What decides is the bytes: `attach::inline_mime`
// sniffs them, so a file that is not really a raster image is attached instead
// of being placed — quietly, because the chip that appears is the whole answer
// and there is nothing the writer needs to do about it.
//
// The naming here is deliberate. Every case renames the file so the extension
// and the content disagree, which is the only way to tell which of the two the
// decision actually reads.

/// A scratch directory that removes itself, and a file in it.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mach-compose-drop-{nanos}-{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Scratch(path)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("write");
        path.to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The smallest thing `sniff_raster_image` calls a PNG: its signature, then
/// filler. The sniffer reads the first eight bytes and nothing else.
fn png_bytes(len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(len.max(8), 0);
    bytes
}

/// A draft with a row, ready to hang files on.
async fn drafted(db: &Db, out: &Outbox, thread: i64) -> String {
    let prepared = dispatch(
        db,
        out,
        json!({ "op": "prepare", "threadId": thread, "kind": "reply", "now": NOW }),
    )
    .await
    .unwrap();
    let draft = prepared["draft"].clone();
    dispatch(db, out, json!({ "op": "saveDraft", "draft": draft, "now": NOW }))
        .await
        .unwrap()["draft"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn dropped(
    db: &Db,
    out: &Outbox,
    draft_id: &str,
    paths: &[String],
    inline: bool,
) -> serde_json::Value {
    dispatch(
        db,
        out,
        json!({
            "op": "attachAdd",
            "draftId": draft_id,
            "paths": paths,
            "inline": inline,
            "now": NOW,
        }),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn an_image_let_go_on_the_message_goes_in_the_message() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let scratch = Scratch::new();
    let id = drafted(&db, &out, thread).await;

    // Named `.dat`, so nothing but the bytes can say this is a picture.
    let path = scratch.write("screenshot.dat", &png_bytes(2048));
    let result = dropped(&db, &out, &id, &[path], true).await;

    let added = &result["added"][0];
    assert_eq!(added["inline"], true, "{result}");
    // The part is about to be drawn rather than opened, so it carries the type
    // the bytes really are and not the one the extension claims.
    assert_eq!(added["mimeType"], "image/png");
    assert!(
        added["contentId"].as_str().unwrap().ends_with("@mach.invalid"),
        "an inline part needs a Content-ID the body can point at: {result}"
    );
    assert_eq!(result["refused"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn something_that_is_not_an_image_is_attached_rather_than_placed() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let scratch = Scratch::new();
    let id = drafted(&db, &out, thread).await;

    // A zip wearing a `.png`. Trusting the name would have put a `cid:` in the
    // body pointing at an archive, which every client draws as a broken image.
    let path = scratch.write("holiday.png", b"PK\x03\x04 not an image at all");
    let result = dropped(&db, &out, &id, &[path], true).await;

    let added = &result["added"][0];
    assert_eq!(added["inline"], false, "{result}");
    // It is still attached, and it still leaves. A drop on the body that
    // refused the file outright would be the app being clever about a mistake
    // the writer did not make.
    assert_eq!(
        result["refused"].as_array().unwrap().len(),
        0,
        "attaching is the answer, not an error: {result}"
    );
    assert_eq!(result["attachments"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_pdf_let_go_on_the_message_attaches_and_says_nothing() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let scratch = Scratch::new();
    let id = drafted(&db, &out, thread).await;

    let path = scratch.write("terms.pdf", b"%PDF-1.7\n% a real enough pdf\n");
    let result = dropped(&db, &out, &id, &[path], true).await;

    assert_eq!(result["added"][0]["inline"], false, "{result}");
    // The extension still decides what the recipient opens it with.
    assert_eq!(result["added"][0]["mimeType"], "application/pdf");
    assert_eq!(result["refused"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_drop_anywhere_else_on_the_composer_never_places_anything() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let scratch = Scratch::new();
    let id = drafted(&db, &out, thread).await;

    // The same bytes that go inline on the body. Off the body, `inline` is
    // false and the answer has to be an ordinary attachment.
    let path = scratch.write("chart.png", &png_bytes(2048));
    let result = dropped(&db, &out, &id, &[path], false).await;

    assert_eq!(result["added"][0]["inline"], false, "{result}");
}

#[tokio::test]
async fn a_file_that_is_refused_is_named_and_the_rest_still_land() {
    let (db, _a, thread, _m) = seeded();
    let out = outbox(&db, FakeTransport::always_ok());
    let scratch = Scratch::new();
    let id = drafted(&db, &out, thread).await;

    let good = scratch.write("chart.png", &png_bytes(64));
    let missing = scratch.0.join("gone.pdf").to_string_lossy().into_owned();
    let result = dropped(&db, &out, &id, &[good, missing], false).await;

    assert_eq!(result["added"].as_array().unwrap().len(), 1);
    let refused = result["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 1, "{result}");
    assert!(
        refused[0].as_str().unwrap().contains("gone.pdf"),
        "a refusal has to say which file: {refused:?}"
    );
}

// ---------------------------------------------------------------------------
// Sending a new message from a different account
// ---------------------------------------------------------------------------
//
// The composer's `From` row. What makes this more than an `account_id` update
// is everything that *cannot* come with it: the Gmail draft id belongs to the
// account that minted it, the mirror is filed under message and thread ids of
// the same provenance, and a new message's conversation is a synthetic thread
// of that account's. A move that carried any of them forward would put a
// `drafts.update` for one account's draft on another account's token — a 404,
// with the real draft still sitting where it was.

/// The whole move, end to end: the draft, the mirror and the conversation.
#[tokio::test]
async fn a_new_message_can_be_sent_from_a_different_account() {
    let db = Db::open_in_memory().unwrap();
    let from = seed_account(&db, "bruno@example.com", Some("Bruno"));
    let to = seed_account(&db, "bruno@northwind.example", Some("Bruno at Northwind"));
    let out = outbox(&db, FakeTransport::always_ok());

    let saved = dispatch(
        &db,
        &out,
        json!({
            "op": "saveDraft",
            "draft": {
                "id": "d-move",
                "accountId": from,
                "kind": "new",
                "to": [{ "email": "tawny@partner.com" }],
                "subject": "Coffee?",
                "body": "Thursday?",
            },
            "now": NOW,
        }),
    )
    .await
    .unwrap()["draft"]
        .clone();
    assert!(saved["threadId"].as_i64().is_some(), "it has one to leave");

    let moved = dispatch(
        &db,
        &out,
        json!({ "op": "moveDraftAccount", "draftId": "d-move", "accountId": to, "now": NOW + 1 }),
    )
    .await
    .unwrap();

    assert_eq!(moved["draft"]["accountId"].as_i64(), Some(to));
    assert_eq!(moved["remote"], json!("none"), "it was never pushed");
    assert_eq!(
        moved["draft"]["subject"].as_str(),
        Some("Coffee?"),
        "the words are the one thing that does come with it"
    );

    // One draft in the mailbox, not two, and it is the other account's now.
    //
    // Not asserted on the thread *id*: `threads` has no AUTOINCREMENT, so the
    // row deleted on the way out gives its rowid straight back to the row
    // created on the way in. What the move has to be true of is the owner.
    let rows = drafts_mailbox(&db);
    assert_eq!(rows.len(), 1, "the old mirror went with the old account");
    let now_at = moved["draft"]["threadId"].as_i64().unwrap();
    let (owner, count): (i64, i64) = db
        .read(|c| {
            let owner = c.query_row(
                "SELECT account_id FROM threads WHERE id = ?1",
                [now_at],
                |row| row.get(0),
            )?;
            let count =
                c.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
            Ok((owner, count))
        })
        .unwrap();
    assert_eq!(owner, to, "a new message's conversation is its account's");
    assert_eq!(count, 1, "and the one it left is not still standing");
}

/// The half that costs a request: the copy left behind in the old account is
/// deleted, and the draft arrives at the new one with nothing Gmail can be told
/// to update — so the next push creates rather than 404s.
#[tokio::test]
async fn moving_a_pushed_draft_deletes_the_copy_it_leaves_behind() {
    let db = Db::open_in_memory().unwrap();
    let from = seed_account(&db, "bruno@example.com", Some("Bruno"));
    let to = seed_account(&db, "bruno@northwind.example", Some("Bruno at Northwind"));
    let transport = FakeTransport::scripted(vec![
        Ok(HttpResponse::json(
            200,
            r#"{"id":"r-draft-move","message":{"id":"gmsg-1","threadId":"gthread-9"}}"#,
        )),
        Ok(HttpResponse::json(200, "{}")),
    ]);
    let out = outbox(&db, Arc::clone(&transport));
    let sync = compose::remote::DraftRemoteSync::new(db.clone(), clients(Arc::clone(&transport)));

    dispatch(
        &db,
        &out,
        json!({
            "op": "saveDraft",
            "draft": {
                "id": "d-pushed",
                "accountId": from,
                "kind": "new",
                "to": [{ "email": "tawny@partner.com" }],
                "subject": "Coffee?",
                "body": "Thursday?",
            },
            "now": NOW,
        }),
    )
    .await
    .unwrap();
    sync.push("d-pushed", NOW).await.unwrap();
    assert_eq!(
        draft::load_draft(&db, "d-pushed")
            .unwrap()
            .unwrap()
            .remote
            .draft_id
            .as_deref(),
        Some("r-draft-move"),
        "the account it is leaving holds a real Gmail draft"
    );

    let moved = dispatch(
        &db,
        &out,
        json!({ "op": "moveDraftAccount", "draftId": "d-pushed", "accountId": to, "now": NOW + 1 }),
    )
    .await
    .unwrap();

    assert_eq!(moved["remote"], json!("deleted"));
    let deletes: Vec<String> = transport
        .requests()
        .into_iter()
        .filter(|request| matches!(request.method, mach_lib::google::HttpMethod::Delete))
        .map(|request| request.url)
        .collect();
    assert_eq!(deletes.len(), 1, "one delete, for the draft left behind");
    assert!(
        deletes[0].contains("r-draft-move"),
        "and it names that draft: {}",
        deletes[0]
    );

    let after = draft::load_draft(&db, "d-pushed").unwrap().unwrap();
    assert_eq!(after.account_id, to);
    assert_eq!(
        after.remote.draft_id, None,
        "nothing left for a `drafts.update` to address under the new token"
    );
    assert_eq!(after.remote.message_id, None);
    assert_eq!(after.remote.thread_id, None);
}

/// A reply answers a conversation one account holds: `reply_to_id`, the
/// `References` chain built from it and the thread it is mirrored into are all
/// that account's rows, and Gmail has no call that answers one account's thread
/// as another. The composer does not draw the control here; this is the other
/// end of the same rule, so a caller that has not read it cannot do it anyway.
#[tokio::test]
async fn a_reply_cannot_be_moved_to_another_account() {
    let (db, _a, thread, _m) = seeded();
    let other = seed_account(&db, "bruno@northwind.example", Some("Bruno at Northwind"));
    let out = outbox(&db, FakeTransport::always_ok());

    let saved = save_body(&db, &out, thread, "on it", NOW).await;
    let id = saved["id"].as_str().unwrap().to_string();

    let refused = dispatch(
        &db,
        &out,
        json!({ "op": "moveDraftAccount", "draftId": id, "accountId": other, "now": NOW + 1 }),
    )
    .await;

    assert!(refused.is_err(), "and it says so rather than half-doing it");
    let unchanged = draft::load_draft(&db, &id).unwrap().unwrap();
    assert_ne!(unchanged.account_id, other, "nothing moved");
    assert_eq!(drafts_mailbox(&db).len(), 1, "and the mirror is where it was");
}

/// `c`, then `From`, before a word is typed.
///
/// That is the order somebody who noticed the wrong address uses, and there is
/// no row at that point: autosave declines to save an empty draft, so nothing
/// has been written, nothing mirrored and nothing pushed. Answering with an
/// error would make the control appear broken exactly when it is most likely to
/// be reached for.
#[tokio::test]
async fn moving_a_draft_that_was_never_saved_is_not_a_failure() {
    let db = Db::open_in_memory().unwrap();
    let _from = seed_account(&db, "bruno@example.com", Some("Bruno"));
    let to = seed_account(&db, "bruno@northwind.example", Some("Bruno at Northwind"));
    let out = outbox(&db, FakeTransport::always_ok());

    let moved = dispatch(
        &db,
        &out,
        json!({ "op": "moveDraftAccount", "draftId": "d-never-saved", "accountId": to, "now": NOW }),
    )
    .await
    .unwrap();

    assert!(moved["draft"].is_null(), "there was nothing to hand back");
    assert_eq!(moved["remote"], json!("none"));
    assert!(drafts_mailbox(&db).is_empty(), "and nothing was conjured up");
}
