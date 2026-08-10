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
//! There are two stores here and the difference between them is the whole
//! lesson of this module.
//!
//! [`the_whole_thing_on_a_store_his_shape`] generates the store the first
//! version of eviction was measured against: 46 000 threads, 66 000 messages,
//! and **every message carrying a `text/plain` part**. It predicted 1.77 GB
//! reclaimed. On the owner's real store the first sweep evicted nine bodies and
//! freed nothing, because 12 481 of its 12 494 candidates had no `body_text` at
//! all and the guard refused every one of them.
//!
//! [`the_census_the_first_sweep_actually_met`] generates a store built from a
//! census of that mailbox instead — the same message count, the same 32 287 rows
//! with HTML, the same 12 494 old-and-large candidates, and 12 481 of those
//! HTML-only. It reports what the old rule would have freed and what the new one
//! does, side by side.

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

// ===========================================================================
// The store the sweep actually met
// ===========================================================================
//
// Every message in `generate()` above has a `text/plain` part, and no mailbox
// looks like that. This one is built from a census of the owner's:
//
// ```text
//   total messages                     66 092
//   with body_html                     32 287
//   body_html > 2048 bytes             27 000
//   older than 90 days                 49 685
//   old AND html > 2048                12 494   <- the candidates
//     ...of those, with NO body_text   12 481   <- refused by the old guard
//   MB of body_html, all messages       1 011
//   MB in old + big html                  472   <- the prize
//   MB of body_text, all messages         140
// ```

/// Old, large, and with no `text/plain` part. The rows the old guard refused.
const CENSUS_OLD_BIG_NO_TEXT: usize = 12_481;
/// Old, large, and carrying the sender's own text. All the first sweep could
/// reach, and the reason its log line said nine bodies and 0 MB.
const CENSUS_OLD_BIG_WITH_TEXT: usize = 13;
/// Large but inside the ninety-day window.
const CENSUS_NEW_BIG: usize = 14_506;
/// With an HTML part under the 2 KB floor.
const CENSUS_SMALL_HTML: usize = 5_287;
/// Plain-text mail, with no HTML part at all.
const CENSUS_NO_HTML: usize = 33_805;

/// Marketing HTML, shaped like the mail that has no `text/plain` alternative: a
/// stylesheet, a table of spacer rows, tracking URLs, and some prose.
///
/// The markup-to-prose ratio is what decides whether this exercise is worth
/// anything, so it is not left to a placeholder. A `<style>` block is in here
/// because deriving text has to be able to throw one away — indexed, it would be
/// a few kilobytes of selectors per message.
fn newsletter_html(bytes: usize, seed: usize) -> String {
    let mut out = String::with_capacity(bytes + 1_024);
    out.push_str(
        "<html><head><style type=\"text/css\">\
         body { margin:0; padding:0; background-color:#f4f4f4; -webkit-text-size-adjust:100%; }\
         .wrap { width:100%; max-width:600px; margin:0 auto; }\
         .btn a { display:inline-block; padding:12px 28px; border-radius:4px; }\
         @media only screen and (max-width:620px) { .wrap { width:100% !important; } \
         .stack { display:block !important; width:100% !important; } }\
         </style></head><body style=\"margin:0;padding:0;background-color:#f4f4f4\">\
         <table class=\"wrap\" role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">",
    );
    let mut block = 0usize;
    // Stop *before* overshooting rather than after, so the generated column
    // totals land on the census rather than a block above it.
    let mut room = bytes.saturating_sub(out.len() + 128);
    while room > 900 {
        block += 1;
        let before = out.len();
        out.push_str(&format!(
            "<tr><td style=\"padding:18px 24px;font-family:Helvetica,Arial,sans-serif;\
             font-size:15px;line-height:23px;color:#333333;background-color:#ffffff;\
             border-bottom:1px solid #eeeeee\">\
             <img src=\"https://cdn.example.com/i/{seed}/{block}.png\" width=\"552\" height=\"180\" \
             style=\"display:block;border:0;outline:none;text-decoration:none\" alt=\"Item {block}\">\
             <p style=\"margin:12px 0 6px;font-weight:600;font-size:17px\">Headline {block}</p>\
             <p style=\"margin:0 0 12px\">The quarterly numbers are attached and the pangolin \
             invoice for item {seed} is still overdue.</p>\
             <span class=\"btn\"><a href=\"https://links.example.com/t/\
             {seed:08x}{block:04x}?utm_source=newsletter&amp;utm_medium=email&amp;utm_campaign=weekly\" \
             style=\"background-color:#2b6cb0;color:#ffffff;text-decoration:none\">Read more</a>\
             </span></td></tr>\
             <tr><td style=\"height:18px;line-height:18px;font-size:0\">&nbsp;</td></tr>"
        ));
        room = room.saturating_sub(out.len() - before);
    }
    out.push_str(
        "</table><img src=\"https://open.example.com/p.gif\" width=\"1\" height=\"1\" \
         style=\"display:none\"></body></html>",
    );
    out
}

/// An HTML part under the sweep's 2 KB floor — a short reply, which is what most
/// mail with a small `body_html` is.
fn small_html(seed: usize) -> String {
    format!(
        "<div dir=\"ltr\"><p>Thanks — item {seed} is fine by me. The pangolin \
         invoice is the only one still open.</p><p>Tawny</p></div>"
    )
}

/// Roughly 2 KB of sender-written plain text, which is what the owner's
/// `body_text` column averages.
fn census_text(seed: usize) -> String {
    let mut out = String::with_capacity(2_200);
    out.push_str(&format!(
        "Thread {seed}. The quarterly numbers are attached and the pangolin \
         invoice is still overdue.\n\n"
    ));
    for n in 0..24 {
        out.push_str(&format!(
            "Point {n}: the schedule for item {seed} moved a week and the \
             invoice against it has not been raised yet.\n"
        ));
    }
    out
}

/// Build a store matching the census. Returns it and the ids of some
/// HTML-only messages, so the search defect can be measured on known rows.
fn generate_census() -> (Store, Vec<i64>) {
    let mut path = std::env::temp_dir();
    path.push(format!("mach-evict-census-{}.sqlite3", std::process::id()));
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

    let old = NOW - 400 * DAY_MS;
    let recent = NOW - 20 * DAY_MS;
    // 472 MB across the 12 494 candidates; 531 MB across the 14 506 large
    // bodies inside the window. Both round to the census totals.
    let candidate_bytes = 472_000_000 / (CENSUS_OLD_BIG_NO_TEXT + CENSUS_OLD_BIG_WITH_TEXT);
    let recent_bytes = 531_000_000 / CENSUS_NEW_BIG;

    // (count, html bytes, has text, old) per bucket. The old-message total is
    // 49 685: the 12 494 candidates, every small-HTML row, and 31 904 of the
    // plain-text rows.
    let buckets: [(usize, usize, bool, bool); 6] = [
        (CENSUS_OLD_BIG_NO_TEXT, candidate_bytes, false, true),
        (CENSUS_OLD_BIG_WITH_TEXT, candidate_bytes, true, true),
        (CENSUS_NEW_BIG, recent_bytes, true, false),
        (CENSUS_SMALL_HTML, 1_500, true, true),
        (31_904, 0, true, true),
        (CENSUS_NO_HTML - 31_904, 0, true, false),
    ];

    let mut html_only_ids: Vec<i64> = Vec::new();
    let mut seed = 0usize;
    for (count, bytes, has_text, is_old) in buckets {
        let date = if is_old { old } else { recent };
        for chunk in (0..count).collect::<Vec<_>>().chunks(500) {
            let mut conn = db.writer();
            let tx = conn.transaction().expect("tx");
            for _ in chunk {
                seed += 1;
                let thread_id = q::upsert_thread(
                    &tx,
                    &NewThread {
                        account_id,
                        gmail_thread_id: format!("t{seed:08x}"),
                        participants: vec![Participant::new("sender@example.com")],
                        subject: format!("Subject {seed}"),
                        snippet: "…".into(),
                        last_message_at: date,
                        is_unread: false,
                        message_count: 1,
                        has_attachments: false,
                        // One in twenty in the trash, which the guard refuses
                        // whatever its text situation.
                        label_ids: if seed % 20 == 7 {
                            vec!["TRASH".into()]
                        } else {
                            vec!["INBOX".into()]
                        },
                    },
                )
                .expect("thread");
                // One in 500 is something Gmail cannot give back.
                let (gmail_id, is_draft) = match seed % 500 {
                    11 => (format!("{DRAFT_ID_PREFIX}{seed}"), true),
                    23 => (format!("{OUTBOX_ID_PREFIX}{seed}"), false),
                    37 => (format!("{seed:08x}"), true),
                    _ => (format!("{seed:08x}"), false),
                };
                let id = q::upsert_message(
                    &tx,
                    &NewMessage {
                        thread_id,
                        account_id,
                        gmail_message_id: gmail_id,
                        from: Participant::new("sender@example.com"),
                        to: vec![Participant::new("owner@example.com")],
                        subject: format!("Subject {seed}"),
                        body_html: match bytes {
                            0 => None,
                            n if n < 2_048 => Some(small_html(seed)),
                            n => Some(newsletter_html(n, seed)),
                        },
                        body_text: has_text.then(|| census_text(seed)),
                        snippet: "…".into(),
                        internal_date: date,
                        is_draft,
                        ..Default::default()
                    },
                )
                .expect("message");
                if !has_text && html_only_ids.len() < 200 && seed % 500 > 40 && seed % 20 != 7 {
                    html_only_ids.push(id);
                }
            }
            tx.commit().expect("commit");
        }
    }

    {
        let conn = db.writer();
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    println!(
        "generated {seed} messages in {:.1}s — {} main, {} wal",
        started.elapsed().as_secs_f64(),
        mb(file_bytes(&path)),
        mb(wal_bytes(&path))
    );

    (Store { path, db }, html_only_ids)
}

/// One number out of the store.
fn count(db: &Db, sql: &str) -> i64 {
    db.read(|conn| Ok(conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)))
        .expect("read")
}

#[test]
#[ignore = "builds a 1.2 GB store; run with --ignored"]
fn the_census_the_first_sweep_actually_met() {
    let (store, html_only_ids) = generate_census();
    let db = &store.db;
    let path = store.path.clone();
    let cutoff = NOW - 90 * DAY_MS;

    // --- the shape, confirmed against the census -------------------------
    println!("\n--- the store ---");
    for (label, sql) in [
        ("total messages", "SELECT count(*) FROM messages".to_string()),
        (
            "with body_html",
            "SELECT count(*) FROM messages WHERE body_html IS NOT NULL".to_string(),
        ),
        (
            "body_html > 2048",
            "SELECT count(*) FROM messages WHERE length(body_html) > 2048".to_string(),
        ),
        (
            "older than 90 days",
            format!("SELECT count(*) FROM messages WHERE internal_date < {cutoff}"),
        ),
        (
            "old AND html > 2048",
            format!(
                "SELECT count(*) FROM messages
                  WHERE internal_date < {cutoff} AND length(body_html) > 2048"
            ),
        ),
        (
            "  ...with no body_text",
            format!(
                "SELECT count(*) FROM messages
                  WHERE internal_date < {cutoff} AND length(body_html) > 2048
                    AND (body_text IS NULL OR trim(body_text) = '')"
            ),
        ),
    ] {
        println!("{label:<26} {:>8}", count(db, &sql));
    }
    let html_mb = count(db, "SELECT coalesce(sum(length(body_html)), 0) FROM messages") as u64;
    let prize_mb = count(
        db,
        &format!(
            "SELECT coalesce(sum(length(body_html)), 0) FROM messages
              WHERE internal_date < {cutoff} AND length(body_html) > 2048"
        ),
    ) as u64;
    let text_mb = count(db, "SELECT coalesce(sum(length(body_text)), 0) FROM messages") as u64;
    println!("{:<26} {:>8}", "MB of body_html", mb(html_mb));
    println!("{:<26} {:>8}", "MB in old + big html", mb(prize_mb));
    println!("{:<26} {:>8}", "MB of body_text", mb(text_mb));

    // --- what the old rule would have done -------------------------------
    //
    // The whole of the first version's guard, written as SQL: everything the
    // sweep would offer, less everything it refused. The `body_text` clause is
    // the one that mattered.
    let old_rule = format!(
        "SELECT count(*), coalesce(sum(length(m.body_html)), 0) FROM messages m
          WHERE m.body_html IS NOT NULL
            AND m.is_draft = 0
            AND m.internal_date < {cutoff}
            AND length(m.body_html) >= 2048
            AND m.gmail_message_id <> ''
            AND m.gmail_message_id NOT LIKE 'mach-%'
            AND m.body_text IS NOT NULL AND trim(m.body_text) <> ''
            AND NOT EXISTS (SELECT 1 FROM thread_labels tl
                             WHERE tl.thread_id = m.thread_id
                               AND tl.gmail_label_id IN ('TRASH', 'SPAM'))"
    );
    let (before_bodies, before_bytes): (i64, i64) = db
        .read(|conn| Ok(conn.query_row(&old_rule, [], |r| Ok((r.get(0)?, r.get(1)?)))?))
        .expect("old rule");
    println!("\n--- before: refuse anything with no body_text ---");
    println!(
        "{before_bodies} bodies, {} freed",
        mb(before_bytes as u64)
    );

    // --- and what this one does ------------------------------------------
    let searchable_before = count(
        db,
        "SELECT count(*) FROM messages
          WHERE body_html IS NOT NULL AND (body_text IS NULL OR trim(body_text) = '')",
    );

    let before_file = file_bytes(&path);
    let swept = Instant::now();
    let report = evict::sweep(db, NOW, &EvictionPolicy::default()).expect("sweep");
    let sweep_secs = swept.elapsed().as_secs_f64();

    println!("\n--- after: derive the text, then drop the HTML ---");
    println!(
        "{} bodies, {} freed — {} of HTML dropped less {} of derived text written",
        report.evicted,
        mb(report.bytes_freed),
        mb(report.bytes_freed + report.bytes_written),
        mb(report.bytes_written)
    );
    println!(
        "{} of them had no text until this sweep wrote one; {:.1}s",
        report.derived, sweep_secs
    );
    for (reason, n) in &report.kept {
        println!("  kept {n:>6}  {}", reason.as_str());
    }

    // Nothing unrecoverable went, at this scale either.
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM messages
              WHERE body_html IS NULL AND html_evicted_at IS NOT NULL
                AND (is_draft = 1 OR gmail_message_id LIKE 'mach-%' OR gmail_message_id = '')"
        ),
        0,
        "something unrecoverable was evicted"
    );
    // And nothing was left holding neither.
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM messages
              WHERE body_html IS NULL AND html_evicted_at IS NOT NULL
                AND (body_text IS NULL OR trim(body_text) = '')"
        ),
        0,
        "a message was left with neither its HTML nor any text"
    );
    // The point of the exercise. The old rule reached a handful of rows on this
    // store; this one reaches almost every candidate, and almost every candidate
    // needed its text writing first.
    assert!(
        report.derived > 11_000,
        "only {} rows had their text derived",
        report.derived
    );
    assert!(
        report.bytes_freed > 20 * before_bytes as u64,
        "{} freed against the old rule's {}",
        mb(report.bytes_freed),
        mb(before_bytes as u64)
    );

    // --- what search can find now ----------------------------------------
    let searchable_after = count(
        db,
        "SELECT count(*) FROM messages WHERE body_text_derived_at IS NOT NULL",
    );
    println!("\n--- search ---");
    println!(
        "{searchable_before} messages had HTML and no indexed body; \
         {searchable_after} of them now have one"
    );
    let hits = db
        .read(|conn| q::search_thread_summaries(conn, "pangolin", 500))
        .expect("search");
    assert!(
        !hits.is_empty(),
        "a phrase that was only ever in the HTML is findable"
    );
    // On known rows, not only in aggregate.
    for id in html_only_ids.iter().take(20) {
        let text: Option<String> = db
            .read(|conn| {
                Ok(conn
                    .query_row("SELECT body_text FROM messages WHERE id = ?1", [*id], |r| {
                        r.get(0)
                    })
                    .expect("row"))
            })
            .expect("read");
        let text = text.expect("an HTML-only message gained text");
        assert!(text.contains("pangolin"), "{text}");
        assert!(!text.contains("max-width"), "the stylesheet is in it: {text}");
    }

    // --- the file --------------------------------------------------------
    let reclaimed = evict::reclaim(db).expect("vacuum");
    println!(
        "\nvacuum: {:.1}s — file {} → {}",
        reclaimed.elapsed.as_secs_f64(),
        mb(before_file),
        mb(file_bytes(&path))
    );
}
