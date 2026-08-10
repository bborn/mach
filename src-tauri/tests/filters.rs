//! Gmail filters: the command layer, the agent's tools, and the sentence the
//! owner is asked to approve.
//!
//! **No Google.** Every request goes to a scripted `HttpTransport` that records
//! what was sent and replays what to answer, exactly as `tests/google.rs` does
//! for the rest of the API. No filter is created anywhere.
//!
//! The tests that carry the design rather than the mechanics:
//!
//!  * `creating_a_filter_needs_the_owner` — the policy, and the reason the
//!    whole feature is not auto-run.
//!  * `the_approval_prompt_reads_as_a_sentence` — no JSON in front of a human,
//!    and `removeLabelIds: ["INBOX"]` said as "skips the inbox".
//!  * `a_filter_with_no_criteria_is_refused` — the shape Google accepts and
//!    nobody means.
//!  * `a_narrow_grant_reports_the_account_rather_than_the_403` — what the owner
//!    sees the first time he runs a build with the new scope in it.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use mach_lib::commands::{AccountClients, CommandDispatcher, CommandError, GoogleClients};
use mach_lib::db::models::{LabelType, NewAccount, NewLabel};
use mach_lib::db::{queries, Db};
use mach_lib::google::types::{Filter, FilterAction, FilterCriteria};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, StaticTokenProvider, TransportError,
};
use mach_lib::ipc::agent::engine::gate::ToolGate;
use mach_lib::ipc::agent::engine::session::{
    ApprovalDesk, NullEmitter, SessionEmitter, SessionSnapshot, SessionStatus, SessionUi,
};
use mach_lib::ipc::agent::engine::tools::{self, ToolContext, ToolPolicy};
use mach_lib::ipc::compose::engine::outbox::Outbox;

// ===========================================================================
// harness
// ===========================================================================

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct TempDb {
    path: std::path::PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mach-filters-test-{}-{}-{}.sqlite3",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).expect("open temp db");
        TempDb { path, db }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.path.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(path));
        }
    }
}

/// Replays one scripted response per request and records everything sent.
struct Scripted {
    responses: Mutex<std::collections::VecDeque<HttpResponse>>,
    fallback: HttpResponse,
    requests: Mutex<Vec<HttpRequest>>,
}

impl Scripted {
    fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Scripted {
            responses: Mutex::new(responses.into_iter().collect()),
            fallback: HttpResponse::json(200, "{}"),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for Scripted {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.requests.lock().unwrap().push(request);
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.fallback.clone());
        Box::pin(async move { Ok(next) })
    }
}

struct Harness {
    db: TempDb,
    google: Arc<Scripted>,
    dispatcher: Arc<CommandDispatcher>,
    outbox: Arc<Outbox>,
    plugins: Arc<mach_lib::plugins::PluginRuntime>,
    account_id: i64,
}

impl Harness {
    fn new(tag: &str, responses: Vec<HttpResponse>) -> Harness {
        let db = TempDb::new(tag);
        let google = Scripted::new(responses);
        let account_id = seed(&db.db);
        let clients: Arc<dyn GoogleClients> = Arc::new(
            AccountClients::new(Arc::clone(&google) as Arc<dyn HttpTransport>)
                .with_account(account_id, Arc::new(StaticTokenProvider::new("token"))),
        );
        let dispatcher = Arc::new(
            CommandDispatcher::new(db.db.clone(), Arc::clone(&clients)).expect("dispatcher"),
        );
        let outbox = Arc::new(Outbox::new(db.db.clone(), clients).expect("outbox"));
        let plugins = Arc::new(mach_lib::plugins::PluginRuntime::new(
            Arc::new(mach_lib::plugins::PluginStore::new(
                &std::env::temp_dir().join(format!("mach-filters-test-{}", std::process::id())),
                false,
            )),
            Vec::new(),
        ));
        Harness {
            db,
            google,
            dispatcher,
            outbox,
            plugins,
            account_id,
        }
    }

    fn tool_context(&self) -> ToolContext {
        ToolContext {
            db: self.db.db.clone(),
            dispatcher: Arc::clone(&self.dispatcher),
            outbox: Arc::clone(&self.outbox),
            plugins: Arc::clone(&self.plugins),
        }
    }

    fn gate(&self) -> ToolGate {
        let snapshot = Arc::new(Mutex::new(SessionSnapshot {
            id: "filters-test".into(),
            title: "test".into(),
            status: SessionStatus::Running,
            created_at: 0,
            context: Vec::new(),
            entries: Vec::new(),
            pending: None,
            error: None,
            backend: None,
        }));
        let ui = Arc::new(SessionUi::new(
            "filters-test",
            snapshot,
            Arc::new(NullEmitter) as Arc<dyn SessionEmitter>,
        ));
        let desk = Arc::new(ApprovalDesk::new(Arc::clone(&ui)));
        ToolGate::new(self.tool_context(), Vec::new(), ui, desk)
    }
}

/// One account, and one user label so the sentence can say "Codes" rather than
/// "Label_18".
fn seed(db: &Db) -> i64 {
    let account_id = db
        .write(|conn| {
            queries::upsert_account(
                conn,
                &NewAccount {
                    email: "alex@lumen.example".into(),
                    display_name: Some("Alex".into()),
                    token_ref: "keychain".into(),
                    colour_index: 0,
                },
            )
        })
        .expect("account");

    for (id, name, kind) in [
        ("INBOX", "Inbox", LabelType::System),
        ("Label_18", "Codes", LabelType::User),
    ] {
        db.write(|conn| {
            queries::upsert_label(
                conn,
                &NewLabel {
                    account_id,
                    gmail_label_id: id.into(),
                    name: name.into(),
                    label_type: kind,
                },
            )
        })
        .expect("label");
    }

    account_id
}

fn login_codes() -> Filter {
    Filter {
        id: String::new(),
        criteria: FilterCriteria {
            from: Some("no-reply@okta.com".into()),
            subject: Some("verification code".into()),
            ..FilterCriteria::default()
        },
        action: FilterAction {
            add_label_ids: vec!["Label_18".into()],
            remove_label_ids: vec!["INBOX".into(), "UNREAD".into()],
            ..FilterAction::default()
        },
    }
}

// ===========================================================================
// the policy
// ===========================================================================

/// The whole point of the feature. A filter acts on mail that has not arrived,
/// forever, and nothing moves when it is made — so there is no moment at which
/// the owner could notice and take it back. It cannot be auto-run.
#[test]
fn creating_a_filter_needs_the_owner() {
    assert_eq!(
        tools::policy_for(tools::CREATE_FILTER_TOOL),
        ToolPolicy::Approve
    );
    assert_eq!(
        tools::policy_for(tools::DELETE_FILTER_TOOL),
        ToolPolicy::Approve
    );
    // Reading the account's own settings changes nothing.
    assert_eq!(tools::policy_for(tools::LIST_FILTERS_TOOL), ToolPolicy::Auto);
}

#[test]
fn the_three_filter_tools_are_on_the_surface_with_schemas() {
    for name in [
        tools::LIST_FILTERS_TOOL,
        tools::CREATE_FILTER_TOOL,
        tools::DELETE_FILTER_TOOL,
    ] {
        let tool = tools::find(name).unwrap_or_else(|| panic!("{name} is not a tool"));
        assert!(!tool.definition.description.is_empty());
        assert_eq!(tool.definition.input_schema["type"], "object");
    }

    // The two the owner asked about have to be reachable by a model that has
    // only read the description.
    let create = tools::find(tools::CREATE_FILTER_TOOL).unwrap();
    let described = create.definition.description.to_ascii_lowercase();
    assert!(described.contains("inbox"), "{described}");
    assert!(described.contains("trash"), "{described}");
}

// ===========================================================================
// the approval prompt
// ===========================================================================

/// What the owner is shown before a standing rule is made.
///
/// Not the arguments: a sentence, with the two Gmail idioms translated. Someone
/// who has never read Gmail's API documentation cannot tell what
/// `removeLabelIds: ["INBOX"]` does, and that is precisely the person being
/// asked to consent.
#[tokio::test]
async fn the_approval_prompt_reads_as_a_sentence() {
    let harness = Harness::new("approve", vec![]);
    let gate = harness.gate();

    let summary = gate
        .approval_summary(
            tools::CREATE_FILTER_TOOL,
            &json!({
                "accountId": harness.account_id,
                "from": "no-reply@okta.com",
                "subject": "verification code",
                "addLabelIds": ["Label_18"],
                "removeLabelIds": ["INBOX", "UNREAD"],
            }),
        )
        .await;

    assert_eq!(
        summary,
        "Create a filter on alex@lumen.example. Mail from no-reply@okta.com and with \
         \u{201c}verification code\u{201d} in the subject. It is labelled Codes, skips the inbox \
         and is marked as read. It applies to mail that arrives from now on."
    );

    // The things a JSON blob would not have said, spelled out.
    for phrase in [
        "skips the inbox",
        // The label by the name he calls it, not Label_18.
        "labelled Codes",
        // The one question the prompt exists to answer.
        "arrives from now on",
    ] {
        assert!(summary.contains(phrase), "missing {phrase:?} in {summary}");
    }
    assert!(!summary.contains("removeLabelIds"), "{summary}");
    assert!(!summary.contains('{'), "{summary}");
}

/// The destructive one has to say the word.
#[tokio::test]
async fn a_filter_that_deletes_mail_says_so() {
    let harness = Harness::new("approve-trash", vec![]);
    let summary = harness
        .gate()
        .approval_summary(
            tools::CREATE_FILTER_TOOL,
            &json!({
                "accountId": harness.account_id,
                "from": "no-reply@okta.com",
                "addLabelIds": ["TRASH"],
            }),
        )
        .await;
    assert!(summary.contains("It is deleted."), "{summary}");
}

/// Forwarding is the one action that puts the owner's mail in front of somebody
/// else, so the address is in the prompt.
#[tokio::test]
async fn a_forwarding_filter_names_the_destination() {
    let harness = Harness::new("approve-forward", vec![]);
    let summary = harness
        .gate()
        .approval_summary(
            tools::CREATE_FILTER_TOOL,
            &json!({
                "accountId": harness.account_id,
                "from": "invoices@lumen.example",
                "forward": "bookkeeper@example.com",
            }),
        )
        .await;
    assert!(
        summary.contains("is forwarded to bookkeeper@example.com"),
        "{summary}"
    );
}

/// A filter id is twenty opaque characters. Approving its deletion on sight is
/// consent to nothing, so the prompt fetches the rule and describes it.
#[tokio::test]
async fn deleting_a_filter_describes_the_rule_rather_than_its_id() {
    let listed = HttpResponse::json(
        200,
        r#"{"filter":[{"id":"ANe1Bmh","criteria":{"from":"no-reply@okta.com"},"action":{"removeLabelIds":["INBOX"]}}]}"#,
    );
    let harness = Harness::new("approve-delete", vec![listed]);

    let summary = harness
        .gate()
        .approval_summary(
            tools::DELETE_FILTER_TOOL,
            &json!({ "accountId": harness.account_id, "filterId": "ANe1Bmh" }),
        )
        .await;

    assert_eq!(
        summary,
        "Delete this filter: Mail from no-reply@okta.com. It skips the inbox. Mail it has \
         already moved stays where it is; only the rule goes."
    );
}

// ===========================================================================
// the command layer
// ===========================================================================

#[tokio::test]
async fn creating_a_filter_sends_the_criteria_and_the_action() {
    let created = HttpResponse::json(
        200,
        r#"{"id":"ANe1BmNEW","criteria":{"from":"no-reply@okta.com","subject":"verification code"},"action":{"addLabelIds":["Label_18"],"removeLabelIds":["INBOX","UNREAD"]}}"#,
    );
    let harness = Harness::new("create", vec![created]);

    let filter = harness
        .dispatcher
        .create_filter(harness.account_id, login_codes())
        .await
        .expect("create");

    assert_eq!(filter.id, "ANe1BmNEW");
    assert_eq!(filter.account_email, "alex@lumen.example");
    assert_eq!(
        filter.description,
        "Mail from no-reply@okta.com and with \u{201c}verification code\u{201d} in the subject. \
         It is labelled Codes, skips the inbox and is marked as read."
    );

    let request = &harness.google.requests()[0];
    assert!(
        request.url.ends_with("/users/me/settings/filters"),
        "{}",
        request.url
    );
    let body: Value = serde_json::from_slice(request.body.as_ref().expect("body")).unwrap();
    assert_eq!(body["criteria"]["from"], "no-reply@okta.com");
    assert_eq!(body["action"]["removeLabelIds"], json!(["INBOX", "UNREAD"]));
}

/// Google accepts a filter with no criteria. It means every message that ever
/// arrives, and nobody has ever typed it on purpose.
#[tokio::test]
async fn a_filter_with_no_criteria_is_refused_before_it_is_sent() {
    let harness = Harness::new("empty-criteria", vec![]);
    let error = harness
        .dispatcher
        .create_filter(
            harness.account_id,
            Filter {
                action: FilterAction {
                    add_label_ids: vec!["TRASH".into()],
                    ..FilterAction::default()
                },
                ..Filter::default()
            },
        )
        .await
        .expect_err("a filter matching everything");

    assert_eq!(error.kind(), "invalid");
    assert!(harness.google.requests().is_empty(), "it went to Google");
}

#[tokio::test]
async fn a_filter_that_does_nothing_is_refused_before_it_is_sent() {
    let harness = Harness::new("empty-action", vec![]);
    let error = harness
        .dispatcher
        .create_filter(
            harness.account_id,
            Filter {
                criteria: FilterCriteria {
                    from: Some("no-reply@okta.com".into()),
                    ..FilterCriteria::default()
                },
                ..Filter::default()
            },
        )
        .await
        .expect_err("a filter with no action");

    assert_eq!(error.kind(), "invalid");
    assert!(harness.google.requests().is_empty());
}

#[tokio::test]
async fn listing_filters_reads_them_live_and_describes_each_one() {
    let listed = HttpResponse::json(
        200,
        r#"{"filter":[
            {"id":"a","criteria":{"from":"no-reply@okta.com"},"action":{"removeLabelIds":["INBOX"]}},
            {"id":"b","criteria":{"query":"list:news.example.com"},"action":{"addLabelIds":["TRASH"]}}
        ]}"#,
    );
    let harness = Harness::new("list", vec![listed]);

    let filters = harness
        .dispatcher
        .list_filters(Some(harness.account_id))
        .await
        .expect("list");

    assert_eq!(filters.len(), 2);
    assert_eq!(
        filters[0].description,
        "Mail from no-reply@okta.com. It skips the inbox."
    );
    assert_eq!(
        filters[1].description,
        "Mail matching list:news.example.com. It is deleted."
    );
    assert_eq!(harness.google.requests()[0].method.as_str(), "GET");
}

#[tokio::test]
async fn deleting_a_filter_addresses_it_by_id() {
    let harness = Harness::new("delete", vec![HttpResponse::new(204, Vec::new())]);
    harness
        .dispatcher
        .delete_filter(harness.account_id, "ANe1Bmh")
        .await
        .expect("delete");

    let request = &harness.google.requests()[0];
    assert_eq!(request.method.as_str(), "DELETE");
    assert!(
        request.url.ends_with("/users/me/settings/filters/ANe1Bmh"),
        "{}",
        request.url
    );
}

// ===========================================================================
// the missing scope
// ===========================================================================

/// The first thing that happens on every account after the scope list grows.
///
/// The credential is alive — mail and calendar are still syncing — and this one
/// endpoint answers 403. It has to come back as the account needing
/// authorization, with the remedy in the sentence, rather than as Google's own
/// wording about authentication scopes.
#[tokio::test]
async fn a_narrow_grant_reports_the_account_rather_than_the_403() {
    let refused = HttpResponse::json(
        403,
        r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","errors":[{"reason":"insufficientPermissions"}],"status":"PERMISSION_DENIED"}}"#,
    );
    let harness = Harness::new("scope", vec![refused]);

    let error = harness
        .dispatcher
        .create_filter(harness.account_id, login_codes())
        .await
        .expect_err("a grant without gmail.settings.basic");

    assert_eq!(error.kind(), "missingScope");
    assert!(matches!(error, CommandError::MissingScope { .. }));
    let sentence = error.to_string();
    assert!(sentence.contains("alex@lumen.example"), "{sentence}");
    assert!(sentence.contains("Preferences"), "{sentence}");

    // And the account is now on the list the status bar and Preferences read,
    // so this is a visible state rather than one error message in one window.
    assert_eq!(
        harness.dispatcher.scope_notices().emails(),
        vec!["alex@lumen.example".to_string()]
    );
}

/// A dead credential and a narrow grant are both "sign in again" and are not
/// the same failure: one stops the mail, the other does not.
#[tokio::test]
async fn a_narrow_grant_is_not_reported_as_a_dead_credential() {
    let refused = HttpResponse::json(
        401,
        r#"{"error":{"code":401,"message":"Invalid Credentials","errors":[{"reason":"authError"}]}}"#,
    );
    let harness = Harness::new("dead", vec![refused]);

    let error = harness
        .dispatcher
        .create_filter(harness.account_id, login_codes())
        .await
        .expect_err("a dead token");

    assert_ne!(error.kind(), "missingScope");
    assert!(
        harness.dispatcher.scope_notices().emails().is_empty(),
        "a 401 was recorded as a missing scope"
    );
}

/// Authorizing the account again is what clears it — nothing else should, and
/// the record must not outlive the grant that caused it.
#[test]
fn a_fresh_grant_clears_the_notice() {
    let notices = mach_lib::commands::ScopeNotices::default();
    notices.record("alex@lumen.example");
    notices.record("alex@lumen.example");
    assert_eq!(notices.emails().len(), 1);

    notices.clear("alex@lumen.example");
    assert!(notices.emails().is_empty());
}
