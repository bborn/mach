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
        })
    }

    fn always_failing(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::new()),
            default: Mutex::new(Ok(HttpResponse::json(status, body.to_string()))),
            requests: Mutex::new(Vec::new()),
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
        })
    }

    fn call_count(&self) -> usize {
        self.requests.lock().unwrap().len()
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
        updated_at: 0,
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
    let bytes = built_bytes(&db, &reply_draft(&db, thread, DraftKind::Reply, "**Yes** — sending now."));
    let text = String::from_utf8_lossy(&bytes).into_owned();

    assert!(text.contains("multipart/alternative"), "{text}");
    assert!(text.contains("Content-Type: text/plain; charset=\"utf-8\""), "{text}");
    assert!(text.contains("Content-Type: text/html; charset=\"utf-8\""), "{text}");

    let parsed = MessageParser::new().parse(&bytes).unwrap();
    let plain = parsed.body_text(0).expect("text part");
    let html = parsed.body_html(0).expect("html part");
    // The markdown source is the plain-text part; the HTML is rendered from it.
    assert!(plain.contains("**Yes** — sending now."), "{plain}");
    assert!(html.contains("<strong>Yes</strong>"), "{html}");
    assert!(!html.contains("**"), "{html}");
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

    assert!(out.cancel(&entry.id).unwrap());

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

    draft::delete_draft(&db, &loaded.id).unwrap();
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
    assert_eq!(transport.call_count(), 0);
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
