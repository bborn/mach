//! Reply suggestions on the credentials he actually has.
//!
//! `tests/suggest.rs` covers the rule, the dedup, the cap and the store, and it
//! does all of it with `ANTHROPIC_API_KEY=test-key` in the environment and a
//! scripted HTTP transport. Every one of those tests passed on the morning this
//! feature shipped without ever producing a suggestion, because the machine it
//! shipped to has no API key and the only path to a model was the one that
//! needs one. A green transport is not a working feature.
//!
//! So this file is the other configuration, and it is the *default* one:
//!
//! - `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` removed from the
//!   environment, and asserted absent through the same probe the resolver uses;
//! - a `claude` executable present, which is what
//!   [`backend::resolve`](mach_lib::ipc::agent::engine::backend::resolve) picks
//!   when it can find one;
//! - the HTTP transport still handed to the engine, and **poisoned** — every
//!   call fails and is counted, so a generation that quietly went back to
//!   `POST /v1/messages` fails the test rather than passing it.
//!
//! It is a separate test binary rather than another module in `suggest.rs`
//! because "no API key is configured" and "`ANTHROPIC_API_KEY=test-key`" cannot
//! both be true in one process, and `MACH_CLAUDE_BIN` is read per resolution —
//! two files that disagreed about it would race.
//!
//! The `claude` here is a shell script. Nothing in this file spends anything.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use mach_lib::db::models::NewAccount;
use mach_lib::db::{queries, Db};
use mach_lib::google::BoxFuture;
use mach_lib::ipc::agent::engine::backend::{Availability, ENV_CLAUDE_BIN, PREF_BACKEND, PREF_MODEL};
use mach_lib::ipc::agent::engine::config::{ENV_API_KEY, ENV_AUTH_TOKEN};
use mach_lib::ipc::agent::engine::error::AgentError;
use mach_lib::ipc::agent::engine::wire::{ChunkStream, ModelCall, ModelTransport};
use mach_lib::ipc::prefs;
use mach_lib::suggest::{self, store, Headers, SuggestBrain};

const ME: &str = "bruno@example.com";

// ===========================================================================
// The configuration he actually runs
// ===========================================================================

/// `MACH_CLAUDE_BIN` and the credential variables are process-wide, so the
/// tests that set them take turns. An async lock rather than a `std` one
/// because every test holds it across a `sleep`.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Put the process into the state Bruno's machine is in: Claude Code present,
/// no API credential anywhere.
///
/// Returns the stub, so a test can read back exactly how it was invoked.
fn no_key_and_a_claude(reply: &str) -> Stub {
    with_claude(Stub::new(reply))
}

/// The same, with a stub already built — for the tests that need a document
/// this one would not write.
fn with_claude(stub: Stub) -> Stub {
    std::env::remove_var(ENV_API_KEY);
    std::env::remove_var(ENV_AUTH_TOKEN);

    std::env::set_var(ENV_CLAUDE_BIN, &stub.exe);

    let available = Availability::probe();
    assert!(
        !available.api_key,
        "this test is about the machine with no API key; something set one"
    );
    assert_eq!(
        available.claude.as_deref(),
        Some(stub.exe.as_path()),
        "the stub should be the `claude` the resolver finds"
    );
    stub
}

// ===========================================================================
// A `claude` that costs nothing
// ===========================================================================

/// A shell script standing in for the CLI: it writes down how it was called and
/// prints the document `--output-format json` promises.
struct Stub {
    dir: PathBuf,
    exe: PathBuf,
}

impl Stub {
    /// `reply` is the text the CLI would have produced — what goes in `result`.
    fn new(reply: &str) -> Stub {
        Stub::with_document(
            &json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": reply,
            })
            .to_string(),
        )
    }

    /// A run that reports what it cost, the way the real CLI does.
    fn costing(reply: &str, usd: f64, input: i64, output: i64) -> Stub {
        Stub::with_document(
            &json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": reply,
                "total_cost_usd": usd,
                "usage": { "input_tokens": input, "output_tokens": output },
            })
            .to_string(),
        )
    }

    /// The same, for a run whose output is not a success document.
    fn with_document(document: &str) -> Stub {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mach-suggest-cli-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("stub dir");

        std::fs::write(dir.join("reply.json"), document).expect("reply");
        let exe = dir.join("claude");
        let script = format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> '{dir}/argv'; done\n\
             printf '%s\\n' \"$(pwd -P)\" > '{dir}/cwd'\n\
             cat > '{dir}/stdin'\n\
             cat '{dir}/reply.json'\n",
            dir = dir.display()
        );
        let mut file = std::fs::File::create(&exe).expect("stub");
        file.write_all(script.as_bytes()).expect("write stub");
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
                .expect("chmod stub");
        }
        Stub { dir, exe }
    }

    /// Every argument the CLI was given, in order. Empty when it never ran.
    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn ran(&self) -> bool {
        self.dir.join("argv").exists()
    }

    /// The prompt, which travels on stdin because it is his mail.
    fn stdin(&self) -> String {
        std::fs::read_to_string(self.dir.join("stdin")).unwrap_or_default()
    }

    fn cwd(&self) -> String {
        std::fs::read_to_string(self.dir.join("cwd"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// The value that followed `flag`.
    fn value_of(&self, flag: &str) -> Option<String> {
        let argv = self.argv();
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1).cloned())
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The HTTP path, poisoned.
///
/// A generation that reached this went over `POST /v1/messages` — which is the
/// bug, not the feature — so it fails and is counted rather than answering.
#[derive(Default)]
struct Poisoned {
    calls: AtomicUsize,
}

impl ModelTransport for Poisoned {
    fn send<'a>(&'a self, _call: ModelCall) -> BoxFuture<'a, Result<ChunkStream, AgentError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(AgentError::transport(
                "the Anthropic API is not how this machine reaches a model",
            ))
        })
    }
}

// ===========================================================================
// A store with one message worth answering
// ===========================================================================

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mach-suggest-cli-db-{}-{}/mach.sqlite3",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        TempDb { path }
    }

    fn open(&self) -> Db {
        Db::open(&self.path).expect("open db")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// One thread, one message from a person, addressed to him.
fn seeded() -> (TempDb, Db, i64) {
    let temp = TempDb::new();
    let db = temp.open();

    let account_id = db
        .write(|conn| {
            queries::upsert_account(
                conn,
                &NewAccount {
                    email: ME.into(),
                    display_name: Some("Bruno".into()),
                    token_ref: ME.into(),
                    colour_index: 0,
                },
            )
        })
        .expect("account");

    db.write_background(mach_lib::db::sync_queries::ensure_schema)
        .expect("sync schema");
    db.write(|conn| {
        conn.execute(
            "INSERT INTO threads (id, account_id, gmail_thread_id, subject)
             VALUES (1, ?1, 't1', 'Lunch on Tuesday')",
            [account_id],
        )?;
        conn.execute(
            "INSERT INTO messages
                 (id, thread_id, account_id, gmail_message_id, from_name, from_email, to_json,
                  subject, body_text, snippet, internal_date)
             VALUES (11, 1, ?1, 'incoming', 'Kate', 'kate@example.org', ?2,
                     'Lunch on Tuesday', 'Are you free on Tuesday?', 'Are you free', 2000)",
            rusqlite::params![
                account_id,
                serde_json::to_string(&json!([{ "email": ME }])).unwrap()
            ],
        )?;
        conn.execute(
            "INSERT INTO sync_message_labels (account_id, gmail_message_id, label_ids)
             VALUES (?1, 'incoming', ?2)",
            rusqlite::params![account_id, json!(["INBOX", "UNREAD"]).to_string()],
        )?;
        Ok(())
    })
    .expect("seed");

    (temp, db, account_id)
}

fn set_pref(db: &Db, key: &str, value: Value) {
    db.write(|conn| prefs::set(conn, key, &value, 0)).expect("pref");
}

fn arrival() -> HashMap<String, Headers> {
    HashMap::from([("incoming".to_string(), Headers::default())])
}

/// The whole door, from an arrival to whatever it comes to.
///
/// `consider` returns immediately and works on a task of its own — the sync
/// pass does not wait on a model — so a test has to. It waits on a process
/// here, not a channel, which is why the grace is measured in tenths of a
/// second rather than yields.
fn fire(db: &Db, brain: SuggestBrain, account_id: i64) {
    suggest::consider(db, brain, account_id, &["incoming".to_string()], arrival());
}

async fn settle_until(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Long enough for a `consider` that is never going to write anything to have
/// finished not writing it.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

fn written(db: &Db) -> bool {
    db.read(|conn| store::fresh_for_thread(conn, 1))
        .ok()
        .flatten()
        .is_some()
}

fn workspace() -> PathBuf {
    std::env::temp_dir().join("mach-suggest-cli-workspace")
}

fn brain(transport: Arc<Poisoned>) -> SuggestBrain {
    SuggestBrain {
        transport,
        workspace: workspace(),
    }
}

// ===========================================================================
// The tests
// ===========================================================================

/// The one that would have caught this.
///
/// No API key anywhere, a `claude` on the machine, and the HTTP transport
/// rigged to fail if it is touched. Against the code as it shipped this cannot
/// pass — `consider` loaded an `AgentConfig`, got `MissingApiKey`, and returned
/// — and it does not pass by accident now either: the assertion is a stance row
/// in the store, not a transport that was called.
#[tokio::test]
async fn a_machine_with_claude_code_and_no_api_key_writes_a_reply() {
    let _guard = ENV.lock().await;
    let stances = json!([
        { "label": "Say Tuesday works", "body": "Tuesday works — two o'clock?" },
        { "label": "Push it a week", "body": "Could we do the following Tuesday instead?" },
    ])
    .to_string();
    let stub = no_key_and_a_claude(&stances);

    let (_temp, db, account_id) = seeded();
    let transport = Arc::new(Poisoned::default());
    fire(&db, brain(Arc::clone(&transport)), account_id);
    settle_until(|| written(&db)).await;

    let suggestion = db
        .read(|conn| store::fresh_for_thread(conn, 1))
        .expect("read")
        .expect("a reply should have been written");
    assert_eq!(suggestion.stances.len(), 2);
    assert_eq!(suggestion.stances[0].label, "Say Tuesday works");
    assert_eq!(suggestion.stances[0].body, "Tuesday works — two o'clock?");
    assert_eq!(suggestion.message_id, 11);

    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        0,
        "the Anthropic HTTP path was used on a machine that has no key for it"
    );
    assert!(stub.ran(), "Claude Code was never asked");
}

/// The model rule, on the path that now writes the replies.
///
/// `suggest::DEFAULT_MODEL` is pinned by a unit test; this pins that the value
/// actually reaches the CLI, and that the *agent's* model — the one somebody
/// sets to `opus` for ⌘K — does not leak into a call made against every
/// inbound message.
#[tokio::test]
async fn the_cheap_model_is_the_one_that_runs_and_the_agent_s_is_not() {
    let _guard = ENV.lock().await;
    let stub = no_key_and_a_claude(&json!([{ "label": "Yes", "body": "Tuesday works." }]).to_string());

    let (_temp, db, account_id) = seeded();
    set_pref(&db, PREF_MODEL, json!("claude-opus-5"));

    fire(&db, brain(Arc::new(Poisoned::default())), account_id);
    settle_until(|| written(&db)).await;

    assert_eq!(
        stub.value_of("--model").as_deref(),
        Some(suggest::DEFAULT_MODEL),
        "the unattended pass did not run on the cheap model"
    );
    assert!(
        !stub.argv().iter().any(|a| a.contains("opus")),
        "the agent's model reached the unattended pass: {:?}",
        stub.argv()
    );
}

/// What the CLI is allowed to do while it writes a reply: nothing.
#[tokio::test]
async fn the_one_shot_has_no_tools_no_mcp_and_no_saved_session() {
    let _guard = ENV.lock().await;
    let stub = no_key_and_a_claude(&json!([{ "label": "Yes", "body": "Tuesday works." }]).to_string());

    let (_temp, db, account_id) = seeded();
    fire(&db, brain(Arc::new(Poisoned::default())), account_id);
    settle_until(|| written(&db)).await;

    let argv = stub.argv();
    assert!(argv.iter().any(|a| a == "--print"), "{argv:?}");
    assert_eq!(stub.value_of("--output-format").as_deref(), Some("json"));
    assert_eq!(stub.value_of("--tools").as_deref(), Some(""));
    assert_eq!(stub.value_of("--setting-sources").as_deref(), Some(""));
    assert!(argv.iter().any(|a| a == "--strict-mcp-config"), "{argv:?}");
    assert!(argv.iter().any(|a| a == "--disable-slash-commands"), "{argv:?}");
    assert!(
        argv.iter().any(|a| a == "--no-session-persistence"),
        "a reply nobody uses should not leave a transcript behind: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--mcp-config" || a == "--resume"),
        "{argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a.contains("dangerously")),
        "{argv:?}"
    );

    // The system prompt is Mach's own text and goes in the argument vector; the
    // prompt is his mail and does not.
    let system = stub.value_of("--system-prompt").expect("a system prompt");
    assert!(system.contains("Bruno") || system.contains(ME), "{system}");
    assert!(
        !argv.iter().any(|a| a.contains("Are you free on Tuesday?")),
        "his mail was put in an argument vector"
    );
    assert!(
        stub.stdin().contains("Are you free on Tuesday?"),
        "the message never reached the prompt"
    );

    assert_eq!(
        stub.cwd(),
        std::fs::canonicalize(workspace())
            .unwrap_or_else(|_| workspace())
            .to_string_lossy(),
        "the child ran somewhere other than Mach's own directory"
    );
}

/// A CLI that fails writes no row — and, unlike the state this shipped in, says
/// so. The line goes to stderr, which a test cannot read without capturing the
/// process's own output; what is asserted here is the half that is checkable,
/// which is that a failure is not a panic and not a half-written row.
#[tokio::test]
async fn a_failing_cli_writes_nothing_and_does_not_take_the_pass_down() {
    let _guard = ENV.lock().await;
    std::env::remove_var(ENV_API_KEY);
    std::env::remove_var(ENV_AUTH_TOKEN);
    let stub = Stub::with_document(
        &json!({ "type": "result", "is_error": true, "result": "Invalid model name" }).to_string(),
    );
    std::env::set_var(ENV_CLAUDE_BIN, &stub.exe);

    let (_temp, db, account_id) = seeded();
    fire(&db, brain(Arc::new(Poisoned::default())), account_id);
    settle_until(|| stub.ran()).await;
    settle().await;

    assert!(stub.ran());
    assert!(
        db.read(|conn| store::fresh_for_thread(conn, 1))
            .expect("read")
            .is_none(),
        "a failed run must not leave a row"
    );
}

/// A backend that cannot answer a one-shot is a stated failure, not a silence
/// — and, critically, not a fall-through to spending money a different way.
#[tokio::test]
async fn a_custom_command_backend_writes_nothing() {
    let _guard = ENV.lock().await;
    let stub = no_key_and_a_claude(&json!([{ "label": "Yes", "body": "Tuesday works." }]).to_string());

    let (_temp, db, account_id) = seeded();
    set_pref(&db, PREF_BACKEND, json!("command"));

    let transport = Arc::new(Poisoned::default());
    fire(&db, brain(Arc::clone(&transport)), account_id);
    settle().await;

    assert!(!stub.ran(), "Claude Code was not the chosen backend");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(db
        .read(|conn| store::fresh_for_thread(conn, 1))
        .expect("read")
        .is_none());
}

/// An owner who *does* configure a key and asks for the API still gets it. The
/// CLI is the default, not the only way.
#[tokio::test]
async fn the_api_stays_available_when_it_is_the_one_that_was_asked_for() {
    let _guard = ENV.lock().await;
    let stub = no_key_and_a_claude(&json!([{ "label": "Yes", "body": "Tuesday works." }]).to_string());
    std::env::set_var(ENV_API_KEY, "configured-on-purpose");

    let (_temp, db, account_id) = seeded();
    set_pref(&db, PREF_BACKEND, json!("anthropicApi"));

    let transport = Arc::new(Poisoned::default());
    fire(&db, brain(Arc::clone(&transport)), account_id);
    settle_until(|| transport.calls.load(Ordering::SeqCst) > 0).await;
    std::env::remove_var(ENV_API_KEY);

    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        1,
        "the explicitly chosen API backend was not the one used"
    );
    assert!(!stub.ran(), "Claude Code answered a request meant for the API");
}

/// What the run cost, on the path he is actually on.
///
/// The API path prices tokens against a table; this one does not have to. Claude
/// Code reports `total_cost_usd` with the answer, so the figure in the ledger is
/// the one the program that made the call arrived at, and a table going stale
/// cannot make it wrong.
#[tokio::test]
async fn the_cli_s_own_price_is_what_reaches_the_ledger() {
    let _guard = ENV.lock().await;
    let stub = with_claude(Stub::costing(
        &json!([{ "label": "Yes", "body": "Tuesday works." }]).to_string(),
        0.0193,
        2_100,
        380,
    ));

    let (_temp, db, account_id) = seeded();
    fire(&db, brain(Arc::new(Poisoned::default())), account_id);
    settle_until(|| written(&db)).await;
    assert!(stub.ran(), "Claude Code was never asked");

    let (model, cost, input, output) = db
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT model, cost_usd, input_tokens, output_tokens
                   FROM reply_suggestion_outcomes WHERE kind = 'generated'",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<f64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                },
            )?)
        })
        .expect("a completed CLI run must leave a generation row");

    assert_eq!(cost, Some(0.0193), "the CLI's own figure did not reach the row");
    assert_eq!(input, Some(2_100));
    assert_eq!(output, Some(380));
    assert_eq!(model, suggest::DEFAULT_MODEL);

    // And the same figure is what the cap counts and the panel reads. The row
    // was stamped off the wall clock, so the window has to be read against it.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let budget = db.read(move |conn| suggest::budget::state(conn, now)).expect("budget");
    assert_eq!(budget.day_count, 1);
    assert_eq!(budget.day_priced, 1, "the CLI path is a priced path");
    assert!((budget.day_spend_usd - 0.0193).abs() < 1e-9);
}
