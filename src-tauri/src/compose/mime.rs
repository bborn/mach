//! RFC822 generation — the bytes Gmail puts on the wire.
//!
//! `mail-builder` does the structure (`multipart/alternative`), the body
//! transfer encoding, and the unstructured headers. Two things are done here
//! instead:
//!
//! **Address headers.** `mail-builder`'s `rfc2047_encode` emits an encoded
//! display name *inside a quoted-string* — `"=?utf-8?B?Sm9zw6k=?=" <a@b>`.
//! RFC 2047 §5 says an encoded-word may not appear inside a quoted-string, and
//! a client that follows the spec renders the literal `=?utf-8?B?…?=` in its
//! From column. [`render_mailboxes`] emits a bare encoded-word instead, splits
//! it into ≤75-octet words at UTF-8 character boundaries, and quotes only
//! ASCII names that actually need quoting.
//!
//! **Threading.** [`references_for_reply`] implements RFC 5322 §3.6.4 rather
//! than the common shortcut of "In-Reply-To only". A client that groups by
//! `References` — Apple Mail, Thunderbird, most mailing list archives — needs
//! the full chain, and a reply deep in a thread that carries only the parent's
//! id starts a new conversation in those clients while looking correct in
//! Gmail. That failure is invisible from here, which is why it is tested on the
//! bytes.
//!
//! **Header injection is refused here, not repaired.** See [`check_header_text`]
//! and the block above it for the whole argument; the short version is that a
//! CR or LF in any value that becomes a header ends that header and starts a new
//! one, `mail-builder` will not stop it, and [`build_rfc822`] is the one place
//! every send, every draft and every outbox row passes through.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::MimePart;
use mail_builder::MessageBuilder;
use serde::{Deserialize, Serialize};

use crate::db::models::{Message, Participant};

use super::{ComposeError, Result};

/// A name/address pair on its way out. `db::models::Participant` is the same
/// shape but is the *stored* form; keeping them separate stops a serialization
/// concern of the local store from becoming a wire-format concern of SMTP.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub email: String,
}

impl Mailbox {
    pub fn new(email: impl Into<String>) -> Self {
        Mailbox {
            name: None,
            email: email.into(),
        }
    }

    pub fn named(name: impl Into<String>, email: impl Into<String>) -> Self {
        let name = name.into();
        Mailbox {
            name: (!name.trim().is_empty()).then_some(name),
            email: email.into(),
        }
    }

    pub fn from_participant(p: &Participant) -> Self {
        Mailbox {
            name: p
                .name
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty()),
            email: p.email.trim().to_string(),
        }
    }

    pub fn to_participant(&self) -> Participant {
        Participant {
            name: self.name.clone(),
            email: self.email.clone(),
        }
    }
}

/// One file riding along with a message.
///
/// The bytes are owned rather than borrowed because they come out of SQLite and
/// go into `mail-builder`, and the two have no lifetime in common.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingAttachment {
    /// Already sanitized by [`attach::add_bytes`](super::attach::add_bytes).
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    /// Drawn in the body, addressed by [`content_id`](Self::content_id).
    pub inline: bool,
    /// Bare — no angle brackets and no `cid:`. Both are added on write.
    pub content_id: String,
}

impl OutgoingAttachment {
    /// The part, with the disposition its role calls for.
    ///
    /// An inline part keeps its filename in the `Content-Disposition` as well as
    /// carrying a `Content-ID`. Nothing in the rendering needs it — the body
    /// addresses the part by id — but "Save image" in the recipient's client
    /// offers `image.png` without it, for every image in the message.
    fn part(&self) -> MimePart<'static> {
        let body = MimePart::new(self.mime_type.clone(), self.bytes.clone());
        if self.inline {
            body.header(
                "Content-Disposition",
                ContentType::new("inline").attribute("filename", self.filename.clone()),
            )
            .cid(self.content_id.clone())
        } else {
            body.attachment(self.filename.clone())
        }
    }
}

/// Everything needed to produce one RFC822 message. No clock, no database, no
/// network — so every field of the output is a pure function of this struct and
/// the tests can assert on the bytes.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub from: Mailbox,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub subject: String,
    /// `text/plain` part.
    pub text: String,
    /// `text/html` part.
    pub html: String,
    /// Files. Empty for most messages, and when it is empty the structure is
    /// exactly what it always was — `multipart/alternative` at the top.
    pub attachments: Vec<OutgoingAttachment>,
    /// Bare id, no angle brackets — they are added on write.
    pub in_reply_to: Option<String>,
    /// Bare ids, oldest first.
    pub references: Vec<String>,
    /// Bare id for this message.
    pub message_id: String,
    pub date_ms: i64,
}

impl Outgoing {
    pub fn recipient_count(&self) -> usize {
        self.to.len() + self.cc.len() + self.bcc.len()
    }
}

// ---------------------------------------------------------------------------
// header safety
// ---------------------------------------------------------------------------
//
// A header ends at a CRLF. Any CR or LF inside a value that becomes a header
// therefore ends that header early and starts a new one, and the sender of the
// value — not the owner — chooses what that new header says. `Bcc:` is the
// interesting one: Gmail honours a `Bcc` in `raw` and strips it before
// delivery, so a smuggled `Bcc` is a silent extra recipient on a message the
// owner believes he addressed himself.
//
// `mail-builder` does not filter any of it. Measured against the pinned 0.4.4:
//
// | value                              | result             |
// |------------------------------------|--------------------|
// | short ASCII subject, CR LF          | written verbatim   |
// | same subject, ~50+ characters longer| RFC 2047 encoded   |
// | any non-ASCII subject               | RFC 2047 encoded   |
// | `In-Reply-To` id                    | written verbatim   |
// | `Raw` address header                | written verbatim   |
// | attachment `Content-Type`           | written verbatim   |
// | `Content-ID`                        | written verbatim   |
//
// The length asymmetry in the first two rows is why this is not obvious, and
// why it cannot be fixed by leaning on the library. `Text::write_header` calls
// `get_encoding_type(text, is_inline = true, is_body = false)`, and in that mode
// the `is_inline && ch == b'\n'` arm matches *before* the arm that would set
// `needs_encoding` for a header. A long subject only escapes because it trips a
// separate `line_len > 77` fold check. So "it gets encoded" is true of some
// subjects, by accident, and of none of the other fields — `Raw`, `MessageId`
// and a `ContentType` attribute have no encoder in front of them at all.
//
// **Refused, not repaired.** Stripping the CR and sending the rest would put a
// message on the wire that the owner did not write, with no sign that anything
// happened — and in the case that matters, a draft the agent wrote after
// reading hostile mail, the stripped version still reads plausibly and syncs to
// his phone. Failure has to be visible; that is the rule this project has paid
// for most. An error naming the field is something he can act on. A quietly
// edited subject is not.
//
// **Here, and not further out.** Every send, every Gmail draft and every outbox
// row is built by `build_rfc822`. A check at an IPC command or in the agent's
// tool schema is correct until somebody adds the next caller.

/// The bytes that end a header, plus the one that ends a string for whatever
/// parses it next.
fn line_breaking(ch: char) -> bool {
    matches!(ch, '\r' | '\n' | '\0')
}

/// Reject a value that would not survive being written as a header.
fn check_header_text(field: &str, value: &str) -> Result<()> {
    if value.chars().any(line_breaking) {
        return Err(ComposeError::invalid(format!(
            "the {field} contains a line break, which a mail header cannot carry"
        )));
    }
    Ok(())
}

/// The characters that let one address smuggle a second mailbox into the same
/// header. `render_mailbox` wraps the address in `<…>`, so a `>` closes it early
/// and everything after it is read as another recipient; `,` and `;` separate
/// mailboxes; `"` opens a quoted-string that swallows the rest of the line.
///
/// This is the same rejection set as `agent::tools::is_address`, which already
/// guards the agent's own arguments. Duplicating it is the point: that check can
/// be reached around by a caller that is not the agent.
fn check_address(field: &str, mailbox: &Mailbox) -> Result<()> {
    if let Some(name) = &mailbox.name {
        check_header_text(&format!("{field} display name"), name)?;
    }
    let email = mailbox.email.trim();
    if email
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '"'))
    {
        return Err(ComposeError::invalid(format!(
            "the {field} address {email:?} contains a character a mail header cannot carry"
        )));
    }
    Ok(())
}

/// A `Message-ID`, `In-Reply-To` or `References` id. `MessageId` writes it
/// inside `<…>`, so the brackets are structural here too.
fn check_message_id(field: &str, id: &str) -> Result<()> {
    if id
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>'))
    {
        return Err(ComposeError::invalid(format!(
            "the {field} {id:?} contains a character a mail header cannot carry"
        )));
    }
    Ok(())
}

/// Build the message. The only fallible parts are "no recipients", a value that
/// cannot go in a header, and an I/O error writing into a `Vec`, which cannot
/// happen.
pub fn build_rfc822(msg: &Outgoing) -> Result<Vec<u8>> {
    if msg.from.email.trim().is_empty() {
        return Err(ComposeError::invalid(
            "a message needs a From address; the account it is sent from is unknown",
        ));
    }
    if msg.recipient_count() == 0 {
        return Err(ComposeError::invalid("a message needs at least one recipient"));
    }

    // Everything that becomes a header, checked before anything is written. The
    // order matters only for which error the owner sees first.
    check_header_text("subject", &msg.subject)?;
    check_address("From", &msg.from)?;
    for (field, list) in [("To", &msg.to), ("Cc", &msg.cc), ("Bcc", &msg.bcc)] {
        for mailbox in list.iter() {
            check_address(field, mailbox)?;
        }
    }
    check_message_id("Message-ID", strip_brackets(&msg.message_id))?;
    if let Some(parent) = &msg.in_reply_to {
        check_message_id("In-Reply-To id", strip_brackets(parent))?;
    }
    for reference in &msg.references {
        check_message_id("References id", strip_brackets(reference))?;
    }
    // Attachment metadata is a header parameter rather than a header, but a CR
    // in a `Content-Type` ends that header just the same, and a `Content-ID` is
    // written raw inside `<…>`. `filename` is the one `mail-builder` does escape
    // — it is checked anyway, so the boundary has no exceptions to remember.
    for file in &msg.attachments {
        check_header_text("attachment filename", &file.filename)?;
        check_header_text("attachment content type", &file.mime_type)?;
        check_message_id("attachment Content-ID", &file.content_id)?;
    }

    let mut builder = MessageBuilder::new()
        .header("From", Raw::new(render_mailbox(&msg.from)))
        .header("To", Raw::new(render_mailboxes(&msg.to)));

    if !msg.cc.is_empty() {
        builder = builder.header("Cc", Raw::new(render_mailboxes(&msg.cc)));
    }
    if !msg.bcc.is_empty() {
        // Gmail honours a Bcc header in `raw` and strips it before delivery, so
        // the header is the whole mechanism — there is no separate envelope.
        builder = builder.header("Bcc", Raw::new(render_mailboxes(&msg.bcc)));
    }

    builder = builder
        .subject(msg.subject.clone())
        .date(Date::new(msg.date_ms.div_euclid(1000)))
        .message_id(MessageId::new(strip_brackets(&msg.message_id).to_string()));

    if let Some(parent) = &msg.in_reply_to {
        let parent = strip_brackets(parent);
        if !parent.is_empty() {
            builder = builder.in_reply_to(MessageId::new(parent.to_string()));
        }
    }
    if !msg.references.is_empty() {
        let refs: Vec<String> = msg
            .references
            .iter()
            .map(|r| strip_brackets(r).to_string())
            .filter(|r| !r.is_empty())
            .collect();
        if !refs.is_empty() {
            builder = builder.references(MessageId::new_list(refs.into_iter()));
        }
    }

    // Both parts, always. A text-only reply reads as plain in every client; an
    // html-only reply is unreadable in the ones that refuse HTML, and lands in
    // more spam filters.
    let (inline, attached): (Vec<_>, Vec<_>) =
        msg.attachments.iter().partition(|file| file.inline);

    if inline.is_empty() {
        // The shape this has always produced, kept verbatim rather than routed
        // through the builder below: no message without an inline image should
        // change structure because this feature was added.
        //
        // With files, it becomes `multipart/mixed` wrapping the same
        // `multipart/alternative` — the nesting every mail client expects, and
        // the reason the alternative is built first rather than being flattened
        // alongside the attachments.
        builder = builder.text_body(msg.text.clone()).html_body(msg.html.clone());
        for file in &attached {
            builder = builder.attachment(
                file.mime_type.clone(),
                file.filename.clone(),
                file.bytes.clone(),
            );
        }
    } else {
        builder = builder.body(related_body(msg, &inline, &attached));
    }

    builder
        .write_to_vec()
        .map_err(|e| ComposeError::Mime(e.to_string()))
}

/// The structure an inline image needs, which `mail-builder` will not produce
/// on its own.
///
/// Its [`inline`](MessageBuilder::inline) helper puts the image *beside* the
/// alternative inside `multipart/mixed`, and RFC 2387 says a `cid:` reference
/// resolves within a `multipart/related`. Gmail and Apple Mail both resolve it
/// from the flat shape anyway; Outlook and several webmails do not, and show the
/// image a second time at the bottom as an unnamed attachment. So the nesting is
/// built by hand:
///
/// ```text
/// multipart/mixed                                (only when files are attached too)
/// ├── multipart/related; type="multipart/alternative"
/// │   ├── multipart/alternative
/// │   │   ├── text/plain
/// │   │   └── text/html                          ← <img src="cid:…">
/// │   └── image/png; Content-ID: <…>             ← Content-Disposition: inline
/// └── application/pdf                            ← Content-Disposition: attachment
/// ```
///
/// The `type` parameter on the related part names the root — which client is
/// meant to be displayed rather than resolved — and is what stops a reader that
/// takes the first part literally from rendering the image alone.
fn related_body(
    msg: &Outgoing,
    inline: &[&OutgoingAttachment],
    attached: &[&OutgoingAttachment],
) -> MimePart<'static> {
    let alternative = MimePart::new(
        "multipart/alternative",
        vec![
            MimePart::new("text/plain", msg.text.clone()),
            MimePart::new("text/html", msg.html.clone()),
        ],
    );

    let mut related_parts = Vec::with_capacity(inline.len() + 1);
    related_parts.push(alternative);
    related_parts.extend(inline.iter().map(|file| file.part()));
    let related = MimePart::new(
        ContentType::new("multipart/related").attribute("type", "multipart/alternative"),
        related_parts,
    );

    if attached.is_empty() {
        return related;
    }
    let mut mixed_parts = Vec::with_capacity(attached.len() + 1);
    mixed_parts.push(related);
    mixed_parts.extend(attached.iter().map(|file| file.part()));
    MimePart::new("multipart/mixed", mixed_parts)
}

// ---------------------------------------------------------------------------
// threading
// ---------------------------------------------------------------------------

// The two functions below lift a value out of *somebody else's* message and
// hand it back as a default the owner never typed. That is the one place where
// dropping a character is right rather than dishonest: it is a parse of foreign
// data, not an edit of his text.
//
// Refusing here instead would hand a sender a way to disable the reply button.
// A `Message-ID` carrying a CR would make `build_rfc822` refuse every reply in
// that thread, naming a header there is no control for — an error he can read
// and cannot act on, which is the failure mode the reject is supposed to avoid.
// So the derived values are made clean at the point they are derived, and
// `build_rfc822`'s refusal stays where it belongs: on values that reached it
// some way nobody anticipated.

/// Is this a message id that can be written as one?
///
/// A `Message-ID` is `dot-atom "@" dot-atom` — whitespace and `<>` are not in
/// it — so anything carrying them is malformed at the source, and dropping it
/// costs a threading hint rather than correctness.
fn usable_id(id: &str) -> bool {
    !id.chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>'))
}

/// Turn the line breaks in a foreign subject into spaces.
///
/// The injected text stays visible — as text, in the subject field, where he
/// can see it and delete it — instead of becoming a header.
fn flatten_subject(subject: &str) -> String {
    subject
        .chars()
        .map(|c| if line_breaking(c) { ' ' } else { c })
        .collect()
}

/// Strip one layer of `<…>`, and any surrounding whitespace.
pub fn strip_brackets(id: &str) -> &str {
    let id = id.trim();
    id.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(id)
        .trim()
}

/// Split a `References`/`In-Reply-To` header value into bare ids.
///
/// The header is a whitespace-separated list of `<id>` tokens, but real mail
/// contains comma-separated ones too, so both separators are accepted.
pub fn parse_id_list(header: &str) -> Vec<String> {
    header
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .map(strip_brackets)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `(in_reply_to, references)` for a reply to `parent`, per RFC 5322 §3.6.4.
///
/// The chain is the parent's own `References` (or, failing that, its
/// `In-Reply-To`) with the parent's `Message-ID` appended. Nothing is trimmed:
/// some clients cap the chain at twenty ids to keep the header short, and a
/// trimmed chain is exactly how a long thread splits in a client that groups by
/// the *first* id. A long header is cheaper than a broken thread.
pub fn references_for_reply(parent: &Message) -> (Option<String>, Vec<String>) {
    let parent_id = parent
        .rfc822_message_id
        .as_deref()
        .map(strip_brackets)
        .filter(|s| !s.is_empty())
        .filter(|s| usable_id(s))
        .map(str::to_string);

    let mut chain: Vec<String> = parent
        .references
        .as_deref()
        .map(parse_id_list)
        .filter(|v| !v.is_empty())
        .or_else(|| {
            parent
                .in_reply_to
                .as_deref()
                .map(parse_id_list)
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_default();
    chain.retain(|id| usable_id(id));

    if let Some(id) = &parent_id {
        if !chain.iter().any(|existing| existing == id) {
            chain.push(id.clone());
        }
    }

    (parent_id, chain)
}

/// A `Message-ID` for a message Mach is sending.
///
/// The domain is the sender's own, which is what receiving servers expect and
/// what stops some filters from scoring the message. Gmail replaces this on
/// send — the value matters because the local optimistic copy is written with
/// it, so the thread the user is looking at has a consistent identity before
/// the real one arrives.
pub fn generate_message_id(from_email: &str, now_ms: i64, entropy: u64) -> String {
    let domain = from_email
        .rsplit_once('@')
        .map(|(_, d)| d)
        .filter(|d| !d.is_empty())
        .unwrap_or("mach.local");
    format!("{now_ms:x}.{entropy:x}.mach@{domain}")
}

// ---------------------------------------------------------------------------
// subject
// ---------------------------------------------------------------------------

/// `Re: ` without stacking.
///
/// Strips every leading reply prefix — `Re:`, `RE:`, `Re[2]:`, `Re :`, and the
/// `Fwd:` a reply-to-a-forward carries — then adds exactly one. Only *reply*
/// prefixes are stripped for a reply: Gmail's own answer to "Fwd: Invoice" is
/// "Re: Fwd: Invoice", and dropping the Fwd would rename someone else's thread.
pub fn reply_subject(subject: &str) -> String {
    let subject = flatten_subject(subject);
    let base = strip_prefixes(&subject, &["re"]);
    if base.is_empty() {
        "Re:".to_string()
    } else {
        format!("Re: {base}")
    }
}

/// `Fwd: ` without stacking, by the same rule.
pub fn forward_subject(subject: &str) -> String {
    let subject = flatten_subject(subject);
    let base = strip_prefixes(&subject, &["fwd", "fw"]);
    if base.is_empty() {
        "Fwd:".to_string()
    } else {
        format!("Fwd: {base}")
    }
}

/// Repeatedly remove a leading `word:` / `word[n]:` prefix.
fn strip_prefixes<'a>(subject: &'a str, words: &[&str]) -> &'a str {
    let mut rest = subject.trim();
    'outer: loop {
        for word in words {
            if let Some(stripped) = strip_one(rest, word) {
                rest = stripped.trim_start();
                continue 'outer;
            }
        }
        return rest;
    }
}

fn strip_one<'a>(subject: &'a str, word: &str) -> Option<&'a str> {
    let lower = subject.to_ascii_lowercase();
    let rest = lower.strip_prefix(word)?;
    // `Re[2]:` and `Re(2):` are what some clients count replies with.
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix(|c| c == '[' || c == '(') {
        Some(after) => {
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after = &after[digits.len()..];
            after.strip_prefix(|c| c == ']' || c == ')')?.trim_start()
        }
        None => rest,
    };
    rest.strip_prefix(':')?;
    // The offsets are the same in the lowercase copy: `to_ascii_lowercase` is
    // byte-for-byte length preserving.
    let consumed = subject.len() - rest.len() + 1;
    Some(&subject[consumed..])
}

// ---------------------------------------------------------------------------
// address rendering
// ---------------------------------------------------------------------------

/// `Jane Doe <jane@x.com>, "Doe, John" <john@x.com>, =?UTF-8?B?Sm9zw6k=?= <j@x.com>`
pub fn render_mailboxes(list: &[Mailbox]) -> String {
    list.iter()
        .map(render_mailbox)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn render_mailbox(mailbox: &Mailbox) -> String {
    let email = mailbox.email.trim();
    match mailbox
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        None => format!("<{email}>"),
        Some(name) => format!("{} <{email}>", encode_phrase(name)),
    }
}

/// RFC 5322 `atext` plus space and dot — the characters a display name may
/// carry unquoted. A leading or trailing dot is still fine inside a phrase.
fn is_plain_phrase_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'/'
                | b'='
                | b'?'
                | b'^'
                | b'_'
                | b'`'
                | b'{'
                | b'|'
                | b'}'
                | b'~'
                | b' '
                | b'.'
        )
}

/// A display name, encoded exactly as much as it needs to be.
pub fn encode_phrase(name: &str) -> String {
    if !name.is_ascii() {
        return encoded_words(name);
    }
    // `?` and `=` are atext, but a name that happens to look like an
    // encoded-word must not be mistaken for one by the receiver.
    let looks_encoded = name.contains("=?") && name.contains("?=");
    if !looks_encoded && name.bytes().all(is_plain_phrase_byte) {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for ch in name.chars() {
        match ch {
            '\r' | '\n' => continue,
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// The base64 payload of one encoded-word must keep the whole word — prefix,
/// payload, suffix — inside RFC 2047's 75-octet limit. `=?UTF-8?B?` + `?=` is
/// twelve, leaving 63, rounded down to a multiple of four: 60 base64 characters,
/// which is 45 input bytes.
const MAX_ENCODED_INPUT_BYTES: usize = 45;

fn encoded_words(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut chunk = String::new();
    for ch in name.chars() {
        // Never split a character across two encoded-words: each word is
        // decoded independently, and half a UTF-8 sequence decodes to nothing.
        if chunk.len() + ch.len_utf8() > MAX_ENCODED_INPUT_BYTES {
            words.push(one_encoded_word(&chunk));
            chunk.clear();
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        words.push(one_encoded_word(&chunk));
    }
    // Whitespace between adjacent encoded-words is removed by the decoder, so
    // the space is a fold point rather than content.
    words.join(" ")
}

fn one_encoded_word(chunk: &str) -> String {
    format!("=?UTF-8?B?{}?=", BASE64.encode(chunk.as_bytes()))
}
