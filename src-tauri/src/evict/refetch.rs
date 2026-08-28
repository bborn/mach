//! Getting an evicted body back from Gmail.
//!
//! The other half of [`super`]. A message whose HTML was dropped renders from
//! `body_text` the instant it is opened; this is what runs behind that, once,
//! and what caches the answer so the second open costs nothing.
//!
//! It reaches Google through [`GoogleClients`], which is the same seam the
//! command layer uses and the reason this is testable against a scripted
//! transport rather than against the owner's mailbox.

use std::sync::Arc;

use rusqlite::OptionalExtension;

use crate::commands::GoogleClients;
use crate::db::models::is_local_message_id;
use crate::db::{Db, DbError};
use crate::google::gmail::MessageFormat;
use crate::google::GoogleError;

/// Why a body could not be brought back.
///
/// Each variant is a sentence the reading pane puts on screen. Failure being
/// visible is the project's rule, and a body that silently stayed text would be
/// indistinguishable from a message that never had HTML.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("no message with id {0} in the local store")]
    NotFound(i64),

    /// Refused before any request. Nothing in this state is refetchable, and
    /// nothing in this state should ever have been evicted — reaching it means
    /// the guard was bypassed, so it is an error rather than a no-op.
    #[error("this message was written here and Gmail has no copy of it")]
    Unrecoverable,

    #[error("this message is no longer in Gmail")]
    Gone,

    #[error("Gmail has no HTML for this message")]
    NoHtml,

    #[error("could not reach Gmail: {0}")]
    Google(#[from] GoogleError),

    #[error("local store: {0}")]
    Db(#[from] DbError),

    #[error("{0}")]
    Account(String),
}

impl RestoreError {
    /// The stable tag, matching the shape the rest of the IPC surface uses.
    pub fn kind(&self) -> &'static str {
        match self {
            RestoreError::NotFound(_) => "notFound",
            RestoreError::Unrecoverable => "unrecoverable",
            RestoreError::Gone => "gone",
            RestoreError::NoHtml => "noHtml",
            RestoreError::Google(_) => "google",
            RestoreError::Db(_) => "db",
            RestoreError::Account(_) => "unknownAccount",
        }
    }
}

/// What a restore did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restored {
    /// The HTML is back in the store.
    Fetched { bytes: usize },
    /// It was never gone — the row already had its body. Two readers opening the
    /// same message, or a sync that re-fetched it first.
    AlreadyResident,
}

/// The row facts a restore needs.
struct Target {
    account_id: i64,
    gmail_message_id: String,
    evicted: bool,
    resident: bool,
    /// What the row is currently rendered by, so the restore can tell whether
    /// the markup it is about to store says anything not already findable.
    body_text: Option<String>,
    /// Whether that `body_text` is itself a reading of this message's markup,
    /// written by the sweep on the way out. If it is, the markup is already
    /// indexed and there is nothing here to add.
    body_text_derived: bool,
    /// Whether the row already carries the markup's text for the index.
    has_search_text: bool,
}

fn target(db: &Db, message_id: i64) -> Result<Target, RestoreError> {
    let found = db.read(|conn| {
        Ok(conn
            .query_row(
                "SELECT account_id,
                        gmail_message_id,
                        html_evicted_at IS NOT NULL,
                        body_html IS NOT NULL,
                        body_text,
                        body_text_derived_at IS NOT NULL,
                        search_text IS NOT NULL
                   FROM messages WHERE id = ?1",
                [message_id],
                |row| {
                    Ok(Target {
                        account_id: row.get(0)?,
                        gmail_message_id: row.get(1)?,
                        evicted: row.get(2)?,
                        resident: row.get(3)?,
                        body_text: row.get(4)?,
                        body_text_derived: row.get(5)?,
                        has_search_text: row.get(6)?,
                    })
                },
            )
            .optional()?)
    })?;
    found.ok_or(RestoreError::NotFound(message_id))
}

/// Fetch this message's HTML from Gmail and store it.
///
/// Idempotent, and cheap when there is nothing to do: a row that already holds
/// its body returns [`Restored::AlreadyResident`] without a request, which is
/// what makes two opens of the same message cost one fetch.
///
/// The write is `WHERE html_evicted_at IS NOT NULL`, so a sync pass that
/// restored the body first is not overwritten by a slower request carrying the
/// same bytes.
///
/// It also fills the row's `search_text` while the markup is in hand, for the
/// rows nothing else can reach — see the comment on the write.
pub async fn restore_html(
    db: &Db,
    clients: &Arc<dyn GoogleClients>,
    message_id: i64,
) -> Result<Restored, RestoreError> {
    let target = target(db, message_id)?;

    if target.resident {
        return Ok(Restored::AlreadyResident);
    }
    // Belt and braces on the eviction guard. Nothing local can be evicted, so
    // nothing local can be here; if one ever is, saying so beats a 404 from a
    // request that was never going to work.
    if is_local_message_id(&target.gmail_message_id) {
        return Err(RestoreError::Unrecoverable);
    }
    if !target.evicted {
        // Never had HTML. Not an error, and not a request either.
        return Err(RestoreError::NoHtml);
    }

    let gmail = clients
        .gmail(target.account_id)
        .map_err(|e| RestoreError::Account(e.to_string()))?;

    let message = match gmail
        .messages_get("me", &target.gmail_message_id, MessageFormat::Full)
        .await
    {
        Ok(message) => message,
        Err(e) if e.is_not_found() => return Err(RestoreError::Gone),
        Err(e) => return Err(RestoreError::Google(e)),
    };

    let html = message
        .extract_body()
        .html
        .filter(|h| !h.trim().is_empty())
        .ok_or(RestoreError::NoHtml)?;

    let bytes = html.len();
    let now = super::now_ms();

    /*
     * The last rows this can reach.
     *
     * `search_text` — the readable text of the markup, for `messages_fts` and
     * for nothing else — is written by sync when a message arrives, and was
     * written once by `db::backfill::derive_search_text` for the mail that
     * arrived before sync did that. Neither can reach a message whose HTML had
     * already been evicted, because there was no markup here to read. On the
     * owner's store that is 34 752 messages: evicted, and carrying only what
     * their sender wrote.
     *
     * There is markup here now, because somebody opened the message and this
     * function went and got it. So the same derivation runs on the way past, at
     * the cost of one sanitize on a request that has already taken a round trip.
     *
     * A row whose `body_text` the sweep derived is skipped before that: its
     * markup is already indexed, under `body_text`, and storing the same text
     * again would put every one of its terms in twice.
     *
     * The write fills `search_text` only where it is NULL, so a sync that got
     * there first keeps what it wrote. The restore itself is unconditional on
     * that — the reader waited for this body and must get it either way.
     */
    let derived = (!target.has_search_text && !target.body_text_derived)
        .then(|| crate::render::text::searchable_text(target.body_text.as_deref(), &html))
        .flatten();
    db.write(|conn| {
        conn.execute(
            "UPDATE messages
                SET body_html        = ?2,
                    html_restored_at = ?3,
                    search_text      = coalesce(search_text, ?4)
              WHERE id = ?1 AND html_evicted_at IS NOT NULL",
            rusqlite::params![message_id, html, now, derived],
        )?;
        Ok(())
    })?;

    Ok(Restored::Fetched { bytes })
}
