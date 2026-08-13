//! The rows: writing stances, reading the fresh ones, counting what happened.
//!
//! Every function here takes a connection and returns a value, so the whole
//! store is drivable from a test with a hand-seeded database and no engine, no
//! network and no clock.
//!
//! # Staleness is a comparison, not a sweep
//!
//! A suggestion row names the message it answers. It is fresh only while that
//! message is still the newest thing in its conversation — so a reply from the
//! correspondent, a message he sends by any other means, and a draft he starts
//! himself all invalidate it by the same rule, with no bookkeeping to forget.
//!
//! The consequence is that a stale row is never *shown*, whether or not anything
//! has got round to deleting it. [`purge_stale`] deletes them because a table
//! that only grows is a table that eventually matters; correctness does not
//! depend on it having run.

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Result as DbResult;

use super::prompt::Stance;

/// A stored set of stances, as the reading pane gets them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub thread_id: i64,
    /// The message these answer.
    pub message_id: i64,
    pub stances: Vec<Stance>,
    pub model: String,
    pub created_at: i64,
}

/// What can be recorded about a set of stances. Deliberately small: five
/// numbers answer "is this feature paying for itself", and the sixth would be
/// a number nobody acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A set was written to disk. The denominator.
    Suggested,
    /// He pressed one and got a composer.
    Picked,
    /// He sent it approximately as it arrived.
    SentAsWritten,
    /// He sent it after rewriting a substantial part.
    SentEdited,
    /// He chose to write it himself instead.
    Dismissed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Suggested => "suggested",
            Outcome::Picked => "picked",
            Outcome::SentAsWritten => "sentAsWritten",
            Outcome::SentEdited => "sentEdited",
            Outcome::Dismissed => "dismissed",
        }
    }

    pub fn parse(value: &str) -> Option<Outcome> {
        match value {
            "suggested" => Some(Outcome::Suggested),
            "picked" => Some(Outcome::Picked),
            "sentAsWritten" => Some(Outcome::SentAsWritten),
            "sentEdited" => Some(Outcome::SentEdited),
            "dismissed" => Some(Outcome::Dismissed),
            _ => None,
        }
    }
}

/// The counters, and the one ratio worth looking at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    pub suggested: i64,
    pub picked: i64,
    pub sent_as_written: i64,
    pub sent_edited: i64,
    pub dismissed: i64,
}

impl Counters {
    /// Sent roughly as written, over everything suggested — the number that
    /// decides whether this feature is worth its cost. `None` before anything
    /// has been suggested, because zero out of zero is not zero.
    pub fn as_written_rate(&self) -> Option<f64> {
        if self.suggested <= 0 {
            return None;
        }
        Some(self.sent_as_written as f64 / self.suggested as f64)
    }
}

/// Write a set of stances for a thread, replacing whatever was there.
///
/// Replacing rather than appending: the newest message is the question, and a
/// second answer to an older one is a stale row wearing a fresh row's clothes.
pub fn save(
    conn: &Connection,
    account_id: i64,
    thread_id: i64,
    message_id: i64,
    gmail_message_id: &str,
    stances: &[Stance],
    model: &str,
    now_ms: i64,
) -> DbResult<()> {
    let json = serde_json::to_string(stances).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT INTO reply_suggestions
             (thread_id, account_id, message_id, gmail_message_id, stances, model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (thread_id) DO UPDATE SET
             account_id       = excluded.account_id,
             message_id       = excluded.message_id,
             gmail_message_id = excluded.gmail_message_id,
             stances          = excluded.stances,
             model            = excluded.model,
             created_at       = excluded.created_at",
        params![
            thread_id,
            account_id,
            message_id,
            gmail_message_id,
            json,
            model,
            now_ms
        ],
    )?;
    Ok(())
}

/// The stances for a conversation, if they still answer its newest message.
///
/// The `NOT EXISTS` is the whole staleness rule: any message in the thread that
/// is newer than the one the row names — by date, then by id, which is the same
/// order the reading pane uses — means the question has changed. Drafts are
/// included in that, so starting to write a reply by hand takes the row out of
/// the running as surely as receiving another message does.
pub fn fresh_for_thread(conn: &Connection, thread_id: i64) -> DbResult<Option<Suggestion>> {
    let row = conn
        .query_row(
            "SELECT s.thread_id, s.message_id, s.stances, s.model, s.created_at
               FROM reply_suggestions s
               JOIN messages anchor ON anchor.id = s.message_id
              WHERE s.thread_id = ?1
                AND NOT EXISTS (
                    SELECT 1 FROM messages newer
                     WHERE newer.thread_id = s.thread_id
                       AND ( newer.internal_date >  anchor.internal_date
                          OR (newer.internal_date =  anchor.internal_date
                              AND newer.id > anchor.id) )
                )",
            params![thread_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;

    let Some((thread_id, message_id, stances, model, created_at)) = row else {
        return Ok(None);
    };
    let stances: Vec<Stance> = serde_json::from_str(&stances).unwrap_or_default();
    if stances.is_empty() {
        return Ok(None);
    }
    Ok(Some(Suggestion {
        thread_id,
        message_id,
        stances,
        model,
        created_at,
    }))
}

/// Whether a thread already has a row, fresh or not. Asked before generating,
/// so a history sweep that reports the same message twice does not pay for it
/// twice.
pub fn exists_for_message(conn: &Connection, thread_id: i64, message_id: i64) -> DbResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM reply_suggestions WHERE thread_id = ?1 AND message_id = ?2
         )",
        params![thread_id, message_id],
        |row| row.get(0),
    )?)
}

/// Delete every row that no longer answers its thread's newest message.
///
/// Housekeeping, not correctness — [`fresh_for_thread`] already refuses to hand
/// a stale row back. Returns how many went.
pub fn purge_stale(conn: &Connection) -> DbResult<usize> {
    Ok(conn.execute(
        "DELETE FROM reply_suggestions
          WHERE thread_id IN (
              SELECT s.thread_id
                FROM reply_suggestions s
                LEFT JOIN messages anchor ON anchor.id = s.message_id
               WHERE anchor.id IS NULL
                  OR EXISTS (
                     SELECT 1 FROM messages newer
                      WHERE newer.thread_id = s.thread_id
                        AND ( newer.internal_date >  anchor.internal_date
                           OR (newer.internal_date =  anchor.internal_date
                               AND newer.id > anchor.id) )
                  )
          )",
        [],
    )?)
}

/// Take a conversation's stances away by hand — the picked-and-sent case, where
/// there is nothing left to suggest.
pub fn forget(conn: &Connection, thread_id: i64) -> DbResult<()> {
    conn.execute(
        "DELETE FROM reply_suggestions WHERE thread_id = ?1",
        params![thread_id],
    )?;
    Ok(())
}

/// Note what happened. `stance_index` and `stance_label` are absent for
/// `Suggested`, which is about a set rather than about one of them.
pub fn record(
    conn: &Connection,
    outcome: Outcome,
    stance_index: Option<i64>,
    stance_label: &str,
    now_ms: i64,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO reply_suggestion_outcomes (kind, stance_index, stance_label, at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![outcome.as_str(), stance_index, stance_label, now_ms],
    )?;
    Ok(())
}

/// Every counter, in one pass.
pub fn counters(conn: &Connection) -> DbResult<Counters> {
    let mut stmt =
        conn.prepare("SELECT kind, count(*) FROM reply_suggestion_outcomes GROUP BY kind")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut counters = Counters::default();
    for row in rows {
        let (kind, count) = row?;
        match Outcome::parse(&kind) {
            Some(Outcome::Suggested) => counters.suggested = count,
            Some(Outcome::Picked) => counters.picked = count,
            Some(Outcome::SentAsWritten) => counters.sent_as_written = count,
            Some(Outcome::SentEdited) => counters.sent_edited = count,
            Some(Outcome::Dismissed) => counters.dismissed = count,
            None => {}
        }
    }
    Ok(counters)
}

/// Which stance labels have been picked, most first. A handful, because this is
/// read to answer "which of these does he actually want" and a long tail of
/// once-each labels answers nothing.
pub fn winning_labels(conn: &Connection, limit: usize) -> DbResult<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT stance_label, count(*) AS n
           FROM reply_suggestion_outcomes
          WHERE kind = 'picked' AND stance_label <> ''
          GROUP BY lower(stance_label)
          ORDER BY n DESC, stance_label ASC
          LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute(
            "INSERT INTO accounts (id, email) VALUES (1, 'bruno@example.com')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, account_id, gmail_thread_id) VALUES (7, 1, 't7')",
            [],
        )
        .unwrap();
        conn
    }

    fn message(conn: &Connection, id: i64, at: i64) {
        conn.execute(
            "INSERT INTO messages (id, thread_id, account_id, gmail_message_id, internal_date)
             VALUES (?1, 7, 1, ?2, ?3)",
            params![id, format!("m{id}"), at],
        )
        .unwrap();
    }

    fn stances() -> Vec<Stance> {
        vec![
            Stance {
                label: "Say you'll be there".into(),
                body: "Tuesday works.".into(),
            },
            Stance {
                label: "Ask for a raincheck".into(),
                body: "Can we push it a week?".into(),
            },
        ]
    }

    #[test]
    fn stances_round_trip() {
        let conn = db();
        message(&conn, 100, 1_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "claude-sonnet-5", 5).unwrap();

        let found = fresh_for_thread(&conn, 7).unwrap().unwrap();
        assert_eq!(found.stances, stances());
        assert_eq!(found.message_id, 100);
        assert_eq!(found.model, "claude-sonnet-5");
        assert_eq!(found.created_at, 5);
    }

    #[test]
    fn a_thread_with_no_row_has_no_stances() {
        assert_eq!(fresh_for_thread(&db(), 7).unwrap(), None);
    }

    #[test]
    fn a_newer_message_makes_them_stale() {
        let conn = db();
        message(&conn, 100, 1_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        assert!(fresh_for_thread(&conn, 7).unwrap().is_some());

        message(&conn, 101, 2_000);
        assert_eq!(
            fresh_for_thread(&conn, 7).unwrap(),
            None,
            "a message arriving after the stances were written must retire them"
        );
    }

    #[test]
    fn a_reply_of_his_own_makes_them_stale() {
        let conn = db();
        message(&conn, 100, 1_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();

        // He answered another way. Same timestamp, higher id — the reading
        // pane's own tie-break, and the case a date comparison alone misses.
        message(&conn, 101, 1_000);
        assert_eq!(fresh_for_thread(&conn, 7).unwrap(), None);
    }

    #[test]
    fn a_draft_he_starts_himself_makes_them_stale() {
        let conn = db();
        message(&conn, 100, 1_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        conn.execute(
            "INSERT INTO messages (id, thread_id, account_id, gmail_message_id, internal_date, is_draft)
             VALUES (200, 7, 1, 'd200', 3000, 1)",
            [],
        )
        .unwrap();
        assert_eq!(fresh_for_thread(&conn, 7).unwrap(), None);
    }

    #[test]
    fn an_older_message_arriving_late_does_not_make_them_stale() {
        let conn = db();
        message(&conn, 100, 5_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        // A backfill filling in history behind the newest message is not news.
        message(&conn, 99, 1_000);
        assert!(fresh_for_thread(&conn, 7).unwrap().is_some());
    }

    #[test]
    fn saving_again_replaces_rather_than_duplicates() {
        let conn = db();
        message(&conn, 100, 1_000);
        message(&conn, 101, 2_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        save(
            &conn,
            1,
            7,
            101,
            "m101",
            &[Stance {
                label: "Say no".into(),
                body: "No.".into(),
            }],
            "m",
            9,
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM reply_suggestions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let found = fresh_for_thread(&conn, 7).unwrap().unwrap();
        assert_eq!(found.message_id, 101);
        assert_eq!(found.stances.len(), 1);
    }

    #[test]
    fn an_empty_stance_list_is_no_suggestion() {
        let conn = db();
        message(&conn, 100, 1_000);
        save(&conn, 1, 7, 100, "m100", &[], "m", 5).unwrap();
        assert_eq!(fresh_for_thread(&conn, 7).unwrap(), None);
    }

    #[test]
    fn purging_removes_the_stale_and_keeps_the_fresh() {
        let conn = db();
        conn.execute(
            "INSERT INTO threads (id, account_id, gmail_thread_id) VALUES (8, 1, 't8')",
            [],
        )
        .unwrap();
        message(&conn, 100, 1_000);
        conn.execute(
            "INSERT INTO messages (id, thread_id, account_id, gmail_message_id, internal_date)
             VALUES (300, 8, 1, 'm300', 1000)",
            [],
        )
        .unwrap();

        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        save(&conn, 1, 8, 300, "m300", &stances(), "m", 5).unwrap();
        message(&conn, 101, 9_000); // thread 7 moves on

        assert_eq!(purge_stale(&conn).unwrap(), 1);
        assert_eq!(fresh_for_thread(&conn, 7).unwrap(), None);
        assert!(fresh_for_thread(&conn, 8).unwrap().is_some());
    }

    #[test]
    fn deleting_the_thread_takes_the_row_and_leaves_the_history() {
        let conn = db();
        message(&conn, 100, 1_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        record(&conn, Outcome::Suggested, None, "", 1).unwrap();
        record(&conn, Outcome::Picked, Some(0), "Say you'll be there", 2).unwrap();

        conn.execute("DELETE FROM threads WHERE id = 7", []).unwrap();

        let rows: i64 = conn
            .query_row("SELECT count(*) FROM reply_suggestions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "the state goes with the conversation");
        let counters = counters(&conn).unwrap();
        assert_eq!(counters.suggested, 1, "the history does not");
        assert_eq!(counters.picked, 1);
    }

    #[test]
    fn forgetting_a_thread_removes_its_row() {
        let conn = db();
        message(&conn, 100, 1_000);
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        forget(&conn, 7).unwrap();
        assert_eq!(fresh_for_thread(&conn, 7).unwrap(), None);
    }

    #[test]
    fn a_row_for_this_message_is_not_written_twice() {
        let conn = db();
        message(&conn, 100, 1_000);
        assert!(!exists_for_message(&conn, 7, 100).unwrap());
        save(&conn, 1, 7, 100, "m100", &stances(), "m", 5).unwrap();
        assert!(exists_for_message(&conn, 7, 100).unwrap());
        assert!(!exists_for_message(&conn, 7, 101).unwrap());
    }

    #[test]
    fn the_counters_count() {
        let conn = db();
        for _ in 0..10 {
            record(&conn, Outcome::Suggested, None, "", 1).unwrap();
        }
        for _ in 0..4 {
            record(&conn, Outcome::Picked, Some(0), "Say yes", 2).unwrap();
        }
        record(&conn, Outcome::Picked, Some(1), "Say no", 2).unwrap();
        for _ in 0..3 {
            record(&conn, Outcome::SentAsWritten, Some(0), "Say yes", 3).unwrap();
        }
        record(&conn, Outcome::SentEdited, Some(1), "Say no", 3).unwrap();
        record(&conn, Outcome::Dismissed, None, "", 4).unwrap();

        let counters = counters(&conn).unwrap();
        assert_eq!(counters.suggested, 10);
        assert_eq!(counters.picked, 5);
        assert_eq!(counters.sent_as_written, 3);
        assert_eq!(counters.sent_edited, 1);
        assert_eq!(counters.dismissed, 1);
        assert_eq!(counters.as_written_rate(), Some(0.3));

        let winners = winning_labels(&conn, 5).unwrap();
        assert_eq!(winners[0], ("Say yes".to_string(), 4));
    }

    #[test]
    fn a_rate_needs_a_denominator() {
        assert_eq!(Counters::default().as_written_rate(), None);
    }

    #[test]
    fn every_outcome_round_trips_through_its_name() {
        for outcome in [
            Outcome::Suggested,
            Outcome::Picked,
            Outcome::SentAsWritten,
            Outcome::SentEdited,
            Outcome::Dismissed,
        ] {
            assert_eq!(Outcome::parse(outcome.as_str()), Some(outcome));
        }
        assert_eq!(Outcome::parse("nonsense"), None);
    }
}
