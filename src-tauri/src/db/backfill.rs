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

use rusqlite::{Connection, Result};

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
}
