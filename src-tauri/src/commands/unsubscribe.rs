//! `Command::Unsubscribe` — the one command with no local write and no inverse.
//!
//! The shape is the reverse of every other command in this layer. There is no
//! optimistic write to make, nothing to roll back, and the remote call is not
//! to Google. What it shares is the reporting: it returns a [`CommandResult`],
//! so a sender that answers `500` reaches the status bar through the same path
//! a Gmail refusal does.
//!
//! # The rule runs here, not at the call site
//!
//! The frontend already knows whether an offer exists — that is how the button
//! is drawn — and it is asked again anyway. Two reasons, and neither is
//! defensive programming for its own sake:
//!
//!  * the UI's copy of the answer can be minutes old, and the message may have
//!    been moved to Spam since;
//!  * the agent can dispatch this, and an agent talked into unsubscribing from
//!    a message the rule would refuse is a machine for confirming his address
//!    to a spammer.
//!
//! So the store is read, [`crate::unsub::rule::verdict`] is re-run, and a
//! verdict that is not [`Verdict::Unsubscribe`] refuses with the reason.

use crate::commands::error::{CommandError, CommandFailure, FailureKind};
use crate::commands::{CommandDispatcher, CommandResult};
use crate::unsub::rule::{Decline, Verdict};
use crate::unsub::{run, store, Target};

/// Unsubscribe from the list a message came from.
pub async fn execute(
    dispatcher: &CommandDispatcher,
    message_id: i64,
) -> Result<CommandResult, CommandError> {
    let candidate = dispatcher
        .db
        .read(move |conn| store::candidate(conn, message_id))?
        .ok_or_else(|| CommandError::Invalid {
            message: format!("no message with id {message_id} in the local store"),
        })?;

    let target = match crate::unsub::verdict(&candidate) {
        Verdict::Unsubscribe(target) => target,
        Verdict::ReportSpam(reason) => {
            return Ok(refused(message_id, spam_sentence(reason)));
        }
        Verdict::Nothing(reason) => {
            return Ok(refused(message_id, nothing_sentence(reason)));
        }
    };

    // The account the message arrived on, needed for the `mailto:` path and
    // read here so the failure is a plain `Err` rather than a refusal.
    let account = dispatcher.db.read(move |conn| account_for(conn, message_id))?;

    let outcome = match &target {
        Target::OneClick { url } => {
            let http = dispatcher
                .unsub_http
                .as_ref()
                .ok_or_else(|| CommandError::Invalid {
                    message: "this build has no way to make an unsubscribe request".into(),
                })?;
            run::one_click(http.as_ref(), url).await
        }
        Target::Mail { to, subject, body } => {
            let (account_id, from) = account.ok_or_else(|| CommandError::Invalid {
                message: format!("message {message_id} has no account to send from"),
            })?;
            let gmail = dispatcher.clients.gmail(account_id)?;
            run::send_mail(
                &gmail,
                &dispatcher.user_id,
                &from,
                to,
                subject,
                body.as_deref(),
                crate::ipc::compose::now_ms(),
                entropy(),
            )
            .await
        }
        // Reached only if the frontend dispatched a command for an offer it was
        // told needs a browser. Refused rather than silently opened: a command
        // must not do something other than what it says.
        Target::Link { .. } => Err(run::Refused {
            message: "this sender's unsubscribe is a web page; open it instead".into(),
            retriable: false,
        }),
    };

    match outcome {
        Ok(()) => Ok(CommandResult {
            ok: true,
            message: "Unsubscribed".to_string(),
            undo: None,
            undo_label: None,
            applied: vec![message_id],
            failed: Vec::new(),
        }),
        Err(refusal) => Ok(CommandResult {
            ok: false,
            message: refusal.message.clone(),
            undo: None,
            undo_label: None,
            applied: Vec::new(),
            failed: vec![CommandFailure {
                ids: vec![message_id],
                // `Server` and `Forbidden` rather than a new variant: the
                // frontend maps every `FailureKind` to a label, and a kind it
                // has never heard of would read as a blank.
                kind: if refusal.retriable {
                    FailureKind::Server
                } else {
                    FailureKind::Forbidden
                },
                message: refusal.message,
                retriable: refusal.retriable,
                // Nothing was written locally, so nothing was put back.
                rolled_back: false,
            }],
        }),
    }
}

/// A refusal by the rule. `ok: false` with no `failed` entry: nothing was
/// attempted, so there is nothing to report per id — only the sentence.
fn refused(message_id: i64, message: String) -> CommandResult {
    CommandResult {
        ok: false,
        message,
        undo: None,
        undo_label: None,
        applied: Vec::new(),
        failed: vec![CommandFailure {
            ids: vec![message_id],
            // `Invalid` is exactly this case: the command layer declined to
            // make the request at all.
            kind: FailureKind::Invalid,
            message: "Mach will not unsubscribe from this message".to_string(),
            retriable: false,
            rolled_back: false,
        }],
    }
}

fn spam_sentence(reason: Decline) -> String {
    match reason {
        Decline::UnknownSender => {
            "Nothing in this mailbox vouches for that sender, so unsubscribing would only \
             confirm the address. Report it as spam instead."
        }
        Decline::NotBulkMail => {
            "That unsubscribe header is on a message that is not otherwise bulk mail. \
             Report it as spam instead."
        }
        other => return nothing_sentence(other),
    }
    .to_string()
}

fn nothing_sentence(reason: Decline) -> String {
    match reason {
        Decline::NoHeader => "That message has no unsubscribe to use.",
        Decline::Unusable(_) => "That message's unsubscribe link is not one Mach will use.",
        Decline::NotAnArrival => "That is your own message.",
        Decline::InTrash => "That message is in the trash.",
        Decline::AlreadySpam => "That message is already in Spam, and Gmail has been told.",
        Decline::NotBulkMail | Decline::UnknownSender => {
            "Mach will not unsubscribe from that message."
        }
    }
    .to_string()
}

/// The account a message belongs to, and that account's own address.
fn account_for(
    conn: &rusqlite::Connection,
    message_id: i64,
) -> crate::db::Result<Option<(i64, String)>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT a.id, a.email
               FROM messages m JOIN accounts a ON a.id = m.account_id
              WHERE m.id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// Entropy for the outgoing `Message-ID`. The composer uses the same trick.
fn entropy() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish()
}
