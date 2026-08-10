//! What the store actually costs when the sync loop is writing.
//!
//! Three questions, none of which had a number attached to them:
//!
//!   * how long the sync loop holds the write lock for one batch, against a
//!     store the size of the owner's;
//!   * how long a user command (archive one conversation) waits for that lock,
//!     worst case as well as typical;
//!   * whether reads slow down too, and what the write-ahead log does to them
//!     when it is allowed to grow without a checkpoint.
//!
//! It generates its own store. It never opens the real one.
//!
//! ```sh
//! cargo run --release --example store_probe -- gen     /tmp/probe/mach.sqlite3
//! cargo run --release --example store_probe -- measure /tmp/probe/mach.sqlite3
//! cargo run --release --example store_probe -- attrib  /tmp/probe/mach.sqlite3
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use mach_lib::db::command_queries as cq;
use mach_lib::db::models::{NewAccount, NewLabel, NewMessage, Participant};
use mach_lib::db::{queries, sync_queries, Db};

// Shape of the owner's mailbox, as reported: ~46k threads, ~66k messages.
const THREADS: usize = 46_000;
const MESSAGES: usize = 66_000;
// 2.3 GB over 66k messages is ~35 KB a message once indexes are counted.
const HTML_BYTES: usize = 20_000;
const TEXT_BYTES: usize = 5_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "measure".into());
    let path = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "/tmp/mach-probe/mach.sqlite3".into()),
    );
    match mode.as_str() {
        "gen" => generate(&path),
        "measure" => measure(&path),
        "attrib" => attrib(&path),
        "wal" => wal_growth(&path),
        other => Err(format!("unknown mode {other}").into()),
    }
}

// ---------------------------------------------------------------------------
// a store the size of his
// ---------------------------------------------------------------------------

fn lorem(seed: usize, bytes: usize) -> String {
    const WORDS: &[&str] = &[
        "invoice", "meeting", "schedule", "attached", "regarding", "quarterly", "shipment",
        "confirm", "receipt", "delivery", "proposal", "contract", "renewal", "reminder",
        "account", "balance", "statement", "december", "monday", "thanks", "regards", "please",
        "review", "attached", "document", "update", "project", "timeline", "budget", "approve",
    ];
    let mut out = String::with_capacity(bytes + 16);
    let mut i = seed;
    while out.len() < bytes {
        out.push_str(WORDS[i % WORDS.len()]);
        out.push(' ');
        i = i.wrapping_mul(1_103_515_245).wrapping_add(12_345);
    }
    out
}

fn body_html(seed: usize) -> String {
    format!(
        "<html><body><div class=\"wrap\"><p>{}</p></div></body></html>",
        lorem(seed, HTML_BYTES)
    )
}

fn generate(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Err(format!("{} already exists — delete it first", path.display()).into());
    }
    let started = Instant::now();
    let db = Db::open(path)?;
    db.write(sync_queries::ensure_schema)?;
    db.write(cq::ensure_command_schema)?;

    let account_id = db.write(|conn| {
        let id = queries::upsert_account(
            conn,
            &NewAccount {
                email: "probe@example.com".into(),
                display_name: Some("Probe".into()),
                token_ref: String::new(),
                colour_index: 0,
            },
        )?;
        for label in ["INBOX", "UNREAD", "STARRED", "SENT", "TRASH", "Mach/Snoozed"] {
            queries::upsert_label(
                conn,
                &NewLabel {
                    account_id: id,
                    gmail_label_id: label.into(),
                    name: label.into(),
                    label_type: mach_lib::db::models::LabelType::System,
                },
            )?;
        }
        Ok(id)
    })?;

    // Same write path the backfill uses, batched the same way (25 messages a
    // transaction) so the generated file has the same page layout a real
    // backfill would leave behind.
    let mut written = 0usize;
    let now = 1_770_000_000_000i64;
    while written < MESSAGES {
        let batch = 25.min(MESSAGES - written);
        db.write(|conn| {
            let mut touched = Vec::new();
            for k in 0..batch {
                let n = written + k;
                // 66k messages over 46k threads: most threads have one message,
                // some have several.
                let thread_no = n % THREADS;
                let thread_id =
                    sync_queries::ensure_thread(conn, account_id, &format!("t{thread_no:07}"))?;
                let message_id = queries::upsert_message(
                    conn,
                    &NewMessage {
                        thread_id,
                        account_id,
                        gmail_message_id: format!("m{n:07}"),
                        rfc822_message_id: Some(format!("<{n}@example.com>")),
                        in_reply_to: None,
                        references: None,
                        from: Participant {
                            name: Some(format!("Sender {}", n % 900)),
                            email: format!("sender{}@example.com", n % 900),
                        },
                        reply_to: Vec::new(),
                        to: vec![Participant::new("probe@example.com")],
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        subject: format!("Re: {}", lorem(n, 48)),
                        body_html: Some(body_html(n)),
                        body_text: Some(lorem(n + 7, TEXT_BYTES)),
                        snippet: lorem(n + 3, 180),
                        internal_date: now - (n as i64) * 60_000,
                        is_unread: n % 9 == 0,
                        is_draft: false,
                        ..Default::default()
                    },
                )?;
                let _ = message_id;
                let labels: Vec<String> = if n % 3 == 0 {
                    vec!["INBOX".into(), "UNREAD".into()]
                } else {
                    vec!["INBOX".into()]
                };
                sync_queries::set_message_labels(
                    conn,
                    account_id,
                    &format!("m{n:07}"),
                    &labels,
                )?;
                touched.push(thread_id);
            }
            touched.sort_unstable();
            touched.dedup();
            for thread_id in touched {
                sync_queries::recompute_thread(conn, thread_id)?;
            }
            Ok(())
        })?;
        written += batch;
        if written % 5_000 == 0 {
            println!(
                "  {written}/{MESSAGES} messages  ({:.0}s, {})",
                started.elapsed().as_secs_f64(),
                file_sizes(path)
            );
        }
    }

    // A generated store is a fresh backfill; leave it checkpointed so a
    // measurement starts from a known WAL rather than the generator's.
    db.writer()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    println!(
        "generated in {:.0}s — {}",
        started.elapsed().as_secs_f64(),
        file_sizes(path)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// the contention measurement
// ---------------------------------------------------------------------------

struct Samples(Vec<f64>);

impl Samples {
    fn pct(&mut self, p: f64) -> f64 {
        if self.0.is_empty() {
            return 0.0;
        }
        self.0.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let i = ((self.0.len() - 1) as f64 * p).round() as usize;
        self.0[i]
    }
    fn max(&self) -> f64 {
        self.0.iter().cloned().fold(0.0, f64::max)
    }
    fn report(&mut self, label: &str) {
        let (p50, p95, p99, max) = (self.pct(0.5), self.pct(0.95), self.pct(0.99), self.max());
        println!(
            "  {label:<34} n={:<5} p50 {p50:>8.1}ms  p95 {p95:>8.1}ms  p99 {p99:>8.1}ms  max {max:>8.1}ms",
            self.0.len()
        );
    }
}

fn measure(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let db = Db::open(path)?;
    print_pragmas(&db)?;

    let account_id: i64 = db.read(|conn| {
        Ok(conn.query_row("SELECT id FROM accounts LIMIT 1", [], |r| r.get(0))?)
    })?;
    let thread_ids: Vec<i64> = db.read(|conn| {
        let mut stmt = conn.prepare("SELECT id FROM threads ORDER BY id LIMIT 4000")?;
        let out = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(out)
    })?;

    let stop = Arc::new(AtomicBool::new(false));
    let batches = Arc::new(AtomicU64::new(0));

    // --- the sync loop's writer ------------------------------------------
    // Exactly the shape of `sync::mail::spawn_backfill_writer`: one
    // transaction per batch of `message_batch_size` messages, each message
    // upserted with its labels, then one `recompute_thread` per touched
    // thread. The messages are re-upserts of rows that already exist, which is
    // what a history replay over a synced mailbox does.
    let sync_db = db.clone();
    let sync_stop = Arc::clone(&stop);
    let sync_batches = Arc::clone(&batches);
    let sync = std::thread::spawn(move || -> Vec<f64> {
        let mut holds = Vec::new();
        let mut n = 0usize;
        // `MACH_PROBE_LEGACY_WRITER=1` puts the sync loop back on the
        // interactive writer, which is what the numbers before the change were
        // taken with.
        let legacy = std::env::var("MACH_PROBE_LEGACY_WRITER").is_ok();
        // `SyncConfig::message_batch_size`.
        let batch_size: usize = std::env::var("PROBE_BATCH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(25);
        while !sync_stop.load(Ordering::Relaxed) {
            let t0 = Instant::now();
            let run = |db: &Db, f: &mut dyn FnMut(&Connection) -> mach_lib::db::Result<()>| {
                if legacy {
                    db.write(|c| f(c))
                } else {
                    db.write_background(|c| f(c))
                }
            };
            let mut body = |conn: &Connection| -> mach_lib::db::Result<()> {
                    let mut touched = Vec::new();
                    for k in 0..batch_size {
                        let idx = (n * batch_size + k) % MESSAGES;
                        let gid = format!("m{idx:07}");
                        let thread_no = idx % THREADS;
                        let thread_id = sync_queries::ensure_thread(
                            conn,
                            account_id,
                            &format!("t{thread_no:07}"),
                        )?;
                        queries::upsert_message(
                            conn,
                            &NewMessage {
                                thread_id,
                                account_id,
                                gmail_message_id: gid.clone(),
                                rfc822_message_id: Some(format!("<{idx}@example.com>")),
                                in_reply_to: None,
                                references: None,
                                from: Participant {
                                    name: Some(format!("Sender {}", idx % 900)),
                                    email: format!("sender{}@example.com", idx % 900),
                                },
                                reply_to: Vec::new(),
                                to: vec![Participant::new("probe@example.com")],
                                cc: Vec::new(),
                                bcc: Vec::new(),
                                subject: format!("Re: {}", lorem(idx, 48)),
                                body_html: Some(body_html(idx)),
                                body_text: Some(lorem(idx + 7, TEXT_BYTES)),
                                snippet: lorem(idx + 3, 180),
                                internal_date: 1_770_000_000_000 - (idx as i64) * 60_000,
                                is_unread: idx % 9 == 0,
                                is_draft: false,
                                ..Default::default()
                            },
                        )?;
                        sync_queries::set_message_labels(
                            conn,
                            account_id,
                            &gid,
                            &["INBOX".to_string()],
                        )?;
                        touched.push(thread_id);
                    }
                    touched.sort_unstable();
                    touched.dedup();
                    for thread_id in touched {
                        sync_queries::recompute_thread(conn, thread_id)?;
                    }
                    Ok(())
            };
            run(&sync_db, &mut body).expect("sync batch");
            holds.push(t0.elapsed().as_secs_f64() * 1000.0);
            sync_batches.fetch_add(1, Ordering::Relaxed);
            n += 1;
            // What `spawn_backfill_writer` does between batches.
            if !legacy && n % 32 == 0 {
                sync_db
                    .checkpoint_if_large(32 * 1024 * 1024)
                    .expect("checkpoint");
            }
        }
        holds
    });

    // --- a user archiving one conversation --------------------------------
    let user_db = db.clone();
    let user_stop = Arc::clone(&stop);
    let user_threads = thread_ids.clone();
    let user = std::thread::spawn(move || -> (Vec<f64>, Vec<f64>) {
        let mut waits = Vec::new();
        let mut totals = Vec::new();
        let mut i = 0usize;
        while !user_stop.load(Ordering::Relaxed) {
            let thread_id = user_threads[i % user_threads.len()];
            i += 1;
            let t0 = Instant::now();
            // What `commands::mail` does: read the snapshot from the pool,
            // then one short write transaction.
            let snap = user_db
                .read(|conn| cq::thread_snapshot(conn, thread_id))
                .expect("snapshot")
                .expect("thread");
            let target: Vec<String> = snap
                .label_ids
                .iter()
                .filter(|l| l.as_str() != "INBOX")
                .cloned()
                .collect();
            let wait_start = Instant::now();
            let entered = Arc::new(std::sync::Mutex::new(None::<Instant>));
            let entered_c = Arc::clone(&entered);
            user_db
                .write(move |conn| {
                    *entered_c.lock().unwrap() = Some(Instant::now());
                    cq::set_thread_state(conn, thread_id, &target, snap.is_unread)
                })
                .expect("archive");
            let inside = entered.lock().unwrap().expect("entered");
            waits.push((inside - wait_start).as_secs_f64() * 1000.0);
            totals.push(t0.elapsed().as_secs_f64() * 1000.0);
            std::thread::sleep(Duration::from_millis(40));
        }
        (waits, totals)
    });

    // --- the list the UI renders -----------------------------------------
    let read_db = db.clone();
    let read_stop = Arc::clone(&stop);
    let reader = std::thread::spawn(move || -> Vec<f64> {
        let mut out = Vec::new();
        while !read_stop.load(Ordering::Relaxed) {
            let t0 = Instant::now();
            read_db
                .read(|conn| {
                    let mut stmt = conn.prepare(
                        "SELECT id, subject, snippet FROM threads
                         ORDER BY last_message_at DESC, id DESC LIMIT 50",
                    )?;
                    let rows = stmt
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(rows.len())
                })
                .expect("list");
            out.push(t0.elapsed().as_secs_f64() * 1000.0);
            std::thread::sleep(Duration::from_millis(20));
        }
        out
    });

    let run_for = Duration::from_secs(
        std::env::var("PROBE_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30),
    );
    std::thread::sleep(run_for);
    stop.store(true, Ordering::Relaxed);

    let mut holds = Samples(sync.join().unwrap());
    let (waits, totals) = user.join().unwrap();
    let mut waits = Samples(waits);
    let mut totals = Samples(totals);
    let mut reads = Samples(reader.join().unwrap());

    println!("\nunder a running sync ({} batches):", batches.load(Ordering::Relaxed));
    holds.report("sync batch, lock held");
    waits.report("user write, waiting for the lock");
    totals.report("user archive, end to end");
    reads.report("thread list, 50 rows");
    println!("  {}", file_sizes(path));

    // --- the same three, with nothing else running ------------------------
    println!("\nwith the sync loop stopped:");
    let mut idle_waits = Samples(Vec::new());
    let mut idle_totals = Samples(Vec::new());
    for i in 0..60 {
        let thread_id = thread_ids[i % thread_ids.len()];
        let t0 = Instant::now();
        let snap = db.read(|conn| cq::thread_snapshot(conn, thread_id))?.unwrap();
        let target: Vec<String> = snap
            .label_ids
            .iter()
            .filter(|l| l.as_str() != "INBOX")
            .cloned()
            .collect();
        let wait_start = Instant::now();
        let entered = Arc::new(std::sync::Mutex::new(None::<Instant>));
        let entered_c = Arc::clone(&entered);
        db.write(move |conn| {
            *entered_c.lock().unwrap() = Some(Instant::now());
            cq::set_thread_state(conn, thread_id, &target, snap.is_unread)
        })?;
        let inside = entered.lock().unwrap().unwrap();
        idle_waits.0.push((inside - wait_start).as_secs_f64() * 1000.0);
        idle_totals.0.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    idle_waits.report("user write, waiting for the lock");
    idle_totals.report("user archive, end to end");

    let mut idle_reads = Samples(Vec::new());
    for _ in 0..60 {
        let t0 = Instant::now();
        db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, subject, snippet FROM threads
                 ORDER BY last_message_at DESC, id DESC LIMIT 50",
            )?;
            let rows = stmt
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.len())
        })?;
        idle_reads.0.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    idle_reads.report("thread list, 50 rows");

    Ok(())
}

// ---------------------------------------------------------------------------
// what the write-ahead log does when nothing checkpoints it
// ---------------------------------------------------------------------------

fn wal_growth(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let db = Db::open(path)?;
    let account_id: i64 =
        db.read(|conn| Ok(conn.query_row("SELECT id FROM accounts LIMIT 1", [], |r| r.get(0))?))?;

    println!("start: {}", file_sizes(path));
    let stop = Arc::new(AtomicBool::new(false));

    // Readers that never stop asking. `PROBE_READERS` sets how many overlap and
    // `PROBE_READ_SLEEP_MS` the gap between queries: the question is whether a
    // pool that is always mid-read can stop the automatic checkpoint from ever
    // completing, which is what would let the log grow without bound.
    let readers: usize = std::env::var("PROBE_READERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let read_sleep: u64 = std::env::var("PROBE_READ_SLEEP_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    // A wide scan rather than a LIMIT 50 seek: a read transaction open for
    // milliseconds rather than microseconds is what a checkpoint waits behind.
    let wide = std::env::var("PROBE_WIDE_READS").is_ok();
    println!("  {readers} reader(s), {read_sleep}ms apart, wide={wide}");
    let mut reader_handles = Vec::new();
    for _ in 0..readers {
        let read_db = db.clone();
        let read_stop = Arc::clone(&stop);
        reader_handles.push(std::thread::spawn(move || {
            while !read_stop.load(Ordering::Relaxed) {
                let _ = read_db.read(|conn| {
                    let sql = if wide {
                        "SELECT id FROM messages ORDER BY internal_date DESC LIMIT 40000"
                    } else {
                        "SELECT id FROM threads ORDER BY last_message_at DESC, id DESC LIMIT 50"
                    };
                    let mut stmt = conn.prepare(sql)?;
                    let n = stmt
                        .query_map([], |r| r.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<Vec<i64>>>()?
                        .len();
                    Ok(n)
                });
                if read_sleep > 0 {
                    std::thread::sleep(Duration::from_millis(read_sleep));
                }
            }
        }));
    }

    for round in 1..=8 {
        for n in 0..400 {
            let idx = (round * 400 + n) % MESSAGES;
            db.write(|conn| {
                let gid = format!("m{idx:07}");
                let thread_id =
                    sync_queries::ensure_thread(conn, account_id, &format!("t{:07}", idx % THREADS))?;
                queries::upsert_message(
                    conn,
                    &NewMessage {
                        thread_id,
                        account_id,
                        gmail_message_id: gid,
                        rfc822_message_id: None,
                        in_reply_to: None,
                        references: None,
                        from: Participant::new("sender@example.com"),
                        reply_to: Vec::new(),
                        to: Vec::new(),
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        subject: format!("Re: {}", lorem(idx, 48)),
                        body_html: Some(body_html(idx)),
                        body_text: Some(lorem(idx + 7, TEXT_BYTES)),
                        snippet: lorem(idx + 3, 180),
                        internal_date: 1_770_000_000_000 - (idx as i64) * 60_000,
                        is_unread: false,
                        is_draft: false,
                        ..Default::default()
                    },
                )?;
                Ok(())
            })?;
        }
        let t0 = Instant::now();
        let list = db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id FROM threads ORDER BY last_message_at DESC, id DESC LIMIT 50",
            )?;
            let n = stmt
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?
                .len();
            Ok(n)
        })?;
        println!(
            "after {:>5} writes: {}  list({list} rows) {:.2}ms",
            round * 400,
            file_sizes(path),
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }

    stop.store(true, Ordering::Relaxed);
    for h in reader_handles {
        h.join().unwrap();
    }

    let t0 = Instant::now();
    let (busy, log, ckpt): (i64, i64, i64) = db.writer().query_row(
        "PRAGMA wal_checkpoint(TRUNCATE)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    println!(
        "\nTRUNCATE checkpoint: busy={busy} log={log} checkpointed={ckpt} in {:.0}ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    println!("after: {}", file_sizes(path));

    let t0 = Instant::now();
    db.read(|conn| {
        let mut stmt =
            conn.prepare("SELECT id FROM threads ORDER BY last_message_at DESC, id DESC LIMIT 50")?;
        let n = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?
            .len();
        Ok(n)
    })?;
    println!("list after checkpoint: {:.2}ms", t0.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}

// ---------------------------------------------------------------------------
// where the gigabytes are
// ---------------------------------------------------------------------------

fn attrib(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open(path)?;
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
    println!(
        "page_size {page_size}  page_count {page_count}  = {:.2} GB",
        (page_size * page_count) as f64 / 1e9
    );

    match conn.prepare("SELECT name, SUM(pgsize) FROM dbstat GROUP BY name ORDER BY 2 DESC") {
        Ok(mut stmt) => {
            println!("\nby object (dbstat):");
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let total: i64 = rows.iter().map(|(_, b)| b).sum();
            for (name, bytes) in rows {
                println!(
                    "  {name:<34} {:>9.1} MB  {:>5.1}%",
                    bytes as f64 / 1e6,
                    100.0 * bytes as f64 / total as f64
                );
            }
        }
        Err(_) => println!("\n(dbstat not compiled in — falling back to column lengths)"),
    }

    println!("\nby column (uncompressed payload):");
    for (label, sql) in [
        ("messages.body_html", "SELECT SUM(LENGTH(body_html)) FROM messages"),
        ("messages.body_text", "SELECT SUM(LENGTH(body_text)) FROM messages"),
        ("messages.snippet", "SELECT SUM(LENGTH(snippet)) FROM messages"),
        ("messages.subject", "SELECT SUM(LENGTH(subject)) FROM messages"),
        (
            "messages, everything else",
            "SELECT SUM(LENGTH(gmail_message_id) + LENGTH(COALESCE(rfc822_message_id,''))
                      + LENGTH(COALESCE(in_reply_to,'')) + LENGTH(COALESCE(references_header,''))
                      + LENGTH(COALESCE(from_name,'')) + LENGTH(from_email)
                      + LENGTH(to_json) + LENGTH(cc_json) + LENGTH(bcc_json)) FROM messages",
        ),
        ("threads", "SELECT SUM(LENGTH(participants)+LENGTH(subject)+LENGTH(snippet)) FROM threads"),
    ] {
        let bytes: i64 = conn.query_row(sql, [], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(0);
        println!("  {label:<34} {:>9.1} MB", bytes as f64 / 1e6);
    }

    for (label, sql) in [
        ("messages", "SELECT COUNT(*) FROM messages"),
        ("threads", "SELECT COUNT(*) FROM threads"),
        ("thread_labels", "SELECT COUNT(*) FROM thread_labels"),
        ("sync_message_labels", "SELECT COUNT(*) FROM sync_message_labels"),
    ] {
        let n: i64 = conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1);
        println!("  rows in {label:<25} {n:>9}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------

fn print_pragmas(db: &Db) -> Result<(), Box<dyn std::error::Error>> {
    let conn = db.writer();
    for pragma in [
        "journal_mode",
        "synchronous",
        "wal_autocheckpoint",
        "busy_timeout",
        "page_size",
        "cache_size",
        "foreign_keys",
    ] {
        let v: String = conn.pragma_query_value(None, pragma, |r| {
            r.get::<_, i64>(0)
                .map(|n| n.to_string())
                .or_else(|_| r.get::<_, String>(0))
        })?;
        println!("  {pragma:<20} {v}");
    }
    Ok(())
}

fn file_sizes(path: &Path) -> String {
    let mb = |p: PathBuf| {
        std::fs::metadata(p)
            .map(|m| m.len() as f64 / 1e6)
            .unwrap_or(0.0)
    };
    let wal = path.with_extension("sqlite3-wal");
    format!(
        "db {:.0} MB  wal {:.0} MB",
        mb(path.to_path_buf()),
        mb(wal)
    )
}
