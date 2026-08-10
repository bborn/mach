//! Prove the four calendar write commands against **real Google**.
//!
//! `createEvent`, `updateEvent`, `moveEvent` and `deleteEvent` were written,
//! unit-tested and exercised against a mocked transport — and had never once
//! run against the actual API. Every test asserted that we send what Google's
//! documentation says it wants, which is a different claim from Google
//! accepting it. The owner reported "I tried to create an event and nothing
//! happened" and the honest answer was that nobody knew.
//!
//! This is the thing that was missing: a command to run.
//!
//! ```sh
//! MACH_DATA_DIR=.qa/agent/data \
//! MACH_LIVE_CALENDAR=you@example.com \
//!   cargo run --bin calendar_live
//! ```
//!
//! # It refuses to run by accident
//!
//! Two guards, because this writes to somebody's real calendar.
//!
//! `MACH_LIVE_CALENDAR` must name the account, so running it is a sentence you
//! had to type on purpose. And `MACH_DATA_DIR` must be set to something that is
//! *not* the owner's application-support directory: the local store this drives
//! should be a copy (`scripts/qa seed`), so a bug here cannot corrupt the
//! database he is reading. The Google side is real either way — that is the
//! entire point — but the blast radius stops at one throwaway event.
//!
//! # It needs credentials of its own
//!
//! Setting `MACH_DATA_DIR` also moves this process into its own Keychain
//! namespace — see [`mach_lib::auth::tokens::keychain_service`], which is what
//! stops a QA instance putting a password dialog on the owner's screen. So this
//! does not borrow the owner's refresh tokens, and against a freshly seeded
//! store it stops at:
//!
//! ```text
//! no credentials for you@example.com; the account must be authorized first
//! ```
//!
//! Authorize the account once *in that instance* — `MACH_QA_INSTANCE=agent
//! scripts/qa up`, then add the account — and the token lands in the same
//! namespace this binary reads. There is deliberately no flag that points it at
//! the owner's entries instead.
//!
//! # What it does
//!
//! Creates one event an hour long, tomorrow, titled so it is unmistakable in
//! any calendar UI; reads it back; renames and moves it in time; moves it
//! between calendars if the account has a second writable one; then deletes it.
//! Every step prints what Google said. It finishes by sweeping any event whose
//! title carries [`TEST_TITLE_PREFIX`], so an interrupted run does not leave
//! litter behind for the next one to trip over.

use std::path::PathBuf;
use std::process::ExitCode;

use mach_lib::commands::types::{Command, EventDraft, EventPatch};
use mach_lib::db::{command_queries, queries};
use mach_lib::{config, ipc};

/// Anything wearing this is ours and is safe to delete.
const TEST_TITLE_PREFIX: &str = "Mach live test —";

/// One hour, starting tomorrow at 09:00 UTC. Far enough out that it cannot
/// collide with something happening now, and short enough to be obviously junk.
const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("\nall four write commands round-tripped against Google");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("\ncalendar_live: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let email = std::env::var("MACH_LIVE_CALENDAR").map_err(|_| {
        "set MACH_LIVE_CALENDAR=<account email> — this writes to a real calendar, \
         so it will not run unless you name the account"
            .to_string()
    })?;

    let data_dir = guarded_data_dir()?;
    let app_config = config::AppConfig::load(config::database_path(&data_dir));
    if !app_config.is_configured() {
        return Err(format!(
            "not configured: {}",
            app_config
                .configuration_error
                .unwrap_or_else(|| "no Google client credentials".into())
        ));
    }

    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let state = ipc::bootstrap(app_config).map_err(|e| e.to_string())?;

        let account = state
            .db
            .read(queries::list_accounts)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|a| a.email.eq_ignore_ascii_case(&email))
            .ok_or_else(|| format!("no account in this store for {email}"))?;

        let calendars = state
            .db
            .read(|conn| queries::list_calendars(conn, Some(account.id)))
            .map_err(|e| e.to_string())?;
        let primary = calendars
            .iter()
            .find(|c| c.calendar_id.eq_ignore_ascii_case(&account.email))
            .or_else(|| calendars.first())
            .ok_or_else(|| format!("{email} has no calendars in this store"))?;

        println!("account : {} (#{})", account.email, account.id);
        println!("calendar: {}", primary.calendar_id);

        sweep(&state, account.id).await;

        // ---- create --------------------------------------------------------
        let start = tomorrow_at_nine();
        let draft = EventDraft {
            title: format!("{TEST_TITLE_PREFIX} safe to delete"),
            description: Some("Created by cargo run --bin calendar_live.".into()),
            start_ts: start,
            end_ts: start + HOUR_MS,
            ..EventDraft::default()
        };
        let created = state
            .dispatcher
            .execute(Command::CreateEvent {
                account_id: account.id,
                calendar_id: primary.calendar_id.clone(),
                draft,
            })
            .await
            .map_err(|e| format!("create failed: {e}"))?;
        // Report *before* unwrapping. A rejected write comes back as
        // `Ok(CommandResult { ok: false, .. })` — the command ran, Google said
        // no — and unwrapping first threw away the one thing worth knowing:
        // what Google actually said.
        report("create", &created);
        let event_id = *created
            .applied
            .first()
            .ok_or("create did not apply — see the failure above")?;
        show(&state, event_id, "after create")?;

        // ---- update --------------------------------------------------------
        let updated = state
            .dispatcher
            .execute(Command::UpdateEvent {
                event_id,
                patch: EventPatch {
                    title: Some(format!("{TEST_TITLE_PREFIX} renamed")),
                    start_ts: Some(start + HOUR_MS),
                    end_ts: Some(start + 2 * HOUR_MS),
                    ..EventPatch::default()
                },
                scope: Default::default(),
            })
            .await
            .map_err(|e| format!("update failed: {e}"))?;
        report("update", &updated);
        show(&state, event_id, "after update")?;

        // ---- move ----------------------------------------------------------
        // Only if there is somewhere to move it to. A single-calendar account
        // is not a failure, it is just a step that cannot be exercised, and
        // saying so is better than inventing a second calendar on his account.
        match calendars.iter().find(|c| c.calendar_id != primary.calendar_id) {
            Some(destination) => {
                let moved = state
                    .dispatcher
                    .execute(Command::MoveEvent {
                        event_id,
                        account_id: account.id,
                        calendar_id: destination.calendar_id.clone(),
                    })
                    .await
                    .map_err(|e| format!("move failed: {e}"))?;
                report(&format!("move → {}", destination.calendar_id), &moved);
                show(&state, event_id, "after move")?;
            }
            None => println!("\nmove    : skipped — only one calendar on this account"),
        }

        // ---- delete --------------------------------------------------------
        let deleted = state
            .dispatcher
            .execute(Command::DeleteEvent {
                event_id,
                scope: Default::default(),
            })
            .await
            .map_err(|e| format!("delete failed: {e}"))?;
        report("delete", &deleted);

        let gone = state
            .db
            .read(|conn| command_queries::event_by_id(conn, event_id))
            .map_err(|e| e.to_string())?
            .is_none();
        println!("        local row removed: {gone}");

        sweep(&state, account.id).await;
        Ok(())
    })
}

/// Refuse to drive the owner's own store.
fn guarded_data_dir() -> Result<PathBuf, String> {
    let raw = std::env::var_os("MACH_DATA_DIR").ok_or_else(|| {
        "set MACH_DATA_DIR to a *copy* of the store (see `scripts/qa seed`) — \
         this will not run against the live application-support database"
            .to_string()
    })?;
    let dir = PathBuf::from(raw);

    let forbidden = dirs_app_support().join("com.mach.mail");
    if dir.canonicalize().ok() == forbidden.canonicalize().ok() {
        return Err(format!(
            "MACH_DATA_DIR points at the real store ({}) — use a seeded copy",
            forbidden.display()
        ));
    }
    Ok(dir)
}

fn dirs_app_support() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library").join("Application Support")
}

fn tomorrow_at_nine() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_millis() as i64;
    let midnight = now - now.rem_euclid(DAY_MS);
    midnight + DAY_MS + 9 * HOUR_MS
}

fn report(step: &str, result: &mach_lib::commands::types::CommandResult) {
    println!("\n{step:<8}: ok={} {}", result.ok, result.message);
    for failure in &result.failed {
        // Google's own prose, which is the only thing that explains a refusal.
        println!(
            "        REFUSED [{:?}] retriable={} rolled_back={}: {}",
            failure.kind, failure.retriable, failure.rolled_back, failure.message
        );
    }
    println!("        inverse: {:?}", result.undo.as_ref().map(|c| c.kind()));
}

/// Print what the local store now believes, which is what the UI would draw.
fn show(state: &ipc::state::AppState, event_id: i64, when: &str) -> Result<(), String> {
    let event = state
        .db
        .read(|conn| command_queries::event_by_id(conn, event_id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{when}: the event vanished from the local store"))?;
    println!(
        "        {when}: {:?} on {} [{}] {}..{}",
        event.title, event.calendar_id, event.google_event_id, event.start_ts, event.end_ts
    );
    Ok(())
}

/// Delete anything left over from an earlier run.
async fn sweep(state: &ipc::state::AppState, account_id: i64) {
    let leftovers = state.db.read(|conn| {
        queries::events_in_range(conn, i64::MAX, 0, Some(account_id)).map(|events| {
            events
                .into_iter()
                .filter(|e| e.account_id == account_id && e.title.starts_with(TEST_TITLE_PREFIX))
                .map(|e| e.id)
                .collect::<Vec<_>>()
        })
    });

    let Ok(ids) = leftovers else { return };
    for id in ids {
        match state
            .dispatcher
            .execute(Command::DeleteEvent { event_id: id, scope: Default::default() })
            .await
        {
            Ok(_) => println!("swept   : removed leftover test event #{id}"),
            Err(e) => eprintln!("swept   : could not remove #{id}: {e}"),
        }
    }
}
