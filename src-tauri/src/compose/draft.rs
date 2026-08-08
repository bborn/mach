//! Drafts: what you are still typing, and what a reply is made of.
//!
//! # Autosave
//!
//! A draft is a row, saved on a debounce from the editor. Not a file, not
//! `localStorage`: the store is already the thing that survives a crash, it is
//! already backed up with the mailbox, and a draft that lives in the webview is
//! lost by exactly the failure it is supposed to survive. The row is keyed by
//! the draft's own id and indexed by thread, so reopening a conversation finds
//! the half-written reply without a search.
//!
//! # What a draft does *not* contain
//!
//! The quoted original. Quoting is applied at send, from the stored parent
//! message, for two reasons: the user should be able to keep typing without
//! scrolling past a screen of somebody else's mail, and the quote has to be
//! built the same way whether a human or the agent produced the body.

use serde::{Deserialize, Serialize};

use crate::db::models::{Message, Participant};
use crate::db::{command_queries, queries, Db};

use super::address::{forward_recipients, reply_recipients};
use super::markdown;
use super::mime::{
    forward_subject, generate_message_id, references_for_reply, reply_subject, Mailbox, Outgoing,
};
use super::{ensure_compose_schema, ComposeError, Result};

/// Which shape of message this is. The kind is stored rather than inferred so a
/// reopened draft still knows whether Cc was deliberately emptied or never
/// filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DraftKind {
    New,
    Reply,
    ReplyAll,
    Forward,
}

impl DraftKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DraftKind::New => "new",
            DraftKind::Reply => "reply",
            DraftKind::ReplyAll => "replyAll",
            DraftKind::Forward => "forward",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "reply" => DraftKind::Reply,
            "replyAll" => DraftKind::ReplyAll,
            "forward" => DraftKind::Forward,
            _ => DraftKind::New,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: String,
    pub account_id: i64,
    #[serde(default)]
    pub thread_id: Option<i64>,
    /// Local row id of the message being answered. This, not `thread_id`, is
    /// what the threading headers are derived from — a reply belongs to a
    /// specific message in the conversation, and replying to the third message
    /// of five should not claim the fifth as its parent.
    #[serde(default)]
    pub reply_to_id: Option<i64>,
    pub kind: DraftKind,
    #[serde(default)]
    pub to: Vec<Mailbox>,
    #[serde(default)]
    pub cc: Vec<Mailbox>,
    #[serde(default)]
    pub bcc: Vec<Mailbox>,
    #[serde(default)]
    pub subject: String,
    /// Markdown-ish source, exactly as typed.
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub updated_at: i64,
}

impl Draft {
    pub fn is_empty(&self) -> bool {
        self.body.trim().is_empty() && self.subject.trim().is_empty() && self.to.is_empty()
    }
}

/// The thread and message a reply is being written against.
#[derive(Debug, Clone)]
pub struct ReplyContext {
    pub account_id: i64,
    pub account_email: String,
    pub account_name: Option<String>,
    pub thread_id: i64,
    pub gmail_thread_id: String,
    pub parent: Message,
}

// ---------------------------------------------------------------------------
// preparing
// ---------------------------------------------------------------------------

/// Resolve the context for replying to a thread: the account it arrived on and
/// the last message in it.
pub fn context_for_thread(db: &Db, thread_id: i64) -> Result<ReplyContext> {
    let detail = db
        .read(|conn| queries::thread_with_messages(conn, thread_id))?
        .ok_or(ComposeError::UnknownThread(thread_id))?;

    let parent = detail
        .messages
        .last()
        .cloned()
        .ok_or_else(|| ComposeError::invalid("that conversation has no messages to reply to"))?;

    let account_id = detail.thread.account_id;
    let account = db
        .read(|conn| command_queries::account_by_id(conn, account_id))?
        .ok_or(ComposeError::UnknownAccount(account_id))?;

    Ok(ReplyContext {
        account_id,
        account_email: account.email,
        account_name: account.display_name,
        thread_id,
        gmail_thread_id: detail.thread.gmail_thread_id,
        parent,
    })
}

/// A draft, pre-filled from a thread.
///
/// **The from-address is inferred, never chosen.** A reply goes out from the
/// account the thread arrived on, because that is the address the other side
/// wrote to; picking a different one is how a reply ends up in somebody's spam
/// folder and how a work thread accidentally answers from a personal address.
pub fn prepare(db: &Db, thread_id: i64, kind: DraftKind, draft_id: String) -> Result<Draft> {
    let ctx = context_for_thread(db, thread_id)?;

    let (to, cc, subject) = match kind {
        DraftKind::Forward => {
            let r = forward_recipients();
            (r.to, r.cc, forward_subject(&ctx.parent.subject))
        }
        DraftKind::New => (Vec::new(), Vec::new(), String::new()),
        other => {
            let r = reply_recipients(&ctx.parent, &ctx.account_email, other == DraftKind::ReplyAll);
            (r.to, r.cc, reply_subject(&ctx.parent.subject))
        }
    };

    Ok(Draft {
        id: draft_id,
        account_id: ctx.account_id,
        thread_id: Some(ctx.thread_id),
        reply_to_id: Some(ctx.parent.id),
        kind,
        to,
        cc,
        bcc: Vec::new(),
        subject,
        body: String::new(),
        updated_at: 0,
    })
}

// ---------------------------------------------------------------------------
// building the message
// ---------------------------------------------------------------------------

/// What a built draft turns into: the message, plus the Gmail thread it should
/// land in.
#[derive(Debug, Clone)]
pub struct Built {
    pub outgoing: Outgoing,
    pub account_id: i64,
    /// Set only for a reply: this is what makes Gmail file the sent message in
    /// the existing conversation.
    pub gmail_thread_id: Option<String>,
    /// The local thread the optimistic copy belongs in. `None` for a forward or
    /// a fresh message, because neither is part of the conversation it may have
    /// been started from.
    pub thread_id: Option<i64>,
}

/// Turn a draft into the exact bytes' worth of structure that will be sent.
///
/// `now_ms` and `entropy` are arguments rather than reads of the clock so the
/// output is a pure function of its inputs and the tests can assert on the
/// generated RFC822 rather than on a shape.
pub fn build(db: &Db, draft: &Draft, now_ms: i64, entropy: u64) -> Result<Built> {
    let account = db
        .read(|conn| command_queries::account_by_id(conn, draft.account_id))?
        .ok_or(ComposeError::UnknownAccount(draft.account_id))?;

    let from = Mailbox {
        name: account
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty()),
        email: account.email.clone(),
    };

    let parent = match draft.reply_to_id {
        Some(id) => Some(load_message(db, draft.account_id, id)?),
        None => None,
    };

    let (in_reply_to, references) = match (&parent, draft.kind) {
        // A forward starts a new conversation. Threading it onto the original
        // is what makes a forwarded message reappear inside the thread it was
        // taken out of, in the recipient's client and in yours.
        (Some(p), DraftKind::Reply | DraftKind::ReplyAll) => references_for_reply(p),
        _ => (None, Vec::new()),
    };

    let body_text = markdown::to_text(&draft.body);
    let body_html = markdown::to_html(&draft.body);

    let (text, html) = match (&parent, draft.kind) {
        (Some(p), DraftKind::Reply | DraftKind::ReplyAll) => (
            format!("{}\n\n{}", body_text.trim_end(), quote_text(p)),
            format!("{body_html}{}", quote_html(p)),
        ),
        (Some(p), DraftKind::Forward) => (
            format!("{}\n\n{}", body_text.trim_end(), forward_text(p)),
            format!("{body_html}{}", forward_html(p)),
        ),
        _ => (body_text, body_html),
    };

    let is_reply = matches!(draft.kind, DraftKind::Reply | DraftKind::ReplyAll);
    let thread_id = if is_reply { draft.thread_id } else { None };
    let gmail_thread_id = match thread_id {
        Some(id) => db
            .read(|conn| queries::thread_summary(conn, id))?
            .map(|t| t.gmail_thread_id),
        None => None,
    };

    Ok(Built {
        outgoing: Outgoing {
            from: from.clone(),
            to: draft.to.clone(),
            cc: draft.cc.clone(),
            bcc: draft.bcc.clone(),
            subject: draft.subject.clone(),
            text,
            html,
            in_reply_to,
            references,
            message_id: generate_message_id(&from.email, now_ms, entropy),
            date_ms: now_ms,
        },
        account_id: draft.account_id,
        gmail_thread_id,
        thread_id,
    })
}

fn load_message(db: &Db, account_id: i64, message_id: i64) -> Result<Message> {
    // `queries` exposes messages by thread, which is the shape the reading pane
    // wants; a draft holds one row id, so this narrows that read rather than
    // widening a file another unit owns.
    let thread_id: Option<i64> = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT thread_id FROM messages WHERE id = ?1 AND account_id = ?2",
                rusqlite::params![message_id, account_id],
                |row| row.get(0),
            )
            .ok())
    })?;
    let thread_id = thread_id.ok_or(ComposeError::UnknownMessage(message_id))?;
    db.read(|conn| queries::messages_for_thread(conn, thread_id))?
        .into_iter()
        .find(|m| m.id == message_id)
        .ok_or(ComposeError::UnknownMessage(message_id))
}

// ---------------------------------------------------------------------------
// quoting
// ---------------------------------------------------------------------------

/// `On Thu, 7 Aug 2026 at 12:00, José García <jose@example.com> wrote:`
///
/// The shape matters more than the wording: every client that collapses quoted
/// history looks for an attribution line immediately above a quote, and one
/// written any other way leaves the whole original expanded in the reply.
pub fn attribution(message: &Message) -> String {
    use chrono::{Local, TimeZone};
    let when = Local
        .timestamp_millis_opt(message.internal_date)
        .single()
        .map(|t| t.format("%a, %-d %b %Y at %H:%M").to_string())
        .unwrap_or_default();
    let who = match message.from.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => format!("{name} <{}>", message.from.email),
        _ => format!("<{}>", message.from.email),
    };
    if when.is_empty() {
        format!("{who} wrote:")
    } else {
        format!("On {when}, {who} wrote:")
    }
}

fn original_text(message: &Message) -> String {
    message
        .body_text
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| message.snippet.clone())
}

/// `> `-prefixed original, the form every plain-text reader understands.
pub fn quote_text(message: &Message) -> String {
    let mut out = attribution(message);
    out.push('\n');
    for line in original_text(message).replace("\r\n", "\n").split('\n') {
        out.push('>');
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// The `gmail_quote` shape.
///
/// Not an aesthetic choice: Gmail, Apple Mail and Outlook all key their
/// "show trimmed content" affordance off a `blockquote` immediately after an
/// attribution line, and Gmail specifically off `class="gmail_quote"`. Emitting
/// anything else means the recipient sees the entire thread re-pasted under
/// every reply.
pub fn quote_html(message: &Message) -> String {
    format!(
        "<div class=\"gmail_quote\">\
         <div dir=\"ltr\" class=\"gmail_attr\">{}<br></div>\
         <blockquote class=\"gmail_quote\" \
         style=\"margin:0 0 0 .8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex\">\
         {}</blockquote></div>",
        markdown::escape(&attribution(message)),
        original_html(message)
    )
}

/// The original's own HTML when it has one, cleaned; otherwise its text.
///
/// Cleaning matters even though this is outgoing: the source is a message
/// written by a stranger, and re-emitting its `<script>` into a reply would
/// hand it to everyone else on the thread.
fn original_html(message: &Message) -> String {
    match message
        .body_html
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        Some(html) => ammonia::clean(html),
        None => {
            let mut out = String::new();
            markdown::escape_into(&mut out, &original_text(message));
            out.replace('\n', "<br>")
        }
    }
}

fn header_line(label: &str, people: &[Participant]) -> String {
    if people.is_empty() {
        return String::new();
    }
    let rendered = people
        .iter()
        .map(|p| match p.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => format!("{name} <{}>", p.email),
            _ => format!("<{}>", p.email),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{label}: {rendered}\n")
}

/// A forward is not a quote. The original is reproduced whole, under the
/// separator every client recognises, with its own headers — because the point
/// of forwarding is that the recipient can see who said it and when.
pub fn forward_text(message: &Message) -> String {
    let mut out = String::from("---------- Forwarded message ---------\n");
    out.push_str(&header_line(
        "From",
        std::slice::from_ref(&message.from),
    ));
    out.push_str(&format!("Date: {}\n", attribution_date(message)));
    out.push_str(&format!("Subject: {}\n", message.subject));
    out.push_str(&header_line("To", &message.to));
    out.push_str(&header_line("Cc", &message.cc));
    out.push('\n');
    out.push_str(&original_text(message));
    out
}

pub fn forward_html(message: &Message) -> String {
    let mut headers = String::new();
    markdown::escape_into(&mut headers, &forward_header_block(message));
    format!(
        "<div class=\"gmail_quote\">\
         <div dir=\"ltr\" class=\"gmail_attr\">{}</div><br>{}</div>",
        headers.replace('\n', "<br>"),
        original_html(message)
    )
}

fn forward_header_block(message: &Message) -> String {
    let mut out = String::from("---------- Forwarded message ---------\n");
    out.push_str(&header_line("From", std::slice::from_ref(&message.from)));
    out.push_str(&format!("Date: {}\n", attribution_date(message)));
    out.push_str(&format!("Subject: {}\n", message.subject));
    out.push_str(&header_line("To", &message.to));
    out.push_str(&header_line("Cc", &message.cc));
    out
}

fn attribution_date(message: &Message) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_millis_opt(message.internal_date)
        .single()
        .map(|t| t.format("%a, %-d %b %Y at %H:%M").to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// persistence
// ---------------------------------------------------------------------------

pub fn save_draft(db: &Db, draft: &Draft, now_ms: i64) -> Result<Draft> {
    db.write(ensure_compose_schema)?;
    let saved = Draft {
        updated_at: now_ms,
        ..draft.clone()
    };
    db.write(|conn| {
        conn.execute(
            "INSERT INTO compose_drafts
                 (id, account_id, thread_id, reply_to_id, kind, to_json, cc_json, bcc_json,
                  subject, body, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                 account_id  = excluded.account_id,
                 thread_id   = excluded.thread_id,
                 reply_to_id = excluded.reply_to_id,
                 kind        = excluded.kind,
                 to_json     = excluded.to_json,
                 cc_json     = excluded.cc_json,
                 bcc_json    = excluded.bcc_json,
                 subject     = excluded.subject,
                 body        = excluded.body,
                 updated_at  = excluded.updated_at",
            rusqlite::params![
                saved.id,
                saved.account_id,
                saved.thread_id,
                saved.reply_to_id,
                saved.kind.as_str(),
                json(&saved.to),
                json(&saved.cc),
                json(&saved.bcc),
                saved.subject,
                saved.body,
                saved.updated_at,
            ],
        )?;
        Ok(())
    })?;
    Ok(saved)
}

pub fn load_draft(db: &Db, id: &str) -> Result<Option<Draft>> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        Ok(conn
            .query_row(
                &format!("SELECT {DRAFT_COLUMNS} FROM compose_drafts WHERE id = ?1"),
                [id],
                map_draft,
            )
            .ok())
    })?)
}

/// The most recently touched draft for a conversation — what reopening a thread
/// should put back in the composer.
pub fn load_draft_for_thread(db: &Db, thread_id: i64) -> Result<Option<Draft>> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {DRAFT_COLUMNS} FROM compose_drafts WHERE thread_id = ?1 \
                     ORDER BY updated_at DESC LIMIT 1"
                ),
                [thread_id],
                map_draft,
            )
            .ok())
    })?)
}

pub fn delete_draft(db: &Db, id: &str) -> Result<()> {
    db.write(ensure_compose_schema)?;
    db.write(|conn| {
        conn.execute("DELETE FROM compose_drafts WHERE id = ?1", [id])?;
        Ok(())
    })?;
    Ok(())
}

const DRAFT_COLUMNS: &str = "id, account_id, thread_id, reply_to_id, kind, to_json, cc_json, \
                             bcc_json, subject, body, updated_at";

fn map_draft(row: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    let kind: String = row.get(4)?;
    let to: String = row.get(5)?;
    let cc: String = row.get(6)?;
    let bcc: String = row.get(7)?;
    Ok(Draft {
        id: row.get(0)?,
        account_id: row.get(1)?,
        thread_id: row.get(2)?,
        reply_to_id: row.get(3)?,
        kind: DraftKind::parse(&kind),
        to: serde_json::from_str(&to).unwrap_or_default(),
        cc: serde_json::from_str(&cc).unwrap_or_default(),
        bcc: serde_json::from_str(&bcc).unwrap_or_default(),
        subject: row.get(8)?,
        body: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn json(list: &[Mailbox]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_string())
}
