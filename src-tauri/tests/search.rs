//! The operator search — the SQL half.
//!
//! The parser lives in TypeScript (`src/lib/search-query.ts`, tested there);
//! what these tests own is everything downstream of the AST: that each operator
//! compiles to a predicate that finds the right threads and nothing else, that
//! the boolean structure means what it says, and — the one that matters most —
//! that no term a user can type can escape into the FTS5 or SQL grammar.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mach_lib::db::models::*;
use mach_lib::db::queries::{
    self as q, compile_search, fts_escape, DateBound, SearchField, SearchFlag, SearchNode,
    SearchRequest,
};
use mach_lib::db::Db;

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
            "mach-search-test-{}-{}-{}.sqlite3",
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

/// One message, and the thread around it, described in the terms the operators
/// address. Everything a test needs to say is a field here.
struct Mail<'a> {
    gmail_id: &'a str,
    subject: &'a str,
    body: &'a str,
    from_name: &'a str,
    from_email: &'a str,
    to: &'a [&'a str],
    cc: &'a [&'a str],
    labels: &'a [&'a str],
    unread: bool,
    at: i64,
    attachment: Option<&'a str>,
}

impl<'a> Default for Mail<'a> {
    fn default() -> Self {
        Mail {
            gmail_id: "m1",
            subject: "Hello",
            body: "body",
            from_name: "Tawny Steller",
            from_email: "tawny@example.com",
            to: &["alex@example.com"],
            cc: &[],
            labels: &["INBOX"],
            unread: false,
            at: 1_000,
            attachment: None,
        }
    }
}

fn people(addresses: &[&str]) -> Vec<Participant> {
    addresses
        .iter()
        .map(|email| Participant {
            name: None,
            email: (*email).to_string(),
        })
        .collect()
}

/// Writes a one-message thread and returns its row id.
fn seed(db: &Db, account_id: i64, mail: &Mail<'_>) -> i64 {
    let conn = db.writer();
    let thread_id = q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: mail.gmail_id.to_string(),
            participants: vec![Participant {
                name: Some(mail.from_name.to_string()),
                email: mail.from_email.to_string(),
            }],
            subject: mail.subject.to_string(),
            snippet: mail.body.chars().take(40).collect(),
            last_message_at: mail.at,
            is_unread: mail.unread,
            message_count: 1,
            has_attachments: mail.attachment.is_some(),
            label_ids: mail.labels.iter().map(|l| l.to_string()).collect(),
        },
    )
    .expect("thread");

    let message_id = q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: mail.gmail_id.to_string(),
            rfc822_message_id: Some(format!("<{}@example.com>", mail.gmail_id)),
            reply_to: Vec::new(),
            in_reply_to: None,
            references: None,
            from: Participant {
                name: Some(mail.from_name.to_string()),
                email: mail.from_email.to_string(),
            },
            to: people(mail.to),
            cc: people(mail.cc),
            bcc: vec![],
            subject: mail.subject.to_string(),
            body_html: None,
            body_text: Some(mail.body.to_string()),
            snippet: mail.body.chars().take(40).collect(),
            internal_date: mail.at,
            is_unread: mail.unread,
            is_draft: false,
        },
    )
    .expect("message");

    if let Some(filename) = mail.attachment {
        q::upsert_attachment(
            &conn,
            &NewAttachment {
                message_id,
                gmail_attachment_id: Some(format!("att-{}", mail.gmail_id)),
                filename: filename.to_string(),
                mime_type: "application/pdf".to_string(),
                size_bytes: 1,
                local_path: None,
            },
        )
        .expect("attachment");
    }

    thread_id
}

fn account(db: &Db, email: &str) -> i64 {
    let conn = db.writer();
    q::upsert_account(
        &conn,
        &NewAccount {
            email: email.to_string(),
            display_name: Some(email.to_string()),
            token_ref: format!("keychain:{email}"),
            colour_index: 1,
        },
    )
    .expect("account")
}

/// Runs a query and returns the subjects it found, newest first.
fn subjects(db: &Db, node: &SearchNode) -> Vec<String> {
    let conn = db.reader();
    q::search_threads_filtered(&conn, node, &SearchRequest::default())
        .expect("search")
        .into_iter()
        .map(|t| t.subject)
        .collect()
}

fn text(value: &str) -> SearchNode {
    SearchNode::Text {
        value: value.to_string(),
        prefix: false,
    }
}

fn field(field: SearchField, value: &str) -> SearchNode {
    SearchNode::Field {
        field,
        value: value.to_string(),
        prefix: false,
    }
}

/// A small mailbox with one of everything the operators can ask about.
fn corpus() -> (TempDb, i64) {
    let t = TempDb::new("corpus");
    let account_id = account(&t, "alex@example.com");

    seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "t1",
            subject: "Invoice for February",
            body: "the velocipede has shipped",
            from_name: "Billing",
            from_email: "billing@stripe.com",
            to: &["alex@example.com"],
            labels: &["INBOX", "Label_7"],
            unread: true,
            at: 3_000,
            attachment: Some("invoice-feb.pdf"),
            ..Default::default()
        },
    );
    seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "t2",
            subject: "Lunch on Thursday",
            body: "meet at the dirigible hangar",
            from_name: "Tawny Steller",
            from_email: "tawny@example.com",
            to: &["alex@example.com"],
            cc: &["dana@example.com"],
            labels: &["INBOX", "STARRED"],
            unread: false,
            at: 2_000,
            ..Default::default()
        },
    );
    seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "t3",
            subject: "Receipt",
            body: "thank you for your order",
            from_name: "Shop",
            from_email: "orders@globex.example",
            to: &["dana@example.com"],
            labels: &["TRASH"],
            unread: true,
            at: 1_000,
            ..Default::default()
        },
    );

    // A user label, so `label:` by name has something to resolve.
    {
        let conn = t.writer();
        q::upsert_label(
            &conn,
            &NewLabel {
                account_id,
                gmail_label_id: "Label_7".to_string(),
                name: "Receipts".to_string(),
                label_type: LabelType::User,
            },
        )
        .expect("label");
    }

    (t, account_id)
}

// ---------------------------------------------------------------------------
// operators
// ---------------------------------------------------------------------------

#[test]
fn bare_terms_search_subject_and_body() {
    let (t, _) = corpus();
    assert_eq!(subjects(&t, &text("velocipede")), ["Invoice for February"]);
    assert_eq!(subjects(&t, &text("dirigible")), ["Lunch on Thursday"]);
    // Subject and body are one index as far as a bare term is concerned.
    assert_eq!(subjects(&t, &text("receipt")), ["Receipt"]);
}

#[test]
fn a_prefix_term_matches_mid_word_so_typing_finds_things() {
    let (t, _) = corpus();
    assert!(subjects(
        &t,
        &SearchNode::Text {
            value: "veloci".to_string(),
            prefix: false,
        }
    )
    .is_empty());
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Text {
                value: "veloci".to_string(),
                prefix: true,
            }
        ),
        ["Invoice for February"]
    );
}

#[test]
fn from_matches_address_and_display_name() {
    let (t, _) = corpus();
    assert_eq!(
        subjects(&t, &field(SearchField::From, "stripe")),
        ["Invoice for February"]
    );
    assert_eq!(
        subjects(&t, &field(SearchField::From, "tawny steller")),
        ["Lunch on Thursday"]
    );
    assert!(subjects(&t, &field(SearchField::From, "nobody")).is_empty());
}

#[test]
fn from_still_finds_a_thread_whose_sender_rollup_overflowed() {
    /*
     * `threads.participants` caps at ten senders, and the `from:` predicate
     * uses it as a prefilter. On a longer thread the rollup is incomplete, so
     * the prefilter has to defer to the exact per-message check rather than
     * rule the thread out. This is that case: the rollup names nobody useful,
     * `message_count` says it might have been truncated, and the message is
     * found anyway.
     */
    let t = TempDb::new("rollup");
    let account_id = account(&t, "alex@example.com");
    let thread_id = seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "long",
            subject: "Long thread",
            from_name: "Someone Else",
            from_email: "someone@example.com",
            at: 5_000,
            ..Default::default()
        },
    );

    {
        let conn = t.writer();
        // A rollup that is full to the cap — which is what the sync loop leaves
        // behind on a thread with more than ten distinct senders, and the only
        // signal the prefilter has that it cannot be trusted.
        let full: Vec<Participant> = (0..10)
            .map(|n| Participant {
                name: Some(format!("Sender {n}")),
                email: format!("sender{n}@example.com"),
            })
            .collect();
        conn.execute(
            "UPDATE threads SET message_count = 40, participants = ?2 WHERE id = ?1",
            rusqlite::params![thread_id, serde_json::to_string(&full).unwrap()],
        )
        .expect("lengthen");
        q::upsert_message(
            &conn,
            &NewMessage {
                thread_id,
                account_id,
                gmail_message_id: "long-11".to_string(),
                rfc822_message_id: None,
                reply_to: Vec::new(),
                in_reply_to: None,
                references: None,
                from: Participant {
                    name: Some("Eleventh Sender".into()),
                    email: "eleventh@latecomer.example".into(),
                },
                to: vec![],
                cc: vec![],
                bcc: vec![],
                subject: "Long thread".to_string(),
                body_html: None,
                body_text: Some("still going".to_string()),
                snippet: "still going".to_string(),
                internal_date: 5_001,
                is_unread: false,
                is_draft: false,
            },
        )
        .expect("late message");
    }

    assert_eq!(
        subjects(&t, &field(SearchField::From, "latecomer.example")),
        ["Long thread"],
        "a sender past the participants cap must still be findable"
    );
}

#[test]
fn to_and_cc_address_the_right_header() {
    let (t, _) = corpus();
    assert_eq!(
        subjects(&t, &field(SearchField::To, "dana@example.com")),
        ["Receipt"]
    );
    assert_eq!(
        subjects(&t, &field(SearchField::Cc, "dana@example.com")),
        ["Lunch on Thursday"]
    );
    assert!(subjects(&t, &field(SearchField::Bcc, "dana@example.com")).is_empty());
}

#[test]
fn subject_looks_only_at_the_subject() {
    let (t, _) = corpus();
    assert_eq!(
        subjects(&t, &field(SearchField::Subject, "invoice")),
        ["Invoice for February"]
    );
    // "velocipede" is in the body of that same thread, so a bare term finds it
    // and `subject:` must not.
    assert!(subjects(&t, &field(SearchField::Subject, "velocipede")).is_empty());
}

#[test]
fn labels_answer_to_both_their_gmail_id_and_their_name() {
    let (t, _) = corpus();
    assert_eq!(
        subjects(&t, &field(SearchField::Label, "TRASH")),
        ["Receipt"]
    );
    // `in:inbox` produces the id; `label:receipts` produces the name a user
    // typed, case and all.
    assert_eq!(subjects(&t, &field(SearchField::Label, "inbox")).len(), 2);
    assert_eq!(
        subjects(&t, &field(SearchField::Label, "receipts")),
        ["Invoice for February"]
    );
}

#[test]
fn flags_read_the_thread_row() {
    let (t, _) = corpus();
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Flag {
                flag: SearchFlag::Unread
            }
        ),
        ["Invoice for February", "Receipt"]
    );
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Flag {
                flag: SearchFlag::Read
            }
        ),
        ["Lunch on Thursday"]
    );
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Flag {
                flag: SearchFlag::Starred
            }
        ),
        ["Lunch on Thursday"]
    );
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Flag {
                flag: SearchFlag::Attachment
            }
        ),
        ["Invoice for February"]
    );
}

#[test]
fn filename_finds_the_thread_that_carries_the_file() {
    let (t, _) = corpus();
    assert_eq!(
        subjects(&t, &field(SearchField::Filename, "pdf")),
        ["Invoice for February"]
    );
    assert_eq!(
        subjects(&t, &field(SearchField::Filename, "invoice-feb.pdf")),
        ["Invoice for February"]
    );
    assert!(subjects(&t, &field(SearchField::Filename, "xlsx")).is_empty());
}

#[test]
fn dates_bound_the_thread_by_its_most_recent_message() {
    let (t, _) = corpus();
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Date {
                bound: DateBound::After,
                ts: 2_000
            }
        ),
        ["Invoice for February", "Lunch on Thursday"]
    );
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Date {
                bound: DateBound::Before,
                ts: 2_000
            }
        ),
        ["Receipt"]
    );
}

// ---------------------------------------------------------------------------
// structure
// ---------------------------------------------------------------------------

#[test]
fn and_or_and_not_compose() {
    let (t, _) = corpus();

    assert_eq!(
        subjects(
            &t,
            &SearchNode::And {
                nodes: vec![
                    field(SearchField::From, "stripe"),
                    SearchNode::Flag {
                        flag: SearchFlag::Unread
                    },
                ],
            }
        ),
        ["Invoice for February"]
    );

    assert_eq!(
        subjects(
            &t,
            &SearchNode::Or {
                nodes: vec![text("velocipede"), text("dirigible")],
            }
        ),
        ["Invoice for February", "Lunch on Thursday"]
    );

    assert_eq!(
        subjects(
            &t,
            &SearchNode::And {
                nodes: vec![
                    field(SearchField::Label, "INBOX"),
                    SearchNode::Not {
                        node: Box::new(field(SearchField::From, "stripe")),
                    },
                ],
            }
        ),
        ["Lunch on Thursday"]
    );
}

#[test]
fn in_anywhere_is_the_identity_not_a_filter() {
    let (t, _) = corpus();
    assert_eq!(subjects(&t, &SearchNode::All).len(), 3);
    assert_eq!(
        subjects(
            &t,
            &SearchNode::Or {
                nodes: vec![text("velocipede"), SearchNode::All],
            }
        )
        .len(),
        3
    );
}

#[test]
fn a_term_the_tokenizer_cannot_index_matches_nothing_rather_than_everything() {
    let (t, _) = corpus();
    // `£££` has no indexable token in it. Dropping the term would hand back the
    // whole mailbox, which is the wrong answer to a query with a word in it.
    assert!(subjects(&t, &text("£££")).is_empty());
    assert!(subjects(
        &t,
        &SearchNode::And {
            nodes: vec![text("velocipede"), text("£££")],
        }
    )
    .is_empty());
}

#[test]
fn a_pathologically_deep_tree_is_bounded_rather_than_a_stack_overflow() {
    let (t, _) = corpus();
    let mut node = text("velocipede");
    for _ in 0..500 {
        node = SearchNode::And { nodes: vec![node] };
    }
    // Past the depth cap the rest of the tree stops narrowing, so the answer
    // widens rather than the process dying.
    assert!(!subjects(&t, &node).is_empty());
}

#[test]
fn results_are_newest_first_and_paginate_by_cursor() {
    let (t, _) = corpus();
    let conn = t.reader();

    let first = q::search_threads_filtered(
        &conn,
        &SearchNode::All,
        &SearchRequest {
            limit: 2,
            ..Default::default()
        },
    )
    .expect("page 1");
    assert_eq!(
        first.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(),
        ["Invoice for February", "Lunch on Thursday"]
    );

    let cursor = first.last().map(ThreadSummary::cursor);
    let second = q::search_threads_filtered(
        &conn,
        &SearchNode::All,
        &SearchRequest {
            limit: 2,
            after: cursor,
            ..Default::default()
        },
    )
    .expect("page 2");
    assert_eq!(
        second.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(),
        ["Receipt"]
    );
}

#[test]
fn a_search_can_be_scoped_to_one_account() {
    let t = TempDb::new("accounts");
    let a = account(&t, "a@example.com");
    let b = account(&t, "b@example.com");
    seed(
        &t,
        a,
        &Mail {
            gmail_id: "ta",
            subject: "Alpha",
            body: "velocipede",
            ..Default::default()
        },
    );
    seed(
        &t,
        b,
        &Mail {
            gmail_id: "tb",
            subject: "Beta",
            body: "velocipede",
            ..Default::default()
        },
    );

    let conn = t.reader();
    let scoped = q::search_threads_filtered(
        &conn,
        &text("velocipede"),
        &SearchRequest {
            account_id: Some(b),
            ..Default::default()
        },
    )
    .expect("scoped");
    assert_eq!(
        scoped.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(),
        ["Beta"]
    );
}

// ---------------------------------------------------------------------------
// injection
// ---------------------------------------------------------------------------

#[test]
fn fts_escaping_neutralises_every_operator_fts5_has() {
    // Each of these means something to FTS5 and nothing to the user. After
    // escaping they are one quoted literal, so the grammar cannot see them.
    for raw in [
        "*",
        "NEAR",
        "AND",
        "OR",
        "NOT",
        "a OR b",
        "a NEAR(b c)",
        "^start",
        "col:value",
        "-minus",
        "{subject}",
        "a\"b",
        "\"",
        "\"\"",
        "\" OR messages_fts MATCH \"",
    ] {
        let Some(expr) = fts_escape(raw, false) else {
            continue;
        };
        assert!(expr.starts_with('"') && expr.ends_with('"'), "{raw} -> {expr}");
        // Every quote inside is doubled, so the literal cannot be closed early:
        // strip the wrapper and no odd run of quotes can remain.
        let inner = &expr[1..expr.len() - 1];
        for run in inner.split(|c| c != '"').filter(|s| !s.is_empty()) {
            assert_eq!(run.len() % 2, 0, "unbalanced quote run in {expr}");
        }
    }
}

#[test]
fn a_malicious_query_cannot_break_out_of_the_fts_expression() {
    /*
     * The attack this is written against: get the search box to end the quoted
     * term and start a new FTS5 expression, or a new SQL statement. If either
     * worked the query would either error (a visible break) or return rows it
     * was never asked for (a silent one), so both are asserted.
     */
    let t = TempDb::new("injection");
    let account_id = account(&t, "alex@example.com");
    seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "secret",
            subject: "Salary review",
            body: "confidential compensation details",
            ..Default::default()
        },
    );
    seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "plain",
            subject: "Lunch",
            body: "sandwiches",
            at: 2_000,
            ..Default::default()
        },
    );

    /*
     * Every one of these is read as a *literal*, so the only thing that can
     * match is a thread whose text really contains the whole phrase. That is
     * the difference the escaping makes, and it is what each expectation below
     * pins: `NEAR(salary lunch, 10)` must find nothing, because no message says
     * that; `salary"*` may find the salary thread, because after the tokenizer
     * drops the punctuation the user did type the word salary.
     */
    let hostile: &[(&str, bool)] = &[
        ("\" OR messages_fts MATCH \"salary", false),
        ("\" OR \"a\" OR \"", false),
        ("salary\"*", true),
        ("*", false),
        ("NEAR(salary lunch, 10)", false),
        ("^salary", true),
        ("subject : salary", false),
        ("'; DROP TABLE messages; --", false),
        ("') OR 1=1 --", false),
        ("%", false),
        ("_", false),
        ("\\", false),
        ("'||(SELECT body_text FROM messages)||'", false),
        ("salary OR 1=1", false),
    ];

    for (raw, text_may_match) in hostile.iter().copied() {
        let conn = t.reader();
        for (label, node) in [
            ("text", text(raw)),
            ("subject", field(SearchField::Subject, raw)),
            ("from", field(SearchField::From, raw)),
            ("to", field(SearchField::To, raw)),
            ("label", field(SearchField::Label, raw)),
            ("filename", field(SearchField::Filename, raw)),
        ] {
            let found = q::search_threads_filtered(&conn, &node, &SearchRequest::default())
                .unwrap_or_else(|e| panic!("{label} query {raw:?} must not error: {e}"));
            // Only the full-text and subject paths read the phrase at all; the
            // address, label and filename paths are substring matches and none
            // of these strings is a substring of anything in the corpus.
            let allowed = text_may_match && (label == "text" || label == "subject");
            assert!(
                allowed || !found.iter().any(|t| t.subject == "Salary review"),
                "{label} query {raw:?} reached a thread it did not name"
            );
            assert!(
                !found.iter().any(|t| t.subject == "Lunch"),
                "{label} query {raw:?} was read as an operator, not as text"
            );
        }
    }

    // And the table is still there — a `DROP` that took effect would show up
    // here rather than in the assertions above.
    let conn = t.reader();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .expect("messages table survives");
    assert_eq!(n, 2);
}

#[test]
fn every_user_value_is_bound_never_interpolated() {
    // The compiled SQL is asserted directly: a value that appears in the text
    // of the statement is a value that got there by string concatenation.
    let (sql, args) = compile_search(&SearchNode::And {
        nodes: vec![
            text("'; DROP TABLE messages; --"),
            field(SearchField::From, "bob' OR '1'='1"),
            field(SearchField::To, "%_%"),
            field(SearchField::Label, "Receipts"),
            SearchNode::Date {
                bound: DateBound::After,
                ts: 1234,
            },
        ],
    });

    assert!(!sql.contains("DROP"), "{sql}");
    assert!(!sql.contains("bob"), "{sql}");
    assert!(!sql.contains("Receipts"), "{sql}");
    assert!(!sql.contains("1234"), "{sql}");
    assert!(!sql.contains(';'), "a compiled predicate is one expression: {sql}");
    assert!(args.len() >= 5, "every value is bound: {args:?}");
}

#[test]
fn like_wildcards_typed_by_a_user_are_literal() {
    let t = TempDb::new("wildcards");
    let account_id = account(&t, "alex@example.com");
    seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "pct",
            subject: "Discount",
            from_name: "Sale 100%off",
            from_email: "sale@example.com",
            ..Default::default()
        },
    );
    seed(
        &t,
        account_id,
        &Mail {
            gmail_id: "other",
            subject: "Nothing to do with it",
            from_name: "Someone",
            from_email: "someone@example.com",
            at: 2_000,
            ..Default::default()
        },
    );

    // `%` is a wildcard in LIKE. Typed into the box it is a percent sign, so it
    // must find the sender that has one and *only* that one — unescaped it
    // would match every sender in the mailbox.
    assert_eq!(
        subjects(&t, &field(SearchField::From, "100%off")),
        ["Discount"]
    );
    assert_eq!(subjects(&t, &field(SearchField::From, "%")), ["Discount"]);
    assert!(subjects(&t, &field(SearchField::From, "_")).is_empty());
}

// ---------------------------------------------------------------------------
// the seam
// ---------------------------------------------------------------------------

#[test]
fn the_ast_deserializes_from_exactly_what_typescript_sends() {
    // Pinned against `src/lib/search-query.ts`: same tag, same field names,
    // same spellings. A rename on either side breaks here rather than silently
    // returning the whole mailbox.
    let json = r#"{
        "type": "and",
        "nodes": [
            { "type": "text", "value": "invoice", "prefix": true },
            { "type": "field", "field": "from", "value": "stripe" },
            { "type": "field", "field": "subject", "value": "feb", "prefix": false },
            { "type": "flag", "flag": "unread" },
            { "type": "not", "node": { "type": "flag", "flag": "starred" } },
            { "type": "date", "bound": "after", "ts": 1700000000000 },
            { "type": "or", "nodes": [{ "type": "all" }] }
        ]
    }"#;

    let node: SearchNode = serde_json::from_str(json).expect("the wire shape must parse");
    let SearchNode::And { nodes } = node else {
        panic!("expected an and");
    };
    assert_eq!(nodes.len(), 7);
    assert_eq!(
        nodes[0],
        SearchNode::Text {
            value: "invoice".to_string(),
            prefix: true
        }
    );
    assert_eq!(
        nodes[1],
        SearchNode::Field {
            field: SearchField::From,
            value: "stripe".to_string(),
            prefix: false
        }
    );
    assert_eq!(
        nodes[5],
        SearchNode::Date {
            bound: DateBound::After,
            ts: 1_700_000_000_000
        }
    );
}

// ---------------------------------------------------------------------------
// latency, against a real mailbox
// ---------------------------------------------------------------------------

/// Times the compiled queries against a real store.
///
/// Ignored by default because it needs a database this repository does not
/// carry. Point it at a *copy* — `MACH_QA_INSTANCE=x scripts/qa seed` makes
/// one — and run:
///
/// ```text
/// MACH_SEARCH_BENCH_DB=.qa/x/data/mach.sqlite3 \
///   cargo test --test search -- --ignored --nocapture
/// ```
///
/// The connection is opened read-only, so it cannot migrate or write the copy
/// even by accident.
#[test]
#[ignore]
fn bench_against_a_real_store() {
    let Ok(path) = std::env::var("MACH_SEARCH_BENCH_DB") else {
        eprintln!("set MACH_SEARCH_BENCH_DB to a copy of a real store");
        return;
    };
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open the bench database read-only");
    conn.execute_batch("PRAGMA cache_size = -16000; PRAGMA temp_store = MEMORY;")
        .expect("pragmas");

    let messages: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    let threads: i64 = conn
        .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
        .unwrap();
    println!("\n{threads} threads / {messages} messages — {path}\n");

    let cases: Vec<(&str, SearchNode)> = vec![
        ("invoice", text("invoice")),
        (
            "invoice (prefix)",
            SearchNode::Text {
                value: "invoi".into(),
                prefix: true,
            },
        ),
        ("the  (very common word)", text("the")),
        ("subject:invoice", field(SearchField::Subject, "invoice")),
        ("from:stripe", field(SearchField::From, "stripe")),
        (
            "from:nobodyatall (no hits)",
            field(SearchField::From, "nobodyatallzzz"),
        ),
        (
            "invoice from:stripe",
            SearchNode::And {
                nodes: vec![text("invoice"), field(SearchField::From, "stripe")],
            },
        ),
        (
            "is:unread in:inbox",
            SearchNode::And {
                nodes: vec![
                    SearchNode::Flag {
                        flag: SearchFlag::Unread,
                    },
                    field(SearchField::Label, "INBOX"),
                ],
            },
        ),
        ("has:attachment filename:pdf", SearchNode::And {
            nodes: vec![
                SearchNode::Flag {
                    flag: SearchFlag::Attachment,
                },
                field(SearchField::Filename, "pdf"),
            ],
        }),
        (
            "invoice OR receipt -is:read",
            SearchNode::And {
                nodes: vec![
                    SearchNode::Or {
                        nodes: vec![text("invoice"), text("receipt")],
                    },
                    SearchNode::Not {
                        node: Box::new(SearchNode::Flag {
                            flag: SearchFlag::Read,
                        }),
                    },
                ],
            },
        ),
        ("to:<an address>", field(SearchField::To, "@")),
        (
            "to:<no such address> (worst case, unindexed)",
            field(SearchField::To, "zzznosuchaddresszzz"),
        ),
    ];

    for (label, node) in cases {
        let started = std::time::Instant::now();
        let rows = q::search_threads_filtered(
            &conn,
            &node,
            &SearchRequest {
                limit: 50,
                ..Default::default()
            },
        )
        .expect("search");
        println!(
            "{:>8.1} ms  {:>4} rows   {label}",
            started.elapsed().as_secs_f64() * 1000.0,
            rows.len()
        );
    }
    println!();
}
