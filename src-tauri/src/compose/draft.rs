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
    /// Written in some other client and finished here.
    ///
    /// Its own kind rather than `Reply` because of what a rebuild would
    /// otherwise do to the text. The body of a draft written on the phone is
    /// already the *whole* message — the typed part and whatever the other
    /// client quoted underneath it — so treating it as a reply and quoting the
    /// parent again would append a second copy of the original on every save.
    /// An adopted draft is reproduced exactly as Gmail holds it, and Mach adds
    /// nothing to the body it did not write.
    ///
    /// It still threads: [`build`] takes the `In-Reply-To` and `References`
    /// chain from the parent, and the conversation from the draft's own Gmail
    /// thread, so finishing a phone draft here and sending it lands in the same
    /// conversation everywhere.
    Adopted,
}

impl DraftKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DraftKind::New => "new",
            DraftKind::Reply => "reply",
            DraftKind::ReplyAll => "replyAll",
            DraftKind::Forward => "forward",
            DraftKind::Adopted => "adopted",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "reply" => DraftKind::Reply,
            "replyAll" => DraftKind::ReplyAll,
            "forward" => DraftKind::Forward,
            "adopted" => DraftKind::Adopted,
            _ => DraftKind::New,
        }
    }

    /// Whether this draft belongs to a conversation that already exists.
    ///
    /// A forward deliberately does not: threading it onto the original is what
    /// makes a forwarded message reappear inside the thread it was taken out of.
    pub fn continues_a_thread(self) -> bool {
        matches!(
            self,
            DraftKind::Reply | DraftKind::ReplyAll | DraftKind::Adopted
        )
    }
}

/// Where a draft stands with Gmail.
///
/// `Pending` is not a failure and is not worth saying out loud — it is the
/// half-second between typing and the push landing. `Failed` is the one the
/// composer shows, because a draft that exists only inside this Mac while the
/// owner reads mail on his phone is exactly the thing that must never be
/// silent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteState {
    /// Written locally; Gmail has not been told, or has not been told this
    /// version yet.
    #[default]
    Pending,
    /// Gmail holds this text. It is on the phone.
    Synced,
    /// Google refused or could not be reached. The draft is local only, and
    /// the UI says so.
    Failed,
}

impl RemoteState {
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteState::Pending => "pending",
            RemoteState::Synced => "synced",
            RemoteState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "synced" => RemoteState::Synced,
            "failed" => RemoteState::Failed,
            _ => RemoteState::Pending,
        }
    }
}

/// The Gmail half of a draft: its identity over there, and whether it got
/// there.
///
/// Deliberately a block of its own rather than six fields on [`Draft`], because
/// none of it is the editor's to set. [`save_draft`] writes the columns the
/// editor owns and leaves these alone, so a draft that round-trips through the
/// UI — which rebuilds the object on every keystroke — cannot lose the id that
/// says which Gmail draft it *is*. Losing that id is how one draft becomes two.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftRemote {
    pub state: RemoteState,
    /// What `drafts.update` and `drafts.delete` address.
    #[serde(default)]
    pub draft_id: Option<String>,
    /// The ordinary Gmail message id of the draft, once it has one.
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Why the last push failed, verbatim. `None` unless `state` is `Failed`.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub synced_at: i64,
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
    /// Read-only from the editor's side; see [`DraftRemote`].
    #[serde(default)]
    pub remote: DraftRemote,
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

    // The last message somebody actually sent. Mach now writes a draft into the
    // conversation it answers — that is what makes it visible in Drafts and on
    // the phone — so `.last()` would happily thread a reply onto the user's own
    // unsent text.
    let parent = detail
        .messages
        .iter()
        .rev()
        .find(|m| !m.is_draft)
        .or_else(|| detail.messages.last())
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
        remote: DraftRemote::default(),
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
        //
        // An adopted draft gets the chain for the same reason a reply does: it
        // *is* a reply, written elsewhere, and the headers are what make it
        // thread in clients that have never heard of Gmail's thread ids.
        (Some(p), DraftKind::Reply | DraftKind::ReplyAll | DraftKind::Adopted) => {
            references_for_reply(p)
        }
        _ => (None, Vec::new()),
    };

    let body_text = markdown::to_text(&draft.body);
    let body_html = markdown::to_html(&draft.body);

    // `Adopted` is absent from both arms on purpose: its body already contains
    // whatever the client that wrote it quoted, and quoting the parent a second
    // time here is how one save would turn his phone's text into two copies of
    // the original.
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

    let thread_id = if draft.kind.continues_a_thread() {
        draft.thread_id
    } else {
        None
    };
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

/// Write what the editor owns, and nothing else.
///
/// The `ON CONFLICT` list is deliberately the editor's columns only. The Gmail
/// identity is written by [`super::remote`] and read back below: the editor
/// hands back an object it rebuilt from its own state on every keystroke, and
/// letting that object write `gmail_draft_id` would mean one dropped field
/// turned an update of a Gmail draft into a second one.
///
/// Saving always leaves the row `pending`, because it has just become newer
/// than whatever Gmail holds.
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
                  subject, body, updated_at, remote_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending')
             ON CONFLICT(id) DO UPDATE SET
                 account_id   = excluded.account_id,
                 thread_id    = excluded.thread_id,
                 reply_to_id  = excluded.reply_to_id,
                 kind         = excluded.kind,
                 to_json      = excluded.to_json,
                 cc_json      = excluded.cc_json,
                 bcc_json     = excluded.bcc_json,
                 subject      = excluded.subject,
                 body         = excluded.body,
                 updated_at   = excluded.updated_at,
                 remote_state = 'pending'",
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
    // Re-read rather than return what was passed in: the stored row is the one
    // that knows which Gmail draft this is.
    Ok(load_draft(db, &saved.id)?.unwrap_or(saved))
}

/// Record what Gmail said about a draft. Never touches the text.
pub fn set_remote(db: &Db, draft_id: &str, remote: &DraftRemote) -> Result<()> {
    db.write(ensure_compose_schema)?;
    db.write(|conn| {
        conn.execute(
            "UPDATE compose_drafts
                SET gmail_draft_id   = ?2,
                    gmail_message_id = ?3,
                    gmail_thread_id  = ?4,
                    remote_state     = ?5,
                    remote_error     = ?6,
                    remote_synced_at = ?7
              WHERE id = ?1",
            rusqlite::params![
                draft_id,
                remote.draft_id,
                remote.message_id,
                remote.thread_id,
                remote.state.as_str(),
                remote.error,
                remote.synced_at,
            ],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// Every draft Gmail has not been told about yet — what a push pass walks.
pub fn drafts_needing_push(db: &Db) -> Result<Vec<Draft>> {
    db.write(ensure_compose_schema)?;
    Ok(db.read(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {DRAFT_COLUMNS} FROM compose_drafts \
             WHERE remote_state <> 'synced' ORDER BY updated_at"
        ))?;
        let rows = stmt.query_map([], map_draft)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?)
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

/// The draft that a message row in a conversation *is*.
///
/// The reading pane renders a draft as a message — that is what
/// [`super::mirror`] puts there — so activating that row has to reach the
/// editable copy in `compose_drafts`. Two ids answer, because the mirror is
/// renamed the instant Gmail accepts the push (see [`super::mirror::adopt`]):
/// before that it is filed under `mach-draft:<draft id>`, and after it under
/// Gmail's own message id, which `compose_drafts.gmail_message_id` also holds.
/// Reading only the placeholder would work for about half a second and then
/// quietly stop finding anything.
///
/// A draft with no editable copy here is **adopted** rather than refused. See
/// [`adopt_remote_draft`].
///
/// `None` now means one thing only: this row is a draft whose Gmail draft id
/// Mach has not learned yet, because `users.drafts.list` has not run since the
/// draft appeared. It resolves itself within a sync pass.
pub fn load_draft_for_message(db: &Db, message_id: i64, now_ms: i64) -> Result<Option<Draft>> {
    db.write(ensure_compose_schema)?;
    let gmail_message_id: Option<String> = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT gmail_message_id FROM messages WHERE id = ?1 AND is_draft = 1",
                [message_id],
                |row| row.get(0),
            )
            .ok())
    })?;
    let Some(gmail_message_id) = gmail_message_id else {
        return Ok(None);
    };
    if let Some(draft_id) = gmail_message_id.strip_prefix(super::mirror::MIRROR_PREFIX) {
        return load_draft(db, draft_id);
    }
    let existing: Option<Draft> = db.read(|conn| {
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {DRAFT_COLUMNS} FROM compose_drafts WHERE gmail_message_id = ?1 \
                     ORDER BY updated_at DESC LIMIT 1"
                ),
                [&gmail_message_id],
                map_draft,
            )
            .ok())
    })?;
    match existing {
        // Mach's own row wins whenever there is one. It holds the text as the
        // editor last left it, which may be newer than the copy that came down
        // from Gmail, and re-adopting over it would throw away the difference.
        Some(draft) => Ok(Some(draft)),
        None => adopt_remote_draft(db, message_id, now_ms),
    }
}

/// Take over a draft written in another client.
///
/// # What this is for
///
/// A draft written on the phone arrives through ordinary message sync as an
/// ordinary message carrying the `DRAFT` label, and Mach used to stop there:
/// the row said "Draft", and activating it said the draft was not editable
/// here. The missing piece was never the text — that had synced like any other
/// message — it was the **draft id**, which `users.messages.get` does not carry
/// and only `users.drafts.list` reports. `sync::mail` now learns it and writes
/// it onto the message; this is where it is spent.
///
/// # What is copied, and what is not
///
/// Recipients, subject and the body come from the message row Mach already has,
/// so no network is in the path and the composer opens on a local read like
/// everything else. The kind is [`DraftKind::Adopted`], which is what keeps the
/// body verbatim — see that variant for why re-quoting would be a corruption
/// rather than a nicety.
///
/// # One draft, still
///
/// The row is written in a single statement holding both the text *and* the
/// Gmail identity, `synced` from the first instant. Two consequences, and both
/// are the point. Adoption never pushes: the local copy is a faithful copy, so
/// there is nothing to tell Google, and merely *opening* his phone's draft does
/// not rewrite it. And there is no window in which a row exists with text but
/// no `gmail_draft_id` — a row in that state is one `push` away from
/// `drafts.create`, which is exactly how a draft becomes two.
///
/// The local id is derived from the Gmail draft id rather than minted, so
/// adopting the same draft twice — two activations of the same row, or two
/// windows — converges on one row instead of racing to make two.
fn adopt_remote_draft(db: &Db, message_id: i64, now_ms: i64) -> Result<Option<Draft>> {
    let Some(gmail_draft_id) = db.read(|conn| queries::message_draft_id(conn, message_id))? else {
        return Ok(None);
    };

    let located: Option<(i64, i64)> = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT account_id, thread_id FROM messages WHERE id = ?1 AND is_draft = 1",
                [message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok())
    })?;
    let Some((account_id, thread_id)) = located else {
        return Ok(None);
    };

    let detail = db
        .read(|conn| queries::thread_with_messages(conn, thread_id))?
        .ok_or(ComposeError::UnknownThread(thread_id))?;
    let message = detail
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .ok_or(ComposeError::UnknownMessage(message_id))?;

    // The message this draft answers, if it answers one. A draft written on the
    // phone as a fresh message has no parent, and must still open — so this is
    // an `Option`, not a failure.
    let parent = detail.messages.iter().rev().find(|m| !m.is_draft);

    let body = message
        .body_text
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| message.snippet.clone());

    let draft = Draft {
        id: adopted_draft_id(&gmail_draft_id),
        account_id,
        thread_id: Some(thread_id),
        reply_to_id: parent.map(|p| p.id),
        kind: DraftKind::Adopted,
        to: mailboxes(&message.to),
        cc: mailboxes(&message.cc),
        bcc: mailboxes(&message.bcc),
        subject: message.subject.clone(),
        body,
        updated_at: now_ms,
        remote: DraftRemote {
            state: RemoteState::Synced,
            draft_id: Some(gmail_draft_id),
            message_id: Some(message.gmail_message_id.clone()),
            thread_id: Some(detail.thread.gmail_thread_id.clone()),
            error: None,
            synced_at: now_ms,
        },
    };

    insert_adopted(db, &draft)?;
    load_draft(db, &draft.id)
}

/// The local row id an adopted draft gets. Derived, so adoption is idempotent.
fn adopted_draft_id(gmail_draft_id: &str) -> String {
    format!("gmail-draft-{gmail_draft_id}")
}

/// Write an adopted draft: the editor's columns and the Gmail identity in one
/// statement, because a row that has one without the other is a duplicate
/// waiting for the next push. `DO NOTHING` on conflict, so a second adoption of
/// the same draft finds the first one rather than overwriting it.
fn insert_adopted(db: &Db, draft: &Draft) -> Result<()> {
    db.write(ensure_compose_schema)?;
    db.write(|conn| {
        conn.execute(
            "INSERT INTO compose_drafts
                 (id, account_id, thread_id, reply_to_id, kind, to_json, cc_json, bcc_json,
                  subject, body, updated_at, gmail_draft_id, gmail_message_id, gmail_thread_id,
                  remote_state, remote_error, remote_synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'synced', NULL, ?15)
             ON CONFLICT(id) DO NOTHING",
            rusqlite::params![
                draft.id,
                draft.account_id,
                draft.thread_id,
                draft.reply_to_id,
                draft.kind.as_str(),
                json(&draft.to),
                json(&draft.cc),
                json(&draft.bcc),
                draft.subject,
                draft.body,
                draft.updated_at,
                draft.remote.draft_id,
                draft.remote.message_id,
                draft.remote.thread_id,
                draft.remote.synced_at,
            ],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// Drop the composer's copy of every draft this account holds that Gmail no
/// longer lists, and take its row out of the conversation.
///
/// Called by the drafts sweep in `sync::mail` with the complete set of draft ids
/// Google returned, so anything missing from it was sent or deleted somewhere
/// else. Without this, a draft thrown away on the phone would sit here for ever
/// and — worse — the next edit would push it back, which is Mach resurrecting
/// something the owner deliberately deleted.
///
/// Two guards, and both have a specific accident behind them.
///
/// `listed_at` is the instant *before* the request went out: a draft created
/// here while the response was in flight is newer than the answer, and reaping
/// it would delete a draft that had just been written. Rows synced at or after
/// that instant are left alone.
///
/// The mirror is only taken out of the thread when the message row is still a
/// draft. A draft **sent** from the phone also disappears from `drafts.list`,
/// and its message is now an ordinary sent message in the conversation — which
/// must survive.
pub fn forget_drafts_missing_from(
    db: &Db,
    account_id: i64,
    live: &[String],
    listed_at: i64,
) -> Result<Vec<String>> {
    db.write(ensure_compose_schema)?;
    let doomed: Vec<Draft> = db.read(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {DRAFT_COLUMNS} FROM compose_drafts
              WHERE account_id = ?1 AND gmail_draft_id IS NOT NULL AND remote_synced_at < ?2"
        ))?;
        let rows = stmt.query_map(rusqlite::params![account_id, listed_at], map_draft)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })?;

    let mut removed = Vec::new();
    for draft in doomed {
        let Some(remote_id) = draft.remote.draft_id.clone() else {
            continue;
        };
        if live.iter().any(|id| *id == remote_id) {
            continue;
        }
        if still_a_draft_row(db, account_id, draft.remote.message_id.as_deref())? {
            super::mirror::unmirror(db, &draft)?;
        }
        delete_draft(db, &draft.id)?;
        removed.push(remote_id);
    }
    Ok(removed)
}

fn still_a_draft_row(db: &Db, account_id: i64, gmail_message_id: Option<&str>) -> Result<bool> {
    let Some(gmail_message_id) = gmail_message_id.filter(|id| !id.is_empty()) else {
        return Ok(false);
    };
    let id = gmail_message_id.to_string();
    Ok(db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT is_draft FROM messages WHERE account_id = ?1 AND gmail_message_id = ?2",
                rusqlite::params![account_id, id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false))
    })?)
}

fn mailboxes(people: &[Participant]) -> Vec<Mailbox> {
    people.iter().map(Mailbox::from_participant).collect()
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
                             bcc_json, subject, body, updated_at, gmail_draft_id, \
                             gmail_message_id, gmail_thread_id, remote_state, remote_error, \
                             remote_synced_at";

fn map_draft(row: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    let kind: String = row.get(4)?;
    let to: String = row.get(5)?;
    let cc: String = row.get(6)?;
    let bcc: String = row.get(7)?;
    let remote_state: String = row.get(14)?;
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
        remote: DraftRemote {
            draft_id: row.get(11)?,
            message_id: row.get(12)?,
            thread_id: row.get(13)?,
            state: RemoteState::parse(&remote_state),
            error: row.get(15)?,
            synced_at: row.get(16)?,
        },
    })
}

fn json(list: &[Mailbox]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_string())
}
