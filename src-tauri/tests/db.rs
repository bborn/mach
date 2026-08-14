//! Tests for the local SQLite store (U1).
//!
//! These are written against the public API of `mach_lib::db`. They use
//! temp-file databases (so WAL is real) except where an in-memory database is
//! explicitly the thing under test. No external crates: temp paths come from
//! `std::env::temp_dir()` with a process/counter-unique name and are removed on
//! drop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mach_lib::db::command_queries as cq;
use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::schema;
use mach_lib::db::Db;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp-file database that deletes itself (and its -wal/-shm siblings).
struct TempDb {
    path: PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "mach-db-test-{}-{}-{}.sqlite3",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).expect("open temp db");
        TempDb { path, db }
    }

    fn reopen(&self) -> Db {
        Db::open(&self.path).expect("reopen temp db")
    }
}

impl std::ops::Deref for TempDb {
    type Target = Db;
    fn deref(&self) -> &Db {
        &self.db
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }
}

fn account(db: &Db, email: &str, colour: i64) -> i64 {
    let conn = db.writer();
    q::upsert_account(
        &conn,
        &NewAccount {
            email: email.to_string(),
            display_name: Some(email.to_string()),
            token_ref: format!("keychain:{email}"),
            colour_index: colour,
        },
    )
    .expect("upsert account")
}

fn thread(db: &Db, account_id: i64, gmail_id: &str, subject: &str, at: i64) -> i64 {
    let conn = db.writer();
    q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: gmail_id.to_string(),
            participants: vec![Participant {
                name: Some("Tawny".into()),
                email: "tawny@example.com".into(),
            }],
            subject: subject.to_string(),
            snippet: format!("snippet of {subject}"),
            last_message_at: at,
            is_unread: true,
            message_count: 1,
            has_attachments: false,
            label_ids: vec!["INBOX".to_string()],
        },
    )
    .expect("upsert thread")
}

fn message(db: &Db, thread_id: i64, account_id: i64, gmail_id: &str, subject: &str, body: &str, at: i64) -> i64 {
    let conn = db.writer();
    q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: gmail_id.to_string(),
            rfc822_message_id: Some(format!("<{gmail_id}@example.com>")),
            reply_to: Vec::new(),
            in_reply_to: None,
            references: None,
            from: Participant {
                name: Some("Tawny".into()),
                email: "tawny@example.com".into(),
            },
            to: vec![Participant {
                name: None,
                email: "alex@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: subject.to_string(),
            body_html: Some(format!("<p>{body}</p>")),
            body_text: Some(body.to_string()),
            snippet: body.chars().take(40).collect(),
            internal_date: at,
            is_unread: true,
            is_draft: false,
            ..Default::default()
        },
    )
    .expect("upsert message")
}

// ---------------------------------------------------------------------------
// migrations
// ---------------------------------------------------------------------------

#[test]
fn migrations_apply_cleanly_on_a_fresh_db() {
    let t = TempDb::new("fresh");
    let conn = t.reader();

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, schema::LATEST_VERSION as i64);

    for table in [
        "accounts",
        "threads",
        "thread_labels",
        "messages",
        "labels",
        "attachments",
        "events",
        "calendars",
        "messages_fts",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "expected table/vtable `{table}` to exist");
    }
}

#[test]
fn migrations_are_idempotent() {
    let t = TempDb::new("idem");

    let account_id = account(&t, "a@example.com", 0);
    let thread_id = thread(&t, account_id, "t1", "Hello", 1_000);

    // Re-running the migration runner directly must be a no-op.
    {
        let mut conn = t.writer();
        schema::migrate(&mut conn).expect("second migrate");
        schema::migrate(&mut conn).expect("third migrate");
    }
    // Re-opening the database re-runs the runner too.
    let again = t.reopen();

    let conn = again.reader();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, schema::LATEST_VERSION as i64);

    // Data survived; nothing was dropped and recreated.
    let found = q::thread_with_messages(&conn, thread_id).unwrap();
    assert!(found.is_some(), "thread must survive re-migration");
}

#[test]
fn upgrading_deletes_the_retired_density_preference_and_leaves_the_rest() {
    // The upgrade path for a store written before the app settled on one thread
    // row. `density` has to go; every other setting in the same table has to
    // still be there afterwards.
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    // Named rather than derived from LATEST_VERSION. Migration 10 is the one
    // that drops `density`, and this used to say "everything before the newest",
    // which stopped meaning that the moment migration 11 was appended: the
    // store was brought up past the deletion before the row was even inserted.
    const DENSITY_WAS_RETIRED_IN: u32 = 10;

    for migration in schema::MIGRATIONS
        .iter()
        .filter(|m| m.version < DENSITY_WAS_RETIRED_IN)
    {
        let tx = conn.transaction().unwrap();
        tx.execute_batch(migration.sql).unwrap();
        tx.pragma_update(None, "user_version", migration.version)
            .unwrap();
        tx.commit().unwrap();
    }

    conn.execute(
        "INSERT INTO preferences (key, value, updated_at) VALUES
            ('density', '\"compact\"', 1),
            ('theme', '\"dark\"', 1)",
        [],
    )
    .unwrap();

    assert_eq!(schema::migrate(&mut conn).unwrap(), schema::LATEST_VERSION);

    let keys: Vec<String> = conn
        .prepare("SELECT key FROM preferences ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(keys, vec!["theme".to_string()]);
}

#[test]
fn an_older_database_gains_the_new_event_columns_without_losing_its_events() {
    // The upgrade path, not the fresh-install path: `ALTER TABLE` runs against a
    // populated `events`, and every new column has to read back as "we were
    // never told" rather than as a value nobody meant.
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let target = schema::MIGRATIONS
        .iter()
        .map(|m| m.version)
        .filter(|v| *v < schema::LATEST_VERSION)
        .max()
        .expect("a version before the newest");

    for migration in schema::MIGRATIONS.iter().filter(|m| m.version <= target) {
        let tx = conn.transaction().unwrap();
        tx.execute_batch(migration.sql).unwrap();
        tx.pragma_update(None, "user_version", migration.version)
            .unwrap();
        tx.commit().unwrap();
    }

    conn.execute(
        "INSERT INTO accounts (id, email, token_ref) VALUES (1, 'a@example.com', '')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (account_id, calendar_id, google_event_id, title, start_ts, end_ts)
         VALUES (1, 'primary', 'evt-1', 'Standup', 10, 20)",
        [],
    )
    .unwrap();

    assert_eq!(schema::migrate(&mut conn).unwrap(), schema::LATEST_VERSION);

    let events = q::events_in_range(&conn, 0, 100, None).unwrap();
    assert_eq!(events.len(), 1, "the row survived the ALTER");
    assert_eq!(events[0].title, "Standup");
    assert!(events[0].recurrence.is_empty());
    assert!(events[0].reminders.is_none());
    assert!(events[0].ical_uid.is_none());
    // Not `Some(false)`. "We do not know who organized this" must not read as
    // "you are not the organizer", or the first launch after the upgrade would
    // make every pre-existing event uneditable until the next sync.
    assert_eq!(events[0].organizer_self, None);
    assert_eq!(events[0].guests_can_modify, None);
    // Migration 7's columns, read on a row that predates them. A conference
    // nobody mentioned is `None` rather than an empty conference block, and an
    // unknown visibility is `None` rather than "default" — the store does not
    // get to invent Google's answers.
    assert!(events[0].conference.is_none());
    assert!(events[0].creator.is_none());
    assert!(events[0].attachments.is_empty());
    assert!(events[0].visibility.is_none());
    assert!(events[0].transparency.is_none());
    // No guest list was stored, so the addresses are projected into one. Both
    // are empty here; the projection is asserted against real attendees in
    // `a_guest_list_without_answers_is_projected_from_the_addresses`.
    assert!(events[0].guests.is_empty());
}

#[test]
fn a_guest_list_without_answers_is_projected_from_the_addresses() {
    // Every row written before migration 7 has addresses and no answers, and so
    // does every row a local guest-list edit has just touched. A reader must not
    // have to know which of the two columns holds the truth, so `guests` is
    // filled from `attendees` when it is `NULL` — with no `response`, which is
    // the honest way to say that nobody has told us.
    let db = Db::open_in_memory().unwrap();
    let account_id = q::upsert_account(
        &db.writer(),
        &NewAccount {
            email: "alex@example.com".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = db.writer();
    q::upsert_event(
        &conn,
        &NewEvent {
            account_id,
            calendar_id: "primary".into(),
            google_event_id: "evt-1".into(),
            title: "Standup".into(),
            start_ts: 10,
            end_ts: 20,
            attendees: vec![
                Participant {
                    name: Some("Tawny".into()),
                    email: "tawny@example.com".into(),
                },
                Participant::new("sean@offerlab.com"),
            ],
            status: "confirmed".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let events = q::events_in_range(&conn, 0, 100, None).unwrap();
    let guests = &events[0].guests;
    assert_eq!(guests.len(), 2);
    assert_eq!(guests[0].email, "tawny@example.com");
    assert_eq!(guests[0].name.as_deref(), Some("Tawny"));
    assert_eq!(guests[0].response, None, "no answer is not needsAction");
    assert!(!guests[1].organizer);
}

// ---------------------------------------------------------------------------
// calendar metadata (migration 6)
// ---------------------------------------------------------------------------

fn calendar(account_id: i64, calendar_id: &str) -> NewCalendar {
    NewCalendar {
        account_id,
        calendar_id: calendar_id.to_string(),
        selected: true,
        synced_at: 1_000,
        ..Default::default()
    }
}

#[test]
fn a_calendar_row_round_trips_every_field_google_sends() {
    let t = TempDb::new("calendar-round-trip");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    q::upsert_calendar(
        &conn,
        &NewCalendar {
            summary: Some("Ben — school".into()),
            summary_override: Some("Dad/Ben Schedule".into()),
            description: Some("Pickups".into()),
            time_zone: Some("America/Chicago".into()),
            color_id: Some("7".into()),
            background_color: Some("#9fe1e7".into()),
            foreground_color: Some("#000000".into()),
            access_role: Some("reader".into()),
            is_primary: false,
            ..calendar(a, "ben@group.calendar.google.com")
        },
    )
    .expect("upsert calendar");

    let rows = q::list_calendars(&conn, Some(a)).expect("list");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.summary.as_deref(), Some("Ben — school"));
    assert_eq!(row.background_color.as_deref(), Some("#9fe1e7"));
    assert_eq!(row.time_zone.as_deref(), Some("America/Chicago"));
    // The override is the name the user recognises, so it is the one `title`
    // answers with.
    assert_eq!(row.title(), Some("Dad/Ben Schedule"));
    assert!(!row.writable(), "a reader may not be offered an editor");
    assert!(row.selected);
    assert!(!row.deleted);
}

#[test]
fn an_unknown_access_role_stays_permissive() {
    // Silence, and anything Google invents after this was written, must mean
    // "not told" rather than "denied" — the same rule `organizer_self` follows.
    let t = TempDb::new("calendar-access");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    for (role, writable) in [
        (None, true),
        (Some("owner"), true),
        (Some("writer"), true),
        (Some("reader"), false),
        (Some("freeBusyReader"), false),
        (Some("somethingNew"), true),
    ] {
        q::upsert_calendar(
            &conn,
            &NewCalendar {
                access_role: role.map(str::to_string),
                ..calendar(a, "c@example.com")
            },
        )
        .unwrap();
        let rows = q::list_calendars(&conn, Some(a)).unwrap();
        assert_eq!(rows[0].writable(), writable, "role {role:?}");
    }
}

#[test]
fn resyncing_replaces_metadata_rather_than_accumulating_rows() {
    let t = TempDb::new("calendar-upsert");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    q::upsert_calendar(
        &conn,
        &NewCalendar {
            summary: Some("Old name".into()),
            description: Some("Once had one".into()),
            ..calendar(a, "team@group.calendar.google.com")
        },
    )
    .unwrap();
    q::upsert_calendar(
        &conn,
        &NewCalendar {
            summary: Some("New name".into()),
            synced_at: 2_000,
            ..calendar(a, "team@group.calendar.google.com")
        },
    )
    .unwrap();

    let rows = q::list_calendars(&conn, Some(a)).unwrap();
    assert_eq!(rows.len(), 1, "one calendar, one row");
    assert_eq!(rows[0].title(), Some("New name"));
    // Unlike an event upsert, a NULL here is a real answer: the description was
    // cleared in Google and must not be resurrected from the old row.
    assert_eq!(rows[0].description, None);
    assert_eq!(q::calendars_synced_at(&conn, a).unwrap(), Some(2_000));
}

#[test]
fn an_unsubscribed_calendar_is_tombstoned_rather_than_deleted() {
    let t = TempDb::new("calendar-tombstone");
    let a = account(&t, "a@example.com", 0);
    let conn = t.writer();

    q::upsert_calendar(&conn, &calendar(a, "kept@example.com")).unwrap();
    q::upsert_calendar(&conn, &calendar(a, "gone@group.calendar.google.com")).unwrap();

    let marked =
        q::tombstone_missing_calendars(&conn, a, &["kept@example.com".to_string()]).unwrap();
    assert_eq!(marked, 1);

    let rows = q::list_calendars(&conn, Some(a)).unwrap();
    assert_eq!(rows.len(), 2, "the row survives so its events keep a name");
    let gone = rows
        .iter()
        .find(|c| c.calendar_id.starts_with("gone"))
        .unwrap();
    assert!(gone.deleted);

    // Running it again marks nothing new, so a steady state does not churn.
    let again =
        q::tombstone_missing_calendars(&conn, a, &["kept@example.com".to_string()]).unwrap();
    assert_eq!(again, 0);
}

#[test]
fn calendars_belong_to_their_account_and_leave_with_it() {
    let t = TempDb::new("calendar-cascade");
    let a = account(&t, "a@example.com", 0);
    let b = account(&t, "b@example.com", 1);
    let conn = t.writer();

    q::upsert_calendar(&conn, &calendar(a, "shared@group.calendar.google.com")).unwrap();
    q::upsert_calendar(&conn, &calendar(b, "shared@group.calendar.google.com")).unwrap();
    // The same Google calendar subscribed from two accounts is two rows, because
    // the name and the colour are per-subscription.
    assert_eq!(q::list_calendars(&conn, None).unwrap().len(), 2);

    conn.execute("DELETE FROM accounts WHERE id = ?1", [b]).unwrap();
    let rows = q::list_calendars(&conn, None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account_id, a);
}

#[test]
fn pragmas_are_configured_for_a_desktop_app() {
    let t = TempDb::new("pragma");
    let conn = t.reader();

    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal.to_lowercase(), "wal");

    let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(fk, 1, "foreign keys must be on for every connection");

    let sync: i64 = conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
    assert_eq!(sync, 1, "synchronous = NORMAL (1) is the WAL sweet spot");

    // The automatic checkpoint runs inside the commit that crosses its
    // threshold, so at the default of 1000 pages it lands on one sync batch in
    // seven and turns an 11 ms batch into a 350 ms one. Checkpointing is
    // `Db::checkpoint_if_large`'s job now; this is only a backstop.
    let autockpt: i64 = conn
        .query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))
        .unwrap();
    assert!(
        autockpt >= 65536,
        "the automatic checkpoint must not be in the commit path: {autockpt}"
    );
}

// ---------------------------------------------------------------------------
// FTS5 triggers
// ---------------------------------------------------------------------------

#[test]
fn inserting_a_message_makes_it_findable() {
    let t = TempDb::new("fts-insert");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Invoice", 1_000);
    message(&t, th, a, "m1", "Invoice", "the quarterly velocipede statement", 1_000);

    let conn = t.reader();
    let hits = q::search_threads(&conn, "velocipede", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].thread_id, th);

    // Subject is indexed too.
    let hits = q::search_threads(&conn, "invoice", 10).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn updating_a_body_changes_what_matches() {
    let t = TempDb::new("fts-update");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    message(&t, th, a, "m1", "Subject", "original velocipede text", 1_000);

    // Same gmail message id => upsert path => UPDATE => trigger must reindex.
    message(&t, th, a, "m1", "Subject", "replaced with dirigible text", 1_000);

    let conn = t.reader();
    assert!(
        q::search_threads(&conn, "velocipede", 10).unwrap().is_empty(),
        "old term must be gone from the index"
    );
    assert_eq!(
        q::search_threads(&conn, "dirigible", 10).unwrap().len(),
        1,
        "new term must be in the index"
    );
}

#[test]
fn deleting_a_message_removes_it_from_the_index() {
    let t = TempDb::new("fts-delete");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    let m = message(&t, th, a, "m1", "Subject", "solitary velocipede", 1_000);

    {
        let conn = t.writer();
        q::delete_message(&conn, m).unwrap();
    }

    let conn = t.reader();
    assert!(q::search_threads(&conn, "velocipede", 10).unwrap().is_empty());
}

#[test]
fn cascading_a_thread_delete_also_clears_the_index() {
    // Deleting a thread cascades to its messages; the FTS delete trigger has to
    // fire for those cascade-deleted rows or the index silently drifts.
    let t = TempDb::new("fts-cascade");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    message(&t, th, a, "m1", "Subject", "cascading velocipede", 1_000);

    {
        let conn = t.writer();
        q::delete_thread(&conn, th).unwrap();
    }

    let conn = t.reader();
    assert!(
        q::search_threads(&conn, "velocipede", 10).unwrap().is_empty(),
        "FTS index must not outlive cascade-deleted messages"
    );
    let orphans: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(orphans, 0);

    // Ask the index itself, not the joined query: a join against `messages`
    // would hide a stale FTS entry rather than reveal it, and a drifting index
    // grows forever and starts scoring against text nobody can see.
    let stale: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'velocipede'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0, "messages_fts still holds terms for deleted rows");
}

#[test]
fn deleting_a_message_leaves_no_stale_index_entry() {
    let t = TempDb::new("fts-nostale");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    let m = message(&t, th, a, "m1", "Subject", "solitary velocipede", 1_000);
    {
        let conn = t.writer();
        q::delete_message(&conn, m).unwrap();
    }
    let conn = t.reader();
    let stale: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'velocipede'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0);
}

#[test]
fn search_results_are_ranked_subject_hits_first() {
    let t = TempDb::new("fts-rank");
    let a = account(&t, "a@example.com", 0);

    let body_hit = thread(&t, a, "t1", "Weekly notes", 1_000);
    message(&t, body_hit, a, "m1", "Weekly notes", "we discussed the velocipede at length", 1_000);

    let subject_hit = thread(&t, a, "t2", "Velocipede", 2_000);
    message(&t, subject_hit, a, "m2", "Velocipede", "see attached", 2_000);

    let conn = t.reader();
    let hits = q::search_threads(&conn, "velocipede", 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].thread_id, subject_hit,
        "a subject match outranks a body match"
    );
    assert!(hits[0].score <= hits[1].score, "scores must be ascending");
    assert_eq!(hits[0].account_id, a);
}

#[test]
fn fts_query_is_forgiving_of_user_input() {
    let t = TempDb::new("fts-input");
    let a = account(&t, "a@example.com", 0);
    let th = thread(&t, a, "t1", "Subject", 1_000);
    message(&t, th, a, "m1", "Subject", "quarterly velocipede statement", 1_000);

    let conn = t.reader();
    // Prefix matching, punctuation and FTS operators must not blow up.
    assert_eq!(q::search_threads(&conn, "veloci", 10).unwrap().len(), 1);
    assert_eq!(q::search_threads(&conn, "\"unbalanced", 10).unwrap().len(), 0);
    assert_eq!(q::search_threads(&conn, "AND OR NOT", 10).unwrap().len(), 0);
    assert_eq!(q::search_threads(&conn, "   ", 10).unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// unified cross-account list
// ---------------------------------------------------------------------------

#[test]
fn unified_list_interleaves_accounts_by_timestamp() {
    let t = TempDb::new("unified");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    let a3 = account(&t, "three@example.com", 2);

    thread(&t, a1, "t1", "oldest", 1_000);
    thread(&t, a2, "t2", "second", 2_000);
    thread(&t, a1, "t3", "third", 3_000);
    thread(&t, a3, "t4", "newest", 4_000);

    let conn = t.reader();
    let rows = q::list_threads(&conn, &ThreadQuery::default()).unwrap();

    let subjects: Vec<&str> = rows.iter().map(|r| r.subject.as_str()).collect();
    assert_eq!(subjects, vec!["newest", "third", "second", "oldest"]);

    // The per-account colour bar data rides along on the row.
    assert_eq!(rows[0].account_email, "three@example.com");
    assert_eq!(rows[0].account_colour_index, 2);
    assert_eq!(rows[3].account_email, "one@example.com");

    // Label refs come back with the row.
    assert_eq!(rows[0].label_ids, vec!["INBOX".to_string()]);
}

#[test]
fn unified_list_paginates_with_a_cursor() {
    let t = TempDb::new("paginate");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);

    for i in 0..10 {
        let acct = if i % 2 == 0 { a1 } else { a2 };
        thread(&t, acct, &format!("t{i}"), &format!("s{i}"), 1_000 + i as i64);
    }

    let conn = t.reader();

    let page1 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page1.len(), 4);
    assert_eq!(
        page1.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["s9", "s8", "s7", "s6"]
    );

    let page2 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            after: Some(page1.last().unwrap().cursor()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        page2.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["s5", "s4", "s3", "s2"]
    );

    let page3 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            after: Some(page2.last().unwrap().cursor()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        page3.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["s1", "s0"]
    );

    let page4 = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 4,
            after: Some(page3.last().unwrap().cursor()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(page4.is_empty(), "walking off the end returns empty, not an error");
}

#[test]
fn cursor_breaks_ties_on_identical_timestamps() {
    let t = TempDb::new("ties");
    let a = account(&t, "one@example.com", 0);
    for i in 0..5 {
        thread(&t, a, &format!("t{i}"), &format!("s{i}"), 7_000);
    }

    let conn = t.reader();
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = q::list_threads(
            &conn,
            &ThreadQuery {
                limit: 2,
                after: cursor,
                ..Default::default()
            },
        )
        .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = Some(page.last().unwrap().cursor());
        seen.extend(page.into_iter().map(|r| r.subject));
    }
    assert_eq!(seen.len(), 5, "no row skipped or repeated across pages");
    seen.sort();
    assert_eq!(seen, vec!["s0", "s1", "s2", "s3", "s4"]);
}

#[test]
fn list_can_be_filtered_to_one_account_and_one_label() {
    let t = TempDb::new("filter");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    thread(&t, a1, "t1", "from-one", 1_000);
    thread(&t, a2, "t2", "from-two", 2_000);

    // A thread that is not in the inbox.
    {
        let conn = t.writer();
        q::upsert_thread(
            &conn,
            &NewThread {
                account_id: a1,
                gmail_thread_id: "t3".into(),
                participants: vec![],
                subject: "archived".into(),
                snippet: String::new(),
                last_message_at: 3_000,
                is_unread: false,
                message_count: 1,
                has_attachments: false,
                label_ids: vec!["Label_7".into()],
            },
        )
        .unwrap();
    }

    let conn = t.reader();

    let only_a1 = q::list_threads(
        &conn,
        &ThreadQuery {
            account_id: Some(a1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        only_a1.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["archived", "from-one"]
    );

    let inbox = q::list_threads(
        &conn,
        &ThreadQuery {
            label_id: Some("INBOX".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        inbox.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["from-two", "from-one"]
    );

    let unread_in_a1 = q::list_threads(
        &conn,
        &ThreadQuery {
            account_id: Some(a1),
            unread_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        unread_in_a1.iter().map(|r| r.subject.clone()).collect::<Vec<_>>(),
        vec!["from-one"]
    );
}

// ---------------------------------------------------------------------------
// Archive and Snoozed: the mailboxes Gmail has no label for
// ---------------------------------------------------------------------------

/// A thread carrying exactly the labels given, rather than the helper's INBOX.
fn labelled_thread(db: &Db, account_id: i64, gmail_id: &str, subject: &str, at: i64, labels: &[&str]) -> i64 {
    let conn = db.writer();
    q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: gmail_id.to_string(),
            participants: vec![],
            subject: subject.to_string(),
            snippet: String::new(),
            last_message_at: at,
            is_unread: false,
            message_count: 1,
            has_attachments: false,
            label_ids: labels.iter().map(|l| l.to_string()).collect(),
        },
    )
    .expect("upsert thread")
}

fn subjects(rows: &[ThreadSummary]) -> Vec<String> {
    rows.iter().map(|r| r.subject.clone()).collect()
}

fn in_mailbox(db: &Db, label: &str) -> Vec<String> {
    let conn = db.reader();
    subjects(
        &q::list_threads(
            &conn,
            &ThreadQuery {
                label_id: Some(label.into()),
                ..Default::default()
            },
        )
        .unwrap(),
    )
}

/// Archive is the *absence* of a filing, so there is no row to seek to and it
/// had no query at all: `ARCHIVE` reached the store as an ordinary Gmail label
/// id, matched nothing, and the mailbox was empty for as long as it existed.
#[test]
fn archive_is_everything_filed_nowhere_else() {
    let t = TempDb::new("archive");
    let a = account(&t, "one@example.com", 0);

    labelled_thread(&t, a, "t1", "in the inbox", 1_000, &["INBOX"]);
    labelled_thread(&t, a, "t2", "archived, unlabelled", 2_000, &[]);
    labelled_thread(&t, a, "t3", "archived, under a label", 3_000, &["Label_7"]);
    labelled_thread(&t, a, "t4", "sent", 4_000, &["SENT"]);
    labelled_thread(&t, a, "t5", "spam", 5_000, &["SPAM"]);
    labelled_thread(&t, a, "t6", "trash", 6_000, &["TRASH"]);
    labelled_thread(&t, a, "t7", "drafted", 7_000, &["DRAFT"]);
    // Archived, then replied to and left in the inbox — still in the inbox.
    labelled_thread(&t, a, "t8", "replied, still filed", 8_000, &["INBOX", "Label_7"]);

    assert_eq!(
        in_mailbox(&t, "ARCHIVE"),
        vec!["archived, under a label", "archived, unlabelled"],
        "in the mailbox, and in none of the places that are somewhere else"
    );

    // And the mailboxes it is defined against still answer for themselves.
    assert_eq!(in_mailbox(&t, "INBOX"), vec!["replied, still filed", "in the inbox"]);
    assert_eq!(in_mailbox(&t, "SENT"), vec!["sent"]);
    assert_eq!(in_mailbox(&t, "TRASH"), vec!["trash"]);
}

/// The same asymmetry Drafts already has: `thread_labels` is rebuilt from the
/// per-message union every pass and loses a `DRAFT` row Google has not been
/// told about, so `messages.is_draft` is the durable half. Archive has to read
/// it too, or a conversation you are part-way through answering shows up in
/// both mailboxes.
#[test]
fn a_local_draft_keeps_a_thread_out_of_the_archive() {
    let t = TempDb::new("archive-draft");
    let a = account(&t, "one@example.com", 0);

    let drafting = labelled_thread(&t, a, "t1", "answering this", 1_000, &[]);
    labelled_thread(&t, a, "t2", "done with this", 2_000, &[]);

    {
        let conn = t.writer();
        q::upsert_message(
            &conn,
            &NewMessage {
                thread_id: drafting,
                account_id: a,
                gmail_message_id: "mach-draft:1".into(),
                subject: "answering this".into(),
                internal_date: 1_500,
                is_draft: true,
                ..Default::default()
            },
        )
        .unwrap();
    }

    assert_eq!(in_mailbox(&t, "ARCHIVE"), vec!["done with this"]);
    assert_eq!(in_mailbox(&t, "DRAFT"), vec!["answering this"]);
}

/// Snoozed has no Gmail label either — `Mach/Snoozed` is an ordinary user label
/// whose id differs on every account — so the mailbox reads the wake row, which
/// is the fact both halves of the design agree on.
#[test]
fn snoozed_reads_the_wake_row_and_leaves_the_archive_alone() {
    let t = TempDb::new("snoozed");
    let a = account(&t, "one@example.com", 0);

    let sleeping = labelled_thread(&t, a, "t1", "back on tuesday", 1_000, &["Label_112"]);
    labelled_thread(&t, a, "t2", "archived for good", 2_000, &[]);

    {
        let conn = t.writer();
        cq::ensure_command_schema(&conn).unwrap();
        cq::upsert_snooze(
            &conn,
            &cq::SnoozeRow {
                thread_id: sleeping,
                wake_at: 9_000,
                snoozed_at: 1_100,
                prior_label_ids: vec!["INBOX".into()],
                prior_is_unread: false,
            },
        )
        .unwrap();
    }

    assert_eq!(in_mailbox(&t, "SNOOZED"), vec!["back on tuesday"]);
    assert_eq!(
        in_mailbox(&t, "ARCHIVE"),
        vec!["archived for good"],
        "a conversation coming back is not one you filed away"
    );
}

/// Archive narrows with the account rail, exactly as Sent and Trash do.
#[test]
fn archive_narrows_to_one_account() {
    let t = TempDb::new("archive-account");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);

    labelled_thread(&t, a1, "t1", "one's", 1_000, &[]);
    labelled_thread(&t, a2, "t2", "two's", 2_000, &[]);

    let conn = t.reader();
    let rows = q::list_threads(
        &conn,
        &ThreadQuery {
            account_id: Some(a1),
            label_id: Some("ARCHIVE".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(subjects(&rows), vec!["one's"]);
}

/// Keyset pagination has to survive a mailbox whose membership is a predicate
/// rather than an index seek — the cursor is still the last row's, not a count.
#[test]
fn archive_paginates_with_the_same_cursor_as_every_other_mailbox() {
    let t = TempDb::new("archive-pages");
    let a = account(&t, "one@example.com", 0);
    for i in 0..10 {
        // Every other conversation is still in the inbox, so the pages have to
        // skip rows rather than walk a contiguous run.
        let labels: &[&str] = if i % 2 == 0 { &[] } else { &["INBOX"] };
        labelled_thread(&t, a, &format!("t{i}"), &format!("s{i}"), 1_000 + i as i64, labels);
    }

    let conn = t.reader();
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = q::list_threads(
            &conn,
            &ThreadQuery {
                label_id: Some("ARCHIVE".into()),
                limit: 2,
                after: cursor,
                ..Default::default()
            },
        )
        .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = Some(page.last().unwrap().cursor());
        seen.extend(page.into_iter().map(|r| r.subject));
    }
    assert_eq!(seen, vec!["s8", "s6", "s4", "s2", "s0"]);
}

/// What the empty list needs in order to tell "this mailbox is empty" from
/// "nothing has arrived yet".
#[test]
fn any_threads_says_whether_the_store_has_been_filled() {
    let t = TempDb::new("any-threads");
    {
        let conn = t.reader();
        assert!(!q::any_threads(&conn).unwrap());
    }
    let a = account(&t, "one@example.com", 0);
    thread(&t, a, "t1", "first", 1_000);
    let conn = t.reader();
    assert!(q::any_threads(&conn).unwrap());
}

// ---------------------------------------------------------------------------
// thread with messages
// ---------------------------------------------------------------------------

#[test]
fn thread_with_messages_returns_the_conversation_oldest_first() {
    let t = TempDb::new("thread-msgs");
    let a = account(&t, "one@example.com", 0);
    let th = thread(&t, a, "t1", "Re: lunch", 3_000);
    message(&t, th, a, "m3", "Re: lunch", "third", 3_000);
    message(&t, th, a, "m1", "lunch", "first", 1_000);
    message(&t, th, a, "m2", "Re: lunch", "second", 2_000);

    {
        let conn = t.writer();
        let m = q::thread_with_messages(&conn, th).unwrap().unwrap().messages[0].id;
        q::upsert_attachment(
            &conn,
            &NewAttachment {
                message_id: m,
                gmail_attachment_id: Some("att1".into()),
                filename: "menu.pdf".into(),
                mime_type: "application/pdf".into(),
                size_bytes: 4096,
                local_path: None,
            },
        )
        .unwrap();
    }

    let conn = t.reader();
    let found = q::thread_with_messages(&conn, th).unwrap().unwrap();
    assert_eq!(found.thread.subject, "Re: lunch");
    assert_eq!(
        found
            .messages
            .iter()
            .map(|m| m.body_text.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["first", "second", "third"]
    );
    assert_eq!(found.messages[0].attachments.len(), 1);
    assert_eq!(found.messages[0].attachments[0].filename, "menu.pdf");
    assert_eq!(found.messages[0].from.email, "tawny@example.com");
    assert_eq!(found.messages[0].to.len(), 1);

    assert!(q::thread_with_messages(&conn, 9_999).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// foreign keys
// ---------------------------------------------------------------------------

#[test]
fn foreign_keys_are_enforced() {
    let t = TempDb::new("fk-enforce");
    let a = account(&t, "one@example.com", 0);
    let conn = t.writer();
    let err = q::upsert_message(
        &conn,
        &NewMessage {
            thread_id: 4_242,
            account_id: a,
            gmail_message_id: "m1".into(),
            rfc822_message_id: None,
            reply_to: Vec::new(),
            in_reply_to: None,
            references: None,
            from: Participant::default(),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            subject: String::new(),
            body_html: None,
            body_text: None,
            snippet: String::new(),
            internal_date: 0,
            is_unread: false,
            is_draft: false,
            ..Default::default()
        },
    );
    assert!(err.is_err(), "orphan message must be rejected");
}

#[test]
fn deleting_a_thread_cascades_to_messages_and_attachments() {
    let t = TempDb::new("fk-thread");
    let a = account(&t, "one@example.com", 0);
    let th = thread(&t, a, "t1", "s", 1_000);
    let m = message(&t, th, a, "m1", "s", "body", 1_000);
    {
        let conn = t.writer();
        q::upsert_attachment(
            &conn,
            &NewAttachment {
                message_id: m,
                gmail_attachment_id: None,
                filename: "a.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 3,
                local_path: None,
            },
        )
        .unwrap();
        q::delete_thread(&conn, th).unwrap();
    }

    let conn = t.reader();
    for (table, expected) in [("threads", 0), ("messages", 0), ("attachments", 0), ("thread_labels", 0)] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, expected, "{table} should be empty after cascade");
    }
}

#[test]
fn deleting_an_account_cascades_to_everything_it_owns() {
    let t = TempDb::new("fk-account");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    let th1 = thread(&t, a1, "t1", "s1", 1_000);
    message(&t, th1, a1, "m1", "s1", "keepme", 1_000);
    let th2 = thread(&t, a2, "t2", "s2", 2_000);
    message(&t, th2, a2, "m2", "s2", "keepme", 2_000);

    {
        let conn = t.writer();
        q::upsert_label(
            &conn,
            &NewLabel {
                account_id: a1,
                gmail_label_id: "INBOX".into(),
                name: "Inbox".into(),
                label_type: LabelType::System,
            },
        )
        .unwrap();
        q::upsert_event(
            &conn,
            &NewEvent {
                account_id: a1,
                calendar_id: "primary".into(),
                google_event_id: "e1".into(),
                title: "standup".into(),
                description: None,
                location: None,
                start_ts: 10,
                end_ts: 20,
                is_all_day: false,
                attendees: vec![],
                rsvp_status: Some(RsvpStatus::Accepted),
                recurring_event_id: None,
                status: "confirmed".into(),
                html_link: None,
                updated_at: 0,
                ..Default::default()
            },
        )
        .unwrap();
        q::delete_account(&conn, a1).unwrap();
    }

    let conn = t.reader();
    let rows = q::list_threads(&conn, &ThreadQuery::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account_email, "two@example.com");

    for table in ["labels", "events"] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table} rows for the deleted account should be gone");
    }

    // The surviving account's message is still searchable; the deleted one is not.
    let hits = q::search_threads(&conn, "keepme", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].thread_id, th2);
}

// ---------------------------------------------------------------------------
// empty database edge cases
// ---------------------------------------------------------------------------

#[test]
fn an_empty_database_returns_empty_results_not_errors() {
    let t = TempDb::new("empty");
    let conn = t.reader();

    assert!(q::list_threads(&conn, &ThreadQuery::default()).unwrap().is_empty());
    assert!(q::list_threads(
        &conn,
        &ThreadQuery {
            account_id: Some(1),
            label_id: Some("INBOX".into()),
            unread_only: true,
            limit: 25,
            after: Some(ThreadCursor {
                last_message_at: 5,
                id: 5
            }),
        }
    )
    .unwrap()
    .is_empty());
    assert!(q::search_threads(&conn, "anything", 10).unwrap().is_empty());
    assert!(q::thread_with_messages(&conn, 1).unwrap().is_none());
    assert!(q::list_accounts(&conn).unwrap().is_empty());
    assert!(q::list_labels(&conn, None).unwrap().is_empty());
    assert!(q::events_in_range(&conn, 0, i64::MAX, None).unwrap().is_empty());
    assert!(q::unread_counts(&conn).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// accounts / labels / events
// ---------------------------------------------------------------------------

#[test]
fn account_upsert_is_stable_and_carries_the_history_watermark() {
    let t = TempDb::new("accounts");
    let id = account(&t, "one@example.com", 3);
    let same = account(&t, "one@example.com", 3);
    assert_eq!(id, same, "upsert by email must not create a second row");

    {
        let conn = t.writer();
        q::set_history_id(&conn, id, Some("987654321")).unwrap();
        q::set_calendar_sync_token(&conn, id, Some("tok-abc")).unwrap();
    }

    let conn = t.reader();
    let accounts = q::list_accounts(&conn).unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].history_id.as_deref(), Some("987654321"));
    assert_eq!(accounts[0].calendar_sync_token.as_deref(), Some("tok-abc"));
    assert_eq!(accounts[0].colour_index, 3);
    assert_eq!(accounts[0].token_ref, "keychain:one@example.com");
}

#[test]
fn events_in_range_spans_accounts_and_clips_to_the_window() {
    let t = TempDb::new("events");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    {
        let conn = t.writer();
        for (acct, id, title, start, end) in [
            (a1, "e1", "before", 0i64, 50i64),
            (a1, "e2", "inside", 200, 300),
            (a2, "e3", "overlaps-start", 90, 150),
            (a2, "e4", "after", 900, 950),
        ] {
            q::upsert_event(
                &conn,
                &NewEvent {
                    account_id: acct,
                    calendar_id: "primary".into(),
                    google_event_id: id.into(),
                    title: title.into(),
                    description: None,
                    location: None,
                    start_ts: start,
                    end_ts: end,
                    is_all_day: false,
                    attendees: vec![Participant {
                        name: None,
                        email: "x@example.com".into(),
                    }],
                    rsvp_status: None,
                    recurring_event_id: None,
                    status: "confirmed".into(),
                    html_link: None,
                    updated_at: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }

    let conn = t.reader();
    let window = q::events_in_range(&conn, 100, 400, None).unwrap();
    assert_eq!(
        window.iter().map(|e| e.title.clone()).collect::<Vec<_>>(),
        vec!["overlaps-start", "inside"]
    );
    assert_eq!(window[0].attendees.len(), 1);

    let only_a1 = q::events_in_range(&conn, 0, i64::MAX, Some(a1)).unwrap();
    assert_eq!(only_a1.len(), 2);
}

#[test]
fn unread_counts_are_reported_per_account() {
    let t = TempDb::new("unread");
    let a1 = account(&t, "one@example.com", 0);
    let a2 = account(&t, "two@example.com", 1);
    thread(&t, a1, "t1", "s", 1);
    thread(&t, a1, "t2", "s", 2);
    thread(&t, a2, "t3", "s", 3);
    {
        let conn = t.writer();
        q::set_thread_unread(&conn, 1, false).unwrap();
    }

    let conn = t.reader();
    let counts = q::unread_counts(&conn).unwrap();
    let get = |id: i64| counts.iter().find(|c| c.account_id == id).map(|c| c.unread).unwrap_or(0);
    assert_eq!(get(a1), 1);
    assert_eq!(get(a2), 1);
}

// ---------------------------------------------------------------------------
// connection handling
// ---------------------------------------------------------------------------

#[test]
fn in_memory_databases_work_for_tests() {
    let db = Db::open_in_memory().expect("open in-memory");
    let a = {
        let conn = db.writer();
        q::upsert_account(
            &conn,
            &NewAccount {
                email: "mem@example.com".into(),
                display_name: None,
                token_ref: "k".into(),
                colour_index: 0,
            },
        )
        .unwrap()
    };
    let conn = db.reader();
    assert_eq!(q::list_accounts(&conn).unwrap().len(), 1);
    assert_eq!(q::list_accounts(&conn).unwrap()[0].id, a);
}

#[test]
fn a_reader_sees_committed_writes_while_the_writer_keeps_working() {
    // The point of WAL: the UI's reads never queue behind the sync loop's
    // writes. Two threads, one Db handle, no deadlock, no "database is locked".
    let t = TempDb::new("concurrent");
    let a = account(&t, "one@example.com", 0);

    let writer_db = t.db.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..200 {
            let conn = writer_db.writer();
            q::upsert_thread(
                &conn,
                &NewThread {
                    account_id: a,
                    gmail_thread_id: format!("t{i}"),
                    participants: vec![],
                    subject: format!("s{i}"),
                    snippet: String::new(),
                    last_message_at: i as i64,
                    is_unread: false,
                    message_count: 1,
                    has_attachments: false,
                    label_ids: vec!["INBOX".into()],
                },
            )
            .expect("concurrent write");
        }
    });

    let mut max_seen = 0;
    for _ in 0..200 {
        let conn = t.reader();
        let rows = q::list_threads(&conn, &ThreadQuery::default()).unwrap();
        max_seen = max_seen.max(rows.len());
    }
    writer.join().unwrap();

    let conn = t.reader();
    let rows = q::list_threads(
        &conn,
        &ThreadQuery {
            limit: 500,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 200);
    let _ = max_seen;
}

#[test]
fn readers_cannot_write() {
    let t = TempDb::new("readonly");
    let conn = t.reader();
    let err = conn.execute(
        "INSERT INTO accounts (email, token_ref, colour_index, created_at) VALUES ('x', 'y', 0, 0)",
        [],
    );
    assert!(err.is_err(), "pool readers must be query_only");
}


// ---------------------------------------------------------------------------
// the sync loop must not hold the door shut
// ---------------------------------------------------------------------------

/// One batch of work of the shape the sync loop commits, against `threads`.
fn batch(db: &Db, tag: &str, rows: usize) {
    db.write_background(|conn| {
        for i in 0..rows {
            conn.execute(
                "UPDATE threads SET snippet = ?2 WHERE gmail_thread_id = ?1",
                rusqlite::params![format!("t{i}"), format!("{tag} {i}")],
            )?;
        }
        Ok(())
    })
    .expect("sync batch");
}

/// A user write completes promptly while a sync batch is in flight.
///
/// This is the property the store exists to provide, and until
/// `write_background` it did not hold. The sync loop's batches are short — a
/// median of 13 ms against the owner's mailbox — but it released the write
/// connection and immediately asked for it back, and `Mutex` hands the lock to
/// whoever asks, not to whoever has waited longest. On macOS the relocking
/// thread wins nearly every time, so a user command waited not for one batch
/// but for however many happened before it got lucky: measured against a
/// generated store of the owner's shape, p95 1.6 s and a worst case of 4.5 s
/// for a write whose own work is under a millisecond.
///
/// So the test reproduces that shape rather than describing it: a background
/// writer running batches back to back with no gap, and user writes going in
/// while they run. The assertion is a bound on the **worst** one, because a
/// median that passes while the tail is seconds long is exactly the bug.
#[test]
fn a_user_write_does_not_queue_behind_the_sync_loop() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const ROWS: usize = 400;

    let t = TempDb::new("writer-priority");
    let account_id = account(&t, "sync@example.com", 0);

    // Enough rows that a batch is real work rather than a no-op.
    {
        let conn = t.writer();
        for i in 0..ROWS {
            q::upsert_thread(
                &conn,
                &NewThread {
                    account_id,
                    gmail_thread_id: format!("t{i}"),
                    participants: vec![Participant::new("a@b.c")],
                    subject: format!("subject {i}"),
                    snippet: "x".repeat(200),
                    last_message_at: i as i64,
                    is_unread: false,
                    message_count: 1,
                    has_attachments: false,
                    label_ids: vec!["INBOX".into()],
                },
            )
            .expect("seed");
        }
    }

    // What one batch costs on this machine, so the budget is expressed in
    // batches rather than in a wall-clock number that would mean something
    // different on other hardware.
    let batch_ms = {
        let t0 = Instant::now();
        batch(&t.db, "calibrate", ROWS);
        t0.elapsed().as_secs_f64() * 1000.0
    };

    let stop = Arc::new(AtomicBool::new(false));
    let sync_db = t.db.clone();
    let sync_stop = Arc::clone(&stop);
    let sync = std::thread::spawn(move || {
        // Each batch times itself. The calibration above ran on an idle
        // machine; this runs while the user writes are being measured, on
        // whatever the machine is doing then. Deriving the budget from these
        // is what makes the assertion about *batches waited* rather than about
        // milliseconds, which is what it has always claimed to be — and what it
        // was not on a busy two-core CI runner, where it failed twice for want
        // of a threshold that could not follow the hardware down.
        let mut spent: Vec<f64> = Vec::new();
        while !sync_stop.load(Ordering::Relaxed) {
            let t0 = Instant::now();
            batch(&sync_db, "sync", ROWS);
            spent.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        spent
    });

    let mut waits: Vec<f64> = Vec::new();
    for i in 0..40 {
        let t0 = Instant::now();
        t.db.write(|conn| {
            conn.execute(
                "UPDATE threads SET is_unread = 1 WHERE gmail_thread_id = ?1",
                [format!("t{i}")],
            )?;
            Ok(())
        })
        .expect("user write");
        waits.push(t0.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(5));
    }

    stop.store(true, Ordering::Relaxed);
    let mut spent = sync.join().unwrap();
    let batches = spent.len() as u64;
    assert!(
        batches > 5,
        "the sync loop has to have actually been running: {batches} batches"
    );

    // The batch cost as it was *during* the measurement, not before it. The
    // median rather than the mean: one descheduled batch on a shared runner
    // should not buy the assertion slack it has not earned.
    spent.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let observed = spent[spent.len() / 2].max(batch_ms);

    waits.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let typical = waits[waits.len() * 9 / 10];
    let worst = *waits.last().unwrap();

    /*
     * Two assertions, because "a user write does not queue" is a claim about
     * the distribution and the old single `worst < 3 batches` could not express
     * it. On a two-core CI runner one of forty writes is descheduled by the OS
     * and lands at three and a half batches; that is the machine, not the lock,
     * and it failed the build twice for it.
     *
     * The regression this exists to catch is not subtle. Without the standoff a
     * user write waited *tens* of batches — the sync loop simply kept the writer
     * and the user queued behind all of it. So: nine writes in ten finish inside
     * three batches, which is the standoff working in the ordinary case, and
     * not one of them exceeds ten, which is far below the failure it guards and
     * far above anything the scheduler does to a single sample.
     */
    let typical_budget = (observed * 3.0).max(50.0);
    let worst_budget = (observed * 10.0).max(150.0);
    assert!(
        typical < typical_budget && worst < worst_budget,
        "user writes waited p90 {typical:.0}ms / worst {worst:.0}ms while the sync \
         loop ran (one batch is {observed:.1}ms under load, {batch_ms:.1}ms idle, \
          budgets {typical_budget:.0}ms / {worst_budget:.0}ms, {batches} batches ran)"
    );
}

/// The log is folded back into the database file rather than growing forever.
///
/// `wal_autocheckpoint` is supposed to do this and cannot: it is passive, so it
/// gives up whenever a reader is using the log, and this app's reader pool is
/// answering the UI continuously. Measured on a generated store with four
/// readers busy, the log grew linearly to 139 MB over 3,200 messages written and
/// never fell; the owner's had reached 814 MB.
#[test]
fn a_checkpoint_shrinks_the_write_ahead_log() {
    let t = TempDb::new("checkpoint");
    let account_id = account(&t, "wal@example.com", 0);

    let wal_len = || {
        let mut p = t.path.clone().into_os_string();
        p.push("-wal");
        std::fs::metadata(PathBuf::from(p))
            .map(|m| m.len())
            .unwrap_or(0)
    };

    // Hold a reader open across the writes, which is what stops the automatic
    // checkpoint from ever completing.
    let pinned = t.reader();
    for round in 0..40 {
        t.db.write_background(|conn| {
            for i in 0..200 {
                q::upsert_thread(
                    conn,
                    &NewThread {
                        account_id,
                        gmail_thread_id: format!("t{round}-{i}"),
                        participants: vec![Participant::new("a@b.c")],
                        subject: "s".repeat(400),
                        snippet: "x".repeat(400),
                        last_message_at: i,
                        is_unread: false,
                        message_count: 1,
                        has_attachments: false,
                        label_ids: vec!["INBOX".into()],
                    },
                )?;
            }
            Ok(())
        })
        .expect("write");
        let _ = q::list_threads(&pinned, &ThreadQuery::default());
    }
    let grown = wal_len();
    drop(pinned);

    // Below the threshold nothing happens, so a small log never costs a stall.
    assert!(
        !t.db.checkpoint_if_large(grown + 1).expect("checkpoint"),
        "a log under the threshold must be left alone"
    );
    assert_eq!(wal_len(), grown, "and left at its size");

    assert!(
        t.db.checkpoint_if_large(1).expect("checkpoint"),
        "a log over the threshold must be checkpointed"
    );
    assert!(
        wal_len() < grown,
        "the log should have shrunk: {grown} -> {}",
        wal_len()
    );

    // And the rows are all still there afterwards.
    let conn = t.reader();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM threads", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 40 * 200);
}

// ---------------------------------------------------------------------------
// which table a mailbox is driven from
// ---------------------------------------------------------------------------

/// The plan SQLite chose, as one line.
///
/// `list_threads` does not expose the SQL it built and has no reason to: the
/// question here is what the *engine* did, which `EXPLAIN QUERY PLAN` answers
/// and nothing else does. So the two spellings are reproduced below, and each
/// test also calls the real `list_threads` to show the chooser only changes the
/// plan and never the rows.
fn plan(db: &Db, sql: &str, label: &str) -> String {
    let conn = db.reader();
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare");
    let rows = stmt
        .query_map([label], |row| row.get::<_, String>(3))
        .expect("explain")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect");
    rows.join(" | ")
}

const STREAM_HEAD: &str =
    "SELECT t.id FROM threads t JOIN accounts a ON a.id = t.account_id WHERE 1 = 1 AND ";
const AS_EXISTS: &str = "EXISTS (SELECT 1 FROM thread_labels tl \
    WHERE tl.thread_id = t.id AND tl.gmail_label_id = ?1)";
const AS_IN: &str = "t.id IN (SELECT tl.thread_id FROM thread_labels tl \
    WHERE tl.gmail_label_id = ?1)";
const STREAM_TAIL: &str = " ORDER BY t.last_message_at DESC, t.id DESC LIMIT 50";

/// A mailbox holding a handful of threads must not cost a walk of the store.
///
/// This is the shape the owner's Inbox is: 49 conversations out of 47,328,
/// because he triages. The correlated `EXISTS` spelling makes that the most
/// expensive read in the app — SQLite tests every thread in the stream to find
/// the few that match — and it gets worse the tidier the mailbox gets. Measured
/// against his store: 17.3 ms, against 0.14 ms for the same rows gathered from
/// `idx_thread_labels_label` and sorted.
///
/// The assertion is on the plan rather than on a duration, because a duration
/// here would be a duration against a 200-row fixture, which is nothing either
/// way. What regressed is which table SQLite drives from, and that is what the
/// plan says.
#[test]
fn a_sparse_mailbox_is_gathered_from_the_label_not_scanned_from_the_stream() {
    let t = TempDb::new("sparse-mailbox");
    let a = account(&t.db, "owner@example.com", 0);
    for i in 0..200 {
        let id = thread(
            &t.db,
            a,
            &format!("g{i}"),
            &format!("subject {i}"),
            1_700_000_000_000 + i,
        );
        let labels: Vec<String> = if i < 3 {
            vec!["INBOX".into(), "CATEGORY_UPDATES".into()]
        } else {
            vec!["CATEGORY_UPDATES".into()]
        };
        let conn = t.db.writer();
        q::set_thread_labels(&conn, id, &labels).expect("labels");
    }

    let sparse = plan(&t.db, &format!("{STREAM_HEAD}{AS_IN}{STREAM_TAIL}"), "INBOX");
    assert!(
        sparse.contains("idx_thread_labels_label"),
        "the sparse spelling should drive from the label index: {sparse}"
    );

    let rows = t
        .db
        .read(|conn| {
            q::list_threads(
                conn,
                &ThreadQuery {
                    label_id: Some("INBOX".to_string()),
                    ..Default::default()
                },
            )
        })
        .expect("list");
    assert_eq!(rows.len(), 3, "and return exactly the inbox");

    let dense = t
        .db
        .read(|conn| {
            q::list_threads(
                conn,
                &ThreadQuery {
                    label_id: Some("CATEGORY_UPDATES".to_string()),
                    ..Default::default()
                },
            )
        })
        .expect("list");
    assert_eq!(dense.len(), 50, "a dense mailbox still pages at fifty");
}

/// A mailbox on most of the store must keep the plan it already had.
///
/// The `IN (SELECT …)` spelling sorts the label's whole thread set to return
/// fifty rows, so it is the wrong answer above a couple of thousand threads —
/// 41 ms against 0.10 ms for `CATEGORY_UPDATES` on the owner's store. Pinning
/// this is what stops a later "just always use the fast one" making the common
/// mailbox four hundred times slower.
#[test]
fn a_dense_mailbox_still_streams_from_the_thread_index() {
    let t = TempDb::new("dense-mailbox");
    let a = account(&t.db, "owner@example.com", 0);
    for i in 0..40 {
        let id = thread(
            &t.db,
            a,
            &format!("g{i}"),
            &format!("subject {i}"),
            1_700_000_000_000 + i,
        );
        let conn = t.db.writer();
        q::set_thread_labels(&conn, id, &["CATEGORY_UPDATES".to_string()]).expect("labels");
    }
    let p = plan(
        &t.db,
        &format!("{STREAM_HEAD}{AS_EXISTS}{STREAM_TAIL}"),
        "CATEGORY_UPDATES",
    );
    assert!(
        p.contains("idx_threads_stream"),
        "the dense spelling should stream the newest-first index: {p}"
    );
}

// ---------------------------------------------------------------------------
// the address book, and the index it was supposed to be reading
// ---------------------------------------------------------------------------

/// The address book must not read the message bodies to find out who sent them.
///
/// `idx_messages_sender` was built covering and measured at 1.28 s → 0.02 s. It
/// stopped covering when the query learned to keep the newest *name* beside each
/// address: `from_name` is selected and was not in the key, so SQLite went back
/// to `SCAN messages` — 211 ms, and 0.89 GB of message bodies pulled through the
/// page cache on every launch, with nothing in the app saying so.
///
/// Both halves are pinned, because they fail for different reasons: the sender
/// half when a column leaves the key, the recipient half when a function lands
/// around the column.
#[test]
fn the_address_book_never_touches_the_messages_table() {
    let t = TempDb::new("address-book-plan");
    let a = account(&t.db, "owner@example.com", 0);
    let th = thread(&t.db, a, "g1", "hello", 1_700_000_000_000);
    message(&t.db, th, a, "m1", "hello", "body", 1_700_000_000_000);

    let conn = t.db.reader();
    let explain = |sql: &str| -> String {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("prepare");
        stmt.query_map([], |row| row.get::<_, String>(3))
            .expect("explain")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect")
            .join(" | ")
    };

    let senders = explain("SELECT trim(lower(from_email)), from_name, internal_date FROM messages");
    assert!(
        senders.contains("COVERING INDEX idx_messages_sender"),
        "the sender half must be answered from the index alone: {senders}"
    );

    let recipients = explain(
        "SELECT json_extract(v.value, '$.email') FROM messages m, json_each(m.to_json) v \
          WHERE m.is_draft = 0 AND json_valid(m.to_json) \
            AND m.from_email COLLATE NOCASE IN (SELECT lower(email) FROM accounts)",
    );
    assert!(
        recipients.contains("SEARCH m USING INDEX idx_messages_sender"),
        "the recipient half must seek rather than scan: {recipients}"
    );
}

/// Folding case is what the `lower()` was there for, and the collation has to go
/// on doing it: 1,785 of the owner's 67,279 messages do not store the address
/// lowercased, and the ones that matter here are messages he sent.
#[test]
fn the_address_book_still_folds_case_after_the_collation_change() {
    let t = TempDb::new("address-book-case");
    let a = account(&t.db, "owner@example.com", 0);
    let th = thread(&t.db, a, "g1", "hello", 1_700_000_000_000);
    {
        let conn = t.db.writer();
        q::upsert_message(
            &conn,
            &NewMessage {
                thread_id: th,
                account_id: a,
                gmail_message_id: "sent-1".into(),
                // Gmail hands the address back as whoever typed it typed it.
                from: Participant {
                    name: Some("Owner".into()),
                    email: "Owner@Example.com".into(),
                },
                to: vec![Participant {
                    name: Some("Tawny".into()),
                    email: "tawny@example.com".into(),
                }],
                subject: "hello".into(),
                internal_date: 1_700_000_000_000,
                ..Default::default()
            },
        )
        .expect("upsert");
    }

    let book = t
        .db
        .read(|conn| q::address_book(conn, 50))
        .expect("address book");
    let tawny = book
        .iter()
        .find(|c| c.email == "tawny@example.com")
        .expect("the person he wrote to should be in the book");
    assert_eq!(
        tawny.sends, 1,
        "a message from Owner@Example.com is still a message from this account"
    );
}

// ---------------------------------------------------------------------------
// file permissions
// ---------------------------------------------------------------------------

/// The store is `0600`, and a directory Mach created for it is `0700`.
///
/// This matters because the store is not always under `~/Library/Application
/// Support`, where the directory above it is already `0700`. `MACH_DATA_DIR`
/// puts a QA instance's store inside the repo, where every directory on the
/// path is `0755` — and a QA instance holds real mail.
#[cfg(unix)]
#[test]
fn the_store_is_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "mach-perm-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let path = dir.join("mach.sqlite3");

    // `open` runs the migrations, so the -wal exists by the time this returns.
    let db = Db::open(&path).expect("open");

    let mode = |p: &std::path::Path| {
        std::fs::metadata(p)
            .unwrap_or_else(|e| panic!("stat {}: {e}", p.display()))
            .permissions()
            .mode()
            & 0o777
    };

    assert_eq!(
        mode(&path),
        0o600,
        "the store holds every message body in the mailbox"
    );
    assert_eq!(
        mode(&dir),
        0o700,
        "a directory Mach created for the store is its own"
    );
    for suffix in ["-wal", "-shm"] {
        let mut journal = path.clone().into_os_string();
        journal.push(suffix);
        let journal = PathBuf::from(journal);
        if journal.exists() {
            assert_eq!(
                mode(&journal),
                0o600,
                "{} carries committed rows like the store does",
                journal.display()
            );
        }
    }

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
