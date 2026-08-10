//! Tests for the Tauri IPC layer and the app bootstrap (U8).
//!
//! A `#[tauri::command]` cannot be invoked without standing up an application,
//! so the handlers in `ipc::commands` are deliberately empty wrappers and every
//! decision lives in `ipc::reads`, `ipc::state` and `ipc::types`. That is what
//! these tests drive, with a real SQLite database and no Tauri runtime at all.
//!
//! The load-bearing tests are the ones that pin promises the compiler cannot:
//!
//!  * `camel_case_*` — the wire format. A snake_case key here compiles fine and
//!    silently breaks every screen in the frontend, so the assertions are on
//!    `serde_json` output rather than on Rust field names.
//!  * `paginating_the_stream_*` — the keyset cursor neither skips nor repeats a
//!    row, which is the whole reason it is not `LIMIT/OFFSET`.
//!  * `booting_without_credentials_*` — a fresh checkout has no `.env.local`,
//!    and the app has to start anyway and say why it cannot sign in.
//!  * `an_account_whose_keychain_entry_is_gone_*` — the credential can vanish
//!    under us; that is a state to report, not a crash.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mach_lib::auth::tokens::{MemoryTokenStore, Secret, TokenStore};
use mach_lib::commands::CommandError;
use mach_lib::config::AppConfig;
use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::{command_queries, schema, Db};
use mach_lib::ipc::state::restore_accounts;
use mach_lib::ipc::types::{SyncStatusPayload, ThreadPage, ThreadQuery};
use mach_lib::ipc::{reads, IpcError};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp-file database that deletes itself. On-disk rather than in-memory so
/// migrations are exercised against a real file, which is what boot does.
struct TempDb {
    path: PathBuf,
    db: Db,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let path = temp_path(tag);
        let db = Db::open(&path).expect("open temp db");
        TempDb { path, db }
    }

    fn reopen(&self) -> Db {
        Db::open(&self.path).expect("reopen temp db")
    }
}

fn temp_path(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "mach-ipc-test-{}-{}-{}.sqlite3",
        tag,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

impl std::ops::Deref for TempDb {
    type Target = Db;
    fn deref(&self) -> &Db {
        &self.db
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        remove_db_files(&self.path);
    }
}

fn remove_db_files(path: &PathBuf) {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
}

fn account(db: &Db, email: &str, colour: i64) -> i64 {
    let conn = db.writer();
    q::upsert_account(
        &conn,
        &NewAccount {
            email: email.to_string(),
            display_name: Some(email.to_string()),
            token_ref: "com.mach.mail.oauth".to_string(),
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

fn message(db: &Db, thread_id: i64, account_id: i64, gmail_id: &str, subject: &str, body: &str) {
    let conn = db.writer();
    q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: gmail_id.to_string(),
            rfc822_message_id: Some(format!("<{gmail_id}@example.com>")),
            from: Participant {
                name: Some("Tawny".into()),
                email: "tawny@example.com".into(),
            },
            to: vec![Participant::new("alex@example.com")],
            subject: subject.to_string(),
            body_text: Some(body.to_string()),
            snippet: body.chars().take(40).collect(),
            internal_date: 1_700_000_000_000,
            is_unread: true,
            ..Default::default()
        },
    )
    .expect("upsert message");
}

fn event(db: &Db, account_id: i64, calendar_id: &str, google_id: &str, start: i64, end: i64) {
    let conn = db.writer();
    q::upsert_event(
        &conn,
        &NewEvent {
            account_id,
            calendar_id: calendar_id.to_string(),
            google_event_id: google_id.to_string(),
            title: format!("event {google_id}"),
            start_ts: start,
            end_ts: end,
            status: "confirmed".to_string(),
            ..Default::default()
        },
    )
    .expect("upsert event");
}

/// A `calendarList.list` entry as the metadata sweep would have stored it.
fn calendar_meta(account_id: i64, calendar_id: &str, name: Option<&str>) -> NewCalendar {
    NewCalendar {
        account_id,
        calendar_id: calendar_id.to_string(),
        summary: name.map(str::to_string),
        selected: true,
        synced_at: 1_000,
        ..Default::default()
    }
}

fn store_calendar(db: &Db, row: &NewCalendar) {
    let conn = db.writer();
    q::upsert_calendar(&conn, row).expect("upsert calendar");
}

fn json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("serialize")
}

/// Every key name in a JSON tree, so a snake_case leak anywhere is visible.
fn all_keys(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                all_keys(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                all_keys(item, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// migrations
// ---------------------------------------------------------------------------

#[test]
fn migrations_run_on_a_fresh_database_and_bring_it_to_the_latest_version() {
    let db = TempDb::new("fresh");
    let version: i64 = db
        .reader()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("read user_version");
    assert_eq!(version, schema::LATEST_VERSION as i64);
    assert!(
        schema::LATEST_VERSION >= 2,
        "snoozed_threads was promoted to migration 2"
    );
}

#[test]
fn the_snooze_table_now_comes_from_a_migration_not_from_the_command_layer() {
    let db = TempDb::new("snooze-migration");
    // Nothing has constructed a CommandDispatcher, so if the table is here it
    // came from MIGRATIONS.
    let count: i64 = db
        .reader()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'snoozed_threads'",
            [],
            |row| row.get(0),
        )
        .expect("query sqlite_master");
    assert_eq!(count, 1);

    let index: i64 = db
        .reader()
        .query_row(
            "SELECT count(*) FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_snoozed_threads_wake'",
            [],
            |row| row.get(0),
        )
        .expect("query sqlite_master");
    assert_eq!(index, 1);
}

#[test]
fn ensure_command_schema_is_still_idempotent_after_the_migration() {
    let db = TempDb::new("ensure-idempotent");
    // The command layer still calls this on every dispatcher construction; it
    // must find the migrated table and do nothing.
    for _ in 0..3 {
        db.write(command_queries::ensure_command_schema)
            .expect("ensure_command_schema");
    }

    let account_id = account(&db, "alex@example.com", 0);
    let thread_id = thread(&db, account_id, "t1", "Snoozable", 10);
    db.write(|conn| {
        command_queries::upsert_snooze(
            conn,
            &command_queries::SnoozeRow {
                thread_id,
                wake_at: 500,
                snoozed_at: 100,
                prior_label_ids: vec!["INBOX".into()],
                prior_is_unread: true,
            },
        )
    })
    .expect("upsert snooze");

    let due = db
        .read(|conn| command_queries::due_snoozes(conn, 1_000))
        .expect("due snoozes");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].thread_id, thread_id);
}

#[test]
fn migrating_is_idempotent_across_reopens() {
    let db = TempDb::new("idempotent");
    account(&db, "alex@example.com", 0);

    // Reopening runs `migrate` again; it must apply nothing and lose nothing.
    for _ in 0..3 {
        let reopened = db.reopen();
        let accounts = reads::list_accounts(&reopened).expect("list accounts");
        assert_eq!(accounts.len(), 1);
        let version: i64 = reopened
            .reader()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("read user_version");
        assert_eq!(version, schema::LATEST_VERSION as i64);
    }
}

// ---------------------------------------------------------------------------
// reads
// ---------------------------------------------------------------------------

#[test]
fn paginating_the_stream_with_the_cursor_neither_skips_nor_repeats_a_thread() {
    let db = TempDb::new("paginate");
    let account_id = account(&db, "alex@example.com", 0);
    for i in 0..7 {
        thread(&db, account_id, &format!("t{i}"), &format!("Subject {i}"), 100 + i);
    }

    let mut seen: Vec<i64> = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = reads::list_threads(
            &db,
            &ThreadQuery {
                limit: Some(3),
                cursor,
                ..Default::default()
            },
        )
        .expect("list threads");
        pages += 1;
        seen.extend(page.items.iter().map(|t| t.id));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(pages < 10, "pagination did not terminate");
    }

    assert_eq!(pages, 3, "7 rows at 3 a page is three pages");
    assert_eq!(seen.len(), 7, "every thread came back exactly once");

    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 7, "no thread was repeated across pages");

    // The stream is newest first, so the ids must arrive in descending
    // last_message_at order — the cursor must not reorder anything.
    let all = reads::list_threads(&db, &ThreadQuery::default()).expect("list all");
    let expected: Vec<i64> = all.items.iter().map(|t| t.id).collect();
    assert_eq!(seen, expected);
}

#[test]
fn a_full_page_carries_a_cursor_and_a_short_page_does_not() {
    let db = TempDb::new("cursor-end");
    let account_id = account(&db, "alex@example.com", 0);
    for i in 0..4 {
        thread(&db, account_id, &format!("t{i}"), "Subject", 100 + i);
    }

    let full = reads::list_threads(
        &db,
        &ThreadQuery {
            limit: Some(4),
            ..Default::default()
        },
    )
    .expect("full page");
    assert_eq!(full.items.len(), 4);
    assert!(
        full.next_cursor.is_some(),
        "a page that came back full might have more behind it"
    );

    let short = reads::list_threads(
        &db,
        &ThreadQuery {
            limit: Some(10),
            ..Default::default()
        },
    )
    .expect("short page");
    assert_eq!(short.items.len(), 4);
    assert!(short.next_cursor.is_none(), "a short page is the end");
}

#[test]
fn listing_threads_narrows_to_one_account_and_one_label() {
    let db = TempDb::new("narrow");
    let a = account(&db, "a@example.com", 0);
    let b = account(&db, "b@example.com", 1);
    thread(&db, a, "t1", "From A", 200);
    thread(&db, b, "t2", "From B", 300);

    let page = reads::list_threads(
        &db,
        &ThreadQuery {
            account_id: Some(a),
            ..Default::default()
        },
    )
    .expect("list threads");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].account_email, "a@example.com");

    let labelled = reads::list_threads(
        &db,
        &ThreadQuery {
            label_id: Some("INBOX".into()),
            ..Default::default()
        },
    )
    .expect("list threads");
    assert_eq!(labelled.items.len(), 2);

    let missing = reads::list_threads(
        &db,
        &ThreadQuery {
            label_id: Some("Label_nope".into()),
            ..Default::default()
        },
    )
    .expect("list threads");
    assert!(missing.items.is_empty());
}

#[test]
fn searching_returns_fts_matches_ranked_and_hydrated() {
    let db = TempDb::new("search");
    let account_id = account(&db, "alex@example.com", 0);

    let hit = thread(&db, account_id, "t1", "Velocipede maintenance", 300);
    message(
        &db,
        hit,
        account_id,
        "m1",
        "Velocipede maintenance",
        "the front wheel needs truing before Saturday",
    );

    let miss = thread(&db, account_id, "t2", "Invoice", 200);
    message(&db, miss, account_id, "m2", "Invoice", "attached is the invoice");

    let page = reads::search_threads(&db, "velocipede", None).expect("search");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, hit);
    assert_eq!(page.items[0].subject, "Velocipede maintenance");
    assert!(page.next_cursor.is_none(), "search results are ranked, not keyset");

    // Search-as-you-type: a prefix of a word in the body is a match.
    let prefix = reads::search_threads(&db, "trui", None).expect("search prefix");
    assert_eq!(prefix.items.len(), 1);
    assert_eq!(prefix.items[0].id, hit);

    // A blank box is an empty result, not an error.
    assert!(reads::search_threads(&db, "   ", None).expect("blank").items.is_empty());
    // And a query matching nothing is simply empty.
    assert!(reads::search_threads(&db, "pterodactyl", None)
        .expect("no match")
        .items
        .is_empty());
}

#[test]
fn reading_a_thread_returns_its_whole_conversation() {
    let db = TempDb::new("detail");
    let account_id = account(&db, "alex@example.com", 0);
    let thread_id = thread(&db, account_id, "t1", "Subject", 300);
    message(&db, thread_id, account_id, "m1", "Subject", "first");
    message(&db, thread_id, account_id, "m2", "Subject", "second");

    let detail = reads::get_thread(&db, thread_id).expect("get thread");
    assert_eq!(detail.thread.id, thread_id);
    assert_eq!(detail.messages.len(), 2);
}

#[test]
fn calendars_are_derived_from_the_events_actually_held() {
    let db = TempDb::new("calendars");
    let a = account(&db, "a@example.com", 0);
    let b = account(&db, "b@example.com", 1);
    event(&db, a, "a@example.com", "e1", 100, 200);
    event(&db, a, "a@example.com", "e2", 300, 400);
    event(&db, a, "team@group.calendar.google.com", "e3", 100, 200);
    event(&db, b, "b@example.com", "e4", 100, 200);

    let calendars = reads::list_calendars(&db).expect("list calendars");
    assert_eq!(calendars.len(), 3);

    let primary = calendars
        .iter()
        .find(|c| c.id == "a@example.com")
        .expect("account a's primary calendar");
    assert_eq!(primary.account_id, a);
    assert_eq!(primary.account_email, "a@example.com");
    assert_eq!(primary.colour_index, 0);
    assert_eq!(primary.event_count, 2);
}

#[test]
fn stored_metadata_names_a_calendar_that_the_id_could_not() {
    let db = TempDb::new("calendar-names");
    let a = account(&db, "bruno@example.com", 0);
    event(&db, a, "en.usa#holiday@group.v.calendar.google.com", "e1", 100, 200);
    event(&db, a, "c_d814cb@group.calendar.google.com", "e2", 100, 200);

    store_calendar(
        &db,
        &calendar_meta(
            a,
            "en.usa#holiday@group.v.calendar.google.com",
            Some("Holidays in United States"),
        ),
    );
    store_calendar(
        &db,
        &NewCalendar {
            summary: Some("Alicia's calendar".into()),
            // What this account renamed its subscription to. It wins.
            summary_override: Some("Alicia & Bruno".into()),
            background_color: Some("#f83a22".into()),
            access_role: Some("writer".into()),
            ..calendar_meta(a, "c_d814cb@group.calendar.google.com", None)
        },
    );

    let calendars = reads::list_calendars(&db).expect("list calendars");
    let names: Vec<&str> = calendars.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"Holidays in United States"));
    assert!(names.contains(&"Alicia & Bruno"));

    let shared = calendars
        .iter()
        .find(|c| c.id.starts_with("c_d814cb"))
        .expect("the shared calendar");
    assert_eq!(shared.background_color.as_deref(), Some("#f83a22"));
    assert_eq!(shared.access_role.as_deref(), Some("writer"));
    assert_eq!(shared.event_count, 1);
}

#[test]
fn the_primary_calendar_is_named_after_the_account_holder_not_the_address() {
    // Google sends the account's own email as the primary calendar's `summary`
    // and substitutes the display name in its own UI. Anything else here is a
    // sidebar that lists five email addresses and calls them calendars.
    let db = TempDb::new("calendar-primary");
    let a = account(&db, "bruno@example.com", 0);
    {
        let conn = db.writer();
        q::upsert_account(
            &conn,
            &NewAccount {
                email: "bruno@example.com".to_string(),
                display_name: Some("Bruno Bornsztein".to_string()),
                token_ref: String::new(),
                colour_index: 0,
            },
        )
        .expect("name the account");
    }
    store_calendar(
        &db,
        &NewCalendar {
            is_primary: true,
            ..calendar_meta(a, "bruno@example.com", Some("bruno@example.com"))
        },
    );

    let calendars = reads::list_calendars(&db).expect("list calendars");
    assert_eq!(calendars[0].name, "Bruno Bornsztein");
    assert!(calendars[0].primary);
}

#[test]
fn a_primary_calendar_with_no_display_name_falls_back_to_the_address() {
    let db = TempDb::new("calendar-primary-anon");
    let a = account(&db, "solo@example.com", 0);
    {
        let conn = db.writer();
        conn.execute("UPDATE accounts SET display_name = NULL WHERE id = ?1", [a])
            .expect("clear the display name");
    }
    store_calendar(
        &db,
        &NewCalendar {
            is_primary: true,
            ..calendar_meta(a, "solo@example.com", Some("solo@example.com"))
        },
    );

    let calendars = reads::list_calendars(&db).expect("list calendars");
    assert_eq!(calendars[0].name, "solo@example.com");
}

#[test]
fn renaming_your_own_primary_calendar_still_wins() {
    // `summaryOverride` on the primary is a deliberate rename, so the display
    // name substitution must not overrule it.
    let db = TempDb::new("calendar-primary-override");
    let a = account(&db, "bruno@example.com", 0);
    store_calendar(
        &db,
        &NewCalendar {
            is_primary: true,
            summary_override: Some("Work".into()),
            ..calendar_meta(a, "bruno@example.com", Some("bruno@example.com"))
        },
    );

    let calendars = reads::list_calendars(&db).expect("list calendars");
    assert_eq!(calendars[0].name, "Work");
}

#[test]
fn a_calendar_with_events_and_no_metadata_still_appears() {
    // The mid-migration case, and the one that must never regress: a database
    // written before migration 6 has events and no calendar rows at all.
    let db = TempDb::new("calendar-fallback");
    let a = account(&db, "a@example.com", 0);
    event(&db, a, "team@group.calendar.google.com", "e1", 100, 200);
    store_calendar(&db, &calendar_meta(a, "a@example.com", Some("Named one")));

    let calendars = reads::list_calendars(&db).expect("list calendars");
    let ids: Vec<&str> = calendars.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"team@group.calendar.google.com"));

    let derived = calendars
        .iter()
        .find(|c| c.id.starts_with("team"))
        .expect("the calendar with no metadata");
    assert_eq!(derived.name, "team@group.calendar.google.com");
    // Silence is permission: nothing was fetched, so nothing is denied.
    assert_eq!(derived.access_role, None);
    assert!(derived.selected, "an unknown calendar starts visible");
}

#[test]
fn a_metadata_only_calendar_appears_before_it_has_any_events() {
    let db = TempDb::new("calendar-empty");
    let a = account(&db, "a@example.com", 0);
    store_calendar(&db, &calendar_meta(a, "empty@example.com", Some("Empty")));

    let calendars = reads::list_calendars(&db).expect("list calendars");
    assert_eq!(calendars.len(), 1);
    assert_eq!(calendars[0].name, "Empty");
    assert_eq!(calendars[0].event_count, 0);
}

#[test]
fn an_unsubscribed_calendar_keeps_naming_its_events_and_then_leaves() {
    let db = TempDb::new("calendar-unsubscribed");
    let a = account(&db, "a@example.com", 0);
    event(&db, a, "gone@group.calendar.google.com", "e1", 100, 200);
    store_calendar(
        &db,
        &NewCalendar {
            deleted: true,
            ..calendar_meta(a, "gone@group.calendar.google.com", Some("Book club"))
        },
    );

    let calendars = reads::list_calendars(&db).expect("list calendars");
    assert_eq!(calendars.len(), 1, "its events are still on the grid");
    assert_eq!(calendars[0].name, "Book club");
    assert!(calendars[0].deleted);

    // Once the events age out of the store there is nothing left to name.
    {
        let conn = db.writer();
        conn.execute("DELETE FROM events", []).expect("clear events");
    }
    assert!(reads::list_calendars(&db).expect("list").is_empty());
}

#[test]
fn googles_own_visibility_flag_is_carried_across_the_seam() {
    let db = TempDb::new("calendar-selected");
    let a = account(&db, "a@example.com", 0);
    store_calendar(
        &db,
        &NewCalendar {
            selected: false,
            ..calendar_meta(a, "muted@group.calendar.google.com", Some("Muted"))
        },
    );

    let calendars = reads::list_calendars(&db).expect("list calendars");
    assert!(!calendars[0].selected);
}

#[test]
fn listing_events_returns_everything_overlapping_the_window() {
    let db = TempDb::new("events");
    let account_id = account(&db, "alex@example.com", 0);
    event(&db, account_id, "primary", "before", 0, 50);
    event(&db, account_id, "primary", "straddling", 50, 150);
    event(&db, account_id, "primary", "inside", 110, 120);
    event(&db, account_id, "primary", "after", 500, 600);

    let events = reads::list_events(&db, 100, 200).expect("list events");
    let ids: Vec<&str> = events.iter().map(|e| e.google_event_id.as_str()).collect();
    assert_eq!(ids, vec!["straddling", "inside"]);
}

// ---------------------------------------------------------------------------
// typed errors
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_account_id_is_a_typed_error_not_an_empty_list() {
    let db = TempDb::new("unknown-account");
    account(&db, "alex@example.com", 0);

    let error = reads::list_labels(&db, Some(9_999)).expect_err("unknown account");
    assert_eq!(error.kind(), "notFound");
    assert!(
        error.to_string().contains("9999"),
        "the message names the id: {error}"
    );

    let payload = json(&error);
    assert_eq!(payload["kind"], "notFound");
    assert!(payload["message"].is_string());

    // A known account with no labels is an empty list, which is why the two
    // cannot be conflated.
    let accounts = reads::list_accounts(&db).expect("list accounts");
    assert!(reads::list_labels(&db, Some(accounts[0].id))
        .expect("known account")
        .is_empty());
}

#[test]
fn an_unknown_thread_id_is_a_typed_error() {
    let db = TempDb::new("unknown-thread");
    let error = reads::get_thread(&db, 42).expect_err("unknown thread");
    assert_eq!(error.kind(), "notFound");
    assert!(error.to_string().contains("42"));
}

#[test]
fn the_command_layers_own_error_tag_survives_the_boundary() {
    // A command error is not flattened into "command failed" — the frontend
    // branches on the specific tag the command layer chose.
    let error = IpcError::from(CommandError::UnknownAccount { account_id: 7 });
    assert_eq!(error.kind(), "unknownAccount");
    assert_eq!(json(&error)["kind"], "unknownAccount");

    let error = IpcError::from(CommandError::UnknownThread { thread_id: 7 });
    assert_eq!(error.kind(), "unknownThread");
}

#[test]
fn every_error_serializes_as_kind_and_message() {
    let cases: Vec<(IpcError, &str)> = vec![
        (IpcError::NotConfigured("nope".into()), "notConfigured"),
        (IpcError::not_found("thread", 1), "notFound"),
        (IpcError::UnknownPending("abc".into()), "unknownPending"),
        (IpcError::internal("boom"), "internal"),
    ];
    for (error, expected) in cases {
        let payload = json(&error);
        assert_eq!(payload["kind"], expected);
        assert!(
            payload["message"].as_str().is_some_and(|m| !m.is_empty()),
            "every error carries a renderable sentence"
        );
        assert_eq!(payload.as_object().expect("object").len(), 2);
    }
}

// ---------------------------------------------------------------------------
// the wire format
// ---------------------------------------------------------------------------

#[test]
fn camel_case_thread_page() {
    let db = TempDb::new("wire-threads");
    let account_id = account(&db, "alex@example.com", 3);
    let thread_id = thread(&db, account_id, "t1", "Subject", 1_700_000_000_000);
    message(&db, thread_id, account_id, "m1", "Subject", "body");

    let page = reads::list_threads(
        &db,
        &ThreadQuery {
            limit: Some(1),
            ..Default::default()
        },
    )
    .expect("list threads");
    let payload = json(&page);

    assert!(payload["items"].is_array());
    let row = &payload["items"][0];
    for key in [
        "id",
        "accountId",
        "accountEmail",
        "accountColourIndex",
        "gmailThreadId",
        "participants",
        "subject",
        "snippet",
        "lastMessageAt",
        "isUnread",
        "messageCount",
        "hasAttachments",
        "labelIds",
    ] {
        assert!(row.get(key).is_some(), "thread row is missing {key}: {row}");
    }
    assert!(
        row["lastMessageAt"].is_i64(),
        "timestamps are epoch millis as numbers, not strings"
    );

    let cursor = &payload["nextCursor"];
    assert!(cursor.get("lastMessageAt").is_some());
    assert!(cursor.get("id").is_some());

    let mut keys = Vec::new();
    all_keys(&payload, &mut keys);
    let snake: Vec<&String> = keys.iter().filter(|k| k.contains('_')).collect();
    assert!(snake.is_empty(), "snake_case leaked to the wire: {snake:?}");
}

#[test]
fn camel_case_thread_detail() {
    let db = TempDb::new("wire-detail");
    let account_id = account(&db, "alex@example.com", 0);
    let thread_id = thread(&db, account_id, "t1", "Subject", 1_700_000_000_000);
    message(&db, thread_id, account_id, "m1", "Subject", "body");

    let payload = json(&reads::get_thread(&db, thread_id).expect("get thread"));
    assert!(payload.get("thread").is_some());
    let message = &payload["messages"][0];
    for key in [
        "id",
        "threadId",
        "accountId",
        "gmailMessageId",
        "rfc822MessageId",
        "from",
        "to",
        "bodyText",
        "internalDate",
        "isUnread",
        "isDraft",
        "attachments",
    ] {
        assert!(message.get(key).is_some(), "message is missing {key}");
    }

    let mut keys = Vec::new();
    all_keys(&payload, &mut keys);
    let snake: Vec<&String> = keys.iter().filter(|k| k.contains('_')).collect();
    assert!(snake.is_empty(), "snake_case leaked to the wire: {snake:?}");
}

#[test]
fn camel_case_calendars_and_events() {
    let db = TempDb::new("wire-calendar");
    let account_id = account(&db, "alex@example.com", 2);
    event(&db, account_id, "primary", "e1", 100, 200);

    let calendars = json(&reads::list_calendars(&db).expect("calendars"));
    for key in [
        "id",
        "accountId",
        "accountEmail",
        "name",
        "colourIndex",
        "eventCount",
        "primary",
        "selected",
        "deleted",
    ] {
        assert!(calendars[0].get(key).is_some(), "calendar is missing {key}");
    }

    let events = json(&reads::list_events(&db, 0, 1_000).expect("events"));
    for key in [
        "id",
        "accountId",
        "calendarId",
        "googleEventId",
        "startTs",
        "endTs",
        "isAllDay",
        "attendees",
        "updatedAt",
    ] {
        assert!(events[0].get(key).is_some(), "event is missing {key}");
    }
}

#[test]
fn camel_case_sync_status() {
    let payload = json(&SyncStatusPayload::default());
    for key in [
        "running",
        "accounts",
        "lastPassStartedAt",
        "lastPassFinishedAt",
        "configured",
        "configurationError",
        "needsReauthorization",
    ] {
        assert!(payload.get(key).is_some(), "sync status is missing {key}");
    }
}

#[test]
fn camel_case_pending_authorization_handle() {
    let payload = json(&mach_lib::ipc::PendingAuthorizationHandle {
        url: "https://accounts.google.com/o/oauth2/v2/auth?x=1".into(),
        pending_id: "opaque".into(),
    });
    assert_eq!(payload["url"], "https://accounts.google.com/o/oauth2/v2/auth?x=1");
    assert_eq!(payload["pendingId"], "opaque");
}

#[test]
fn thread_query_arrives_from_typescript_in_camel_case() {
    // Exactly what `invoke("list_threads", { query })` sends.
    let query: ThreadQuery = serde_json::from_str(
        r#"{"accountId":3,"labelId":"INBOX","unreadOnly":true,"limit":25,
            "cursor":{"lastMessageAt":1700000000000,"id":42}}"#,
    )
    .expect("deserialize camelCase query");

    assert_eq!(query.account_id, Some(3));
    assert_eq!(query.label_id.as_deref(), Some("INBOX"));
    assert!(query.unread_only);
    assert_eq!(query.effective_limit(), 25);
    assert_eq!(query.cursor.expect("cursor").id, 42);

    // An omitted field is the default, so `invoke("list_threads", { query: {} })`
    // is the opening inbox.
    let empty: ThreadQuery = serde_json::from_str("{}").expect("deserialize empty query");
    assert_eq!(empty.account_id, None);
    assert!(!empty.unread_only);
    assert_eq!(
        empty.effective_limit(),
        mach_lib::db::queries::DEFAULT_PAGE_SIZE
    );

    // `after` is the store's name for the same field; accepting it costs
    // nothing and removes a whole class of frontend/backend mismatch.
    let aliased: ThreadQuery =
        serde_json::from_str(r#"{"after":{"lastMessageAt":1,"id":2}}"#).expect("alias");
    assert_eq!(aliased.cursor.expect("cursor").id, 2);
}

#[test]
fn a_page_size_beyond_the_cap_is_clamped_not_honoured() {
    let query = ThreadQuery {
        limit: Some(1_000_000),
        ..Default::default()
    };
    assert_eq!(query.effective_limit(), mach_lib::db::queries::MAX_PAGE_SIZE);
}

#[test]
fn an_empty_page_serializes_as_an_array_and_a_null_cursor() {
    // The frontend maps over `items` unconditionally, so it must never be null.
    let payload = json(&ThreadPage::default());
    assert_eq!(payload["items"], serde_json::json!([]));
    // `.get`, not indexing: indexing a missing key also yields `Null`, which
    // would let a renamed field pass this assertion.
    assert_eq!(
        payload.get("nextCursor"),
        Some(&serde_json::Value::Null),
        "the key is present and null, not absent"
    );
}

#[test]
fn the_command_catalogue_crosses_the_boundary_as_data() {
    let payload = json(&mach_lib::commands::Command::catalogue().to_vec());
    let specs = payload.as_array().expect("array of specs");
    assert!(specs.iter().any(|s| s["kind"] == "archive"));
    let archive = specs
        .iter()
        .find(|s| s["kind"] == "archive")
        .expect("archive spec");
    assert_eq!(archive["params"][0]["name"], "threadIds");
    assert!(archive["undoable"].as_bool().expect("undoable"));
}

// ---------------------------------------------------------------------------
// configuration and boot
// ---------------------------------------------------------------------------

#[test]
fn a_missing_client_id_is_a_not_configured_state_with_a_renderable_reason() {
    let config = AppConfig::from_values("/tmp/mach.sqlite3", None, None);
    assert!(!config.is_configured());
    let message = config.configuration_error.expect("a reason");
    assert!(message.contains("MACH_GOOGLE_CLIENT_ID"), "{message}");
    assert!(message.contains(".env.local"), "{message}");
}

#[test]
fn a_client_id_without_a_secret_is_also_not_configured() {
    // Google issues a secret with every desktop client and its token endpoint
    // expects one; discovering that as an unexplained 401 later is worse.
    let config = AppConfig::from_values("/tmp/mach.sqlite3", Some("id.apps".into()), None);
    assert!(!config.is_configured());
    assert!(config
        .configuration_error
        .expect("a reason")
        .contains("MACH_GOOGLE_CLIENT_SECRET"));

    // Whitespace is not a credential.
    let blank = AppConfig::from_values(
        "/tmp/mach.sqlite3",
        Some("id.apps".into()),
        Some("   ".into()),
    );
    assert!(!blank.is_configured());
}

#[test]
fn both_variables_present_is_configured() {
    let config = AppConfig::from_values(
        "/tmp/mach.sqlite3",
        Some("id.apps.googleusercontent.com".into()),
        Some("GOCSPX-secret".into()),
    );
    assert!(config.is_configured());
    assert!(config.configuration_error.is_none());
}

#[tokio::test]
async fn booting_without_credentials_starts_the_app_and_reports_not_configured() {
    let path = temp_path("boot-unconfigured");
    let config = AppConfig::from_values(&path, None, None);

    // The whole point: this must not panic, and must not return Err.
    let state = mach_lib::ipc::bootstrap(config).expect("boot without credentials");

    let status = state.status_payload();
    assert!(!status.configured);
    assert!(status
        .configuration_error
        .expect("a reason")
        .contains("MACH_GOOGLE_CLIENT_ID"));
    assert!(!status.running);
    assert!(status.accounts.is_empty());

    // Local reads still work — the store is the source of truth, not Google.
    assert!(reads::list_accounts(&state.db).expect("list accounts").is_empty());
    assert!(!state.should_start_sync(), "nothing to sync, nothing to start");

    // And asking for the OAuth client gives the UI a renderable error rather
    // than a panic.
    let error = state.client_config().expect_err("no client");
    assert_eq!(error.kind(), "notConfigured");

    drop(state);
    remove_db_files(&path);
}

#[tokio::test]
async fn booting_with_credentials_runs_migrations_and_reports_configured() {
    let path = temp_path("boot-configured");
    let config = AppConfig::from_values(
        &path,
        Some("id.apps.googleusercontent.com".into()),
        Some("GOCSPX-secret".into()),
    );

    let state = mach_lib::ipc::bootstrap(config).expect("boot with credentials");
    let status = state.status_payload();
    assert!(status.configured);
    assert!(status.configuration_error.is_none());

    // Boot ran migrations on a brand-new file.
    let version: i64 = state
        .db
        .reader()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("read user_version");
    assert_eq!(version, schema::LATEST_VERSION as i64);

    // No accounts yet, so the loop has nothing to do.
    assert!(!state.should_start_sync());

    drop(state);
    remove_db_files(&path);
}

// ---------------------------------------------------------------------------
// re-authorizing an address already in the store
// ---------------------------------------------------------------------------

/// What "Sign in again" costs: nothing.
///
/// The row is upserted on `email`, which is `UNIQUE`, so the account keeps its
/// primary key and every thread, message and event still points at it. The
/// upsert also names the three columns it writes, so the Gmail history
/// watermark and the calendar sync token survive — a re-authorized account
/// picks up incrementally instead of backfilling from scratch.
#[test]
fn re_authorizing_an_address_keeps_the_row_its_mail_and_its_watermarks() {
    let db = TempDb::new("reauth-persist");
    let id = account(&db, "bruno@example.com", 3);
    let thread_id = thread(&db, id, "t1", "Lunch", 1_700_000_000_000);
    message(&db, thread_id, id, "m1", "Lunch", "one");
    {
        let conn = db.db.writer();
        q::set_history_id(&conn, id, Some("99")).expect("history id");
        q::set_calendar_sync_token(&conn, id, Some("cal-token")).expect("calendar token");
    }

    let again = mach_lib::ipc::state::persist_account(&db.db, "bruno@example.com")
        .expect("persist an address already in the store");

    assert_eq!(again.id, id, "the account keeps its primary key");
    assert_eq!(again.colour_index, 3, "and its place in the palette");
    assert_eq!(again.display_name.as_deref(), Some("bruno@example.com"));
    assert_eq!(again.history_id.as_deref(), Some("99"));
    assert_eq!(again.calendar_sync_token.as_deref(), Some("cal-token"));

    let accounts = db.db.read(q::list_accounts).expect("list accounts");
    assert_eq!(accounts.len(), 1, "no second row for the same address");

    let threads = db
        .db
        .read(|conn| q::list_threads(conn, &Default::default()))
        .expect("list threads");
    assert_eq!(threads.len(), 1, "the stored mail is still there");

    remove_db_files(&db.path);
}

/// The address is remembered so the identity that comes back can be checked
/// against it — see `complete_add_account`.
#[test]
fn a_sign_in_started_for_an_address_reports_a_different_one_as_a_failure() {
    let error = IpcError::WrongAccount {
        expected: "bruno@example.com".to_string(),
        got: "someone.else@example.com".to_string(),
    };
    assert_eq!(error.kind(), "wrongAccount");
    assert_eq!(
        error.to_string(),
        "signed in as someone.else@example.com, not bruno@example.com"
    );
}

// ---------------------------------------------------------------------------
// restoring accounts
// ---------------------------------------------------------------------------

#[test]
fn an_account_whose_keychain_entry_is_gone_needs_reauthorization_rather_than_crashing() {
    let db = TempDb::new("restore");
    account(&db, "kept@example.com", 0);
    account(&db, "lost@example.com", 1);

    let store = MemoryTokenStore::default();
    store
        .save_refresh_token("kept@example.com", &Secret::new("refresh"))
        .expect("save");

    let needs = restore_accounts(&db, &store).expect("restore accounts");
    assert_eq!(needs, vec!["lost@example.com".to_string()]);
}

#[test]
fn restoring_an_empty_store_asks_for_nothing() {
    let db = TempDb::new("restore-empty");
    let needs = restore_accounts(&db, &MemoryTokenStore::default()).expect("restore accounts");
    assert!(needs.is_empty());
}

/// What a seeded QA instance is: accounts from the owner's database, and a
/// Keychain namespace of its own that holds nothing.
///
/// Every address has to come back as needing reauthorization, and the whole
/// check has to *return* — this is the call `restore_accounts_into` makes on a
/// background thread at launch, and the two incidents it is written against
/// were both a Keychain read that never came back.
#[test]
fn a_seeded_store_reports_every_account_as_needing_credentials_and_returns() {
    let db = TempDb::new("restore-seeded");
    for (index, email) in ["one@example.com", "two@example.com", "three@example.com"]
        .into_iter()
        .enumerate()
    {
        account(&db, email, index as i64);
    }

    let started = std::time::Instant::now();
    let mut needs = restore_accounts(&db, &MemoryTokenStore::default()).expect("restore accounts");
    needs.sort();

    assert_eq!(
        needs,
        vec![
            "one@example.com".to_string(),
            "three@example.com".to_string(),
            "two@example.com".to_string(),
        ]
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

// ---------------------------------------------------------------------------
// the wire sample
// ---------------------------------------------------------------------------

/// One fully-populated row of every shape the frontend maps, written out as
/// JSON for `src/lib/ipc.test.ts` to read.
///
/// The frontend builds fresh object literals out of these payloads, so a field
/// Rust starts sending is dropped in silence unless somebody names it in
/// `mapThread`, `mapMessage`, `mapEvent` or `mapCalendar`. That has happened
/// five times — `recurringEventId`, `htmlLink`, migration 5, migration 7,
/// `isDraft` — and every time the field was missing from *both* the mapper and
/// the frontend's hand-written description of the wire, so no amount of
/// TypeScript could have caught it. Only the real payload can.
///
/// The chain that makes it hold:
///
///  1. These are struct literals with no `..Default::default()`, so a new
///     column stops this file compiling until it is given a value here.
///  2. The value is serialized through the real `serde` impls, so the key is
///     spelled exactly as the frontend will receive it.
///  3. The result is compared against the checked-in file, so a sample that
///     was not regenerated fails here rather than passing quietly.
///  4. `ipc.test.ts` then refuses to let the new key through unmapped.
///
/// Deliberately not a `TempDb` round trip: reading rows back would only test
/// the columns the insert helper happens to fill, which is the same "only what
/// somebody thought of" hole one layer down.
#[test]
fn the_wire_sample_is_what_the_frontend_will_receive() {
    let participant = Participant {
        name: Some("Alex Rivera".into()),
        email: "alex@example.com".into(),
    };

    let thread = ThreadSummary {
        id: 41,
        account_id: 3,
        account_email: "alex@example.com".into(),
        account_colour_index: 2,
        gmail_thread_id: "18f0c0ffee".into(),
        participants: vec![participant.clone()],
        subject: "Quarterly review".into(),
        snippet: "Sending the deck ahead of Thursday".into(),
        last_message_at: 1_700_000_000_000,
        is_unread: true,
        message_count: 4,
        has_attachments: true,
        label_ids: vec!["INBOX".into(), "STARRED".into()],
    };

    let message = Message {
        id: 512,
        thread_id: 41,
        account_id: 3,
        gmail_message_id: "18f0c0ffee01".into(),
        rfc822_message_id: Some("<abc@example.com>".into()),
        in_reply_to: Some("<prior@example.com>".into()),
        references: Some("<root@example.com> <prior@example.com>".into()),
        from: participant.clone(),
        reply_to: vec![participant.clone()],
        to: vec![participant.clone()],
        cc: vec![participant.clone()],
        bcc: vec![participant.clone()],
        subject: "Quarterly review".into(),
        body_html: Some("<p>Deck attached.</p>".into()),
        // Empty on purpose, and it is not padding. `mapMessage` reads the
        // snippet only when the plaintext body is empty, so a sample with both
        // filled in would let a short-circuit pass for a field that is never
        // read. Values here are chosen so no `||` or `??` hides a mapper's
        // reach into the payload.
        body_text: Some(String::new()),
        body_text_flowed: false,
        body_text_delsp: false,
        snippet: "Deck attached.".into(),
        internal_date: 1_700_000_000_000,
        is_unread: true,
        is_draft: true,
        attachments: vec![Attachment {
            id: 9,
            message_id: 512,
            gmail_attachment_id: Some("ANGjdJ".into()),
            filename: "deck.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 84_213,
            local_path: Some("/tmp/deck.pdf".into()),
        }],
    };

    let event = Event {
        id: 77,
        account_id: 3,
        calendar_id: "primary".into(),
        google_event_id: "evt_1".into(),
        title: "Quarterly review".into(),
        description: Some("Bring the deck".into()),
        location: Some("Room 4".into()),
        start_ts: 1_700_000_000_000,
        end_ts: 1_700_003_600_000,
        is_all_day: false,
        attendees: vec![participant.clone()],
        rsvp_status: Some(RsvpStatus::Accepted),
        recurring_event_id: Some("evt_series".into()),
        recurrence: vec!["RRULE:FREQ=WEEKLY;BYDAY=TH".into()],
        reminders: Some(EventReminders {
            use_default: false,
            overrides: vec![EventReminder {
                method: "popup".into(),
                minutes: 10,
            }],
        }),
        ical_uid: Some("evt_1@google.com".into()),
        guests: vec![EventGuest {
            email: "sam@example.com".into(),
            name: Some("Sam Okafor".into()),
            response: Some(RsvpStatus::Tentative),
            optional: true,
            organizer: false,
            is_self: false,
            resource: false,
            comment: Some("Might be late".into()),
        }],
        conference: Some(EventConference {
            id: Some("abc-defg-hij".into()),
            name: Some("Google Meet".into()),
            entry_points: vec![ConferenceEntry {
                kind: "video".into(),
                uri: "https://meet.google.com/abc-defg-hij".into(),
                label: Some("meet.google.com/abc-defg-hij".into()),
                pin: Some("123456".into()),
                region_code: Some("US".into()),
            }],
            notes: Some("This meeting is being recorded".into()),
        }),
        creator: Some(participant.clone()),
        attachments: vec![EventAttachment {
            title: "Deck".into(),
            url: "https://drive.google.com/file/d/1".into(),
            mime_type: Some("application/pdf".into()),
        }],
        visibility: Some("private".into()),
        transparency: Some("opaque".into()),
        organizer: Some(participant.clone()),
        organizer_self: Some(true),
        guests_can_modify: Some(true),
        status: "confirmed".into(),
        html_link: Some("https://calendar.google.com/event?eid=1".into()),
        updated_at: 1_700_000_100_000,
    };

    let calendar = mach_lib::ipc::types::Calendar {
        id: "primary".into(),
        account_id: 3,
        account_email: "alex@example.com".into(),
        name: "Alex Rivera".into(),
        colour_index: 2,
        event_count: 12,
        description: Some("Work".into()),
        background_color: Some("#0b8043".into()),
        foreground_color: Some("#ffffff".into()),
        color_id: Some("10".into()),
        access_role: Some("owner".into()),
        time_zone: Some("America/Chicago".into()),
        primary: true,
        selected: true,
        deleted: false,
    };

    let sample = serde_json::json!({
        "thread": thread,
        "message": message,
        "event": event,
        "calendar": calendar,
    });

    let mut rendered = serde_json::to_string_pretty(&sample).expect("serialize sample");
    rendered.push('\n');

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/lib/wire-sample.json");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current != rendered {
        // Rewritten rather than merely reported, because the next thing anyone
        // would do is copy it out of the failure message by hand.
        std::fs::write(&path, &rendered).expect("write wire sample");
        panic!(
            "src/lib/wire-sample.json was out of date and has been regenerated.\n\
             Re-run the frontend tests: a field Rust now sends has to be mapped \
             in src/lib/ipc.ts, or named as deliberately unmapped there."
        );
    }
}
