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

/// Longer than anything a person typed, short enough that one runaway
/// newsletter cannot spend the whole payload on itself.
const CLIPBOARD_BODY_CHARS: usize = 8_000;

/// The ceiling on a whole clipboard payload, in characters.
///
/// Somewhere around fifteen thousand tokens: inside every chat box worth
/// pasting into, and past the point where more mail makes the answer better.
/// What matters more than the exact number is that hitting it is *said* — in
/// the text, at the point it stops, and in the toast.
const CLIPBOARD_TOTAL_CHARS: usize = 60_000;

/// Who a rendered block is for.
///
/// Same items, same resolution against the store; what differs is how much of a
/// conversation comes out and what the text says where it stops.
///
/// The agent's block is small on purpose — it holds `get_thread`, so three
/// messages is almost always the whole ask and the rest is one tool call away.
/// A block on its way to the clipboard has no second call to make: what is not
/// in the text does not exist for whoever receives it. So it carries every
/// message up to a ceiling, and names what it left behind when it hits one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// The model at the other end of a session.
    Model,
    /// A chat window somewhere else, by way of ⌘⌥C.
    Clipboard,
}

impl Audience {
    fn opening(self) -> &'static str {
        match self {
            Audience::Model => "<context>\nWhat the owner is looking at right now:\n",
            // Read by somebody who has never heard of Mach, and addressed *by*
            // the owner rather than about him.
            Audience::Clipboard => {
                "<context>\nThis is what I am looking at in my mail and calendar client:\n"
            }
        }
    }

    fn inline_messages(self) -> usize {
        match self {
            Audience::Model => INLINE_MESSAGES,
            Audience::Clipboard => usize::MAX,
        }
    }

    fn body_chars(self) -> usize {
        match self {
            Audience::Model => INLINE_BODY_CHARS,
            Audience::Clipboard => CLIPBOARD_BODY_CHARS,
        }
    }

    fn total_chars(self) -> usize {
        match self {
            Audience::Model => usize::MAX,
            Audience::Clipboard => CLIPBOARD_TOTAL_CHARS,
        }
    }

    /// What to say where the text stops short.
    fn truncated_hint(self) -> &'static str {
        match self {
            Audience::Model => "call get_thread for the rest",
            Audience::Clipboard => "the rest of this message was not copied",
        }
    }
}

/// A rendered block, and whether anything was left out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// True when a body was clipped or a message did not fit under the ceiling.
    /// The caller says so out loud; the text says so in place.
    pub truncated: bool,
}

/// How much room is left, and whether anything has been dropped for want of it.
struct Budget {
    remaining: usize,
    trimmed: bool,
}

impl Budget {
    fn take(&mut self, chars: usize) -> bool {
        if chars > self.remaining {
            self.trimmed = true;
            return false;
        }
        self.remaining -= chars;
        true
    }
}

/// The `<context>` block prepended to the first user message.
///
/// Empty when nothing is attached — an unadorned question must not arrive
/// wrapped in ceremony.
pub fn render(db: &Db, items: &[ContextItem]) -> Result<String, AgentError> {
    Ok(render_for(db, items, Audience::Model)?.text)
}

/// The same block, at the budget its reader deserves. See [`Audience`].
pub fn render_for(
    db: &Db,
    items: &[ContextItem],
    audience: Audience,
) -> Result<Rendered, AgentError> {
    if items.is_empty() {
        return Ok(Rendered {
            text: String::new(),
            truncated: false,
        });
    }

    let mut budget = Budget {
        remaining: audience.total_chars(),
        trimmed: false,
    };

    let mut out = String::from(audience.opening());
    for item in items {
        out.push_str(&render_item(db, item, audience, &mut budget)?);
    }
    out.push_str("</context>\n\n");
    Ok(Rendered {
        text: out,
        truncated: budget.trimmed,
    })
}

fn render_item(
    db: &Db,
    item: &ContextItem,
    audience: Audience,
    budget: &mut Budget,
) -> Result<String, AgentError> {
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
                let skipped = total.saturating_sub(audience.inline_messages());
                if skipped > 0 {
                    out.push_str(&format!(
                        "({skipped} earlier message(s) not shown — call get_thread for the full conversation)\n"
                    ));
                }

                // Oldest first, and the ceiling bites at the *end*: a
                // conversation cut off before its most recent message would be
                // the one shape of truncation that changes what it says.
                let mut written = 0usize;
                let showing = total - skipped;
                for message in detail.messages.iter().skip(skipped) {
                    let block = format!(
                        "\n--- from {} at {}\n{}\n",
                        who(&message.from),
                        human_time(message.internal_date),
                        clip(&body_of(message), audience.body_chars(), audience, budget)
                    );
                    if !budget.take(block.chars().count()) {
                        break;
                    }
                    out.push_str(&block);
                    written += 1;
                }
                let dropped = showing - written;
                if dropped > 0 {
                    out.push_str(&format!(
                        "\n[{dropped} more message(s) left out — this copy stops at {CLIPBOARD_TOTAL_CHARS} characters]\n"
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

/// What a message *says*, with the history it was quoting on top of taken off.
///
/// A ten-reply thread rendered whole with its quotes carries the first message
/// ten times, and every message after it nine, eight, seven times over. That is
/// most of the payload and none of the information, which matters for a model
/// paying by the token and matters again for a paste into somebody else's chat
/// box. `render/quotes` is the same splitter the reading pane collapses with, so
/// what gets copied is what he can see without expanding anything.
///
/// Both paths fall back to the unsplit body when the split leaves nothing: a
/// bare forward is *all* quote, and a message rendered as an empty string would
/// be a worse answer than a redundant one.
fn body_of(message: &crate::db::models::Message) -> String {
    match (&message.body_text, &message.body_html) {
        (Some(text), _) if !text.trim().is_empty() => {
            let split = crate::render::quotes::split_text(text);
            if split.new.trim().is_empty() {
                text.clone()
            } else {
                split.new
            }
        }
        (_, Some(html)) if !html.trim().is_empty() => {
            let split = crate::render::quotes::split_html(html);
            let source = if split.new.trim().is_empty() {
                html.as_str()
            } else {
                split.new.as_str()
            };
            crate::render::text::from_sanitized(&crate::render::render_html(source).html)
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

fn clip(text: &str, max: usize, audience: Audience, budget: &mut Budget) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    budget.trimmed = true;
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}\n… [truncated — {}]", audience.truncated_hint())
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
- An event or a conversation you read by id is drawn for him as a card, with its time, its \
guests, its video link and a way to open it. So call get_event or get_thread for the one \
your answer is about — that is what puts it on his screen — and then refer to it in a \
sentence. Do not retype what the card already shows as a list of fields.\n\
- To answer a message: draft_reply to write it, then send_draft to send or schedule it. \
send_draft always asks him first — write the draft, then propose sending it; do not ask \
permission in prose beforehand.\n\
- To write to somebody rather than answer them: draft_message, with the subject you want. \
Never reply to an unrelated thread to reach a person — the message would carry that \
thread's subject.\n\
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
