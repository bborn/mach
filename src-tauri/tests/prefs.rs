//! Tests for the preferences store (`ipc::prefs`).
//!
//! Everything here drives the plain functions over a `&Connection`, never a
//! `#[tauri::command]` — for the reason `ipc/mod.rs` states: a command cannot be
//! called without an application, so nothing that decides anything lives in one.
//!
//! The interesting cases are all about *survival*: a database written by a build
//! that knew more keys than this one, a row whose JSON was truncated, a value at
//! the far end of what the sync loop can accept. A settings table that takes the
//! app down when one row is wrong is worse than no settings table.

use mach_lib::db::{schema, Db};
use mach_lib::ipc::prefs;
use serde_json::json;

fn db() -> Db {
    Db::open_in_memory().expect("open in-memory db")
}

// ---------------------------------------------------------------------------
// the migration
// ---------------------------------------------------------------------------

#[test]
fn preferences_table_exists_after_migration() {
    let db = db();
    let present: i64 = db
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'preferences'",
                [],
                |row| row.get(0),
            )?)
        })
        .expect("query sqlite_master");
    assert_eq!(present, 1, "migration must create the preferences table");
}

#[test]
fn the_preferences_migration_did_not_renumber_anything() {
    // The whole chain still has to be strictly increasing and start at 1 —
    // appending is the only legal way to add one, and a renumbered migration
    // would silently skip on every existing database.
    let versions: Vec<u32> = schema::MIGRATIONS.iter().map(|m| m.version).collect();
    assert_eq!(versions.first().copied(), Some(1));
    assert!(
        versions.windows(2).all(|pair| pair[1] > pair[0]),
        "migration versions must ascend: {versions:?}"
    );
    assert!(
        versions.contains(&4),
        "the preferences migration is version 4: {versions:?}"
    );
}

// ---------------------------------------------------------------------------
// round trips
// ---------------------------------------------------------------------------

#[test]
fn a_written_preference_reads_back_unchanged() {
    let db = db();
    db.write(|conn| prefs::set(conn, "theme", &json!("dark"), 1))
        .expect("write theme");

    let stored = db
        .read(|conn| prefs::get(conn, "theme"))
        .expect("read theme");
    assert_eq!(stored, Some(json!("dark")));
}

#[test]
fn every_json_shape_survives_the_round_trip() {
    let db = db();
    db.write(|conn| {
        prefs::set(conn, "theme", &json!("dark"), 1)?;
        prefs::set(conn, "weekStartsOn", &json!(0), 1)?;
        prefs::set(conn, "defaultAccountId", &json!(null), 1)?;
        prefs::set(conn, "workingHours", &json!({ "start": 8, "end": 18 }), 1)?;
        prefs::set(conn, "signatures", &json!({ "3": "— Bruno" }), 1)
    })
    .expect("write a mixed batch");

    let all = db.read(prefs::all).expect("read all");
    assert_eq!(all.get("theme"), Some(&json!("dark")));
    assert_eq!(all.get("weekStartsOn"), Some(&json!(0)));
    assert_eq!(all.get("defaultAccountId"), Some(&json!(null)));
    assert_eq!(
        all.get("workingHours"),
        Some(&json!({ "start": 8, "end": 18 }))
    );
    assert_eq!(all.get("signatures"), Some(&json!({ "3": "— Bruno" })));
}

#[test]
fn writing_a_preference_twice_replaces_it() {
    let db = db();
    db.write(|conn| prefs::set(conn, "theme", &json!("light"), 1))
        .expect("first write");
    db.write(|conn| prefs::set(conn, "theme", &json!("dark"), 2))
        .expect("second write");

    let all = db.read(prefs::all).expect("read all");
    assert_eq!(all.len(), 1, "a rewrite is an update, never a second row");
    assert_eq!(all.get("theme"), Some(&json!("dark")));
}

#[test]
fn an_unwritten_preference_is_none_rather_than_an_error() {
    let db = db();
    let stored = db
        .read(|conn| prefs::get(conn, "neverWritten"))
        .expect("read a missing key");
    assert_eq!(stored, None);
}

// ---------------------------------------------------------------------------
// survival
// ---------------------------------------------------------------------------

#[test]
fn a_corrupt_row_is_skipped_rather_than_fatal() {
    let db = db();
    db.write(|conn| prefs::set(conn, "theme", &json!("dark"), 1))
        .expect("write a good row");
    // Not something the app can write — a truncated write, a hand edit, a
    // downgrade. One bad row must not cost the other eight.
    db.write(|conn| {
        Ok(conn.execute(
            "INSERT INTO preferences (key, value, updated_at) VALUES ('workingHours', '{not json', 1)",
            [],
        )?)
    })
    .expect("write a bad row");

    let all = db.read(prefs::all).expect("read all");
    assert_eq!(all.get("theme"), Some(&json!("dark")));
    assert!(
        !all.contains_key("workingHours"),
        "the unparsable row is dropped"
    );
}

#[test]
fn keys_this_build_has_never_heard_of_come_back_anyway() {
    // A newer build wrote something; this one has no idea what it means. The
    // store's job is to hand it over, not to have an opinion.
    let db = db();
    db.write(|conn| prefs::set(conn, "somethingFromTheFuture", &json!([1, 2]), 1))
        .expect("write");

    let all = db.read(prefs::all).expect("read all");
    assert_eq!(all.get("somethingFromTheFuture"), Some(&json!([1, 2])));
}

// ---------------------------------------------------------------------------
// key validation
// ---------------------------------------------------------------------------

#[test]
fn key_validation_accepts_the_names_the_ui_actually_writes() {
    for key in [
        "theme",
        "weekStartsOn",
        "defaultAccountId",
        "undoWindowSeconds",
        "sendDelaySeconds",
        "syncIntervalSeconds",
        "workingHours",
        "signatures",
    ] {
        assert!(prefs::is_valid_key(key), "{key} should be a valid key");
    }
}

#[test]
fn key_validation_rejects_everything_else() {
    let too_long = "a".repeat(prefs::MAX_KEY_LEN + 1);
    for key in [
        "",
        " theme",
        "theme ",
        "Theme",
        "9lives",
        "with.dot",
        "with-dash",
        "with_underscore",
        "sql'injection",
        too_long.as_str(),
    ] {
        assert!(!prefs::is_valid_key(key), "{key:?} should be rejected");
    }
}

// ---------------------------------------------------------------------------
// the sync interval — the one preference Rust reads for itself
// ---------------------------------------------------------------------------

#[test]
fn an_unset_sync_interval_leaves_the_engine_default_alone() {
    let db = db();
    let interval = db.read(prefs::sync_interval).expect("read");
    assert_eq!(interval, None);
}

#[test]
fn the_sync_interval_reads_back_as_a_duration() {
    let db = db();
    db.write(|conn| prefs::set(conn, prefs::SYNC_INTERVAL_KEY, &json!(300), 1))
        .expect("write");

    let interval = db.read(prefs::sync_interval).expect("read");
    assert_eq!(interval.map(|d| d.as_secs()), Some(300));
}

#[test]
fn the_sync_interval_is_clamped_at_both_ends() {
    assert_eq!(
        prefs::clamp_interval(0.0).as_secs(),
        prefs::MIN_SYNC_INTERVAL_SECS,
        "a zero would spin the loop against Gmail's quota"
    );
    assert_eq!(
        prefs::clamp_interval(-90.0).as_secs(),
        prefs::MIN_SYNC_INTERVAL_SECS
    );
    assert_eq!(
        prefs::clamp_interval(9_999_999.0).as_secs(),
        prefs::MAX_SYNC_INTERVAL_SECS,
        "an interval of weeks looks like a broken app, not a preference"
    );
    assert_eq!(prefs::clamp_interval(59.6).as_secs(), 60, "rounds, not floors");
}

#[test]
fn a_sync_interval_stored_as_a_string_is_ignored() {
    // The frontend writes a number. A string here means something else wrote to
    // the key, and coercing it would be guessing on the app's behalf.
    let db = db();
    db.write(|conn| prefs::set(conn, prefs::SYNC_INTERVAL_KEY, &json!("300"), 1))
        .expect("write");

    assert_eq!(db.read(prefs::sync_interval).expect("read"), None);
}

/// The account a new message goes out from when there is nothing to infer one
/// from. Read on this side because the agent composes without a window.
#[test]
fn the_default_account_reads_back_as_a_number_or_not_at_all() {
    let db = db();
    assert_eq!(db.read(prefs::default_account_id).expect("read"), None);

    db.write(|conn| prefs::set(conn, prefs::DEFAULT_ACCOUNT_KEY, &json!(4), 1))
        .expect("write");
    assert_eq!(db.read(prefs::default_account_id).expect("read"), Some(4));

    // "No default" is how the dialog spells the unset choice, and it means the
    // caller falls back rather than sending from account null.
    db.write(|conn| prefs::set(conn, prefs::DEFAULT_ACCOUNT_KEY, &json!(null), 2))
        .expect("write");
    assert_eq!(db.read(prefs::default_account_id).expect("read"), None);

    db.write(|conn| prefs::set(conn, prefs::DEFAULT_ACCOUNT_KEY, &json!("4"), 3))
        .expect("write");
    assert_eq!(db.read(prefs::default_account_id).expect("read"), None);
}
