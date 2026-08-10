//! What the store costs at the size the owner's store actually is.
//!
//! Everything here is `#[ignore]`d: it builds a ~46,000-thread / ~66,000-message
//! database on disk, which takes tens of seconds and has no business in the
//! normal test run. It is a measuring instrument, not a regression test — the
//! regression tests for the optimistic path are in `src/hooks/*.test.tsx`, where
//! they can assert intermediate frames.
//!
//! ```sh
//! cargo test --test perf -- --ignored --nocapture
//! ```
//!
//! The numbers it reports are the three the archive keystroke is made of:
//!
//!  * `execute` to the moment the local transaction is committed — measured by
//!    the transport, which is only called *after* the commit, so the instant it
//!    is entered is the instant the store agreed;
//!  * `list_threads`, which is what a `threads-changed` refetch pays;
//!  * `get_thread`, which the same refetch also pays for the open conversation.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mach_lib::commands::{AccountClients, Command, CommandDispatcher};
use mach_lib::db::models::{NewAccount, NewMessage, NewThread, Participant, ThreadQuery};
use mach_lib::db::{queries, Db};
use mach_lib::google::{
    BoxFuture, HttpRequest, HttpResponse, HttpTransport, RetryPolicy, StaticTokenProvider,
    TransportError,
};

/// The owner's store, to the nearest round number he gave.
const THREADS: usize = 46_000;
const MESSAGES: usize = 66_000;
/// How many of those are in the inbox. Everything else has been triaged away.
const INBOX: usize = 2_000;
const ACCOUNTS: i64 = 3;

// ---------------------------------------------------------------- transport

/// Records when the first request was issued, and answers instantly.
///
/// The command layer commits locally and *then* calls Google, so the moment
/// this is entered is the moment the local write was durable. That is the
/// number the UI is actually waiting on.
struct StopwatchTransport {
    entered: AtomicI64,
    origin: Mutex<Option<Instant>>,
}

impl StopwatchTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: AtomicI64::new(-1),
            origin: Mutex::new(None),
        })
    }

    fn arm(&self) {
        *self.origin.lock().unwrap() = Some(Instant::now());
        self.entered.store(-1, Ordering::SeqCst);
    }

    /// Microseconds from `arm()` to the first outbound request, or `None` when
    /// the command decided it had nothing to tell Google.
    fn to_remote(&self) -> Option<Duration> {
        match self.entered.load(Ordering::SeqCst) {
            -1 => None,
            micros => Some(Duration::from_micros(micros as u64)),
        }
    }
}

impl HttpTransport for StopwatchTransport {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        if self.entered.load(Ordering::SeqCst) == -1 {
            if let Some(origin) = *self.origin.lock().unwrap() {
                self.entered
                    .store(origin.elapsed().as_micros() as i64, Ordering::SeqCst);
            }
        }
        Box::pin(async move { Ok(HttpResponse::json(200, "{}")) })
    }
}

// ------------------------------------------------------------------- seeding

/// A scratch database path, removed when the guard drops.
///
/// Never anywhere near `~/Library/Application Support/com.mach.mail` — this
/// harness builds its own store and only ever touches that one.
struct Scratch {
    path: std::path::PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let mut path = std::env::temp_dir();
        path.push(format!("mach-perf-{tag}-{}.sqlite3", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        Scratch { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// A store the size of the owner's, on disk, in WAL, with the real schema.
///
/// In-memory would be a different measurement: no WAL, no page cache misses,
/// no filesystem. The whole question here is what the app pays on his machine.
fn seed(path: &std::path::Path) -> (Db, Vec<i64>) {
    let db = Db::open(path).expect("open");

    let accounts: Vec<i64> = (0..ACCOUNTS)
        .map(|i| {
            db.write(|c| {
                queries::upsert_account(
                    c,
                    &NewAccount {
                        email: format!("owner+{i}@example.test"),
                        display_name: None,
                        token_ref: String::new(),
                        colour_index: i as i64,
                    },
                )
            })
            .expect("account")
        })
        .collect();

    // One transaction for the lot. Per-thread transactions would measure
    // fsync, not the schema.
    let now = 1_760_000_000_000i64;
    let extra_messages = MESSAGES - THREADS;
    let inbox_stride = THREADS / INBOX;
    let mut inbox_ids = Vec::with_capacity(INBOX);

    db.write(|c| {
        for n in 0..THREADS {
            let account_id = accounts[n % accounts.len()];
            // Newest first by construction: thread 0 is the most recent.
            let last_message_at = now - (n as i64) * 60_000;
            let in_inbox = n % inbox_stride == 0;
            let unread = in_inbox && n % (inbox_stride * 4) == 0;
            let mut labels = vec![format!("Label_{}", n % 40)];
            if in_inbox {
                labels.push("INBOX".to_string());
            }
            if unread {
                labels.push("UNREAD".to_string());
            }
            if n % 11 == 0 {
                labels.push("STARRED".to_string());
            }
            // A mailbox with almost nothing in it, spread across the whole
            // range: the worst case for `EXISTS (thread_labels …)`, because
            // the index on `(last_message_at DESC, id DESC)` has to be walked
            // to the bottom of the store to find a page of sixty.
            if n % 900 == 0 {
                labels.push("Rare".to_string());
            }

            // A few threads carry a second and third message, to reach the
            // message count without giving every thread the same shape.
            let messages = if n < extra_messages * 2 && n % 2 == 0 { 3 } else { 1 };

            let thread_id = queries::upsert_thread(
                c,
                &NewThread {
                    account_id,
                    gmail_thread_id: format!("t{n}"),
                    participants: vec![
                        Participant::new("someone@example.test"),
                        Participant::new("owner@example.test"),
                    ],
                    subject: format!("Conversation {n} about the quarterly numbers"),
                    snippet: "The attached spreadsheet has the figures we discussed".into(),
                    last_message_at,
                    is_unread: unread,
                    message_count: messages as i64,
                    has_attachments: n % 7 == 0,
                    label_ids: labels,
                },
            )?;
            if in_inbox && inbox_ids.len() < INBOX {
                inbox_ids.push(thread_id);
            }
            for m in 0..messages {
                queries::upsert_message(
                    c,
                    &NewMessage {
                        thread_id,
                        account_id,
                        gmail_message_id: format!("t{n}-m{m}"),
                        from: Participant::new("someone@example.test"),
                        to: vec![Participant::new("owner@example.test")],
                        subject: format!("Conversation {n} about the quarterly numbers"),
                        snippet: "The attached spreadsheet has the figures".into(),
                        body_text: Some(
                            "Here are the numbers you asked for. Let me know if the \
                             totals look wrong and I will send a corrected sheet."
                                .into(),
                        ),
                        internal_date: last_message_at - (messages - m - 1) as i64 * 1_000,
                        is_unread: unread,
                        ..Default::default()
                    },
                )?;
            }
        }
        Ok(())
    })
    .expect("seed");

    db.write(|c| {
        c.execute_batch("ANALYZE;")?;
        Ok(())
    })
    .expect("analyze");

    (db, inbox_ids)
}

fn dispatcher(db: &Db, transport: Arc<StopwatchTransport>) -> CommandDispatcher {
    let mut clients = AccountClients::new(transport)
        .with_gmail_base_url("https://gmail.test/gmail/v1")
        .with_calendar_base_url("https://calendar.test/calendar/v3")
        .with_retry_policy(RetryPolicy::none());
    for account in 1..=ACCOUNTS {
        clients = clients.with_account(account, Arc::new(StaticTokenProvider::new("token")));
    }
    CommandDispatcher::new(db.clone(), Arc::new(clients)).expect("dispatcher")
}

/// The median of a set of samples, which is the number a keystroke feels like.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn report(name: &str, samples: Vec<Duration>) {
    let worst = samples.iter().copied().max().unwrap();
    let mid = median(samples);
    println!(
        "  {name:<44} median {:>7.2}ms   worst {:>7.2}ms",
        mid.as_secs_f64() * 1000.0,
        worst.as_secs_f64() * 1000.0
    );
}

// -------------------------------------------------------------------- reads

#[test]
#[ignore = "builds a 46k-thread store; run explicitly"]
fn reads_at_the_owners_scale() {
    let scratch = Scratch::new("reads");
    let started = Instant::now();
    let (db, _) = seed(&scratch.path);
    println!(
        "\nseeded {THREADS} threads / ~{MESSAGES} messages in {:.1}s\n",
        started.elapsed().as_secs_f64()
    );

    let page = |limit: u32, label: Option<&str>, account: Option<i64>| ThreadQuery {
        account_id: account,
        label_id: label.map(str::to_string),
        unread_only: false,
        limit,
        after: None,
    };

    // Warm the page cache the way a running app's would be.
    for _ in 0..3 {
        db.read(|c| queries::list_threads(c, &page(60, Some("INBOX"), None)))
            .unwrap();
    }

    println!("list_threads:");
    for (name, query) in [
        ("INBOX, 60 rows (first page)", page(60, Some("INBOX"), None)),
        ("INBOX, 300 rows (refresh cap)", page(300, Some("INBOX"), None)),
        ("INBOX, one account, 60 rows", page(60, Some("INBOX"), Some(1))),
        ("all mail, 60 rows", page(60, None, None)),
        ("a user label, 60 rows", page(60, Some("Label_3"), None)),
        ("a 51-thread label (worst case)", page(60, Some("Rare"), None)),
    ] {
        let samples = (0..25)
            .map(|_| {
                let t = Instant::now();
                let rows = db.read(|c| queries::list_threads(c, &query)).unwrap();
                let elapsed = t.elapsed();
                assert!(!rows.is_empty(), "{name} returned nothing");
                elapsed
            })
            .collect();
        report(name, samples);
    }

    println!("\nget_thread:");
    let samples = (0..25)
        .map(|_| {
            let t = Instant::now();
            db.read(|c| queries::thread_with_messages(c, 1)).unwrap();
            t.elapsed()
        })
        .collect();
    report("one conversation", samples);
    println!();
}

// ------------------------------------------------------------------- writes

#[tokio::test]
#[ignore = "builds a 46k-thread store; run explicitly"]
async fn a_command_at_the_owners_scale() {
    let scratch = Scratch::new("writes");
    let started = Instant::now();
    let (db, inbox) = seed(&scratch.path);
    println!(
        "\nseeded {THREADS} threads / ~{MESSAGES} messages in {:.1}s\n",
        started.elapsed().as_secs_f64()
    );

    let transport = StopwatchTransport::new();
    let d = dispatcher(&db, transport.clone());

    println!("execute (local commit / whole command):");
    for (name, build) in [
        (
            "archive one conversation",
            Box::new(|ids: &[i64]| Command::Archive {
                thread_ids: ids.to_vec(),
            }) as Box<dyn Fn(&[i64]) -> Command>,
        ),
        (
            "trash one conversation",
            Box::new(|ids: &[i64]| Command::Trash {
                thread_ids: ids.to_vec(),
            }),
        ),
        (
            "mark one read",
            Box::new(|ids: &[i64]| Command::MarkRead {
                thread_ids: ids.to_vec(),
                read: true,
            }),
        ),
        (
            "star one conversation",
            Box::new(|ids: &[i64]| Command::Star {
                thread_ids: ids.to_vec(),
                starred: true,
            }),
        ),
    ] {
        let mut commits = Vec::new();
        let mut totals = Vec::new();
        for (i, id) in inbox.iter().take(25).enumerate() {
            // A distinct thread per sample, so nothing is measuring a no-op
            // diff against a thread the previous sample already moved.
            let target = inbox[(i * 7 + 3) % inbox.len()].max(*id).min(inbox[inbox.len() - 1]);
            transport.arm();
            let t = Instant::now();
            let result = d.execute(build(&[target])).await.expect("execute");
            totals.push(t.elapsed());
            assert!(result.ok, "{name}: {result:?}");
            if let Some(commit) = transport.to_remote() {
                commits.push(commit);
            }
        }
        if commits.is_empty() {
            println!("  {name:<44} (no remote call — nothing changed)");
        } else {
            report(&format!("{name} — to local commit"), commits);
        }
        report(&format!("{name} — whole command"), totals);
    }

    // Fifty at once: the bulk case, which is one Gmail batch but fifty rows of
    // local work.
    let fifty: Vec<i64> = inbox.iter().skip(200).take(50).copied().collect();
    transport.arm();
    let t = Instant::now();
    let result = d
        .execute(Command::Archive {
            thread_ids: fifty.clone(),
        })
        .await
        .expect("execute");
    let total = t.elapsed();
    assert!(result.ok, "{result:?}");
    println!(
        "\n  {:<44} to local commit {:>7.2}ms   whole {:>7.2}ms",
        "archive fifty conversations",
        transport
            .to_remote()
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(f64::NAN),
        total.as_secs_f64() * 1000.0
    );
    println!();
}
