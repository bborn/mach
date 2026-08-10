//! What eviction is worth on a store the size of the owner's, measured.
//!
//! `#[ignore]` — it builds a two-gigabyte database and vacuums it, which is
//! minutes and several gigabytes of disk, and neither belongs in `cargo test`.
//! Run it deliberately:
//!
//! ```sh
//! cargo test --test evict_scale -- --ignored --nocapture
//! ```
//!
//! The generated store is shaped like the real one rather than sized like it:
//! 46 000 threads, 66 000 messages, a body-size mix taken from what actually
//! arrives (a quarter of it newsletters, a third rich mail, the rest short
//! conversational HTML), and five per cent of it inside the ninety-day window.
//! Drafts, outbox rows and trash are salted through it so the guard is exercised
//! at scale and not only in the unit tests.

use std::path::PathBuf;
use std::time::Instant;

use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::Db;
use mach_lib::evict::{self, EvictionPolicy};
use mach_lib::ipc::render::render_message;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 1_800_000_000_000;

const THREADS: usize = 46_000;
const MESSAGES: usize = 66_000;

/// A body of `bytes` that does not compress to nothing and is not one repeated
/// byte, so page counts are honest.
fn html_of(bytes: usize, seed: usize) -> String {
    let cell = format!(
        "<td style=\"padding:8px;font-family:Helvetica\">item {seed} \
         <a href=\"https://example.com/{seed}\">details</a></td>"
    );
    let mut out = String::with_capacity(bytes + cell.len());
    out.push_str("<html><body><table>");
    while out.len() < bytes {
        out.push_str("<tr>");
        out.push_str(&cell);
        out.push_str("</tr>");
    }
    out.push_str("</table></body></html>");
    out
}

fn text_of(seed: usize) -> String {
    format!(
        "Item {seed}. The quarterly numbers are attached and the pangolin \
         invoice is still overdue. Details at example.com/{seed}."
    )
}

/// Bytes of HTML for message `n`, and how old it is.
fn shape(n: usize) -> (usize, i64) {
    let bytes = match n % 20 {
        0..=4 => 80_000,  // a quarter: newsletters and receipts
        5..=11 => 20_000, // a third: rich mail with images and tables
        _ => 3_000,       // the rest: short conversational HTML
    };
    // Five per cent inside the ninety-day window, the rest spread over six years.
    let age_days = if n % 20 == 19 {
        (n % 80) as i64
    } else {
        90 + (n % 2100) as i64
    };
    (bytes, NOW - age_days * DAY_MS)
}

struct Store {
    path: PathBuf,
    db: Db,
}

impl Drop for Store {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }
}

fn file_bytes(path: &PathBuf) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn wal_bytes(path: &PathBuf) -> u64 {
    let mut p = path.clone().into_os_string();
    p.push("-wal");
    file_bytes(&PathBuf::from(p))
}

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}

/// Build the store. Returns it plus the row ids of a resident and an evictable
/// message, so the open-time measurement has known subjects.
fn generate() -> (Store, Vec<i64>, Vec<i64>) {
    let mut path = std::env::temp_dir();
    path.push(format!("mach-evict-scale-{}.sqlite3", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }

    let db = Db::open(&path).expect("open");
    let started = Instant::now();

    let account_id = {
        let conn = db.writer();
        q::upsert_account(
            &conn,
            &NewAccount {
                email: "owner@example.com".into(),
                display_name: None,
                token_ref: "com.mach.mail.oauth".into(),
                colour_index: 0,
            },
        )
        .expect("account")
    };

    let mut old_ids = Vec::new();
    let mut recent_ids = Vec::new();
    let mut thread_ids: Vec<i64> = Vec::with_capacity(THREADS);

    // Threads first, in chunks, so one transaction is not the whole run.
    for chunk in (0..THREADS).collect::<Vec<_>>().chunks(2_000) {
        let mut conn = db.writer();
        let tx = conn.transaction().expect("tx");
        for n in chunk {
            let (_, date) = shape(*n);
            let id = q::upsert_thread(
                &tx,
                &NewThread {
                    account_id,
                    gmail_thread_id: format!("t{n:08x}"),
                    participants: vec![Participant::new("sender@example.com")],
                    subject: format!("Subject {n}"),
                    snippet: "…".into(),
                    last_message_at: date,
                    is_unread: false,
                    message_count: 1,
                    has_attachments: false,
                    // A twentieth in the trash, which the guard must refuse.
                    label_ids: if n % 20 == 7 {
                        vec!["TRASH".into()]
                    } else {
                        vec!["INBOX".into()]
                    },
                },
            )
            .expect("thread");
            thread_ids.push(id);
        }
        tx.commit().expect("commit");
    }

    for chunk in (0..MESSAGES).collect::<Vec<_>>().chunks(1_000) {
        let mut conn = db.writer();
        let tx = conn.transaction().expect("tx");
        for n in chunk {
            let (bytes, date) = shape(*n);
            let thread_id = thread_ids[n % thread_ids.len()];
            // One in every 500 is something Gmail cannot give back.
            let (gmail_id, is_draft) = match n % 500 {
                11 => (format!("{DRAFT_ID_PREFIX}{n}"), true),
                23 => (format!("{OUTBOX_ID_PREFIX}{n}"), false),
                37 => (format!("{n:08x}"), true),
                _ => (format!("{n:08x}"), false),
            };
            let id = q::upsert_message(
                &tx,
                &NewMessage {
                    thread_id,
                    account_id,
                    gmail_message_id: gmail_id,
                    from: Participant::new("sender@example.com"),
                    to: vec![Participant::new("owner@example.com")],
                    subject: format!("Subject {n}"),
                    body_html: Some(html_of(bytes, *n)),
                    body_text: Some(text_of(*n)),
                    snippet: "…".into(),
                    internal_date: date,
                    is_draft,
                    ..Default::default()
                },
            )
            .expect("message");
            if date < NOW - 90 * DAY_MS && bytes >= 20_000 && n % 500 > 40 && n % 20 != 7 {
                if old_ids.len() < 50 {
                    old_ids.push(id);
                }
            } else if date >= NOW - 90 * DAY_MS && recent_ids.len() < 50 {
                recent_ids.push(id);
            }
        }
        tx.commit().expect("commit");
    }

    // Fold the WAL back so the "before" number is the file and not the log.
    {
        let conn = db.writer();
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }

    println!(
        "generated {MESSAGES} messages / {THREADS} threads in {:.1}s — {} main, {} wal",
        started.elapsed().as_secs_f64(),
        mb(file_bytes(&path)),
        mb(wal_bytes(&path))
    );

    (Store { path, db }, old_ids, recent_ids)
}

#[test]
#[ignore = "builds a 2 GB store; run with --ignored"]
fn the_whole_thing_on_a_store_his_shape() {
    let (store, old_ids, recent_ids) = generate();
    let db = &store.db;
    let path = store.path.clone();

    let before_file = file_bytes(&path);
    let counted: i64 = db
        .read(|conn| {
            Ok(conn
                .query_row("SELECT sum(length(body_html)) FROM messages", [], |r| {
                    r.get(0)
                })
                .expect("sum"))
        })
        .expect("read");
    println!("html stored: {}", mb(counted as u64));

    // --- opening a resident message ---------------------------------------
    let resident_us = time_opens(db, &old_ids);

    // --- the sweep --------------------------------------------------------
    let policy = EvictionPolicy::default();
    let swept = Instant::now();
    let report = evict::sweep(db, NOW, &policy).expect("sweep");
    let sweep_secs = swept.elapsed().as_secs_f64();

    println!(
        "sweep: {:.1}s — examined {}, evicted {}, {} of HTML dropped",
        sweep_secs,
        report.examined,
        report.evicted,
        mb(report.bytes_freed)
    );
    for (reason, n) in &report.kept {
        println!("  kept {n:>6}  {}", reason.as_str());
    }

    // Nothing unrecoverable went, at this scale either.
    let survivors: i64 = db
        .read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT count(*) FROM messages
                      WHERE body_html IS NULL AND html_evicted_at IS NOT NULL
                        AND (is_draft = 1 OR gmail_message_id LIKE 'mach-%' OR gmail_message_id = '')",
                    [],
                    |r| r.get(0),
                )
                .expect("count"))
        })
        .expect("read");
    assert_eq!(survivors, 0, "something unrecoverable was evicted");

    let after_sweep = evict::free_space(db).expect("space");
    let after_sweep_file = file_bytes(&path);
    println!(
        "after the sweep: file {} (was {}), wal {}, free list {}",
        mb(after_sweep_file),
        mb(before_file),
        mb(wal_bytes(&path)),
        mb(after_sweep.reclaimable_bytes() as u64)
    );

    // --- opening an evicted message ---------------------------------------
    let evicted_us = time_opens(db, &old_ids);
    let recent_us = time_opens(db, &recent_ids);

    println!(
        "open: resident {resident_us} µs · evicted (text, what the reader sees now) \
         {evicted_us} µs · a message that was never evicted {recent_us} µs"
    );

    // --- the vacuum -------------------------------------------------------
    let reclaimed = evict::reclaim(db).expect("vacuum");
    let final_file = file_bytes(&path);
    println!(
        "vacuum: {:.1}s — file {} → {} ({} returned), wal {}",
        reclaimed.elapsed.as_secs_f64(),
        mb(before_file),
        mb(final_file),
        mb(before_file.saturating_sub(final_file)),
        mb(wal_bytes(&path))
    );

    // And search still works on the vacuumed store.
    let found = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 5))
        .expect("search");
    assert!(!found.is_empty(), "the index survived the whole run");
}

/// Median microseconds to render one message body, over the sample.
fn time_opens(db: &Db, ids: &[i64]) -> u128 {
    let mut times: Vec<u128> = Vec::with_capacity(ids.len());
    for id in ids {
        let started = Instant::now();
        let _ = render_message(db, *id, false).expect("render");
        times.push(started.elapsed().as_micros());
    }
    times.sort_unstable();
    times.get(times.len() / 2).copied().unwrap_or(0)
}
