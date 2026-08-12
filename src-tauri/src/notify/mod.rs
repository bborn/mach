//! Telling the owner that mail arrived.
//!
//! A mail client you leave open all day has to say something when the mail
//! lands, and until this module existed Mach said nothing at all: no banner, no
//! number on the Dock, nothing but a list that had quietly grown a bold row
//! somewhere below the fold.
//!
//! ```text
//!   sync::mail::incremental ──► notify::announce ──► rule ──► Banner ──► Host ──► mac ──► macOS
//!         (only on an already-        │                                            │
//!          synced account)            └── the notified ring, in `preferences`      │
//!                                                                                  ▼
//!   threads-changed  ──► notify::badge::refresh ──► unread in INBOX ──► Dock    the click,
//!                                                                              back again
//! ```
//!
//! # The three things that must not happen
//!
//! **A banner during the first sync.** A new account's backfill stores a year
//! of mail — tens of thousands of messages, every one of them an "arrival" as
//! far as the store is concerned. The gate is structural rather than a filter:
//! only [`crate::sync::mail::MailSync::incremental`] running on an account that
//! *already had* a history watermark reports arrivals at all, so a backfill,
//! and the catch-up replay that immediately follows one, cannot reach this
//! module. That is the single most important property here and
//! `tests/notify.rs` drives a real backfill to prove it.
//!
//! **The same message twice.** The ids Mach has already spoken about are kept
//! in the `preferences` table, so a replayed history window — or a relaunch
//! mid-pass, or a crash between the write and the banner — cannot produce a
//! second banner. It is a bounded ring rather than a growing set, because the
//! only duplicates that are physically possible come from a replay of a recent
//! window and there is no reason to carry a year of ids to catch them.
//!
//! **Five banners for five messages.** One sweep of one account produces at
//! most one notification. See [`rule::digest`].
//!
//! # Where the decisions live
//!
//! [`rule`] holds the judgement and is pure. This file holds the store: reading
//! the settings, turning message ids into the facts the rule wants, and
//! remembering what was said. [`host`] holds the platform, behind a trait, so
//! that nothing above it has to know whether there is a window — or a macOS —
//! at all.

pub mod badge;
pub mod host;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod rule;

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

use crate::db::{sync_queries, Db, Result as DbResult};
use crate::ipc::prefs;

pub use host::{init, Delivery, Host, Permission};
pub use rule::{Arrival, Banner};

// ===========================================================================
// The preference keys
// ===========================================================================

/// Notifications on or off, for everything. Default on: a mail client that
/// arrives silent is the state this whole module exists to end, and the rule
/// below is conservative enough that "on" is not a firehose.
pub const ENABLED_KEY: &str = "notificationsEnabled";

/// Account id (as a string key, like `signatures`) to `false` for the accounts
/// that should stay quiet. An account that is absent notifies — so adding an
/// account does not require visiting the settings to hear from it.
pub const ACCOUNTS_KEY: &str = "notificationAccounts";

/// The unread count on the Dock icon, on or off.
pub const BADGE_KEY: &str = "badgeEnabled";

/// The ring of `"<account id>:<gmail message id>"` Mach has already spoken
/// about. Not a setting — it is the app remembering what it said, and it is in
/// this table for the same reason `uiSession` is: the row is the unit of
/// change and there was no reason to spend a migration on it.
pub const NOTIFIED_KEY: &str = "notifiedMessages";

/// How many ids the ring holds.
///
/// A duplicate can only arrive from a replayed history window, which is at most
/// one pass wide. Five hundred is two orders of magnitude more than the busiest
/// plausible minute and still about twelve kilobytes — well inside the value
/// limit `ipc::prefs` enforces, with room for the limit to matter later.
pub const NOTIFIED_MEMORY: usize = 500;

// ===========================================================================
// What a click has to reopen
// ===========================================================================

/// The conversation a notification was about, for the banners nothing is
/// watching.
///
/// # This is the fallback, not the main path
///
/// [`host::Delivery::Watched`] is the main path: `notify::mac` sends the banner
/// on a thread of its own and blocks there until macOS reports the interaction,
/// so a click comes back to *this* process carrying nothing — but on a thread
/// that already knows, by closure, which conversation its own banner was about.
/// That is exact, and it does not go through this slot at all.
///
/// This slot covers what that cannot: a banner sent fire-and-forget because too
/// many were already outstanding ([`host::Delivery::Unwatched`]), and a bundled
/// build where clicking the banner activates the application and Tauri reports
/// `RunEvent::Reopen` with nothing in it to say which notification caused it.
/// The target is remembered at delivery time and claimed on the next activation,
/// within [`PENDING_OPEN_TTL_MS`].
///
/// It is deliberately a single slot: a second notification replaces the first,
/// which is the same thing the banner itself does. Being a guess, it is also
/// the reason the TTL is short — see [`PENDING_OPEN_TTL_MS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingOpen {
    pub account_id: i64,
    /// The local `threads.id` — what the UI selects a conversation by.
    pub thread_id: i64,
    pub gmail_thread_id: String,
    pub at_ms: i64,
}

/// How long a delivered notification stays claimable.
///
/// An activation this long after a banner is far more likely to be a Dock click
/// than a click on the banner, and opening a conversation somebody did not ask
/// for is worse than not opening one they did. Two minutes covers noticing a
/// banner, finishing a sentence, and switching over.
pub const PENDING_OPEN_TTL_MS: i64 = 2 * 60 * 1000;

static PENDING_OPEN: Mutex<Option<PendingOpen>> = Mutex::new(None);

fn remember_open(target: PendingOpen) {
    *PENDING_OPEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(target);
}

/// The conversation a notification click should open, consumed.
///
/// Returns `None` once claimed, and `None` for anything older than
/// [`PENDING_OPEN_TTL_MS`], so a Dock click on a quiet afternoon does not
/// reopen this morning's mail.
pub fn take_pending_open() -> Option<PendingOpen> {
    let mut slot = PENDING_OPEN.lock().unwrap_or_else(|p| p.into_inner());
    let target = slot.take()?;
    (now_ms() - target.at_ms <= PENDING_OPEN_TTL_MS).then_some(target)
}

// ===========================================================================
// The entry point the sync loop calls
// ===========================================================================

/// Say something about the messages that just arrived on one account.
///
/// Called from the sync loop *after* its transaction has committed, so nothing
/// is ever announced that a crash could take back. Returns nothing and can fail
/// at nothing: a store error, a missing window, a platform that will not deliver
/// — none of them are reasons for a sync pass to report failure, and a mail
/// client that stopped syncing because it could not draw a banner would be a
/// worse app than one that never had banners.
pub fn announce(db: &Db, account_id: i64, arrived: &[String]) {
    if arrived.is_empty() {
        return;
    }

    // One write transaction: deciding and remembering happen together, so the
    // gap in which a crash could produce a second banner does not exist.
    let planned = db.write(|conn| plan(conn, account_id, arrived));

    let Ok(Some((banner, target, _))) = planned else {
        return;
    };

    if let Some(host) = host::current() {
        // Armed only when nothing is watching the banner itself. A watched
        // banner routes its own click, and arming this as well would mean a
        // Dock click minutes later opened the same conversation a second time.
        if host.show(&banner, &target) == host::Delivery::Unwatched {
            remember_open(target);
        }
        // The count moved with the mail; say so without waiting for the UI to
        // notice and emit `threads-changed`.
        badge::refresh(host.as_ref());
    }
}

/// The whole decision, inside one transaction: what to say, and what to
/// remember having said.
///
/// Split out from [`announce`] because it is the half worth testing — it takes
/// a connection and returns a value, with no platform and no globals anywhere
/// near it.
///
/// The third element of the tuple is what was actually spoken about, which the
/// tests assert on and the caller ignores.
#[allow(clippy::type_complexity)]
pub fn plan(
    conn: &Connection,
    account_id: i64,
    arrived: &[String],
) -> DbResult<Option<(Banner, PendingOpen, Vec<Arrival>)>> {
    if arrived.is_empty() || !enabled(conn)? || !account_enabled(conn, account_id)? {
        return Ok(None);
    }

    let Some(email) = account_email(conn, account_id)? else {
        return Ok(None);
    };

    let mut ring = notified_ring(conn)?;
    let mut worth_saying: Vec<Arrival> = Vec::new();
    for gmail_message_id in arrived {
        let key = ring_key(account_id, gmail_message_id);
        if ring.iter().any(|seen| seen == &key) {
            continue;
        }
        let Some(arrival) = hydrate(conn, account_id, gmail_message_id)? else {
            continue;
        };
        if rule::earns_a_banner(&arrival, &email) {
            ring.push(key);
            worth_saying.push(arrival);
        }
    }

    let Some(last) = worth_saying.last() else {
        return Ok(None);
    };
    let target = PendingOpen {
        account_id,
        thread_id: last.thread_id,
        gmail_thread_id: last.gmail_thread_id.clone(),
        at_ms: now_ms(),
    };

    let label_account = account_count(conn)? > 1;
    let Some(banner) = rule::digest(&worth_saying, &email, label_account) else {
        return Ok(None);
    };

    // Remembered before the banner is shown, never after. Missing a banner
    // because the app died between the two is a shrug; showing one twice is the
    // thing people actually notice.
    write_ring(conn, ring)?;

    Ok(Some((banner, target, worth_saying)))
}

/// Everything the rule wants to know about one message that was just stored.
///
/// `None` when the message is not there — which a replayed history record can
/// legitimately produce, since a message can be added and deleted inside one
/// sweep.
pub fn hydrate(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
) -> DbResult<Option<Arrival>> {
    use rusqlite::OptionalExtension;

    let row: Option<(i64, i64, String, Option<String>, String, String, String)> = conn
        .query_row(
            "SELECT m.id, m.thread_id, t.gmail_thread_id, m.from_name, m.from_email,
                    m.subject, m.snippet
               FROM messages m
               JOIN threads t ON t.id = m.thread_id
              WHERE m.account_id = ?1 AND m.gmail_message_id = ?2",
            rusqlite::params![account_id, gmail_message_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;

    let Some((message_id, thread_id, gmail_thread_id, from_name, from_email, subject, snippet)) =
        row
    else {
        return Ok(None);
    };

    // The label set the sync loop wrote alongside the message, rather than a
    // re-derivation from `is_unread`: the categories the rule turns on are only
    // in the full list.
    let labels = sync_queries::message_labels(conn, account_id, gmail_message_id)?.unwrap_or_default();

    let own_address = account_email(conn, account_id)?.unwrap_or_default();
    let thread_has_own_message: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM messages
              WHERE thread_id = ?1 AND id <> ?2 AND lower(from_email) = lower(?3)
         )",
        rusqlite::params![thread_id, message_id, own_address],
        |row| row.get(0),
    )?;

    Ok(Some(Arrival {
        gmail_message_id: gmail_message_id.to_string(),
        thread_id,
        gmail_thread_id,
        from_name,
        from_email,
        subject,
        snippet,
        labels,
        thread_has_own_message,
    }))
}

// ===========================================================================
// Settings, read from the same table the dialog writes
// ===========================================================================

/// A stored boolean, defaulting to `true`.
///
/// Anything that is not a JSON boolean is the default rather than an error, for
/// the reason `ipc::prefs` documents at length: the row can be written by a
/// newer build or by a person with a SQLite client, and one odd value must cost
/// one setting rather than the feature.
fn flag(conn: &Connection, key: &str) -> DbResult<bool> {
    Ok(prefs::get(conn, key)?
        .as_ref()
        .and_then(Value::as_bool)
        .unwrap_or(true))
}

pub fn enabled(conn: &Connection) -> DbResult<bool> {
    Ok(flag(conn, ENABLED_KEY)? && host::banners_allowed())
}

pub fn badge_enabled(conn: &Connection) -> DbResult<bool> {
    flag(conn, BADGE_KEY)
}

/// Whether one account is allowed to speak. Absent means yes.
pub fn account_enabled(conn: &Connection, account_id: i64) -> DbResult<bool> {
    let Some(map) = prefs::get(conn, ACCOUNTS_KEY)? else {
        return Ok(true);
    };
    Ok(map
        .get(account_id.to_string())
        .and_then(Value::as_bool)
        .unwrap_or(true))
}

// ===========================================================================
// The ring of what has already been said
// ===========================================================================

fn ring_key(account_id: i64, gmail_message_id: &str) -> String {
    format!("{account_id}:{gmail_message_id}")
}

fn notified_ring(conn: &Connection) -> DbResult<Vec<String>> {
    Ok(prefs::get(conn, NOTIFIED_KEY)?
        .as_ref()
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

fn write_ring(conn: &Connection, mut ring: Vec<String>) -> DbResult<()> {
    if ring.len() > NOTIFIED_MEMORY {
        ring.drain(..ring.len() - NOTIFIED_MEMORY);
    }
    prefs::set(conn, NOTIFIED_KEY, &json!(ring), now_ms())
}

// ===========================================================================
// Small store reads
// ===========================================================================

fn account_email(conn: &Connection, account_id: i64) -> DbResult<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT email FROM accounts WHERE id = ?1",
            [account_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

fn account_count(conn: &Connection) -> DbResult<i64> {
    Ok(conn.query_row("SELECT count(*) FROM accounts", [], |row| row.get(0))?)
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
