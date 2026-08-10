//! Behaviour tests for the Gmail / Calendar API clients.
//!
//! These never touch the network. The clients take an injectable
//! `HttpTransport`; every test here drives a scripted fake and asserts on both
//! the requests that were built and the values that came back out.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mach_lib::google::calendar::{CalendarClient, EventsListQuery};
use mach_lib::google::gmail::{
    GmailClient, HistoryListQuery, HistoryType, MessageFormat, MessagesListQuery,
};
use mach_lib::google::types::{
    Event, EventsListResponse, HistoryListResponse, Message, ResponseStatus,
};
use mach_lib::google::{
    BoxFuture, GoogleError, HttpMethod, HttpRequest, HttpResponse, HttpTransport, RetryPolicy,
    Sleeper, StaticTokenProvider, TransportError,
};

// ---------------------------------------------------------------- test doubles

/// A transport that replays a scripted list of responses and records every
/// request it was handed.
struct FakeTransport {
    responses: Mutex<std::collections::VecDeque<Result<HttpResponse, TransportError>>>,
    /// When the script runs dry, repeat this forever (used by the retry tests).
    repeat_last: bool,
    last: Mutex<Option<Result<HttpResponse, TransportError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeTransport {
    fn new(responses: Vec<Result<HttpResponse, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            repeat_last: false,
            last: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn always(response: HttpResponse) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(std::collections::VecDeque::new()),
            repeat_last: true,
            last: Mutex::new(Some(Ok(response))),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn call_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for FakeTransport {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let next = self.responses.lock().unwrap().pop_front();
        let out = match next {
            Some(r) => r,
            None if self.repeat_last => self
                .last
                .lock()
                .unwrap()
                .clone()
                .expect("repeat_last transport needs a response"),
            None => Err(TransportError::new("fake transport script exhausted")),
        };
        Box::pin(async move { out })
    }
}

/// Records the durations the retry loop asked for without ever sleeping.
#[derive(Default)]
struct RecordingSleeper {
    slept: Mutex<Vec<Duration>>,
}

impl RecordingSleeper {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn slept(&self) -> Vec<Duration> {
        self.slept.lock().unwrap().clone()
    }
}

impl Sleeper for RecordingSleeper {
    fn sleep<'a>(&'a self, duration: Duration) -> BoxFuture<'a, ()> {
        self.slept.lock().unwrap().push(duration);
        Box::pin(async {})
    }
}

// ------------------------------------------------------------------- fixtures

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {path:?}: {e}"))
}

fn ok_json(name: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::json(200, fixture(name)))
}

fn err_json(status: u16, name: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse::json(status, fixture(name)))
}

fn gmail(transport: Arc<FakeTransport>) -> GmailClient {
    GmailClient::new(transport, Arc::new(StaticTokenProvider::new("test-token")))
        .with_base_url("https://gmail.test/gmail/v1")
}

fn calendar(transport: Arc<FakeTransport>) -> CalendarClient {
    CalendarClient::new(transport, Arc::new(StaticTokenProvider::new("test-token")))
        .with_base_url("https://calendar.test/calendar/v3")
}

// =========================================================== production wiring

#[test]
fn default_clients_point_at_google_over_the_real_transport() {
    // Constructs the production transport but makes no request.
    let transport = Arc::new(mach_lib::google::ReqwestTransport::new());
    let tokens = Arc::new(StaticTokenProvider::new("t"));
    assert_eq!(
        GmailClient::new(transport.clone(), tokens.clone()).base_url(),
        mach_lib::google::GMAIL_BASE_URL
    );
    assert_eq!(
        CalendarClient::new(transport, tokens).base_url(),
        mach_lib::google::CALENDAR_BASE_URL
    );
}

// ======================================================= MIME body extraction

#[test]
fn nested_multipart_yields_text_html_and_attachments() {
    let msg: Message = serde_json::from_str(&fixture("message_nested_multipart.json")).unwrap();
    let body = msg.extract_body();

    let text = body.text.as_deref().expect("text/plain body");
    assert!(text.contains("revenue up 18%"), "text was: {text:?}");
    assert!(text.contains("-- Tawny"));
    assert!(
        !text.contains("<b>"),
        "text body must not be the html part: {text:?}"
    );

    let html = body.html.as_deref().expect("text/html body");
    assert!(html.starts_with("<div dir=\"ltr\">"), "html was: {html:?}");
    assert!(html.contains("<b>18%</b>"));
    assert!(html.contains("cid:chart-inline-001"));

    // Three leaf non-body parts: inline png, pdf, csv.
    assert_eq!(body.attachments.len(), 3, "{:#?}", body.attachments);

    let files: Vec<_> = body.attachments.iter().filter(|a| !a.inline).collect();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].filename, "Q3-numbers.pdf");
    assert_eq!(files[0].mime_type, "application/pdf");
    assert_eq!(files[0].attachment_id.as_deref(), Some("ANGjdJ_pdf_002"));
    assert_eq!(files[0].size, 158204);
    assert_eq!(files[1].filename, "q3.csv");
    assert_eq!(files[1].mime_type, "text/csv");

    let inline: Vec<_> = body.attachments.iter().filter(|a| a.inline).collect();
    assert_eq!(inline.len(), 1);
    assert_eq!(inline[0].filename, "chart.png");
    assert_eq!(inline[0].content_id.as_deref(), Some("chart-inline-001"));
    assert_eq!(inline[0].part_id.as_deref(), Some("0.1"));
}

/// `format=flowed` lives in the part's `Content-Type` parameters and nowhere
/// else: `mimeType` is Gmail's normalized type and has none. This walks a
/// `multipart/alternative` so the declaration has to be read off the plain part
/// that won the body slot rather than off the message or the HTML alternative.
#[test]
fn a_flowed_plain_part_reports_its_content_type_parameters() {
    let msg: Message = serde_json::from_str(&fixture("message_text_flowed.json")).unwrap();
    let body = msg.extract_body();
    assert!(body.text_flowed, "{body:#?}");
    assert!(body.text_delsp, "{body:#?}");
    assert!(body.text.as_deref().unwrap().contains("waiting \n"));
    // The HTML alternative is still taken, and says nothing about flowing.
    assert!(body.html.is_some(), "{body:#?}");
}

/// The overwhelmingly common case, and the one the reported message is in.
#[test]
fn a_plain_part_that_says_nothing_is_not_flowed() {
    for name in ["message_text_only.json", "message_nested_multipart.json"] {
        let msg: Message = serde_json::from_str(&fixture(name)).unwrap();
        let body = msg.extract_body();
        assert!(!body.text_flowed, "{name}: {body:#?}");
        assert!(!body.text_delsp, "{name}: {body:#?}");
    }
}

/// The message the owner could not open an attachment on.
///
/// Sent from the Gmail web composer with nothing typed in it: a `text/plain`
/// part of `\r\n`, a `text/html` part of `<div dir="ltr"><br></div>\r\n`, and a
/// signed PDF. Gmail stamps **every** file its own composer attaches with a
/// `Content-ID: <f_…>` alongside `Content-Disposition: attachment`, and the walk
/// used to read a Content-ID as "this belongs in the body". So the PDF was
/// classified inline, never became a row in `attachments`, and — the body being
/// empty, with no `cid:` reference to it — appeared nowhere at all.
///
/// His report was exactly that: "this email has an attachment but I can't see
/// it at all or download it".
#[test]
fn a_file_gmails_composer_attached_is_a_file_and_not_part_of_the_body() {
    let msg: Message =
        serde_json::from_str(&fixture("message_gmail_composed_attachment.json")).unwrap();
    let body = msg.extract_body();

    // The body arrived intact. There simply is not one — which is why nothing
    // about a thin body is evidence that a sync dropped anything.
    assert_eq!(body.text.as_deref(), Some("\r\n"));
    assert_eq!(body.html.as_deref(), Some("<div dir=\"ltr\"><br></div>\r\n"));
    assert_eq!(body.html.as_deref().unwrap().len(), 27);

    let files: Vec<_> = body.files().collect();
    assert_eq!(files.len(), 1, "{:#?}", body.attachments);
    assert_eq!(files[0].filename, "Agreement-signed.pdf");
    assert_eq!(files[0].mime_type, "application/pdf");
    assert_eq!(files[0].attachment_id.as_deref(), Some("ANGjdJ_signed_001"));
    assert_eq!(files[0].size, 94218);

    // The Content-ID is still carried, because `cid:` resolution looks the part
    // up by it. Being addressable is not the same as being displayed.
    assert_eq!(files[0].content_id.as_deref(), Some("f_mdz1k9rt0"));
    assert_eq!(body.inline_parts().count(), 0);
}

/// The other side of the same rule: a sender who said nothing about disposition
/// and gave the part a Content-ID still gets an inline part, because that is the
/// mailer that embeds an image without declaring it. Only an explicit
/// `attachment` overrides the Content-ID.
#[test]
fn a_content_id_still_decides_when_the_sender_declared_no_disposition() {
    let msg: Message = serde_json::from_str(&fixture("message_nested_multipart.json")).unwrap();
    let body = msg.extract_body();
    let inline: Vec<_> = body.inline_parts().collect();
    assert_eq!(inline.len(), 1, "{:#?}", body.attachments);
    assert_eq!(inline[0].filename, "chart.png");
    assert_eq!(inline[0].content_id.as_deref(), Some("chart-inline-001"));
    // And the two real files are still files.
    assert_eq!(body.files().count(), 2);
}

#[test]
fn headers_are_readable_case_insensitively() {
    let msg: Message = serde_json::from_str(&fixture("message_nested_multipart.json")).unwrap();
    assert_eq!(msg.header("subject").as_deref(), Some("Q3 numbers"));
    assert_eq!(
        msg.header("FROM").as_deref(),
        Some("Tawny Reeves <tawny@example.com>")
    );
    assert_eq!(msg.header("x-does-not-exist"), None);
    assert_eq!(msg.label_ids.len(), 4);
    assert_eq!(msg.internal_date_ms(), Some(1_754_563_200_000));
}

#[test]
fn html_only_message_extracts_html_and_no_text() {
    let msg: Message = serde_json::from_str(&fixture("message_html_only.json")).unwrap();
    let body = msg.extract_body();
    assert!(body.text.is_none(), "unexpected text: {:?}", body.text);
    let html = body.html.as_deref().expect("html body");
    assert!(html.contains("<h1>Big news</h1>"));
    assert!(body.attachments.is_empty());
}

#[test]
fn text_only_message_extracts_text_and_no_html() {
    let msg: Message = serde_json::from_str(&fixture("message_text_only.json")).unwrap();
    let body = msg.extract_body();
    assert!(body.html.is_none(), "unexpected html: {:?}", body.html);
    assert_eq!(
        body.text.as_deref(),
        Some("backup completed: 4212 files, 0 errors\n")
    );
    assert!(body.attachments.is_empty());
}

// ============================================================ Gmail endpoints

#[tokio::test]
async fn messages_get_full_builds_the_right_request() {
    let t = FakeTransport::new(vec![ok_json("message_nested_multipart.json")]);
    let client = gmail(t.clone());

    let msg = client
        .messages_get("me", "18f2c4a9b1d3e5f7", MessageFormat::Full)
        .await
        .expect("messages.get");
    assert_eq!(msg.id, "18f2c4a9b1d3e5f7");

    let reqs = t.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, HttpMethod::Get);
    assert!(
        reqs[0]
            .url
            .starts_with("https://gmail.test/gmail/v1/users/me/messages/18f2c4a9b1d3e5f7"),
        "url was {}",
        reqs[0].url
    );
    assert!(reqs[0].url.contains("format=full"), "url was {}", reqs[0].url);
    assert_eq!(
        reqs[0].header("authorization").as_deref(),
        Some("Bearer test-token")
    );
}

#[tokio::test]
async fn messages_list_query_is_encoded() {
    let t = FakeTransport::new(vec![ok_json("messages_list_page1.json")]);
    let client = gmail(t.clone());

    let page = client
        .messages_list_page(
            "me",
            &MessagesListQuery::new()
                .q("from:tawny has:attachment")
                .label_ids(["INBOX"])
                .max_results(3)
                .include_spam_trash(false),
            None,
        )
        .await
        .expect("messages.list");

    assert_eq!(page.items.len(), 3);
    assert_eq!(page.next_page_token.as_deref(), Some("msg-page-2"));

    let url = &t.requests()[0].url;
    assert!(url.contains("q=from%3Atawny+has%3Aattachment"), "url {url}");
    assert!(url.contains("labelIds=INBOX"), "url {url}");
    assert!(url.contains("maxResults=3"), "url {url}");
}

#[tokio::test]
async fn messages_list_all_follows_next_page_token() {
    let t = FakeTransport::new(vec![
        ok_json("messages_list_page1.json"),
        ok_json("messages_list_page2.json"),
    ]);
    let client = gmail(t.clone());

    let all = client
        .messages_list_all("me", &MessagesListQuery::new(), None)
        .await
        .expect("messages.list all pages");

    assert_eq!(all.len(), 5, "both pages should be concatenated");
    assert_eq!(all[0].id, "18f2c4a9b1d3e5f7");
    assert_eq!(all[4].id, "18f2c4a9b1d3dddd");

    let reqs = t.requests();
    assert_eq!(reqs.len(), 2, "exactly one request per page");
    assert!(!reqs[0].url.contains("pageToken"));
    assert!(
        reqs[1].url.contains("pageToken=msg-page-2"),
        "url {}",
        reqs[1].url
    );
}

#[tokio::test]
async fn messages_list_all_respects_a_limit_and_stops_early() {
    let t = FakeTransport::new(vec![ok_json("messages_list_page1.json")]);
    let client = gmail(t.clone());

    let all = client
        .messages_list_all("me", &MessagesListQuery::new(), Some(2))
        .await
        .unwrap();

    assert_eq!(all.len(), 2);
    assert_eq!(t.call_count(), 1, "must not fetch page 2 once the cap is hit");
}

#[tokio::test]
async fn messages_modify_sends_add_and_remove_label_ids() {
    let t = FakeTransport::new(vec![ok_json("message_text_only.json")]);
    let client = gmail(t.clone());

    client
        .messages_modify("me", "18f2c4a9b1d3bbbb", &["STARRED"], &["UNREAD", "INBOX"])
        .await
        .expect("messages.modify");

    let req = &t.requests()[0];
    assert_eq!(req.method, HttpMethod::Post);
    assert!(req.url.ends_with("/users/me/messages/18f2c4a9b1d3bbbb/modify"));
    let body: serde_json::Value = serde_json::from_slice(req.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["addLabelIds"], serde_json::json!(["STARRED"]));
    assert_eq!(body["removeLabelIds"], serde_json::json!(["UNREAD", "INBOX"]));
}

#[tokio::test]
async fn messages_send_posts_base64url_raw() {
    let t = FakeTransport::new(vec![Ok(HttpResponse::json(
        200,
        r#"{"id":"newid","threadId":"newthread","labelIds":["SENT"]}"#,
    ))]);
    let client = gmail(t.clone());

    let rfc822 = b"To: tawny@example.com\r\nSubject: hi\r\n\r\nbody\r\n";
    let sent = client
        .messages_send("me", rfc822, Some("18f2c4a9b1d3e5f0"))
        .await
        .expect("messages.send");
    assert_eq!(sent.id, "newid");

    let req = &t.requests()[0];
    assert_eq!(req.method, HttpMethod::Post);
    assert!(req.url.ends_with("/users/me/messages/send"), "{}", req.url);
    let body: serde_json::Value = serde_json::from_slice(req.body.as_ref().unwrap()).unwrap();
    let raw = body["raw"].as_str().expect("raw field");
    assert!(!raw.contains('+') && !raw.contains('/') && !raw.contains('='));
    assert_eq!(
        mach_lib::google::types::decode_base64url(raw).unwrap(),
        rfc822.to_vec()
    );
    assert_eq!(body["threadId"], "18f2c4a9b1d3e5f0");
}

/// The pairing this endpoint exists for.
///
/// `users.messages.get` never returns a draft id, at any format, so a draft
/// written on the phone reaches Mach as a message it cannot address. This is the
/// only call that says which draft a message belongs to, and both halves of each
/// pair have to survive parsing or the mapping is worthless.
#[tokio::test]
async fn drafts_list_pairs_a_draft_id_with_the_message_it_holds() {
    let t = FakeTransport::new(vec![ok_json("drafts_list_page1.json")]);
    let page = gmail(t.clone())
        .drafts_list_page("me", Some(100), None)
        .await
        .expect("drafts.list");

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].id, "r-2009665542525810060");
    assert_eq!(page.items[0].message.id, "19fe4a8c6a5bb04d");
    assert_eq!(page.items[0].message.thread_id, "19fe4a8c6a5bb04c");
    assert_eq!(page.next_page_token.as_deref(), Some("draft-page-2"));

    let url = &t.requests()[0].url;
    assert!(url.contains("/users/me/drafts"), "url {url}");
    assert!(url.contains("maxResults=100"), "url {url}");
}

/// Every page, because the sweep treats what it did not see as deleted.
#[tokio::test]
async fn drafts_list_all_follows_next_page_token() {
    let t = FakeTransport::new(vec![
        ok_json("drafts_list_page1.json"),
        ok_json("drafts_list_page2.json"),
    ]);
    let all = gmail(t.clone())
        .drafts_list_all("me", Some(100))
        .await
        .expect("drafts.list all pages");

    assert_eq!(all.len(), 3, "both pages should be concatenated");
    assert_eq!(all[2].id, "r7758301783805686823");

    let reqs = t.requests();
    assert_eq!(reqs.len(), 2);
    assert!(!reqs[0].url.contains("pageToken"));
    assert!(
        reqs[1].url.contains("pageToken=draft-page-2"),
        "url {}",
        reqs[1].url
    );
}

#[tokio::test]
async fn labels_list_parses() {
    let t = FakeTransport::new(vec![ok_json("labels_list.json")]);
    let labels = gmail(t.clone()).labels_list("me").await.unwrap();
    assert_eq!(labels.len(), 4);
    let user = labels.iter().find(|l| l.id == "Label_18").unwrap();
    assert_eq!(user.name, "Lumen/Invoices");
    assert_eq!(user.label_type.as_deref(), Some("user"));
    assert_eq!(user.messages_total, Some(412));
}

#[tokio::test]
async fn get_profile_parses_email_and_history_id() {
    let t = FakeTransport::new(vec![ok_json("profile.json")]);
    let p = gmail(t.clone()).get_profile("me").await.unwrap();
    assert_eq!(p.email_address, "alex@example.com");
    assert_eq!(p.history_id, "884213");
    assert!(t.requests()[0].url.ends_with("/users/me/profile"));
}

#[tokio::test]
async fn attachment_get_decodes_bytes() {
    let t = FakeTransport::new(vec![ok_json("attachment.json")]);
    let bytes = gmail(t.clone())
        .attachment_get("me", "18f2c4a9b1d3e5f7", "ANGjdJ_pdf_002")
        .await
        .unwrap();
    assert_eq!(bytes, b"hello attachment".to_vec());
    assert!(
        t.requests()[0]
            .url
            .ends_with("/users/me/messages/18f2c4a9b1d3e5f7/attachments/ANGjdJ_pdf_002"),
        "{}",
        t.requests()[0].url
    );
}

// ========================================================== history.list — the
// incremental sync path, including the expired-watermark case.

#[test]
fn history_list_response_parses_all_four_change_kinds() {
    let h: HistoryListResponse = serde_json::from_str(&fixture("history_list.json")).unwrap();
    assert_eq!(h.history.len(), 4);
    assert_eq!(h.history_id.as_deref(), Some("884213"));
    assert_eq!(h.next_page_token.as_deref(), Some("hist-page-2"));

    let added = &h.history[0];
    assert_eq!(added.id, "884201");
    assert_eq!(added.messages_added.len(), 1);
    assert_eq!(added.messages_added[0].message.id, "18f2c4a9b1d3e5f7");
    assert!(added.messages_added[0]
        .message
        .label_ids
        .contains(&"UNREAD".to_string()));

    let removed = &h.history[1];
    assert_eq!(removed.labels_removed.len(), 1);
    assert_eq!(removed.labels_removed[0].label_ids, vec!["UNREAD"]);
    assert_eq!(removed.labels_removed[0].message.id, "18f2c4a9b1d3e5f7");

    let starred = &h.history[2];
    assert_eq!(starred.labels_added.len(), 1);
    assert_eq!(starred.labels_added[0].label_ids, vec!["STARRED"]);

    let deleted = &h.history[3];
    assert_eq!(deleted.messages_deleted.len(), 1);
    assert_eq!(deleted.messages_deleted[0].message.id, "18f2c4a9b1d3dddd");
}

#[tokio::test]
async fn history_list_builds_start_history_id_and_types() {
    let t = FakeTransport::new(vec![ok_json("history_list.json")]);
    let client = gmail(t.clone());

    let page = client
        .history_list(
            "me",
            &HistoryListQuery::new("884200")
                .history_types([HistoryType::MessageAdded, HistoryType::LabelRemoved])
                .max_results(100),
            None,
        )
        .await
        .expect("history.list");
    assert_eq!(page.history.len(), 4);

    let url = &t.requests()[0].url;
    assert!(url.contains("startHistoryId=884200"), "url {url}");
    assert!(url.contains("historyTypes=messageAdded"), "url {url}");
    assert!(url.contains("historyTypes=labelRemoved"), "url {url}");
}

#[tokio::test]
async fn history_list_404_is_history_expired_not_a_generic_error() {
    let t = FakeTransport::new(vec![err_json(404, "history_expired_404.json")]);
    let client = gmail(t.clone());

    let err = client
        .history_list("me", &HistoryListQuery::new("1"), None)
        .await
        .expect_err("expired watermark must fail");

    assert!(
        matches!(err, GoogleError::HistoryExpired { .. }),
        "expected HistoryExpired, got {err:?}"
    );
    assert!(err.is_history_expired());
    assert!(!err.is_rate_limited());
}

#[tokio::test]
async fn a_404_on_a_normal_endpoint_is_not_history_expired() {
    let t = FakeTransport::new(vec![err_json(404, "history_expired_404.json")]);
    let client = gmail(t.clone());

    let err = client
        .messages_get("me", "nope", MessageFormat::Metadata)
        .await
        .expect_err("missing message");

    assert!(
        matches!(err, GoogleError::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
    assert!(!err.is_history_expired());
}

#[tokio::test]
async fn history_list_all_pages_concatenates_and_returns_the_new_watermark() {
    let page2 = r#"{"history":[{"id":"884220","messages":[{"id":"x","threadId":"x"}]}],"historyId":"884221"}"#;
    let t = FakeTransport::new(vec![
        ok_json("history_list.json"),
        Ok(HttpResponse::json(200, page2)),
    ]);
    let client = gmail(t.clone());

    let sweep = client
        .history_list_all("me", &HistoryListQuery::new("884200"), None)
        .await
        .unwrap();

    assert_eq!(sweep.records.len(), 5);
    assert_eq!(sweep.history_id.as_deref(), Some("884221"));
    assert!(t.requests()[1].url.contains("pageToken=hist-page-2"));
}

// ================================================================ error model

#[tokio::test]
async fn a_401_maps_to_auth() {
    let t = FakeTransport::new(vec![err_json(401, "unauthorized_401.json")]);
    let err = gmail(t)
        .get_profile("me")
        .await
        .expect_err("401 must not be swallowed");
    assert!(matches!(err, GoogleError::Auth { .. }), "got {err:?}");
    assert!(err.is_auth());
}

#[tokio::test]
async fn a_403_rate_limit_reason_maps_to_rate_limited_not_forbidden() {
    // Retries are disabled so the classification is what surfaces.
    let t = FakeTransport::always(HttpResponse::json(403, fixture("rate_limit_403.json")));
    let client = gmail(t.clone())
        .with_retry_policy(RetryPolicy::none())
        .with_sleeper(RecordingSleeper::new());

    let err = client.get_profile("me").await.expect_err("403 rate limit");
    assert!(matches!(err, GoogleError::RateLimited { .. }), "got {err:?}");
    assert!(err.is_rate_limited());
    assert_eq!(t.call_count(), 1, "RetryPolicy::none must not retry");
}

#[tokio::test]
async fn a_403_without_a_rate_limit_reason_is_forbidden() {
    let body = r#"{"error":{"code":403,"message":"Insufficient Permission","errors":[{"reason":"insufficientPermissions","domain":"global","message":"Insufficient Permission"}],"status":"PERMISSION_DENIED"}}"#;
    let t = FakeTransport::new(vec![Ok(HttpResponse::json(403, body))]);
    let err = gmail(t).get_profile("me").await.unwrap_err();
    assert!(matches!(err, GoogleError::Forbidden { .. }), "got {err:?}");
}

#[tokio::test]
async fn malformed_json_maps_to_deserialize() {
    let t = FakeTransport::new(vec![Ok(HttpResponse::json(200, "{ this is not json"))]);
    let err = gmail(t).get_profile("me").await.unwrap_err();
    assert!(matches!(err, GoogleError::Deserialize { .. }), "got {err:?}");
}

#[tokio::test]
async fn a_transport_failure_maps_to_network_after_exhausting_retries() {
    let t = FakeTransport::new(vec![Err(TransportError::new("dns failure"))]);
    let client = gmail(t)
        .with_retry_policy(RetryPolicy::none())
        .with_sleeper(RecordingSleeper::new());
    let err = client.get_profile("me").await.unwrap_err();
    assert!(matches!(err, GoogleError::Network { .. }), "got {err:?}");
}

// ==================================================== rate limiting / backoff

#[tokio::test]
async fn a_429_is_retried_the_configured_number_of_times_then_gives_up() {
    let t = FakeTransport::always(HttpResponse::json(429, fixture("rate_limit_429.json")));
    let sleeper = RecordingSleeper::new();
    let client = gmail(t.clone())
        .with_retry_policy(RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
            jitter: 0.0,
        })
        .with_sleeper(sleeper.clone());

    let err = client.get_profile("me").await.expect_err("still limited");
    assert!(matches!(err, GoogleError::RateLimited { .. }), "got {err:?}");

    assert_eq!(t.call_count(), 4, "1 initial attempt + 3 retries");
    assert_eq!(
        sleeper.slept(),
        vec![
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
        ],
        "exponential backoff, and the test never actually slept"
    );
}

#[tokio::test]
async fn backoff_is_capped_at_max_delay() {
    let t = FakeTransport::always(HttpResponse::json(503, r#"{"error":{"code":503,"message":"backend error"}}"#));
    let sleeper = RecordingSleeper::new();
    let client = gmail(t.clone())
        .with_retry_policy(RetryPolicy {
            max_retries: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(4),
            jitter: 0.0,
        })
        .with_sleeper(sleeper.clone());

    let _ = client.get_profile("me").await.unwrap_err();
    assert_eq!(
        sleeper.slept(),
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(4),
        ]
    );
}

#[tokio::test]
async fn jitter_never_exceeds_the_nominal_delay_and_is_never_negative() {
    let t = FakeTransport::always(HttpResponse::json(429, fixture("rate_limit_429.json")));
    let sleeper = RecordingSleeper::new();
    let client = gmail(t)
        .with_retry_policy(RetryPolicy {
            max_retries: 8,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
            jitter: 1.0,
        })
        .with_sleeper(sleeper.clone());

    let _ = client.get_profile("me").await.unwrap_err();
    let slept = sleeper.slept();
    assert_eq!(slept.len(), 8);
    for (i, d) in slept.iter().enumerate() {
        let nominal = Duration::from_millis(100 * (1u64 << i.min(20)));
        assert!(*d <= nominal, "attempt {i}: {d:?} exceeded nominal {nominal:?}");
    }
    assert!(
        slept.iter().collect::<std::collections::HashSet<_>>().len() > 1,
        "full jitter should not produce identical delays: {slept:?}"
    );
}

#[tokio::test]
async fn a_429_that_later_succeeds_returns_the_success() {
    let t = FakeTransport::new(vec![
        err_json(429, "rate_limit_429.json"),
        ok_json("profile.json"),
    ]);
    let client = gmail(t.clone())
        .with_retry_policy(RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(1),
            jitter: 0.0,
        })
        .with_sleeper(RecordingSleeper::new());

    let p = client.get_profile("me").await.expect("second attempt wins");
    assert_eq!(p.history_id, "884213");
    assert_eq!(t.call_count(), 2);
}

#[tokio::test]
async fn retry_after_header_wins_over_computed_backoff() {
    let mut resp = HttpResponse::json(429, fixture("rate_limit_429.json"));
    resp = resp.with_header("Retry-After", "7");
    let t = FakeTransport::always(resp);
    let sleeper = RecordingSleeper::new();
    let client = gmail(t)
        .with_retry_policy(RetryPolicy {
            max_retries: 1,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
            jitter: 0.0,
        })
        .with_sleeper(sleeper.clone());

    let _ = client.get_profile("me").await.unwrap_err();
    assert_eq!(sleeper.slept(), vec![Duration::from_secs(7)]);
}

#[tokio::test]
async fn a_400_is_not_retried() {
    let t = FakeTransport::always(HttpResponse::json(
        400,
        r#"{"error":{"code":400,"message":"Invalid query","errors":[{"reason":"invalidArgument"}]}}"#,
    ));
    let sleeper = RecordingSleeper::new();
    let client = gmail(t.clone())
        .with_retry_policy(RetryPolicy::default())
        .with_sleeper(sleeper.clone());

    let err = client.get_profile("me").await.unwrap_err();
    assert!(matches!(err, GoogleError::Api { status: 400, .. }), "got {err:?}");
    assert_eq!(t.call_count(), 1);
    assert!(sleeper.slept().is_empty());
}

// ========================================================= Calendar endpoints

#[tokio::test]
async fn calendar_list_parses() {
    let t = FakeTransport::new(vec![ok_json("calendar_list.json")]);
    let cals = calendar(t.clone()).calendar_list().await.unwrap();
    assert_eq!(cals.len(), 2);
    assert!(cals[0].primary);
    assert_eq!(cals[0].time_zone.as_deref(), Some("America/Chicago"));
    assert_eq!(cals[0].background_color.as_deref(), Some("#9fe1e7"));
    assert_eq!(cals[1].access_role.as_deref(), Some("reader"));
    assert!(!cals[1].selected);
}

#[test]
fn events_list_all_day_and_timed_events_parse() {
    let page1: EventsListResponse =
        serde_json::from_str(&fixture("events_list_page1.json")).unwrap();
    assert_eq!(page1.items.len(), 3);
    assert_eq!(page1.next_page_token.as_deref(), Some("ev-page-2"));
    assert_eq!(page1.time_zone.as_deref(), Some("America/Chicago"));

    // recurring instance produced by singleEvents=true
    let instance = &page1.items[0];
    assert_eq!(instance.id.as_deref(), Some("7q8r9s0tuvwxyz_20260810T140000Z"));
    assert_eq!(instance.recurring_event_id.as_deref(), Some("7q8r9s0tuvwxyz"));
    assert!(instance.is_instance());
    assert!(instance.recurrence.is_empty(), "instances carry no RRULE");
    let start = instance.start.as_ref().unwrap();
    assert!(!start.is_all_day());
    let dt = start.as_datetime().expect("dateTime parses");
    assert_eq!(dt.to_rfc3339(), "2026-08-10T09:00:00-05:00");
    assert_eq!(dt.offset().local_minus_utc(), -5 * 3600, "offset preserved");
    assert_eq!(start.time_zone.as_deref(), Some("America/Chicago"));
    assert_eq!(instance.attendees.len(), 3);
    assert_eq!(instance.attendees[1].email.as_deref(), Some("dana@northwind.example"));
    assert_eq!(instance.attendees[1].response_status.as_deref(), Some("needsAction"));
    assert!(instance.attendees[0].is_self);
    assert_eq!(
        instance.hangout_link.as_deref(),
        Some("https://meet.google.com/abc-defg-hij")
    );

    // all-day event: `date`, not `dateTime`
    let allday = &page1.items[1];
    let s = allday.start.as_ref().unwrap();
    let e = allday.end.as_ref().unwrap();
    assert!(s.is_all_day());
    assert!(s.as_datetime().is_none());
    assert_eq!(s.as_date().unwrap().to_string(), "2026-08-12");
    assert_eq!(e.as_date().unwrap().to_string(), "2026-08-15");
    assert!(allday.attendees.is_empty());

    // cancelled instance of the recurring series
    let cancelled = &page1.items[2];
    assert_eq!(cancelled.status.as_deref(), Some("cancelled"));
    assert!(cancelled.is_cancelled());
    assert!(cancelled.summary.is_none());

    // page 2: a different timezone must survive intact
    let page2: EventsListResponse =
        serde_json::from_str(&fixture("events_list_page2.json")).unwrap();
    assert_eq!(page2.next_sync_token.as_deref(), Some("CIjJ4-2Ig4oDEAAYASD_"));
    let berlin = &page2.items[0];
    let bdt = berlin.start.as_ref().unwrap().as_datetime().unwrap();
    assert_eq!(bdt.offset().local_minus_utc(), 2 * 3600);
    assert_eq!(bdt.to_rfc3339(), "2026-08-13T15:00:00+02:00");
    assert_eq!(
        berlin.start.as_ref().unwrap().time_zone.as_deref(),
        Some("Europe/Berlin")
    );
}

#[tokio::test]
async fn events_list_sends_single_events_and_the_time_window() {
    let t = FakeTransport::new(vec![ok_json("events_list_page1.json")]);
    let client = calendar(t.clone());

    let page = client
        .events_list_page(
            "primary",
            &EventsListQuery::new()
                .single_events(true)
                .time_min("2026-08-01T00:00:00Z")
                .time_max("2026-09-01T00:00:00Z")
                .order_by("startTime")
                .max_results(250)
                .show_deleted(true),
            None,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);

    let url = &t.requests()[0].url;
    assert!(url.starts_with("https://calendar.test/calendar/v3/calendars/primary/events"), "url {url}");
    assert!(url.contains("singleEvents=true"), "url {url}");
    assert!(url.contains("timeMin=2026-08-01T00%3A00%3A00Z"), "url {url}");
    assert!(url.contains("timeMax=2026-09-01T00%3A00%3A00Z"), "url {url}");
    assert!(url.contains("orderBy=startTime"), "url {url}");
    assert!(url.contains("showDeleted=true"), "url {url}");
}

#[tokio::test]
async fn events_list_all_pages_returns_items_and_the_next_sync_token() {
    let t = FakeTransport::new(vec![
        ok_json("events_list_page1.json"),
        ok_json("events_list_page2.json"),
    ]);
    let client = calendar(t.clone());

    let sweep = client
        .events_list_all("primary", &EventsListQuery::new().single_events(true))
        .await
        .unwrap();

    assert_eq!(sweep.events.len(), 4);
    assert_eq!(sweep.next_sync_token.as_deref(), Some("CIjJ4-2Ig4oDEAAYASD_"));
    assert!(t.requests()[1].url.contains("pageToken=ev-page-2"));
}

#[tokio::test]
async fn events_list_with_a_sync_token_omits_the_time_window() {
    let t = FakeTransport::new(vec![ok_json("events_list_page2.json")]);
    let client = calendar(t.clone());

    client
        .events_list_page("primary", &EventsListQuery::new().sync_token("CIjJ4-2I"), None)
        .await
        .unwrap();

    let url = &t.requests()[0].url;
    assert!(url.contains("syncToken=CIjJ4-2I"), "url {url}");
    assert!(!url.contains("timeMin"), "url {url}");
}

#[tokio::test]
async fn an_expired_sync_token_410_maps_to_sync_token_expired() {
    let t = FakeTransport::new(vec![err_json(410, "sync_token_gone_410.json")]);
    let client = calendar(t);

    let err = client
        .events_list_page("primary", &EventsListQuery::new().sync_token("stale"), None)
        .await
        .unwrap_err();

    assert!(
        matches!(err, GoogleError::SyncTokenExpired { .. }),
        "expected SyncTokenExpired, got {err:?}"
    );
    assert!(err.requires_full_resync());
}

#[tokio::test]
async fn events_insert_posts_the_event_body() {
    let t = FakeTransport::new(vec![ok_json("event_single.json")]);
    let client = calendar(t.clone());

    let mut draft = Event::default();
    draft.summary = Some("Coffee with Tawny".into());
    draft.start = Some(mach_lib::google::types::EventDateTime::date_time(
        "2026-08-20T10:00:00-05:00",
        Some("America/Chicago"),
    ));
    draft.end = Some(mach_lib::google::types::EventDateTime::date_time(
        "2026-08-20T10:30:00-05:00",
        Some("America/Chicago"),
    ));

    client.events_insert("primary", &draft).await.unwrap();

    let req = &t.requests()[0];
    assert_eq!(req.method, HttpMethod::Post);
    assert!(req.url.ends_with("/calendars/primary/events"), "{}", req.url);
    let body: serde_json::Value = serde_json::from_slice(req.body.as_ref().unwrap()).unwrap();
    assert_eq!(body["summary"], "Coffee with Tawny");
    assert_eq!(body["start"]["dateTime"], "2026-08-20T10:00:00-05:00");
    assert_eq!(body["start"]["timeZone"], "America/Chicago");
    assert!(
        body.get("id").is_none(),
        "unset fields must be omitted, not sent as null: {body}"
    );
}

#[tokio::test]
async fn events_patch_sends_only_the_supplied_fields() {
    let t = FakeTransport::new(vec![ok_json("event_single.json")]);
    let client = calendar(t.clone());

    client
        .events_patch(
            "primary",
            "berlin_board_call",
            &serde_json::json!({ "summary": "Board call (moved)" }),
        )
        .await
        .unwrap();

    let req = &t.requests()[0];
    assert_eq!(req.method, HttpMethod::Patch);
    assert!(
        req.url.ends_with("/calendars/primary/events/berlin_board_call"),
        "{}",
        req.url
    );
    let body: serde_json::Value = serde_json::from_slice(req.body.as_ref().unwrap()).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 1);
}

#[tokio::test]
async fn events_delete_issues_a_delete_and_tolerates_204() {
    let t = FakeTransport::new(vec![Ok(HttpResponse::new(204, Vec::new()))]);
    let client = calendar(t.clone());

    client
        .events_delete("primary", "berlin_board_call")
        .await
        .expect("204 with an empty body must not be a deserialize error");

    let req = &t.requests()[0];
    assert_eq!(req.method, HttpMethod::Delete);
    assert!(req.url.ends_with("/calendars/primary/events/berlin_board_call"));
}

#[tokio::test]
async fn rsvp_patches_the_full_attendee_list_with_only_our_status_changed() {
    // get, then patch
    let t = FakeTransport::new(vec![
        ok_json("event_single.json"),
        ok_json("event_single.json"),
    ]);
    let client = calendar(t.clone());

    client
        .events_rsvp(
            "primary",
            "berlin_board_call",
            "alex@example.com",
            ResponseStatus::Accepted,
        )
        .await
        .expect("rsvp");

    let reqs = t.requests();
    assert_eq!(reqs.len(), 2, "read-modify-write");
    assert_eq!(reqs[0].method, HttpMethod::Get);
    assert_eq!(reqs[1].method, HttpMethod::Patch);

    let body: serde_json::Value = serde_json::from_slice(reqs[1].body.as_ref().unwrap()).unwrap();
    let attendees = body["attendees"].as_array().expect("full attendee list");
    assert_eq!(attendees.len(), 2, "Google replaces the whole array");
    assert_eq!(attendees[0]["responseStatus"], "accepted");
    assert_eq!(attendees[1]["email"], "alex@example.com");
    assert_eq!(attendees[1]["responseStatus"], "accepted");
    assert!(
        body.get("summary").is_none(),
        "rsvp must not clobber other fields: {body}"
    );
}

#[tokio::test]
async fn rsvp_for_an_unknown_attendee_is_an_error_not_a_silent_no_op() {
    let t = FakeTransport::new(vec![ok_json("event_single.json")]);
    let client = calendar(t.clone());

    let err = client
        .events_rsvp(
            "primary",
            "berlin_board_call",
            "stranger@example.com",
            ResponseStatus::Declined,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, GoogleError::InvalidRequest { .. }), "got {err:?}");
    assert_eq!(t.call_count(), 1, "must not PATCH when there is nothing to change");
}
