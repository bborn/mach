//! Replies written before he arrives at the conversation.
//!
//! ```text
//!   sync::mail::incremental ──► suggest::plan ──► rule::earns_a_suggestion
//!    (only on an already-      (one transaction)         │
//!     synced account)                                    ▼
//!                                              voice::examples  (his Sent mail, FTS5)
//!                                                        │
//!                                                        ▼
//!                                              prompt + agent::Completer
//!                                                (one call, a cheap model)
//!                                                        │
//!                                                        ▼
//!                                              store::save  ──►  SQLite, and nowhere else
//!
//!   opening a thread ──► ipc::suggest ──► store::fresh_for_thread ──► the stance row
//! ```
//!
//! # The three rules that shape everything here
//!
//! **A suggestion is not a draft.** Nothing in this module calls
//! `drafts.create`, and nothing in it can: it has no Gmail client and no path to
//! one. A stance becomes a Gmail draft at the moment he presses it and the
//! ordinary composer runs, and not one instant before. The hazard is concrete —
//! a Gmail draft appears on his phone and can be sent from it by a thumb — and
//! the defence is structural rather than a rule somebody has to remember.
//!
//! **Nothing announces itself.** No banner, no badge, no toast. The stances are
//! there when he arrives at the conversation and absent otherwise. A feature
//! that interrupts to say it has an opinion has already cost more than it saves.
//!
//! **Stale beats wrong.** A conversation that has moved on since the stances
//! were written gets none. See [`store::fresh_for_thread`] — the check is a
//! comparison against the thread's own newest message, so there is no sweep to
//! forget to run.
//!
//! # Why this is not a session
//!
//! [`crate::ipc::agent::engine::session`] is owner-driven: it opens the drawer, streams into
//! a transcript, and parks on the approval desk for anything that touches
//! another human. Every one of those is wrong here. This runs unattended, has
//! nothing to show, needs no tools, and must never send. So it uses the narrower
//! seam that already exists for ghost text —
//! [`crate::ipc::agent::engine::complete::Completer`]: one request, no tools, no
//! stream, and a string back. The `Brain` trait is the wrong shape for that and
//! always was; `complete` is what a one-shot structured completion is *for*.
//!
//! # Which brain writes it
//!
//! Whichever one ⌘K would use, resolved the same way — see
//! [`crate::ipc::agent::engine::backend::resolve`]. That is a correction, not a
//! flourish: this shipped reaching only `POST /v1/messages`, so on the machine
//! it was built for — Claude Code installed, no `ANTHROPIC_API_KEY` anywhere —
//! every qualifying message since the morning it landed failed at
//! [`AgentConfig::load`] before a model saw it, and `consider` threw the error
//! away. Nothing was written and nothing was said. Both halves of that are
//! fixed here: the CLI is now a way to reach a model, and every way of failing
//! to reach one reaches the log with a reason.
//!
//! The *model* is this module's own ([`model`]), never the agent's: the
//! preference that says `opus` for the drawer must not silently become the
//! model that answers every inbound email.
//!
//! # Cost
//!
//! One call per qualifying message, on Sonnet or Haiku — see [`model`]. Never
//! Opus: this runs against every human message addressed to him, unattended, and
//! the default must not be the expensive one. Bounded by [`MAX_PER_PASS`] and by
//! the rule that a message is planned only once ever, and switched off entirely
//! by the preference — which means *nothing generates* rather than *nothing
//! displays*.

pub mod prompt;
pub mod rule;
pub mod store;
pub mod voice;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;

use crate::ipc::agent::engine::backend::{self, Availability, BackendPrefs};
use crate::ipc::agent::engine::complete::{
    completer_for, Completer, CompletionRequest, MAX_STRUCTURED_TOKENS,
};
use crate::ipc::agent::engine::wire::ModelTransport;
use crate::db::{queries, sync_queries, Db, Result as DbResult};
use crate::google::types as g;
use crate::ipc::prefs;

pub use prompt::Stance;
pub use rule::{earns_a_suggestion, Candidate, Decline};
pub use store::{Counters, Outcome, Suggestion};

/// Everything an engine needs before it may write a reply suggestion.
///
/// One value rather than two arguments because the two are one decision: this
/// engine is the running app, and it is allowed to spend. A test, a fixture or
/// a tool that only wants a backfill has no [`SuggestBrain`] and therefore no
/// way to reach a model at all — see
/// [`SyncEngine::set_suggest_brain`](crate::sync::SyncEngine::set_suggest_brain).
#[derive(Clone)]
pub struct SuggestBrain {
    /// The Anthropic HTTP path. Used only when that is the backend the owner's
    /// preferences and machine actually resolve to; on the default — Claude
    /// Code — nothing touches it.
    pub transport: Arc<dyn ModelTransport>,
    /// Where a backend that spawns a process may run. Mach's own directory
    /// beside the database, the same one ⌘K's sessions use.
    pub workspace: PathBuf,
}

// ===========================================================================
// Settings
// ===========================================================================

/// Whether the agent writes replies at all. **Default on** — he asked for this
/// and should find it working. Off means nothing generates: no model call, no
/// row, no cost.
pub const ENABLED_KEY: &str = "replySuggestions";

/// A model id, or `""` for the default. Free text for the same reason
/// `agentModel` is: a list would be stale the week after it was written.
pub const MODEL_KEY: &str = "replySuggestionModel";

/// The environment override, for a machine rather than a person.
pub const ENV_MODEL: &str = "MACH_SUGGEST_MODEL";

/// Sonnet, not Opus and not Haiku.
///
/// The task is "write four sentences that sound like this specific person, given
/// six examples of how he writes" — harder than finishing a clause, which is why
/// this is not on the ghost-text model, and nowhere near hard enough to want the
/// model that chains reads into an action somebody will be held to. It runs
/// unattended against every human message addressed to him, so the default is
/// the one that can be wrong cheaply.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";

/// At most this many messages in one sync pass get a suggestion written.
///
/// A pass after a weekend can bring in a hundred qualifying messages, and paying
/// for a hundred at once to answer conversations he will read over the following
/// hour is the shape of a bill nobody expected. The rest are simply not
/// suggested; there is no queue, because a queue would eventually drain into the
/// same bill.
///
/// This and the once-per-message rule in [`plan`] are the *whole* spend bound,
/// and between them they are enough. A message is planned only while no row
/// names it, so a forced sync pressed twenty times in a minute costs nothing
/// after the first — which is what a wall-clock throttle would have been for,
/// and it would have been a second answer to a question already answered.
///
/// Four still holds now that a generation can be a *process* rather than an
/// HTTP request, but the reasoning changed and one thing had to be added. Four
/// sequential `claude` runs are four node processes in a row, each a couple of
/// seconds of startup on top of the model call — call it a minute and a half of
/// background work for a busy pass. The default sync gap is sixty seconds, so
/// two passes could overlap, and nothing here bounded how many children were
/// alive at once. [`GENERATING`] does: one at a time, machine-wide.
pub const MAX_PER_PASS: usize = 4;

/// One generation at a time, machine-wide.
///
/// A pass holds this for its whole run, so a second pass arriving while the
/// first is still writing waits rather than doubling the number of live
/// children. Waiting rather than skipping, because a message is only ever
/// considered once: a skipped message is a message that never gets a reply,
/// and there is nothing on screen this is keeping waiting.
static GENERATING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// The last few messages of a thread are the context; the rest is history.
const CONVERSATION_DEPTH: usize = 4;

/// How much of each earlier message goes in the prompt.
const CONVERSATION_CHARS: usize = 1_200;

/// Whether the feature is on. Absent means on.
pub fn enabled(conn: &Connection) -> DbResult<bool> {
    Ok(prefs::get(conn, ENABLED_KEY)?
        .and_then(|v| v.as_bool())
        .unwrap_or(true))
}

/// The model to write on: the preference, then the environment, then Sonnet.
///
/// The preference wins over the environment, unlike ghost text, because the
/// preference is the thing with a control in ⌘, and a variable exported into a
/// shell should not silently outrank a switch he can see.
pub fn model(conn: &Connection) -> DbResult<String> {
    let stored = prefs::get(conn, MODEL_KEY)?
        .and_then(|v| v.as_str().map(str::to_string))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if let Some(stored) = stored {
        return Ok(stored);
    }
    Ok(std::env::var(ENV_MODEL)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string()))
}

// ===========================================================================
// What one message needs before a model sees it
// ===========================================================================

/// The headers the rule reads, lifted off the wire response while they are still
/// in hand. They are not on the message row — nothing else needs them — so they
/// are carried from the sync loop rather than read back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    pub list_unsubscribe: Option<String>,
    pub list_id: Option<String>,
    pub precedence: Option<String>,
    pub auto_submitted: Option<String>,
    pub auto_response_suppress: Option<String>,
}

impl Headers {
    /// Read them off a `users.messages.get` response.
    pub fn of(message: &g::Message) -> Headers {
        Headers {
            list_unsubscribe: message.header("List-Unsubscribe"),
            list_id: message.header("List-Id"),
            precedence: message.header("Precedence"),
            auto_submitted: message.header("Auto-Submitted"),
            auto_response_suppress: message.header("X-Auto-Response-Suppress"),
        }
    }
}

/// One message the model is going to be asked about, with everything it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub account_id: i64,
    pub thread_id: i64,
    pub message_id: i64,
    pub gmail_message_id: String,
    pub owner_name: String,
    pub owner_email: String,
    /// The sender, rendered for the prompt: `Kate <kate@example.org>`.
    pub correspondent: String,
    /// Just the address, for the same-correspondent voice lookup.
    pub correspondent_email: String,
    pub subject: String,
    /// The conversation up to and including this message, oldest first, as
    /// `(who, what)`.
    pub conversation: Vec<(String, String)>,
    /// The message being answered, on its own.
    pub incoming: String,
}

/// Which of a sweep's arrivals deserve a reply written for them.
///
/// One read transaction, and the whole decision: the preference, the rule, the
/// dedup, and the throttle. Split out from [`consider`] for the same reason
/// [`crate::notify::plan`] is — it takes a connection and returns a value, with
/// no network and no globals anywhere near it.
pub fn plan(
    conn: &Connection,
    account_id: i64,
    arrived: &[String],
    headers: &HashMap<String, Headers>,
) -> DbResult<Vec<Job>> {
    if arrived.is_empty() || !enabled(conn)? {
        return Ok(Vec::new());
    }
    let Some((owner_name, owner_email)) = account(conn, account_id)? else {
        return Ok(Vec::new());
    };

    let mut jobs = Vec::new();
    for gmail_message_id in arrived {
        if jobs.len() == MAX_PER_PASS {
            break;
        }
        let Some(message) = queries::message_by_gmail_id(conn, account_id, gmail_message_id)? else {
            continue;
        };
        // Already answered — a replayed history window, or the same message
        // reported by two records in one sweep.
        if store::exists_for_message(conn, message.thread_id, message.id)? {
            continue;
        }

        let labels =
            sync_queries::message_labels(conn, account_id, gmail_message_id)?.unwrap_or_default();
        let header = headers.get(gmail_message_id).cloned().unwrap_or_default();
        let candidate = Candidate {
            from_email: message.from.email.clone(),
            to: message.to.iter().map(|p| p.email.clone()).collect(),
            cc: message.cc.iter().map(|p| p.email.clone()).collect(),
            labels,
            list_unsubscribe: header.list_unsubscribe,
            list_id: header.list_id,
            precedence: header.precedence,
            auto_submitted: header.auto_submitted,
            auto_response_suppress: header.auto_response_suppress,
            thread_has_own_message: thread_has_own_message(
                conn,
                message.thread_id,
                message.id,
                &owner_email,
            )?,
        };
        if earns_a_suggestion(&candidate, &owner_email).is_err() {
            continue;
        }

        let conversation = conversation(conn, message.thread_id)?;

        jobs.push(Job {
            account_id,
            thread_id: message.thread_id,
            message_id: message.id,
            gmail_message_id: gmail_message_id.clone(),
            owner_name: owner_name.clone(),
            owner_email: owner_email.clone(),
            correspondent: display(&message.from),
            correspondent_email: message.from.email.clone(),
            subject: message.subject.clone(),
            conversation,
            incoming: clip(
                body_of(message.body_text.as_deref(), &message.snippet),
                CONVERSATION_CHARS * 2,
            ),
        });
    }
    Ok(jobs)
}

/// Write the stances for one job.
///
/// Two reads and one call: his voice out of the store, the prompt, the model.
/// Returns what was written, which is what the tests assert on and the caller
/// ignores.
///
/// There is still no error *state* for a suggestion — nothing is waiting for
/// one, and a conversation with no stances looks exactly like a conversation
/// that never earned any. But there is now an error *line*. A feature that is
/// switched on, doing nothing, and saying nothing is the state this whole
/// module spent its first day in.
pub async fn generate(
    db: &Db,
    completer: &dyn Completer,
    model_id: &str,
    job: &Job,
    now_ms: i64,
) -> Vec<Stance> {
    let job_examples = {
        let account_id = job.account_id;
        let owner = job.owner_email.clone();
        let correspondent = job.correspondent_email.clone();
        let topic = format!("{} {}", job.subject, job.incoming);
        db.read(move |conn| voice::examples(conn, account_id, &owner, &correspondent, &topic))
            .unwrap_or_default()
    };

    let request = CompletionRequest::structured(
        prompt::system_prompt(&job.owner_name, &job.owner_email),
        prompt::user_prompt(
            &job.correspondent,
            &job.subject,
            &job.conversation,
            &job.incoming,
            &job_examples,
        ),
        MAX_STRUCTURED_TOKENS,
    );

    let text = match completer.complete(model_id, &request).await {
        Ok(text) => text,
        Err(error) => {
            eprintln!(
                "reply suggestions: {} could not write a reply on {model_id} — {error}",
                completer.label()
            );
            return Vec::new();
        }
    };
    let stances = prompt::parse_stances(&text);
    if stances.is_empty() {
        // Usually a refusal, or a model that answered in prose instead of the
        // document it was asked for. Cheap to say, and the alternative is a
        // feature that bills and produces nothing without a trace.
        eprintln!(
            "reply suggestions: {model_id} answered with nothing usable for message {}",
            job.gmail_message_id
        );
        return Vec::new();
    }

    // The write and the counter together, so a crash between them cannot make
    // the hit rate flatter than it should be.
    let written = stances.clone();
    let job = job.clone();
    let model_id = model_id.to_string();
    let _ = db.write_background(move |conn| {
        store::save(
            conn,
            job.account_id,
            job.thread_id,
            job.message_id,
            &job.gmail_message_id,
            &written,
            &model_id,
            now_ms,
        )?;
        store::record(conn, Outcome::Suggested, None, "", now_ms)
    });
    stances
}

/// The sync loop's door in.
///
/// Returns immediately. Everything past the plan happens on a task of its own,
/// because a mail client whose sync pass waits on a model is a mail client that
/// stalls when Anthropic is slow — and there is nothing on screen that this is
/// keeping waiting.
pub fn consider(
    db: &Db,
    brain: SuggestBrain,
    account_id: i64,
    arrived: &[String],
    headers: HashMap<String, Headers>,
) {
    if arrived.is_empty() {
        return;
    }
    let arrived = arrived.to_vec();
    let db = db.clone();

    tokio::spawn(async move {
        let Ok(jobs) = db.read(|conn| plan(conn, account_id, &arrived, &headers)) else {
            return;
        };
        if jobs.is_empty() {
            return;
        }
        let completer = match resolve_completer(&db, brain) {
            Ok(completer) => completer,
            // The line this feature shipped without. A brain that cannot be
            // resolved is not a quiet no-op: it is every message this pass
            // qualified, dropped, for a reason that names its own remedy.
            Err(error) => {
                eprintln!(
                    "reply suggestions: {} message(s) went unanswered — {error}",
                    jobs.len()
                );
                return;
            }
        };
        let Ok(model_id) = db.read(model) else {
            return;
        };

        // Held for the whole pass rather than per job — see [`GENERATING`].
        let _permit = GENERATING.acquire().await;
        for job in jobs {
            generate(&db, completer.as_ref(), &model_id, &job, now_ms()).await;
        }
        // Cheap, and it is the only place that runs often enough to be worth
        // hanging the housekeeping on.
        let _ = db.write_background(|conn| store::purge_stale(conn));
    });
}

/// Which brain writes the replies, resolved exactly as ⌘K resolves it.
///
/// Two departures from the drawer's resolution, both about money:
///
/// - **the model is dropped.** `agentModel` is the owner talking about the
///   drawer, where `opus` is a reasonable answer; this runs unattended against
///   every human message addressed to him, and [`model`] — Sonnet by default —
///   is the only model it may name.
/// - **nothing is pinned.** [`backend::resolve`] takes an optional
///   [`AgentConfig`](crate::ipc::agent::engine::config::AgentConfig) for callers
///   that want a specific one without exporting a variable. There is no such
///   caller here, so the API path is only ever reached through a credential the
///   owner actually configured.
///
/// Resolved after the plan says there is work: a machine with no brain should
/// not be asked about one on every sync pass, only on the passes where it would
/// have mattered.
fn resolve_completer(
    db: &Db,
    brain: SuggestBrain,
) -> Result<Box<dyn Completer>, crate::ipc::agent::engine::AgentError> {
    let prefs = BackendPrefs {
        model: None,
        ..BackendPrefs::load(db)
    };
    let backend = backend::resolve(&prefs, &Availability::probe(), None)?;
    completer_for(backend, brain.transport, brain.workspace)
}

// ===========================================================================
// Small readers
// ===========================================================================

fn account(conn: &Connection, account_id: i64) -> DbResult<Option<(String, String)>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT COALESCE(display_name, ''), email FROM accounts WHERE id = ?1",
            [account_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?)
}

/// The last few messages of a conversation, oldest first, as `(who, what)`.
///
/// A dedicated query rather than [`queries::messages_for_thread`], which is the
/// reading pane's read: it loads every message in the thread with its body and
/// joins the invitation table, and a long conversation has hundreds. This wants
/// four, and it is on a path that runs unattended after every sync pass.
fn conversation(conn: &Connection, thread_id: i64) -> DbResult<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT from_name, from_email, COALESCE(body_text, ''), snippet
           FROM messages
          WHERE thread_id = ?1 AND is_draft = 0
          ORDER BY internal_date DESC, id DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![thread_id, CONVERSATION_DEPTH as i64],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;

    let mut out = Vec::new();
    for row in rows {
        let (name, email, body, snippet) = row?;
        out.push((
            display(&crate::db::models::Participant { name, email }),
            clip(body_of(Some(body.as_str()), &snippet), CONVERSATION_CHARS),
        ));
    }
    // Newest-first off the index, oldest-first into the prompt.
    out.reverse();
    Ok(out)
}

fn thread_has_own_message(
    conn: &Connection,
    thread_id: i64,
    except: i64,
    own_address: &str,
) -> DbResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM messages
              WHERE thread_id = ?1 AND id <> ?2 AND lower(from_email) = lower(?3)
         )",
        rusqlite::params![thread_id, except, own_address],
        |row| row.get(0),
    )?)
}

/// The text of a message, falling back to Gmail's own preview. An HTML-only
/// message whose text has not been derived yet still has a snippet, and a
/// snippet is enough to write a raincheck from.
fn body_of<'a>(body_text: Option<&'a str>, snippet: &'a str) -> &'a str {
    match body_text.map(str::trim) {
        Some(text) if !text.is_empty() => text,
        _ => snippet,
    }
}

fn display(who: &crate::db::models::Participant) -> String {
    match who.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => format!("{name} <{}>", who.email),
        _ => who.email.clone(),
    }
}

fn clip(text: &str, max: usize) -> String {
    // The quoted history is dropped for the same reason it is dropped from a
    // voice example: it is the model's own previous context, restated, and it
    // crowds out the part that is new.
    let trimmed = voice::for_voice(text).unwrap_or_else(|| text.trim().to_string());
    if trimmed.chars().count() <= max {
        return trimmed;
    }
    trimmed.chars().take(max).collect::<String>() + "…"
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_model_is_neither_opus_nor_the_ghost_text_one() {
        // The rule that costs money if it is ever broken, pinned by name.
        assert!(!DEFAULT_MODEL.contains("opus"), "{DEFAULT_MODEL}");
        assert!(
            DEFAULT_MODEL.contains("sonnet") || DEFAULT_MODEL.contains("haiku"),
            "{DEFAULT_MODEL}"
        );
    }

    #[test]
    fn a_message_with_no_text_falls_back_to_its_snippet() {
        assert_eq!(body_of(Some("the body"), "the snippet"), "the body");
        assert_eq!(body_of(Some("   "), "the snippet"), "the snippet");
        assert_eq!(body_of(None, "the snippet"), "the snippet");
    }

    #[test]
    fn a_sender_is_rendered_with_their_name_when_there_is_one() {
        use crate::db::models::Participant;
        assert_eq!(
            display(&Participant {
                name: Some("Kate".into()),
                email: "kate@example.org".into()
            }),
            "Kate <kate@example.org>"
        );
        assert_eq!(
            display(&Participant {
                name: Some("  ".into()),
                email: "kate@example.org".into()
            }),
            "kate@example.org"
        );
    }

    #[test]
    fn clipping_drops_the_quoted_history() {
        let body = "My actual answer, which is long enough to survive the voice trim.\n\n\
                    > their earlier question\n> and more of it";
        let clipped = clip(body, 500);
        assert!(clipped.contains("My actual answer"));
        assert!(!clipped.contains("their earlier question"));
    }

    #[test]
    fn a_short_message_that_is_all_quote_still_reaches_the_model() {
        // `for_voice` refuses it as a *voice example* — too short to teach
        // anything — but it is the message being answered, so it must not
        // vanish on the way into the prompt.
        let clipped = clip("ok?", 500);
        assert_eq!(clipped, "ok?");
    }
}
