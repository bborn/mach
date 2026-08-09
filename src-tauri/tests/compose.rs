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
        remote: Default::default(),
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
        Some("Sending the link this afternoon."),
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

    assert_eq!(found["draft"]["body"].as_str(), Some("Thursday?"));
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
        Some("First thought, rewritten on the train."),
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
