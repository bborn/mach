//! One-time repairs to rows that were stored wrong.
//!
//! Distinct from migrations, which change the *shape* of the database. These
//! change the *content* of rows that a since-fixed bug wrote badly, and they
//! need real code rather than SQL — the snippet repair below decodes HTML
//! entities, which cannot be expressed as nested `REPLACE()` calls without
//! reintroducing the very double-decoding bug it exists to undo (see
//! `render::entities`).
//!
//! Each repair records its own completion in `preferences` under a `mach.`
//! key, so it runs once and costs nothing afterwards. That table is the prefs
//! store, but `parsePreferences` on the frontend reads a fixed whitelist of
//! keys and ignores everything else, so an internal marker is invisible there.
//! The alternative — a marker table — would mean a schema migration for
//! bookkeeping, which is a poor trade.
//!
//! # Two shapes of repair
//!
//! [`decode_snippets`] takes a connection and does the whole job in one
//! transaction, because its unit of work is a string function over a column and
//! the whole of it on the owner's store is a few hundred milliseconds.
//!
//! [`derive_search_text`] takes the [`Db`] instead. Its unit of work is an
//! HTML sanitize per row, which is a millisecond each, and it has thousands of
//! rows to do; one transaction over all of them would hold the single writer
//! for the length of the job and put every keystroke behind it, which is the
//! exact stall [`Db::write_background`] was written to fix. So it batches, and
//! between batches it lets go.

use rusqlite::{Connection, Result};

use crate::db::Db;
use crate::render::entities;

/// Marks the snippet repair done. Namespaced so it cannot collide with a
/// user-facing preference key.
const SNIPPETS_DECODED: &str = "mach.snippetsDecoded";

/// Rewrites message snippets that still hold raw HTML entities.
///
/// Gmail returns `snippet` HTML-encoded even though it is plain text, and
/// nothing decoded it until `sync::convert` started doing so. Every message
/// synced before that has an inbox row reading `Sure I&#39;m free Thursday`;
/// on the owner's store that was 18,909 of 61,132 messages, so waiting for
/// natural re-sync would have left a third of the mailbox looking broken more
/// or less forever.
///
/// Returns the number of rows rewritten, or 0 if the repair had already run.
///
/// Safe to interrupt: the marker is written in the same transaction as the
/// updates, so a crash halfway leaves the work to be redone rather than
/// half-recorded. Safe to re-run in any case, because decoding is only applied
/// where it changes something and `render::entities` never re-reads its own
/// output.
pub fn decode_snippets(conn: &mut Connection) -> Result<usize> {
    if already_done(conn, SNIPPETS_DECODED)? {
        return Ok(0);
    }

    // Both tables, and `threads` is the one that matters: a conversation row
    // carries its own copy of the last message's snippet (see
    // `sync_queries::recompute_thread`), and that copy is what the inbox
    // actually renders. Repairing only `messages` would fix the column nobody
    // looks at and leave the list exactly as wrong as before.
    let mut rewritten = 0;
    let tx = conn.transaction()?;
    for table in ["messages", "threads"] {
        rewritten += decode_column(&tx, table)?;
    }
    mark_done(&tx, SNIPPETS_DECODED)?;
    tx.commit()?;

    Ok(rewritten)
}

/// Decodes the `snippet` column of one table. Returns rows actually changed.
fn decode_column(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<usize> {
    // Only rows that could possibly contain an entity — on a large mailbox
    // that excludes most of the table. `table` is one of two literals above,
    // never user input, so formatting it into the SQL is sound.
    let candidates: Vec<(i64, String)> = {
        let mut stmt =
            tx.prepare(&format!("SELECT id, snippet FROM {table} WHERE snippet LIKE '%&%'"))?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>>>()?
    };

    let mut update = tx.prepare(&format!("UPDATE {table} SET snippet = ?1 WHERE id = ?2"))?;
    let mut rewritten = 0usize;
    for (id, snippet) in candidates {
        let decoded = entities::decode(&snippet);
        // "Ben & Jerry's" decodes to itself; writing it back would dirty a page
        // for nothing.
        if decoded != snippet {
            update.execute(rusqlite::params![decoded, id])?;
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

// ---------------------------------------------------------------------------
// what the markup says
// ---------------------------------------------------------------------------

/// Marks the search-text repair done.
const SEARCH_TEXT_DERIVED: &str = "mach.searchTextDerived";

/// The floor on how much markup is worth reading. The same one
/// [`crate::evict`] uses, and the same one sync applies at ingest.
const MIN_HTML_BYTES: i64 = 2048;

/// Rows read per pass, and rows written per transaction.
///
/// The read is the wider of the two because it is a scan the writer is not
/// waiting on. 200 rows of write is a few hundred milliseconds of sanitizing
/// done *before* the transaction opens and a few hundred UPDATEs inside it,
/// which is well under the five-second `busy_timeout` anything else would wait.
const READ_BATCH: usize = 500;
const WRITE_BATCH: usize = 200;

/// Fills `search_text` for the mail that arrived before sync started filling it.
///
/// # What these rows are missing
///
/// `messages_fts` indexes `subject`, `body_text` and `search_text` (migration
/// 23). The third column is the readable text of `body_html`, and it is what
/// makes a message findable by what its markup says rather than only by what
/// its sender put in the `text/plain` part. Sync writes it for every message it
/// stores; every message stored before that has it NULL, and is findable by
/// half of itself.
///
/// On the owner's store 14 349 rows have resident HTML over the floor, and
/// 12 689 of them carry at least one word their plain part does not. Deriving
/// all of them costs 15 s of CPU and stores 87 MB of text.
///
/// # Why it takes the `Db` and not a connection
///
/// Deriving text is a sanitize per row, around a millisecond, and there are
/// thousands of rows. In one transaction that is fifteen seconds of the single
/// writer, and every interactive write — a keystroke that archives a
/// conversation, a draft autosave — queues behind it. So:
///
///  * the scan and the derivation run on pooled readers, which block nothing;
///  * the writes go through [`Db::write_background`], which stands aside for
///    any interactive writer already queued, at each batch boundary;
///  * a batch is [`WRITE_BATCH`] rows, so the longest anything waits is one
///    batch of UPDATEs rather than the whole job.
///
/// This is the discipline the eviction sweep runs on, for the same reason, and
/// the shape is the same as [`decode_snippets`] where it can be: a `mach.`
/// marker in `preferences`, written once at the end.
///
/// # Interruption
///
/// Safe. The marker is written only once the scan has run out of rows, so a
/// crash halfway means the work is redone rather than half-recorded. Redoing it
/// is cheap and cannot be wrong: the same HTML yields the same text, and a row
/// this pass already filled is excluded by `search_text IS NULL` on the next.
///
/// Stopping it running for ever is the marker's real job. The rows whose markup
/// carries nothing new stay NULL — 1 660 of them on that store — and without
/// the marker every boot would derive them again to reach the same answer.
///
/// Returns the number of rows filled, or 0 if the repair had already run.
pub fn derive_search_text(db: &Db) -> crate::db::Result<usize> {
    if db.read(|conn| Ok(already_done(conn, SEARCH_TEXT_DERIVED)?))? {
        return Ok(0);
    }

    let mut filled = 0usize;
    let mut cursor = 0i64;
    loop {
        // A keyset cursor rather than an offset, for the reason the sweep uses
        // one: this pass mutates rows the predicate selects, so an offset would
        // skip a row for every one it filled.
        let batch: Vec<(i64, Option<String>, String)> =
            db.read(|conn| Ok(candidates(conn, cursor)?))?;
        if batch.is_empty() {
            break;
        }
        cursor = batch.last().map(|(id, _, _)| *id).unwrap_or(cursor);
        let short = batch.len() < READ_BATCH;

        let mut pending: Vec<(i64, String)> = Vec::new();
        for (id, body_text, body_html) in batch {
            if let Some(derived) =
                crate::render::text::searchable_text(body_text.as_deref(), &body_html)
            {
                pending.push((id, derived));
            }
        }

        for chunk in pending.chunks(WRITE_BATCH) {
            filled += db.write_background(|conn| {
                // `search_text IS NULL` rather than a compare-and-swap on the
                // value: this only ever fills a column that is empty, so a sync
                // that wrote one between the read and here keeps what it wrote.
                let mut update = conn.prepare(
                    "UPDATE messages SET search_text = ?2
                      WHERE id = ?1 AND search_text IS NULL",
                )?;
                let mut n = 0usize;
                for (id, derived) in chunk {
                    n += update.execute(rusqlite::params![id, derived])?;
                }
                Ok(n)
            })?;
        }

        if short {
            break;
        }
    }

    db.write_background(|conn| {
        mark_done(conn, SEARCH_TEXT_DERIVED)?;
        Ok(())
    })?;
    Ok(filled)
}

/// The rows this repair will look at, narrowed as far as SQL can narrow them.
///
/// SQL cannot ask the question — whether a derivation says anything the plain
/// part does not is a question about words, and SQLite has no opinion about
/// words — so every clause here is only about not reading rows whose answer is
/// already known:
///
///  * `body_html IS NOT NULL`, because there is nothing to read otherwise. Rows
///    whose markup was evicted before this shipped are out of reach until
///    somebody opens one; [`crate::evict::refetch`] catches them then.
///  * `search_text IS NULL`, because a row that has one is done.
///  * `body_text_derived_at IS NULL`, because a row whose `body_text` the sweep
///    read out of this same markup already has it indexed, and a second copy
///    would put every one of its terms in twice.
///  * `length(body_html) >= 2048`. Under that there is not enough markup to be
///    saying anything the plain part is not.
///
/// A row with no `body_text` at all is included, and is the case this helps
/// most: it has nothing in the index but its subject until the eviction sweep
/// reaches it, which on age alone can be ninety days away.
fn candidates(conn: &Connection, after_id: i64) -> Result<Vec<(i64, Option<String>, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, body_text, body_html
           FROM messages
          WHERE id > ?1
            AND body_html IS NOT NULL
            AND length(body_html) >= ?2
            AND search_text IS NULL
            AND body_text_derived_at IS NULL
          ORDER BY id
          LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(
            rusqlite::params![after_id, MIN_HTML_BYTES, READ_BATCH as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

fn already_done(conn: &Connection, key: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM preferences WHERE key = ?1",
        [key],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn mark_done(conn: &Connection, key: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO preferences (key, value, updated_at) VALUES (?1, 'true', 0)",
        [key],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    /// A message row is mostly NOT NULL columns; this inserts the minimum.
    fn insert_message(conn: &Connection, id: i64, snippet: &str) {
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, created_at)
             VALUES (1, 'a@b.c', 'a', 0)
             ON CONFLICT(id) DO NOTHING",
            [],
        )
        .ok();
        conn.execute(
            "INSERT INTO threads (id, account_id, gmail_thread_id, snippet)
             VALUES (1, 1, 't1', ?1)
             ON CONFLICT(id) DO NOTHING",
            [snippet],
        )
        .expect("insert thread");
        conn.execute(
            "INSERT INTO messages (id, account_id, thread_id, gmail_message_id, snippet)
             VALUES (?1, 1, 1, ?2, ?3)",
            rusqlite::params![id, format!("m{id}"), snippet],
        )
        .expect("insert message");
    }

    #[test]
    fn rewrites_encoded_snippets_and_leaves_the_rest_alone() {
        let db = Db::open_in_memory().expect("db");
        let mut conn = db.writer();

        insert_message(&conn, 1, "Sure I&#39;m free Thursday");
        insert_message(&conn, 2, "Ben & Jerry's");
        insert_message(&conn, 3, "nothing here at all");

        // One message row plus the thread row that carries the same text.
        assert_eq!(decode_snippets(&mut conn).expect("backfill"), 2);

        let read = |id: i64| -> String {
            conn.query_row("SELECT snippet FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .expect("read")
        };
        assert_eq!(read(1), "Sure I'm free Thursday");
        // A bare ampersand is not an entity and must survive untouched.
        assert_eq!(read(2), "Ben & Jerry's");
        assert_eq!(read(3), "nothing here at all");
    }

    #[test]
    fn repairs_the_thread_row_the_inbox_actually_renders() {
        // `ThreadRow` shows `thread.snippet`, not the message's. Repairing only
        // `messages` would leave the list looking exactly as broken.
        let db = Db::open_in_memory().expect("db");
        let mut conn = db.writer();
        insert_message(&conn, 1, "Sure I&#39;m free Thursday");

        decode_snippets(&mut conn).expect("backfill");

        let thread: String = conn
            .query_row("SELECT snippet FROM threads WHERE id = 1", [], |r| r.get(0))
            .expect("read");
        assert_eq!(thread, "Sure I'm free Thursday");
    }

    #[test]
    fn runs_once_and_then_costs_nothing() {
        let db = Db::open_in_memory().expect("db");
        let mut conn = db.writer();
        insert_message(&conn, 1, "a &amp; b");

        // The message row and the thread row that mirrors it.
        assert_eq!(decode_snippets(&mut conn).expect("first"), 2);
        // The marker, not the absence of work, is what stops it: a snippet that
        // legitimately contains "&amp;" as text would otherwise be rewritten on
        // every single boot.
        assert_eq!(decode_snippets(&mut conn).expect("second"), 0);
    }

    #[test]
    fn is_idempotent_if_the_marker_is_lost() {
        let db = Db::open_in_memory().expect("db");
        let mut conn = db.writer();
        insert_message(&conn, 1, "Sure I&#39;m free");

        decode_snippets(&mut conn).expect("first");
        conn.execute("DELETE FROM preferences WHERE key = ?1", [SNIPPETS_DECODED])
            .expect("clear marker");
        decode_snippets(&mut conn).expect("second");

        let snippet: String = conn
            .query_row("SELECT snippet FROM messages WHERE id = 1", [], |r| r.get(0))
            .expect("read");
        // Decoding an already-decoded snippet must not mangle it further.
        assert_eq!(snippet, "Sure I'm free");
    }

    // -----------------------------------------------------------------------
    // derive_search_text
    // -----------------------------------------------------------------------

    /// What the markup says and the plain part does not.
    const PROSE: &str = "Your armadillo brush ships Tuesday from the Leeds warehouse, \
         and the courier will leave it with a neighbour if nobody answers the door.";

    /// A plain part padded out with tracking URLs, the way marketing mail is.
    fn stub() -> String {
        let mut text = String::from("Your order is on its way");
        text.push_str(&" ".repeat(6000));
        for n in 0..40 {
            text.push_str(&format!(
                " https://click.example.test/t?token=dHJraWQ9NjIxMDk2NzMwfn{n}\
                 5+bGlua2lkPTQxNjY5MzUxMzV+fn5tZXRob2Q9bGlua35&hash=59ABB30F15A97766"
            ));
        }
        text
    }

    /// Markup padded past the 2 KB floor.
    fn markup(prose: &str) -> String {
        format!(
            "<html><body><table><tr><td><p>{prose}</p></td></tr></table>{}</body></html>",
            "<div style=\"padding:0\">&nbsp;</div>".repeat(80)
        )
    }

    fn insert_body(conn: &Connection, id: i64, text: Option<&str>, html: Option<&str>) {
        conn.execute(
            "INSERT INTO accounts (id, email, display_name, created_at)
             VALUES (1, 'a@b.c', 'a', 0)
             ON CONFLICT(id) DO NOTHING",
            [],
        )
        .ok();
        conn.execute(
            "INSERT INTO threads (id, account_id, gmail_thread_id, snippet)
             VALUES (1, 1, 't1', '…')
             ON CONFLICT(id) DO NOTHING",
            [],
        )
        .expect("thread");
        conn.execute(
            "INSERT INTO messages (id, account_id, thread_id, gmail_message_id,
                                   subject, snippet, body_text, body_html)
             VALUES (?1, 1, 1, ?2, 'Your order has shipped', '…', ?3, ?4)",
            rusqlite::params![id, format!("m{id}"), text, html],
        )
        .expect("message");
    }

    fn search_text(db: &Db, id: i64) -> Option<String> {
        db.read(|conn| {
            Ok(conn.query_row("SELECT search_text FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })?)
        })
        .expect("read")
    }

    fn hits(db: &Db, term: &str) -> i64 {
        db.read(|conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH ?1",
                [term],
                |r| r.get(0),
            )?)
        })
        .expect("search")
    }

    #[test]
    fn a_message_becomes_findable_by_what_its_markup_says() {
        // The whole point, asserted through the index rather than the column:
        // `armadillo` is in the markup and nowhere else.
        let db = Db::open_in_memory().expect("db");
        db.write(|conn| {
            insert_body(conn, 1, Some(&stub()), Some(&markup(PROSE)));
            Ok(())
        })
        .expect("seed");
        assert_eq!(hits(&db, "armadillo"), 0);

        assert_eq!(derive_search_text(&db).expect("backfill"), 1);

        assert!(search_text(&db, 1).expect("filled").contains("armadillo"));
        assert_eq!(hits(&db, "armadillo"), 1);
        // And what the sender wrote is still indexed, and still untouched.
        assert_eq!(hits(&db, "order"), 1);
        let body: Option<String> = db
            .read(|conn| {
                Ok(conn.query_row("SELECT body_text FROM messages WHERE id = 1", [], |r| {
                    r.get(0)
                })?)
            })
            .expect("read");
        assert_eq!(body, Some(stub()));
    }

    #[test]
    fn markup_that_repeats_the_plain_part_is_left_null() {
        let db = Db::open_in_memory().expect("db");
        db.write(|conn| {
            // Says exactly what the plain part says.
            insert_body(conn, 1, Some(PROSE), Some(&markup(PROSE)));
            // No markup at all.
            insert_body(conn, 2, Some(&stub()), None);
            // Markup under the 2 KB floor.
            insert_body(conn, 3, Some(&stub()), Some("<p>Ships Tuesday</p>"));
            Ok(())
        })
        .expect("seed");

        assert_eq!(derive_search_text(&db).expect("backfill"), 0);

        for id in 1..=3 {
            assert_eq!(search_text(&db, id), None, "row {id}");
        }
    }

    #[test]
    fn a_body_the_sweep_already_derived_is_not_stored_twice() {
        // Its `body_text` is this markup's text, indexed under that column. A
        // second copy would put every one of its terms in the index twice.
        let db = Db::open_in_memory().expect("db");
        db.write(|conn| {
            insert_body(conn, 1, Some(PROSE), Some(&markup(PROSE)));
            conn.execute(
                "UPDATE messages SET body_text_derived_at = 1 WHERE id = 1",
                [],
            )?;
            Ok(())
        })
        .expect("seed");

        assert_eq!(derive_search_text(&db).expect("backfill"), 0);
        assert_eq!(search_text(&db, 1), None);
    }

    #[test]
    fn a_message_with_no_plain_part_gets_all_of_it() {
        // Nothing of this message is indexed but its subject until the eviction
        // sweep reaches it, which on age alone can be ninety days.
        let db = Db::open_in_memory().expect("db");
        db.write(|conn| {
            insert_body(conn, 1, None, Some(&markup(PROSE)));
            Ok(())
        })
        .expect("seed");

        assert_eq!(derive_search_text(&db).expect("backfill"), 1);
        assert_eq!(hits(&db, "armadillo"), 1);
    }

    #[test]
    fn it_runs_once_and_then_costs_nothing() {
        let db = Db::open_in_memory().expect("db");
        db.write(|conn| {
            insert_body(conn, 1, Some(&stub()), Some(&markup(PROSE)));
            Ok(())
        })
        .expect("seed");

        assert_eq!(derive_search_text(&db).expect("first"), 1);
        assert_eq!(derive_search_text(&db).expect("second"), 0);
    }

    #[test]
    fn it_is_idempotent_if_the_marker_is_lost() {
        // The marker is written only once the scan runs out of rows, so a crash
        // halfway means the whole thing is redone. Redoing it has to be free of
        // consequence.
        let db = Db::open_in_memory().expect("db");
        db.write(|conn| {
            insert_body(conn, 1, Some(&stub()), Some(&markup(PROSE)));
            Ok(())
        })
        .expect("seed");

        derive_search_text(&db).expect("first");
        let after_first = search_text(&db, 1);
        db.write(|conn| {
            conn.execute("DELETE FROM preferences WHERE key = ?1", [SEARCH_TEXT_DERIVED])?;
            Ok(())
        })
        .expect("clear marker");

        assert_eq!(derive_search_text(&db).expect("second"), 0);
        assert_eq!(search_text(&db, 1), after_first);
        assert_eq!(hits(&db, "armadillo"), 1, "and not indexed twice");
    }

    #[test]
    fn the_scan_walks_past_rows_it_did_not_change() {
        // The cursor is a keyset over `id`, and a row left alone stays in the
        // predicate. A cursor that only advanced on a write would spin on the
        // first row it skipped.
        let db = Db::open_in_memory().expect("db");
        db.write(|conn| {
            let stub = stub();
            let html = markup(PROSE);
            for id in 1..=20 {
                // The odd rows say exactly what their markup says.
                let text = if id % 2 == 0 { stub.as_str() } else { PROSE };
                insert_body(conn, id, Some(text), Some(&html));
            }
            Ok(())
        })
        .expect("seed");

        assert_eq!(derive_search_text(&db).expect("backfill"), 10);
        for id in 1..=20 {
            assert_eq!(search_text(&db, id).is_some(), id % 2 == 0, "row {id}");
        }
    }
}
