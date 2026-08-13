//! Carrying out an unsubscribe, once something has decided it may happen.
//!
//! Nothing here decides anything. It takes a [`Target`] that
//! [`super::rule`] has already approved and does the one thing that target
//! means, then says truthfully whether it worked.
//!
//! # Neither path involves the webview
//!
//! The `POST` is made from Rust against [`super::http`]'s own client, so no page
//! is parsed, no script runs, no cookie jar is consulted and no referrer is
//! sent. The `mailto:` goes out through the Gmail client the rest of the app
//! already uses. The URL never crosses into the frontend at all — the only
//! thing the UI is told is which of the three kinds it is.
//!
//! # What counts as success, and what this cannot know
//!
//! For one-click, a `2xx`. RFC 8058 defines the interaction as "POST this
//! body, get a success status, you are unsubscribed", so a `2xx` is the
//! sender's own word for it and there is nothing further to check. A `2xx`
//! whose body says "click here to confirm" is a sender not implementing the
//! standard they advertised; that is indistinguishable from success here, and
//! it is named in the report rather than guessed at.
//!
//! For `mailto:`, success means Gmail accepted the message for delivery. What
//! the list processor at the other end does with it is not observable, and no
//! mail client has ever been able to observe it.
//!
//! Everything else — a `4xx`, a `5xx`, a timeout, a redirect chain that walked
//! somewhere we will not follow — is a failure and says so. That is the
//! standing rule of this project and the reason the failure type carries a
//! sentence rather than a bool.

use crate::ipc::compose::engine::mime::{self, Mailbox, Outgoing};
use crate::google::gmail::GmailClient;
use crate::google::{HttpMethod, HttpRequest, HttpTransport};
use crate::unsub::target::{Target, ONE_CLICK_BODY, ONE_CLICK_CONTENT_TYPE};

/// Why an unsubscribe did not happen.
///
/// Never carries the URL or anything out of the response body. The sentence
/// goes on screen and may reach a log; neither is a place for a token that
/// identifies him to the sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub message: String,
    /// Whether dispatching the same unsubscribe again could plausibly work.
    /// A `5xx` or a timeout, yes; a `404`, no.
    pub retriable: bool,
}

impl Refused {
    fn new(message: impl Into<String>, retriable: bool) -> Self {
        Refused {
            message: message.into(),
            retriable,
        }
    }
}

/// `POST List-Unsubscribe=One-Click`, exactly as RFC 8058 specifies it.
///
/// Two headers and nothing else. No `Authorization`, no `Cookie`, no
/// `Referer` — the client refuses to add the last one and the other two are
/// simply never built.
pub async fn one_click(http: &dyn HttpTransport, url: &str) -> Result<(), Refused> {
    let request = HttpRequest {
        method: HttpMethod::Post,
        url: url.to_string(),
        headers: vec![
            ("content-type".into(), ONE_CLICK_CONTENT_TYPE.into()),
            // Some list processors answer `text/html` to anything and a few
            // answer JSON. Mach reads neither; this only stops the handful
            // that refuse a request with no `Accept` at all.
            ("accept".into(), "*/*".into()),
        ],
        body: Some(ONE_CLICK_BODY.as_bytes().to_vec()),
    };

    let response = http
        .execute(request)
        .await
        .map_err(|e| Refused::new(e.message.clone(), true))?;

    if response.is_success() {
        return Ok(());
    }
    Err(refusal_for(response.status))
}

/// Send the message a `mailto:` unsubscribe asks for.
///
/// It goes through `users.messages.send` rather than the outbox, on purpose.
/// The outbox exists to hold a message for ten seconds so ⌘Z can take it back
/// and to mirror a draft into the conversation it answers; an unsubscribe wants
/// neither. He has already made the decision, there is nothing to recall it
/// from once the sender has read it, and a draft row for it in the newsletter's
/// own thread would be noise.
///
/// It still lands in Gmail's Sent, which is the record that it happened.
#[allow(clippy::too_many_arguments)]
pub async fn send_mail(
    gmail: &GmailClient,
    user_id: &str,
    from: &str,
    to: &[String],
    subject: &str,
    body: Option<&str>,
    now_ms: i64,
    entropy: u64,
) -> Result<(), Refused> {
    if from.trim().is_empty() {
        return Err(Refused::new(
            "the account this newsletter arrived on has no address to send from",
            false,
        ));
    }
    if to.is_empty() {
        return Err(Refused::new("the unsubscribe address was empty", false));
    }

    let outgoing = Outgoing {
        from: Mailbox::new(from),
        to: to.iter().map(Mailbox::new).collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.to_string(),
        // Both parts are what `build_rfc822` produces for every message. A list
        // processor reads the subject; the body is here because RFC 2369 lets
        // one be specified and a few processors want a token in it.
        text: body.unwrap_or_default().to_string(),
        html: String::new(),
        attachments: Vec::new(),
        in_reply_to: None,
        references: Vec::new(),
        message_id: mime::generate_message_id(from, now_ms, entropy),
        date_ms: now_ms,
    };

    let rfc822 = mime::build_rfc822(&outgoing)
        .map_err(|e| Refused::new(format!("the unsubscribe message could not be built: {e}"), false))?;

    gmail
        .messages_send(user_id, &rfc822, None)
        .await
        .map(|_| ())
        .map_err(|e| Refused::new(format!("Gmail refused to send the unsubscribe: {e}"), true))
}

/// Whether the action described by a target can be carried out at all.
pub fn is_automatic(target: &Target) -> bool {
    target.is_automatic()
}

/// A sentence for a status code, and whether it is worth trying again.
fn refusal_for(status: u16) -> Refused {
    match status {
        401 | 403 => Refused::new(
            format!("the sender refused the unsubscribe ({status})"),
            false,
        ),
        404 | 410 => Refused::new(
            format!("the sender's unsubscribe link is gone ({status})"),
            false,
        ),
        405 => Refused::new(
            // The sender advertised one-click and then refused the POST. It is
            // their bug, and the fallback is the page.
            "the sender advertised one-click unsubscribe but refused it (405)",
            false,
        ),
        429 => Refused::new("the sender asked to be tried again later (429)", true),
        500..=599 => Refused::new(format!("the sender's server failed ({status})"), true),
        other => Refused::new(format!("the sender refused the unsubscribe ({other})"), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::{BoxFuture, HttpResponse, TransportError};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<HttpRequest>>,
        answer: Mutex<Option<Result<HttpResponse, TransportError>>>,
    }

    impl Recorder {
        fn answering(status: u16) -> Self {
            Recorder {
                seen: Mutex::new(Vec::new()),
                answer: Mutex::new(Some(Ok(HttpResponse::new(status, Vec::new())))),
            }
        }
        fn failing(message: &str) -> Self {
            Recorder {
                seen: Mutex::new(Vec::new()),
                answer: Mutex::new(Some(Err(TransportError::new(message)))),
            }
        }
        fn last(&self) -> HttpRequest {
            self.seen.lock().unwrap().last().cloned().expect("a request")
        }
    }

    impl HttpTransport for Recorder {
        fn execute<'a>(
            &'a self,
            request: HttpRequest,
        ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
            self.seen.lock().unwrap().push(request);
            let answer = self.answer.lock().unwrap().clone();
            Box::pin(async move {
                answer.unwrap_or_else(|| Ok(HttpResponse::new(200, Vec::new())))
            })
        }
    }

    #[tokio::test]
    async fn the_one_click_post_is_exactly_what_rfc_8058_specifies() {
        let http = Recorder::answering(200);
        one_click(&http, "https://example.com/u/9f2a").await.expect("ok");

        let request = http.last();
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, "https://example.com/u/9f2a");
        assert_eq!(
            request.body.as_deref(),
            Some(b"List-Unsubscribe=One-Click".as_slice()),
            "the body is the exact string the RFC names"
        );
        assert_eq!(
            request.header("content-type"),
            Some("application/x-www-form-urlencoded")
        );
    }

    #[tokio::test]
    async fn the_post_carries_no_credentials() {
        let http = Recorder::answering(200);
        one_click(&http, "https://example.com/u/9f2a").await.expect("ok");
        let request = http.last();
        for header in ["authorization", "cookie", "referer", "x-goog-api-key"] {
            assert_eq!(request.header(header), None, "{header} must not be sent");
        }
        assert_eq!(request.headers.len(), 2, "only content-type and accept");
    }

    #[tokio::test]
    async fn every_2xx_is_a_success() {
        for status in [200, 201, 202, 204] {
            let http = Recorder::answering(status);
            assert!(
                one_click(&http, "https://example.com/u").await.is_ok(),
                "{status} should succeed"
            );
        }
    }

    #[tokio::test]
    async fn a_refusal_is_reported_rather_than_swallowed() {
        for (status, retriable) in [(404, false), (403, false), (405, false), (500, true), (429, true)] {
            let http = Recorder::answering(status);
            let refused = one_click(&http, "https://example.com/u")
                .await
                .expect_err("should fail");
            assert!(
                refused.message.contains(&status.to_string())
                    || status == 405 && refused.message.contains("405"),
                "{status}: {}", refused.message
            );
            assert_eq!(refused.retriable, retriable, "{status}");
        }
    }

    #[tokio::test]
    async fn a_redirect_to_a_login_page_is_not_a_success() {
        // The client stops rather than following a hop it will not accept, so
        // the 302 itself arrives here. It is a failure.
        let http = Recorder::answering(302);
        let refused = one_click(&http, "https://example.com/u")
            .await
            .expect_err("a redirect that was not followed is not a success");
        assert!(refused.message.contains("302"));
    }

    #[tokio::test]
    async fn a_transport_failure_surfaces_as_retriable() {
        let http = Recorder::failing("the sender did not answer in time");
        let refused = one_click(&http, "https://example.com/u")
            .await
            .expect_err("should fail");
        assert_eq!(refused.message, "the sender did not answer in time");
        assert!(refused.retriable);
    }

    #[test]
    fn a_refusal_never_names_the_url() {
        for status in [400, 403, 404, 405, 410, 429, 500, 503, 599] {
            let refused = refusal_for(status);
            assert!(!refused.message.contains("http"), "{status}: {}", refused.message);
        }
    }

}
