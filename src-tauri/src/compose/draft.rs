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

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::db::models::{Message, Participant, ThreadSummary};
use crate::db::{command_queries, queries, Db};

use super::address::{forward_recipients, reply_recipients_for};
use super::markdown;
use super::mime::{
    forward_subject, generate_message_id, references_for_reply, reply_subject, Mailbox, Outgoing,
    OutgoingAttachment,
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

/// How to read [`Draft::body`].
///
/// # Why this is stored rather than sniffed
///
/// The composer used to be a `<textarea>` and the body used to be markdown-ish
/// source. It is now a rich-text editor and the body is HTML. Both kinds of row
/// exist in the same table on the same Mac: a reply half-written last week is
/// still markdown, and there is no honest way to tell one from the other by
/// looking — `<b>hi</b>` is a legal thing to have *typed* into the old editor,
/// and a plain sentence with no tags is a legal thing for the new one to emit.
///
/// So the row says which it is. Old rows say `markdown` because that is the
/// column default, which is the whole reason the default is not `html`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BodyFormat {
    /// The markdown-ish grammar in [`super::markdown`]. Read, never written.
    #[default]
    Markdown,
    /// HTML, as the editor emits it. Cleaned by [`super::html::sanitize`] on the
    /// way out rather than on the way in, so what the editor holds is exactly
    /// what it put there.
    Html,
}

impl BodyFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            BodyFormat::Markdown => "markdown",
            BodyFormat::Html => "html",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "html" => BodyFormat::Html,
            _ => BodyFormat::Markdown,
        }
    }
}

/// The two body parts of a message, derived from whichever format the draft is
/// in. `(text, html)`.
///
/// **The direction of the derivation is the change.** Under markdown the source
/// was the `text/plain` part and the HTML was generated from it. Under HTML
/// there is no source but the HTML, so the plain part is read *back out* of it
/// by [`super::html::to_text`] — structure and all, because a plain-text part
/// with the tags merely removed is one run-on paragraph.
pub fn body_parts(draft: &Draft) -> (String, String) {
    match draft.body_format {
        BodyFormat::Markdown => (
            markdown::to_text(&draft.body),
            markdown::to_html(&draft.body),
        ),
        BodyFormat::Html => {
            let html = super::html::sanitize(&draft.body);
            (super::html::to_text(&html), html)
        }
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
    /// The body, exactly as the editor left it. [`body_format`](Self::body_format)
    /// says what it is.
    #[serde(default)]
    pub body: String,
    /// HTML or markdown-ish source. Owned by the editor, unlike [`remote`](Self::remote).
    #[serde(default)]
    pub body_format: BodyFormat,
    #[serde(default)]
    pub updated_at: i64,
    /// Read-only from the editor's side; see [`DraftRemote`].
    #[serde(default)]
    pub remote: DraftRemote,
    /// Read-only from the editor's side, like [`remote`](Self::remote): files
    /// are added and removed through their own operations, and a draft object
    /// rebuilt on every keystroke must not be able to drop one by omission.
    #[serde(default)]
    pub attachments: Vec<super::attach::Attachment>,
}

impl Draft {
    /// Nothing worth keeping. Used to decide whether a draft is worth a row, a
    /// push to Gmail, or a confirmation before it is thrown away.
    ///
    /// An attachment counts as content even with no text at all: "here, look at
    /// this" with the file and no sentence is a message somebody meant to write.
    pub fn is_empty(&self) -> bool {
        self.body_text().trim().is_empty()
            && self.subject.trim().is_empty()
            && self.to.is_empty()
            && self.cc.is_empty()
            && self.bcc.is_empty()
            && self.attachments.is_empty()
    }

    /// The body as prose, whichever format it is in — so "is there anything in
    /// this?" is not answered `true` by `<div><br></div>`, which is what an
    /// untouched rich-text editor contains.
    pub fn body_text(&self) -> String {
        match self.body_format {
            BodyFormat::Markdown => self.body.clone(),
            BodyFormat::Html => super::html::to_text(&self.body),
        }
    }
}

/// The thread and message a reply is being written against.
#[derive(Debug, Clone)]
pub struct ReplyContext {
    pub account_id: i64,
    pub account_email: String,
    pub account_name: Option<String>,
    /// Every address the app holds an account for, this one included.
    ///
    /// Recipients are filtered against all of them rather than against
    /// `account_email` alone: a reply-all that Ccs you at your other address is
    /// still mailing you your own message. See [`super::address`].
    pub my_addresses: Vec<String>,
    pub thread_id: i64,
    pub gmail_thread_id: String,
    /// The conversation's subject, which a message in it can lack: `Subject` is
    /// an optional header and the sync pass stores what Gmail gives it.
    pub thread_subject: String,
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

    context_around(db, detail.thread, parent)
}

/// The same context, aimed at **one message** rather than at the newest one.
///
/// A long conversation is not a single question: answering the fourth message
/// of eleven has to produce a reply whose `In-Reply-To`, `References`,
/// recipients and quoted block all come from *that* message. Every one of those
/// is derived from [`ReplyContext::parent`], so pointing the parent somewhere
/// else is the whole of it — nothing downstream needs to know which route the
/// context arrived by.
///
/// The thread is read from the message rather than passed in beside it, so a
/// message id and a thread id can never disagree about which conversation this
/// belongs to.
pub fn context_for_message(db: &Db, message_id: i64) -> Result<ReplyContext> {
    let thread_id: Option<i64> = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT thread_id FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .ok())
    })?;
    let thread_id = thread_id.ok_or(ComposeError::UnknownMessage(message_id))?;
    let detail = db
        .read(|conn| queries::thread_with_messages(conn, thread_id))?
        .ok_or(ComposeError::UnknownThread(thread_id))?;
    let parent = detail
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .cloned()
        .ok_or(ComposeError::UnknownMessage(message_id))?;
    // A draft is not something to answer. The reading pane renders one as a
    // message row (see [`super::mirror`]), so it is a row the pointer can
    // reach; threading a reply onto the user's own unsent text is the mistake
    // `context_for_thread` skips drafts to avoid, and it must not come back in
    // through this door.
    if parent.is_draft {
        return Err(ComposeError::invalid(
            "that message is an unsent draft, not something to answer",
        ));
    }

    context_around(db, detail.thread, parent)
}

/// Everything a context needs that is not the parent: which account, and which
/// conversation.
fn context_around(db: &Db, thread: ThreadSummary, parent: Message) -> Result<ReplyContext> {
    let account_id = thread.account_id;
    let accounts = db.read(queries::list_accounts)?;
    let account = accounts
        .iter()
        .find(|a| a.id == account_id)
        .cloned()
        .ok_or(ComposeError::UnknownAccount(account_id))?;
    let my_addresses = accounts.into_iter().map(|a| a.email).collect();

    Ok(ReplyContext {
        account_id,
        account_email: account.email,
        account_name: account.display_name,
        my_addresses,
        thread_id: thread.id,
        gmail_thread_id: thread.gmail_thread_id,
        thread_subject: thread.subject,
        parent,
    })
}

impl ReplyContext {
    /// What `Re:` and `Fwd:` are put in front of.
    ///
    /// The message's own subject, falling back to the conversation's. A message
    /// row can carry an empty `Subject` — the header is optional and the sync
    /// pass stores what Gmail gives it — while the thread it belongs to has one,
    /// and taking the message's blindly is how the composer opens showing `Re:`
    /// with nothing after it.
    pub fn original_subject(&self) -> &str {
        if self.parent.subject.trim().is_empty() {
            &self.thread_subject
        } else {
            &self.parent.subject
        }
    }
}

/// A draft, pre-filled from a thread.
///
/// **The from-address is inferred, never chosen.** A reply goes out from the
/// account the thread arrived on, because that is the address the other side
/// wrote to; picking a different one is how a reply ends up in somebody's spam
/// folder and how a work thread accidentally answers from a personal address.
pub fn prepare(db: &Db, thread_id: i64, kind: DraftKind, draft_id: String) -> Result<Draft> {
    Ok(prepared(context_for_thread(db, thread_id)?, kind, draft_id))
}

/// A draft, pre-filled from **one message** rather than from the newest one in
/// its conversation.
///
/// What `r` on the fourth message of eleven reaches. Everything that makes a
/// reply land in the right place comes from the parent — the `In-Reply-To` and
/// `References` chain in [`build`], the recipients here, the quoted block — so
/// this and [`prepare`] differ in exactly one thing: which message that is.
pub fn prepare_reply_to(
    db: &Db,
    message_id: i64,
    kind: DraftKind,
    draft_id: String,
) -> Result<Draft> {
    Ok(prepared(context_for_message(db, message_id)?, kind, draft_id))
}

fn prepared(ctx: ReplyContext, kind: DraftKind, draft_id: String) -> Draft {
    let (to, cc, subject) = match kind {
        DraftKind::Forward => {
            let r = forward_recipients();
            (r.to, r.cc, forward_subject(ctx.original_subject()))
        }
        DraftKind::New => (Vec::new(), Vec::new(), String::new()),
        other => {
            let r = reply_recipients_for(
                &ctx.parent,
                &ctx.account_email,
                &ctx.my_addresses,
                other == DraftKind::ReplyAll,
            );
            (r.to, r.cc, reply_subject(ctx.original_subject()))
        }
    };

    Draft {
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
        // A prepared draft has no body at all, so either format would render
        // the same nothing. `Html` is the honest answer: the editor that is
        // about to open is a rich-text one, and the first keystroke is HTML.
        body_format: BodyFormat::Html,
        updated_at: 0,
        remote: DraftRemote::default(),
        attachments: Vec::new(),
    }
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
    /// The `compose_drafts` row this was built from.
    pub draft_id: String,
    /// The Gmail draft behind it, when there is one. Carried this far because
    /// the send is decided by whether it exists: a message that is already a
    /// draft on Google is *sent as that draft*, and one that is not is sent as
    /// a new message. See [`outbox::Outbox::queue`](super::outbox::Outbox::queue).
    pub gmail_draft_id: Option<String>,
    /// The message id Gmail filed that draft under.
    pub gmail_draft_message_id: Option<String>,
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

    let (body_text, body_html) = body_parts(draft);

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

    // The bytes, not the metadata: this is the one moment they are needed, and
    // the one place they are read. A draft object handed round the UI carries
    // filenames and sizes only.
    let attachments = super::attach::list_with_bytes(db, &draft.id)?
        .into_iter()
        .map(|(meta, bytes)| OutgoingAttachment {
            filename: meta.filename,
            mime_type: meta.mime_type,
            inline: meta.inline,
            content_id: meta.content_id,
            bytes,
        })
        .collect();

    Ok(Built {
        outgoing: Outgoing {
            from: from.clone(),
            to: draft.to.clone(),
            cc: draft.cc.clone(),
            bcc: draft.bcc.clone(),
            subject: draft.subject.clone(),
            text,
            html,
            attachments,
            in_reply_to,
            references,
            message_id: generate_message_id(&from.email, now_ms, entropy),
            date_ms: now_ms,
        },
        account_id: draft.account_id,
        gmail_thread_id,
        thread_id,
        draft_id: draft.id.clone(),
        gmail_draft_id: draft.remote.draft_id.clone().filter(|id| !id.is_empty()),
        gmail_draft_message_id: draft.remote.message_id.clone().filter(|id| !id.is_empty()),
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
    // A draft that has been sent or thrown away stays that way. The composer's
    // last autosave can land after either, and writing this row back would put
    // a draft of an already-sent reply into the conversation and — through the
    // push behind it — onto Gmail. See [`retire`].
    if is_retired(db, &draft.id)? {
        return Ok(draft.clone());
    }
    let saved = Draft {
        updated_at: now_ms,
        ..draft.clone()
    };
    db.write(|conn| {
        conn.execute(
            "INSERT INTO compose_drafts
                 (id, account_id, thread_id, reply_to_id, kind, to_json, cc_json, bcc_json,
                  subject, body, body_format, updated_at, remote_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'pending')
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
                 body_format  = excluded.body_format,
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
                saved.body_format.as_str(),
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
    let found = db.read(|conn| {
        Ok(conn
            .query_row(
                &format!("SELECT {DRAFT_COLUMNS} FROM compose_drafts WHERE id = ?1"),
                [id],
                map_draft,
            )
            .ok())
    })?;
    with_attachments(db, found)
}

/// The most recently touched draft for a conversation — what reopening a thread
/// should put back in the composer.
pub fn load_draft_for_thread(db: &Db, thread_id: i64) -> Result<Option<Draft>> {
    db.write(ensure_compose_schema)?;
    let found = db.read(|conn| {
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
    })?;
    with_attachments(db, found)
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
    if let Some(draft) = existing {
        // Mach's own row, still pointing at this message. It holds the text as
        // the editor last left it.
        return Ok(Some(draft));
    }

    let Some(gmail_draft_id) = db.read(|conn| queries::message_draft_id(conn, message_id))? else {
        return Ok(None);
    };

    // A row for this *draft*, filed under a message id that no longer exists.
    // `drafts.update` mints a new message id every time, whoever calls it — so
    // this is what a draft edited on the phone after Mach adopted it looks like
    // from here, and following the draft id is the only way to recognise it as
    // the same draft rather than a new one.
    let by_draft: Option<Draft> = db.read(|conn| {
        Ok(conn
            .query_row(
                &format!("SELECT {DRAFT_COLUMNS} FROM compose_drafts WHERE gmail_draft_id = ?1"),
                [&gmail_draft_id],
                map_draft,
            )
            .ok())
    })?;

    match by_draft {
        Some(existing) => reconcile_adopted(db, existing, message_id, now_ms).map(Some),
        None => adopt_remote_draft(db, message_id, gmail_draft_id, now_ms),
    }
}

/// Two writers, one draft: Mach adopted it, and then it was edited somewhere
/// else.
///
/// **Last write wins, decided by time rather than by which end asked.** Gmail
/// stamps a draft's message with the moment it was saved, and this row carries
/// the moment the editor last touched it, so the two are directly comparable
/// and the newer one is kept. The alternative rules are both worse: "the local
/// copy wins" silently discards what he typed on his phone, and "the remote
/// copy wins" silently discards what he typed here.
///
/// It is still a loss when the two were edited in parallel — the older side's
/// words go — and there is no merge that would not invent a message neither
/// person wrote. What it will not do is lose the *newer* text, which is the one
/// somebody is expecting to find.
///
/// Either way the row is re-pointed at the message that exists now. Leaving it
/// on the old id would have the next save mirror a draft onto a message row
/// Gmail has already deleted, and the conversation would show two.
fn reconcile_adopted(
    db: &Db,
    existing: Draft,
    message_id: i64,
    now_ms: i64,
) -> Result<Draft> {
    let Some(fresh) = read_draft_message(db, message_id)? else {
        return Ok(existing);
    };

    let remote_is_newer = fresh.message.internal_date > existing.updated_at;
    let mut updated = Draft {
        remote: DraftRemote {
            message_id: Some(fresh.message.gmail_message_id.clone()),
            thread_id: Some(fresh.gmail_thread_id.clone()),
            ..existing.remote.clone()
        },
        thread_id: Some(fresh.thread_id),
        ..existing
    };
    if remote_is_newer {
        updated.to = mailboxes(&fresh.message.to);
        updated.cc = mailboxes(&fresh.message.cc);
        updated.bcc = mailboxes(&fresh.message.bcc);
        updated.subject = fresh.message.subject.clone();
        let (body, body_format) = body_of(&fresh.message);
        updated.body = body;
        updated.body_format = body_format;
        updated.updated_at = fresh.message.internal_date;
        updated.reply_to_id = fresh.parent_id;
        // Nothing to push: this text *is* what Gmail holds.
        updated.remote.state = RemoteState::Synced;
        updated.remote.error = None;
        updated.remote.synced_at = now_ms;
        write_adopted(db, &updated, true)?;
    } else {
        // The text stays as he left it here; only the identity moves, so the
        // next save mirrors onto the message that exists rather than the one
        // Gmail replaced.
        set_remote(db, &updated.id, &updated.remote)?;
    }
    Ok(load_draft(db, &updated.id)?.unwrap_or(updated))
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
fn adopt_remote_draft(
    db: &Db,
    message_id: i64,
    gmail_draft_id: String,
    now_ms: i64,
) -> Result<Option<Draft>> {
    let Some(fresh) = read_draft_message(db, message_id)? else {
        return Ok(None);
    };

    let (body, body_format) = body_of(&fresh.message);
    let draft = Draft {
        id: adopted_draft_id(&gmail_draft_id),
        account_id: fresh.message.account_id,
        thread_id: Some(fresh.thread_id),
        reply_to_id: fresh.parent_id,
        kind: DraftKind::Adopted,
        to: mailboxes(&fresh.message.to),
        cc: mailboxes(&fresh.message.cc),
        bcc: mailboxes(&fresh.message.bcc),
        subject: fresh.message.subject.clone(),
        body,
        body_format,
        updated_at: now_ms,
        remote: DraftRemote {
            state: RemoteState::Synced,
            draft_id: Some(gmail_draft_id),
            message_id: Some(fresh.message.gmail_message_id.clone()),
            thread_id: Some(fresh.gmail_thread_id.clone()),
            error: None,
            synced_at: now_ms,
        },
        attachments: Vec::new(),
    };

    write_adopted(db, &draft, false)?;
    load_draft(db, &draft.id)
}

/// A draft message as the store holds it, with the two things around it an
/// adopted draft needs: the conversation it is in, and the message it answers.
struct DraftMessage {
    message: Message,
    thread_id: i64,
    gmail_thread_id: String,
    /// The last message in the thread somebody actually sent. `None` for a fresh
    /// message written on the phone, which has nothing to answer and must still
    /// open.
    parent_id: Option<i64>,
}

fn read_draft_message(db: &Db, message_id: i64) -> Result<Option<DraftMessage>> {
    let thread_id: Option<i64> = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT thread_id FROM messages WHERE id = ?1 AND is_draft = 1",
                [message_id],
                |row| row.get(0),
            )
            .ok())
    })?;
    let Some(thread_id) = thread_id else {
        return Ok(None);
    };
    let detail = db
        .read(|conn| queries::thread_with_messages(conn, thread_id))?
        .ok_or(ComposeError::UnknownThread(thread_id))?;
    let message = detail
        .messages
        .iter()
        .find(|m| m.id == message_id)
        .cloned()
        .ok_or(ComposeError::UnknownMessage(message_id))?;
    let parent_id = detail
        .messages
        .iter()
        .rev()
        .find(|m| !m.is_draft)
        .map(|m| m.id);
    Ok(Some(DraftMessage {
        message,
        thread_id,
        gmail_thread_id: detail.thread.gmail_thread_id,
        parent_id,
    }))
}

/// The body to edit, and what format it is in.
///
/// Gmail's own HTML alternative when the draft has one, cleaned — a draft
/// written on the phone with a bold word in it should open here with the word
/// still bold, and the editor is a rich-text editor now, so it can hold it. The
/// plain-text alternative is wrapped rather than handed over raw, because the
/// editor renders what it is given and a `<` in somebody's draft would otherwise
/// eat the rest of the message.
///
/// The snippet only when there is neither: an empty composer over a draft that
/// plainly has words in it would be worse than not opening at all.
fn body_of(message: &Message) -> (String, BodyFormat) {
    if let Some(html) = message
        .body_html
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        let cleaned = super::html::sanitize(html);
        if !cleaned.trim().is_empty() {
            return (cleaned, BodyFormat::Html);
        }
    }
    let text = message
        .body_text
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| message.snippet.clone());
    (super::html::from_plain_text(&text), BodyFormat::Html)
}

/// The local row id an adopted draft gets. Derived, so adoption is idempotent.
fn adopted_draft_id(gmail_draft_id: &str) -> String {
    format!("gmail-draft-{gmail_draft_id}")
}

/// Write an adopted draft: the editor's columns and the Gmail identity in one
/// statement, because a row that has one without the other is a duplicate
/// waiting for the next push — a row with text and no `gmail_draft_id` is what
/// `drafts.create` is reached from.
///
/// `overwrite` is the difference between adopting and reconciling. Adoption
/// leaves an existing row alone (`DO NOTHING`), so two activations of the same
/// draft row converge instead of racing; reconciliation has already decided that
/// the remote copy is the newer one, and replaces the text with it.
fn write_adopted(db: &Db, draft: &Draft, overwrite: bool) -> Result<()> {
    db.write(ensure_compose_schema)?;
    let conflict = if overwrite {
        "DO UPDATE SET
             account_id       = excluded.account_id,
             thread_id        = excluded.thread_id,
             reply_to_id      = excluded.reply_to_id,
             kind             = excluded.kind,
             to_json          = excluded.to_json,
             cc_json          = excluded.cc_json,
             bcc_json         = excluded.bcc_json,
             subject          = excluded.subject,
             body             = excluded.body,
             body_format      = excluded.body_format,
             updated_at       = excluded.updated_at,
             gmail_draft_id   = excluded.gmail_draft_id,
             gmail_message_id = excluded.gmail_message_id,
             gmail_thread_id  = excluded.gmail_thread_id,
             remote_state     = excluded.remote_state,
             remote_error     = NULL,
             remote_synced_at = excluded.remote_synced_at"
    } else {
        "DO NOTHING"
    };
    let sql = format!(
        "INSERT INTO compose_drafts
             (id, account_id, thread_id, reply_to_id, kind, to_json, cc_json, bcc_json,
              subject, body, updated_at, gmail_draft_id, gmail_message_id, gmail_thread_id,
              remote_state, remote_error, remote_synced_at, body_format)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, NULL, ?16, ?17)
         ON CONFLICT(id) {conflict}"
    );
    db.write(|conn| {
        conn.execute(
            &sql,
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
                draft.remote.state.as_str(),
                draft.remote.synced_at,
                draft.body_format.as_str(),
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
        delete_draft(db, &draft.id, listed_at)?;
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

/// Forget the row, and everything it was carrying.
///
/// The files go with it in the same call rather than being swept later: a
/// `compose_attachments` row whose draft is gone is 25 MB of database nothing
/// can reach, and there is no other owner to inherit it.
///
/// A tombstone goes in as the row comes out — see [`retire`] — so that an
/// autosave still in flight cannot write the draft back a moment later.
pub fn delete_draft(db: &Db, id: &str, now_ms: i64) -> Result<()> {
    db.write(ensure_compose_schema)?;
    retire(db, id, now_ms)?;
    super::attach::delete_for_draft(db, id)?;
    db.write(|conn| {
        conn.execute("DELETE FROM compose_drafts WHERE id = ?1", [id])?;
        Ok(())
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// tombstones
// ---------------------------------------------------------------------------

/// What a retired draft still knows about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetiredDraft {
    pub id: String,
    pub account_id: i64,
    pub thread_id: Option<i64>,
    pub gmail_draft_id: Option<String>,
    pub gmail_message_id: Option<String>,
    pub gmail_thread_id: Option<String>,
    pub retired_at: i64,
}

/// Record that a draft has stopped existing.
///
/// # The race this closes
///
/// The composer autosaves on a debounce, so the last save of a reply can be
/// on the wire at the instant `⌘⏎` is pressed. It arrives after the send has
/// already taken the row out, writes it back — `save_draft` is an upsert, and
/// an upsert has no opinion about whether the row *should* exist — and the push
/// behind it finds no `gmail_draft_id` and calls `drafts.create`. The result is
/// a Gmail draft holding the text of a message that has just been sent, which
/// syncs down and sits in the conversation next to the reply. That is the bug,
/// and this is the only place it can be stopped: nothing in a save's own
/// arguments says the draft is over.
///
/// The identity is kept rather than just the id, so [`revive`] can hand it back
/// to a recalled send.
pub fn retire(db: &Db, id: &str, now_ms: i64) -> Result<()> {
    db.write(ensure_compose_schema)?;
    let Some(draft) = load_draft(db, id)? else {
        // Nothing to describe. A tombstone with no identity would still be
        // worth writing, but every caller reaches here through a row it just
        // read, so an absent row means the draft was already retired.
        return Ok(());
    };
    let now = now_ms;
    // The words as well as the identity. Undo has to put the draft back into the
    // conversation, and the composer is not a place to keep the only copy of it:
    // the window that sent the message can be closed, or the app relaunched,
    // inside the ten seconds.
    let draft_json = serde_json::to_string(&draft).ok();
    db.write(|conn| {
        conn.execute(
            "INSERT INTO compose_retired_drafts
                 (id, account_id, thread_id, gmail_draft_id, gmail_message_id,
                  gmail_thread_id, retired_at, draft_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 gmail_draft_id   = COALESCE(excluded.gmail_draft_id, gmail_draft_id),
                 gmail_message_id = COALESCE(excluded.gmail_message_id, gmail_message_id),
                 gmail_thread_id  = COALESCE(excluded.gmail_thread_id, gmail_thread_id),
                 draft_json       = COALESCE(excluded.draft_json, draft_json),
                 retired_at       = excluded.retired_at",
            rusqlite::params![
                draft.id,
                draft.account_id,
                draft.thread_id,
                draft.remote.draft_id,
                draft.remote.message_id,
                draft.remote.thread_id,
                now,
                draft_json,
            ],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// Has this draft been sent or thrown away?
pub fn is_retired(db: &Db, id: &str) -> Result<bool> {
    Ok(db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT 1 FROM compose_retired_drafts WHERE id = ?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    })?)
}

/// Undo a retirement, and say what the draft used to be.
///
/// Called when a send is recalled inside the undo window. The Gmail identity
/// comes back because the draft on Google was never deleted (`drafts.send` would
/// have consumed it, and the send did not happen) — without it the recalled
/// draft would save as a stranger and `drafts.create` a second copy beside the
/// one already there.
///
/// **The text comes back too**, out of `draft_json`. It used to be the
/// composer's job to save it again, which is fine while the composer that sent
/// the message is still on screen and is nothing at all if it is not: recalling
/// from a reopened window, or after a relaunch, revived an identity with no
/// words in it, and the conversation showed no draft. See [`retire`].
pub fn revive(db: &Db, id: &str) -> Result<Option<RetiredDraft>> {
    db.write(ensure_compose_schema)?;
    let found: Option<(RetiredDraft, Option<String>)> = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, account_id, thread_id, gmail_draft_id, gmail_message_id,
                        gmail_thread_id, retired_at, draft_json
                   FROM compose_retired_drafts WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        RetiredDraft {
                            id: row.get(0)?,
                            account_id: row.get(1)?,
                            thread_id: row.get(2)?,
                            gmail_draft_id: row.get(3)?,
                            gmail_message_id: row.get(4)?,
                            gmail_thread_id: row.get(5)?,
                            retired_at: row.get(6)?,
                        },
                        row.get(7)?,
                    ))
                },
            )
            .optional()?)
    })?;
    db.write(|conn| {
        conn.execute("DELETE FROM compose_retired_drafts WHERE id = ?1", [id])?;
        Ok(())
    })?;

    let Some((retired, draft_json)) = found else {
        return Ok(None);
    };

    // The whole draft, when the tombstone kept one. Written through the ordinary
    // save so that everything a saved draft has — the `pending` state that gets
    // it pushed again, the re-read below — is what a recalled one has.
    if let Some(draft) = draft_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Draft>(json).ok())
    {
        save_draft(db, &draft, retired.retired_at)?;
        set_remote(
            db,
            &draft.id,
            &DraftRemote {
                state: if draft.remote.draft_id.is_some() {
                    RemoteState::Synced
                } else {
                    RemoteState::Pending
                },
                ..draft.remote.clone()
            },
        )?;
        return Ok(Some(retired));
    }

    // Only a draft Gmail actually holds gets its row put back. Anything else
    // has nothing to point at, and an empty row would offer an empty composer
    // on that conversation for ever.
    if retired.gmail_draft_id.is_some() {
        db.write(|conn| {
            conn.execute(
                "INSERT INTO compose_drafts
                     (id, account_id, thread_id, kind, gmail_draft_id, gmail_message_id,
                      gmail_thread_id, remote_state, remote_synced_at, updated_at)
                 VALUES (?1, ?2, ?3, 'reply', ?4, ?5, ?6, 'synced', ?7, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     gmail_draft_id   = excluded.gmail_draft_id,
                     gmail_message_id = excluded.gmail_message_id,
                     gmail_thread_id  = excluded.gmail_thread_id",
                rusqlite::params![
                    retired.id,
                    retired.account_id,
                    retired.thread_id,
                    retired.gmail_draft_id,
                    retired.gmail_message_id,
                    retired.gmail_thread_id,
                    retired.retired_at,
                ],
            )?;
            Ok(())
        })?;
    }
    Ok(Some(retired))
}

/// Drop tombstones older than an instant. Housekeeping, run beside
/// [`Outbox::forget_sent`](super::outbox::Outbox::forget_sent): an autosave
/// racing a send is a matter of milliseconds, and a day is long past the point
/// where a save with that id could be anything but a new draft.
pub fn forget_retired_before(db: &Db, before_ms: i64) -> Result<usize> {
    db.write(ensure_compose_schema)?;
    Ok(db.write(|conn| {
        Ok(conn.execute(
            "DELETE FROM compose_retired_drafts WHERE retired_at < ?1",
            [before_ms],
        )?)
    })?)
}

const DRAFT_COLUMNS: &str = "id, account_id, thread_id, reply_to_id, kind, to_json, cc_json, \
                             bcc_json, subject, body, updated_at, gmail_draft_id, \
                             gmail_message_id, gmail_thread_id, remote_state, remote_error, \
                             remote_synced_at, body_format";

/// Row → draft, **without** the attachment list.
///
/// The list is a second query, so it is added by [`with_attachments`] at the
/// handful of places that hand a draft to the UI or build a message from it.
/// Doing it here instead would put a query per row inside every sweep that
/// walks the table.
fn map_draft(row: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    let kind: String = row.get(4)?;
    let to: String = row.get(5)?;
    let cc: String = row.get(6)?;
    let bcc: String = row.get(7)?;
    let remote_state: String = row.get(14)?;
    let body_format: String = row.get(17)?;
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
        body_format: BodyFormat::parse(&body_format),
        updated_at: row.get(10)?,
        remote: DraftRemote {
            draft_id: row.get(11)?,
            message_id: row.get(12)?,
            thread_id: row.get(13)?,
            state: RemoteState::parse(&remote_state),
            error: row.get(15)?,
            synced_at: row.get(16)?,
        },
        attachments: Vec::new(),
    })
}

/// Fill in what a draft is carrying.
fn with_attachments(db: &Db, draft: Option<Draft>) -> Result<Option<Draft>> {
    let Some(draft) = draft else { return Ok(None) };
    let attachments = super::attach::list(db, &draft.id)?;
    Ok(Some(Draft {
        attachments,
        ..draft
    }))
}

fn json(list: &[Mailbox]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_string())
}
