//! Who a reply goes to.
//!
//! This is arithmetic, not policy, and it is where mail clients most visibly
//! embarrass their users: a reply-all that mails you your own message, or that
//! lists the same person twice because one header spelled the address in caps.
//!
//! The rules, in the order they are applied:
//!
//! 1. **The reply goes to the author.** Except when the author is you: see
//!    below.
//! 2. **Your own address never appears.** The account the thread arrived on is
//!    removed from every field. Only that address — your *other*
//!    accounts are, as far as this thread is concerned, other people, and
//!    silently dropping them would lose a real recipient.
//! 3. **Cc is preserved**, minus anyone already in To, minus yourself.
//! 4. **Everything is deduped case-insensitively on the address**, keeping the
//!    first occurrence — which is the one whose display name the thread has
//!    been showing.
//!
//! One case is easy to get wrong and is pinned by a test: replying to a message
//! **you** sent. `From` is you, so rule 2 would empty the To field. The answer
//! there is the original `To` — you are continuing your own message, not
//! writing to yourself.
//!
//! # `Reply-To`
//!
//! A sender who set `Reply-To` asked for the answer to go somewhere else, and
//! it wins over `From`. Mailing lists are the case that matters: without this,
//! replying to a list goes to whichever individual happened to post rather than
//! back to the list, which is both wrong and embarrassing in public.
//!
//! It does *not* change the "replying to your own message" rule below — that is
//! about who wrote the thing, which is `From` regardless of where answers were
//! asked to go.

use crate::db::models::{Message, Participant};

use super::mime::Mailbox;

/// A resolved recipient set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipients {
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
}

impl Recipients {
    pub fn is_empty(&self) -> bool {
        self.to.is_empty() && self.cc.is_empty()
    }
}

/// Case-insensitive dedupe on the address, first occurrence wins.
///
/// The display name of the first occurrence is kept, except that a later
/// occurrence carrying a name beats an earlier one that had none — an address
/// seen bare in `To` and named in `Cc` should still render as a person.
pub fn dedupe(list: impl IntoIterator<Item = Mailbox>) -> Vec<Mailbox> {
    let mut out: Vec<Mailbox> = Vec::new();
    for mailbox in list {
        let key = mailbox.email.to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        match out.iter_mut().find(|m| m.email.eq_ignore_ascii_case(&key)) {
            Some(existing) => {
                if existing.name.is_none() && mailbox.name.is_some() {
                    existing.name = mailbox.name;
                }
            }
            None => out.push(mailbox),
        }
    }
    out
}

fn without(list: Vec<Mailbox>, excluded: &[String]) -> Vec<Mailbox> {
    list.into_iter()
        .filter(|m| !excluded.iter().any(|e| e.eq_ignore_ascii_case(&m.email)))
        .collect()
}

fn to_mailboxes(people: &[Participant]) -> Vec<Mailbox> {
    people
        .iter()
        .filter(|p| !p.email.trim().is_empty())
        .map(Mailbox::from_participant)
        .collect()
}

/// The recipients of a reply.
///
/// `self_address` is the address of the account the thread arrived on — the
/// address the reply will be *from*, which is the only one that must not also
/// receive it. `reply_all` false narrows the result to the single To field.
pub fn reply_recipients(message: &Message, self_address: &str, reply_all: bool) -> Recipients {
    let me = vec![self_address.trim().to_string()];

    // Rule 1. `Reply-To` wins over `From` when the sender set one: they asked
    // for the answer to go elsewhere. Mailing lists depend on this — without
    // it, a reply to a list goes to whoever happened to post rather than back
    // to the list.
    let author = if message.reply_to.is_empty() {
        to_mailboxes(std::slice::from_ref(&message.from))
    } else {
        to_mailboxes(&message.reply_to)
    };
    let from_is_me = message.from.email.eq_ignore_ascii_case(self_address.trim());

    // Rule 2's exception: your own message. Continue it rather than answering
    // yourself.
    let primary = if from_is_me {
        to_mailboxes(&message.to)
    } else {
        author
    };

    let to = dedupe(without(primary, &me));

    if !reply_all {
        return Recipients { to, cc: Vec::new() };
    }

    // Everyone else who was on it: the original To (unless it already became
    // the To above) plus the original Cc.
    let mut rest: Vec<Mailbox> = Vec::new();
    if !from_is_me {
        rest.extend(to_mailboxes(&message.to));
    }
    rest.extend(to_mailboxes(&message.cc));

    let already: Vec<String> = to
        .iter()
        .map(|m| m.email.clone())
        .chain(me.iter().cloned())
        .collect();
    let cc = dedupe(without(rest, &already));

    // A reply-all whose To emptied out (you were the only recipient of your own
    // message) should promote the Cc rather than send nowhere.
    if to.is_empty() && !cc.is_empty() {
        return Recipients { to: cc, cc: Vec::new() };
    }

    Recipients { to, cc }
}

/// The recipients of a forward: none. A forward is addressed by hand, and
/// pre-filling it from the thread is how people accidentally forward a private
/// thread back to its author.
pub fn forward_recipients() -> Recipients {
    Recipients::default()
}

/// Parse a comma-separated recipient line the way a person types it:
/// `Jane <jane@x.com>, bob@y.com`.
///
/// Deliberately forgiving: nothing is rejected here. What cannot be parsed
/// becomes a bare address and fails visibly at send, which is better than a
/// field that refuses a keystroke. The composer's own field keeps the raw text
/// and parses on the way out; this is the same grammar for callers that only
/// have a string — the agent, and a future paste handler.
pub fn parse_list(input: &str) -> Vec<Mailbox> {
    let mut out = Vec::new();
    for chunk in split_top_level(input) {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        out.push(parse_one(chunk));
    }
    dedupe(out)
}

fn parse_one(chunk: &str) -> Mailbox {
    if let (Some(open), Some(close)) = (chunk.rfind('<'), chunk.rfind('>')) {
        if open < close {
            let email = chunk[open + 1..close].trim().to_string();
            let name = chunk[..open].trim().trim_matches('"').trim().to_string();
            return Mailbox {
                name: (!name.is_empty()).then_some(name),
                email,
            };
        }
    }
    Mailbox {
        name: None,
        email: chunk.to_string(),
    }
}

/// Split on commas that are not inside a quoted display name.
fn split_top_level(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    for (index, ch) in input.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ',' | ';' if !quoted => {
                out.push(&input[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&input[start..]);
    out
}
