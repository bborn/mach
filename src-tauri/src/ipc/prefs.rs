//! Preferences: the key/value store behind ⌘, and the two commands that reach it.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `get_preferences` | — | `{ [key]: JSON }` — every stored row |
//! | `set_preference` | `key`, `value` | `void` |
//!
//! # Rust holds the bytes; TypeScript holds the meaning
//!
//! Nothing here knows that `density` is one of two words or that
//! `undoWindowSeconds` is a number with a floor. That belongs in `src/lib/prefs.ts`,
//! which has to do the defaulting and clamping anyway — the store can hand back
//! a key written by a newer build, or by a hand-edited database, and the UI must
//! survive either. Validating the same rules twice would only give the two
//! copies somewhere to disagree.
//!
//! What this layer does enforce is what a *store* is responsible for: a key
//! that is a plausible key, and a value small enough that a runaway write cannot
//! turn the settings table into a blob store. Both are shape checks, not
//! meaning checks.
//!
//! # The one exception
//!
//! [`SYNC_INTERVAL_KEY`] is read on this side, because the thing it controls is
//! on this side: the background loop's gap between passes. Writing it applies to
//! the running engine immediately (see [`SyncEngine::set_poll_interval`]), and
//! `bootstrap` reads it back at launch, so the setting survives a restart
//! without the frontend having to replay it.
//!
//! [`SyncEngine::set_poll_interval`]: crate::sync::SyncEngine::set_poll_interval

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;
use tauri::State;

use crate::db::Result as DbResult;

use super::error::IpcError;
use super::state::AppState;

/// The gap between background sync passes, in seconds. Read by `bootstrap`.
pub const SYNC_INTERVAL_KEY: &str = "syncIntervalSeconds";

/// Long enough for any name the UI would choose, short enough that a key is
/// obviously a key and not a payload somebody put in the wrong column.
pub const MAX_KEY_LEN: usize = 64;

/// 64 KiB. A signature is the largest thing anyone would legitimately store
/// here and it is a paragraph; this is three orders of magnitude of headroom.
pub const MAX_VALUE_BYTES: usize = 64 * 1024;

/// The floor and ceiling the sync interval is held to before it reaches the
/// engine. A zero would turn the loop into a spin against Gmail's quota, and a
/// value of days is indistinguishable from "off" while still looking like it
/// works.
pub const MIN_SYNC_INTERVAL_SECS: u64 = 15;
pub const MAX_SYNC_INTERVAL_SECS: u64 = 6 * 60 * 60;

// ===========================================================================
// Pure parts — everything `tests/prefs.rs` drives
// ===========================================================================

/// Whether a string is shaped like a preference key.
///
/// Deliberately narrow: lower camelCase letters and digits, no dots, no colons,
/// no spaces. The keys are written by exactly one file and read by exactly one
/// file, so anything outside that alphabet arrived by accident or by somebody
/// probing, and either way the honest answer is no.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key.chars().all(|c| c.is_ascii_alphanumeric())
}

fn check_key(key: &str) -> Result<(), IpcError> {
    if is_valid_key(key) {
        return Ok(());
    }
    Err(IpcError::internal(format!(
        "{key:?} is not a usable preference key — letters and digits only, \
         starting with a lower-case letter, at most {MAX_KEY_LEN} characters"
    )))
}

/// Every stored preference, keyed as it was written.
///
/// A row whose value will not parse as JSON is **skipped**, not fatal. The
/// alternative is that one corrupt row — a partial write, a hand edit, a
/// downgrade that wrote a shape this build's serializer cannot read — takes the
/// entire settings surface down with it, which is a much worse failure than
/// silently falling back to one default.
///
/// `BTreeMap` so the wire order is stable, which makes a snapshot in a test
/// mean something.
pub fn all(conn: &Connection) -> DbResult<BTreeMap<String, Value>> {
    let mut stmt = conn.prepare("SELECT key, value FROM preferences ORDER BY key")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut out = BTreeMap::new();
    for row in rows {
        let (key, raw) = row?;
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            out.insert(key, value);
        }
    }
    Ok(out)
}

/// One preference, or `None` if it has never been written (or will not parse).
pub fn get(conn: &Connection, key: &str) -> DbResult<Option<Value>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM preferences WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .ok();
    Ok(raw.and_then(|text| serde_json::from_str(&text).ok()))
}

/// Write one preference. An existing row is replaced, never appended to.
pub fn set(conn: &Connection, key: &str, value: &Value, now_ms: i64) -> DbResult<()> {
    conn.execute(
        "INSERT INTO preferences (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        rusqlite::params![key, value.to_string(), now_ms],
    )?;
    Ok(())
}

/// The stored sync interval, clamped, or `None` to leave [`SyncConfig`]'s own
/// default alone.
///
/// Accepts a JSON number and nothing else. A string that happens to contain
/// digits is not silently coerced: the frontend writes a number, so a string
/// here means something wrote to this key that had no business doing so.
///
/// [`SyncConfig`]: crate::sync::SyncConfig
pub fn sync_interval(conn: &Connection) -> DbResult<Option<Duration>> {
    Ok(get(conn, SYNC_INTERVAL_KEY)?
        .as_ref()
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite())
        .map(clamp_interval))
}

/// The same clamp `sync_interval` applies, exposed so the write path and the
/// read path cannot drift.
pub fn clamp_interval(seconds: f64) -> Duration {
    let bounded = seconds
        .round()
        .clamp(MIN_SYNC_INTERVAL_SECS as f64, MAX_SYNC_INTERVAL_SECS as f64);
    Duration::from_secs(bounded as u64)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ===========================================================================
// The commands
// ===========================================================================

/// Everything that has ever been written, in one round trip.
///
/// One call rather than a call per key: the dialog needs all of them and so
/// does boot, and a settings surface that renders in nine awaits would flash
/// nine defaults on the way to being right.
#[tauri::command]
pub fn get_preferences(state: State<'_, AppState>) -> Result<BTreeMap<String, Value>, IpcError> {
    Ok(state.db.read(all)?)
}

/// Write one preference, and apply it if it is one this side owns.
#[tauri::command]
pub fn set_preference(
    state: State<'_, AppState>,
    key: String,
    value: Value,
) -> Result<(), IpcError> {
    check_key(&key)?;
    let encoded = value.to_string();
    if encoded.len() > MAX_VALUE_BYTES {
        return Err(IpcError::internal(format!(
            "that preference value is {} bytes; the limit is {MAX_VALUE_BYTES}",
            encoded.len()
        )));
    }

    state.db.write(|conn| set(conn, &key, &value, now_ms()))?;

    // The sync loop is already running with the old gap, and a preference that
    // needs a relaunch to mean anything is a preference people stop trusting.
    if key == SYNC_INTERVAL_KEY {
        if let Some(seconds) = value.as_f64().filter(|s| s.is_finite()) {
            state.sync.set_poll_interval(clamp_interval(seconds));
        }
    }

    Ok(())
}
