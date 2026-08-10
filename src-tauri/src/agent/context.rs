//! What "this" means.
//!
//! `⌘K` from anywhere has to let the owner talk about **whatever is on the
//! screen** without naming it. So the frontend attaches what is selected — the
//! open thread, the selected event, the day the calendar is anchored on, the
//! search that is showing — and this module turns those references into the
//! text the model actually reads.
//!
//! Two rules make it trustworthy:
//!
//! 1. **Resolve, don't assert.** A [`ContextItem`] carries an id; this module
//!    reads the row and renders the real subject, participants and body. The
//!    frontend's label is only what the *owner* sees on the removable line, so
//!    the two can never quietly disagree about which thread is attached.
//! 2. **Show it.** Every item comes back with a `label`, and the session
//!    surfaces those. Implicit attachment is what makes it feel like talking
//!    about the screen; showing it is what stops the agent silently working
//!    from the wrong thing.
//!
//! Time is here for the same reason. "next tues" only resolves if the model
//! knows what today is, in the owner's timezone, so [`system_prompt`] states it
//! outright with the epoch milliseconds alongside — a model that has both never
//! has to guess an offset.

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::db::{queries, Db};

use super::error::AgentError;

/// One thing the owner was looking at.
///
/// `kind` is open rather than an enum on purpose: the frontend attaches what it
/// has, and a kind this build does not know how to expand still renders its
/// label and its detail line rather than being dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextItem {
    /// Stable within a session, so the UI can remove one by id.
    pub id: String,
    /// `thread`, `event`, `day`, `search`, `mailbox`, `selection`.
    pub kind: String,
    /// What the removable line says: `Re: Series A data room`.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<i64>,
    /// A free-text detail the frontend already knows — the current search
    /// string, the day in view, the mailbox name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// How much of an attached thread to inline before making the agent call
/// `get_thread` for the rest. Two messages is almost always the whole ask
/// ("reply to this") and keeps the first request small.
const INLINE_MESSAGES: usize = 3;
const INLINE_BODY_CHARS: usize = 2_000;

/// The `<context>` block prepended to the first user message.
///
/// Empty when nothing is attached — an unadorned question must not arrive
/// wrapped in ceremony.
pub fn render(db: &Db, items: &[ContextItem]) -> Result<String, AgentError> {
    if items.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::from("<context>\nWhat the owner is looking at right now:\n");
    for item in items {
        out.push_str(&render_item(db, item)?);
    }
    out.push_str("</context>\n\n");
    Ok(out)
}

fn render_item(db: &Db, item: &ContextItem) -> Result<String, AgentError> {
    let mut out = format!("\n[{}] {}\n", item.kind, item.label);

    if let Some(thread_id) = item.thread_id {
        match db.read(|conn| queries::thread_with_messages(conn, thread_id))? {
            Some(detail) => {
                out.push_str(&format!(
                    "threadId: {}  account: {}  labels: {}\n",
                    detail.thread.id,
                    detail.thread.account_email,
                    detail.thread.label_ids.join(", ")
                ));
                out.push_str(&format!("subject: {}\n", detail.thread.subject));
                let total = detail.messages.len();
                let skipped = total.saturating_sub(INLINE_MESSAGES);
                if skipped > 0 {
                    out.push_str(&format!(
                        "({skipped} earlier message(s) not shown — call get_thread for the full conversation)\n"
                    ));
                }
                for message in detail.messages.iter().skip(skipped) {
                    out.push_str(&format!(
                        "\n--- from {} at {}\n{}\n",
                        who(&message.from),
                        human_time(message.internal_date),
                        clip(&body_of(message), INLINE_BODY_CHARS)
                    ));
                }
            }
            // The row can be gone — a sync pass deleted it between the ⌘K and
            // the request. Say so instead of pretending it is attached.
            None => out.push_str(&format!(
                "threadId: {thread_id} (no longer in the local store)\n"
            )),
        }
    }

    if let Some(event_id) = item.event_id {
        let event = db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, start_ts, end_ts, location FROM events WHERE id = ?1",
            )?;
            let mut rows = stmt.query([event_id])?;
            let found = match rows.next()? {
                Some(row) => Some((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                )),
                None => None,
            };
            Ok(found)
        })?;
        match event {
            Some((id, title, start, end, location)) => out.push_str(&format!(
                "eventId: {id}  {title}\nwhen: {} – {}{}\n",
                human_time(start),
                human_time(end),
                location.map(|l| format!("\nwhere: {l}")).unwrap_or_default(),
            )),
            None => out.push_str(&format!(
                "eventId: {event_id} (no longer in the local store)\n"
            )),
        }
    }

    if let Some(detail) = &item.detail {
        out.push_str(&format!("{detail}\n"));
    }

    Ok(out)
}

fn body_of(message: &crate::db::models::Message) -> String {
    match (&message.body_text, &message.body_html) {
        (Some(text), _) if !text.trim().is_empty() => crate::render::quotes::split_text(text).new,
        (_, Some(html)) if !html.trim().is_empty() => {
            super::tools::html_to_text(&crate::render::render_html(html).html)
        }
        _ => message.snippet.clone(),
    }
}

/// `Tawny Chen <tawny@example.com>`, degrading to whichever half exists.
pub fn who(p: &crate::db::models::Participant) -> String {
    match (&p.name, p.email.trim()) {
        (Some(name), "") => name.clone(),
        (Some(name), email) => format!("{name} <{email}>"),
        (None, "") => "(unknown)".to_string(),
        (None, email) => email.to_string(),
    }
}

fn clip(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}\n… [truncated — call get_thread for the rest]")
}

/// `Tue 12 Aug 2026, 09:30` in the owner's timezone. Used in prompts and in the
/// one-line summaries the drawer shows, so both read the same.
pub fn human_time(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(dt) => dt.format("%a %-d %b %Y, %H:%M").to_string(),
        None => format!("{ms} (unix ms)"),
    }
}

/// The system prompt.
///
/// Written as context rather than commands, and short: the tool descriptions
/// carry the mechanics, and a long prompt would only compete with them.
pub fn system_prompt(db: &Db, now_ms: i64, has_plugin_tools: bool) -> String {
    let now = Local.timestamp_millis_opt(now_ms).single();
    let clock = match now {
        Some(dt) => format!(
            "It is {} ({}), unix milliseconds {now_ms}.",
            dt.format("%A %-d %B %Y, %H:%M"),
            dt.format("%Z %:z"),
        ),
        None => format!("The current time is unix milliseconds {now_ms}."),
    };

    let accounts = db
        .read(queries::list_accounts)
        .map(|list| {
            list.iter()
                .map(|a| format!("{} (accountId {})", a.email, a.id))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    // Only when at least one is offered. Standing text about a threat that is
    // not present is text the model has to read on every unrelated turn.
    let plugins = if has_plugin_tools {
        format!("\n\n{}", super::plugin_tools::PLUGIN_PROMPT)
    } else {
        String::new()
    };

    format!(
        "You are the agent inside Mach, the user's mail and calendar client. You are talking to \
them about what is on their screen.\n\n\
{clock} Resolve relative dates against that, in their local timezone, and pass unix \
milliseconds to tools. \"Next Tuesday\" means the Tuesday of next week; a bare weekday \
means the next one to come.\n\n\
Accounts: {accounts}\n\n\
Everything you do goes through the same typed commands the keyboard uses, and every one of \
them that acts on mail is undoable. There is no other path to Gmail or Calendar.\n\n\
How to work:\n\
- The <context> block is what he is looking at. \"this\", \"it\" and \"that thread\" mean \
whatever is in it. Never ask which thread he means when the context already says.\n\
- Read before you act. Use get_thread, search_threads and list_events rather than guessing.\n\
- To answer a message: draft_reply to write it, then send_draft to send or schedule it. \
send_draft always asks him first — write the draft, then propose sending it; do not ask \
permission in prose beforehand.\n\
- Match his voice in replies: direct, lowercase-ish, no corporate throat-clearing, no \
\"I hope this email finds you well\", no signature (the account adds one).\n\
- To stop a kind of mail arriving in future — receipts, login codes, a mailing list — \
create_filter makes a Gmail filter. It is a standing rule and is not undoable, so it asks \
him first, and it does nothing to mail already in the mailbox: deal with that separately \
with search_threads and archive or trash, which are undoable.\n\
- Say what you did in one or two sentences. He can see the tool calls; do not narrate them.\n\
- If something cannot be done, say so plainly and stop.{plugins}"
    )
}
