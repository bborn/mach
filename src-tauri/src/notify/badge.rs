//! The number on the Dock icon.
//!
//! # It counts the inbox, not the store
//!
//! The obvious query is "threads where `is_unread`", and it is wrong in a way
//! that shows up the first time somebody selects everything and archives it: an
//! archived conversation is still unread. Gmail's own unread count means
//! *unread in the inbox*, every mail client on the platform means that, and
//! anything else produces a badge that says 43 over an empty inbox — a number
//! the app cannot explain and the user cannot clear.
//!
//! So the count joins `thread_labels` and asks for `INBOX`, minus the bulk
//! tabs. Inbox in the window is Primary — promotions and updates live under
//! Folders — and a badge of 2 over an inbox with nothing unread is the same
//! lie as counting archived mail. Archiving, trashing and Gmail's own filters
//! all take `INBOX` off the thread, which means the badge falls the moment
//! the store does, with no separate bookkeeping to get out of step.
//!
//! # When it is recomputed
//!
//! On every `threads-changed` — the event the IPC layer already emits after a
//! sync pass writes and after any command touches a thread — plus once when the
//! app becomes ready, and once immediately after a notification. That covers
//! every path that can move the number, without this module having to know what
//! any of those paths are.
//!
//! The recompute is a single indexed count against a pooled reader, which never
//! blocks on the sync loop's writer, so doing it eagerly is cheaper than
//! working out whether it was needed.

use rusqlite::Connection;

use crate::db::Result as DbResult;

use super::{badge_enabled, host::Host, rule::BULK_CATEGORIES, rule::INBOX};

/// Unread conversations still in the inbox the window shows: not Promotions,
/// Social, Updates or Forums.
///
/// Deliberately not filtered by the per-account notification setting: muting an
/// account means "do not interrupt me for it", not "pretend its mail is not
/// there". The badge is a count, and a count that silently omitted a mailbox
/// would be the app lying rather than being quiet.
pub fn unread_in_inbox(conn: &Connection) -> DbResult<i64> {
    let bulk = BULK_CATEGORIES
        .iter()
        .map(|c| format!("'{c}'"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(conn.query_row(
        &format!(
            "SELECT count(*)
               FROM threads t
               JOIN thread_labels l ON l.thread_id = t.id
              WHERE t.is_unread = 1 AND l.gmail_label_id = ?1
                AND NOT EXISTS (
                  SELECT 1 FROM thread_labels b
                   WHERE b.thread_id = t.id
                     AND b.gmail_label_id IN ({bulk})
                )"
        ),
        [INBOX],
        |row| row.get(0),
    )?)
}

/// Recompute and apply the badge. Never fails; a store that will not answer
/// leaves the badge alone rather than clearing it to a number it does not know.
pub fn refresh(host: &dyn Host) {
    let Some(db) = host.db() else { return };

    let wanted = db.read(|conn| {
        if !badge_enabled(conn)? {
            return Ok(0);
        }
        unread_in_inbox(conn)
    });

    let Ok(count) = wanted else { return };
    // `None` rather than `Some(0)`: on macOS a zero badge draws a "0" bubble,
    // which is not what "nothing is waiting" looks like.
    host.set_badge((count > 0).then_some(count));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    /// Unread in the inbox, unread but archived, read in the inbox, and an
    /// unread Update still sitting in the inbox — the last is waiting, just
    /// not in the view the badge is for.
    fn seed(db: &Db) {
        db.write(|conn| {
            conn.execute_batch(
                "INSERT INTO accounts (id, email) VALUES (1, 'alex@example.com');
                 INSERT INTO threads (id, account_id, gmail_thread_id, is_unread)
                      VALUES (1, 1, 't1', 1), (2, 1, 't2', 1), (3, 1, 't3', 0), (4, 1, 't4', 1);
                 INSERT INTO thread_labels (thread_id, gmail_label_id)
                      VALUES (1, 'INBOX'), (3, 'INBOX'), (2, 'Receipts'),
                             (4, 'INBOX'), (4, 'CATEGORY_UPDATES');",
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn only_unread_mail_still_in_the_inbox_counts() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        assert_eq!(
            db.read(unread_in_inbox).unwrap(),
            1,
            "archived unread and inbox Updates do not badge"
        );
    }

    #[test]
    fn archiving_everything_takes_the_badge_to_zero() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        db.write(|conn| {
            conn.execute("DELETE FROM thread_labels WHERE gmail_label_id = 'INBOX'", [])?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            db.read(unread_in_inbox).unwrap(),
            0,
            "the threads are still unread; they are no longer waiting"
        );
    }
}
