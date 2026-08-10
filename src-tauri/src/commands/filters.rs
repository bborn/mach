//! Gmail filters: list, create, delete — and the sentence that says what one
//! does.
//!
//! # Why these are methods on the dispatcher and not [`Command`] variants
//!
//! Everything in [`Command`] obeys the four-step contract in the module docs:
//! write locally, call Google, revert on failure, return an inverse. A filter
//! satisfies none of the four. It has no local row, so there is nothing to
//! write first and nothing to revert. It has no inverse: deleting a filter does
//! not undo what the filter did, because the mail it already moved stays moved,
//! and re-creating a deleted filter does not restore its id. And it is not
//! addressed by thread id, which is the unit `CommandResult.applied` is
//! expressed in.
//!
//! So filters live beside the catalogue rather than in it. What they take from
//! the dispatcher is the one thing they need and the one thing that must not be
//! duplicated: the per-account [`GoogleClients`] factory, so the agent reaches
//! Gmail through exactly the object the preferences window reaches it through.
//! There is still no privileged path.
//!
//! [`Command`]: super::Command
//! [`GoogleClients`]: super::GoogleClients
//!
//! # What is not offered: "also apply to matching conversations"
//!
//! Gmail's web UI ends the filter dialog with a checkbox that runs the new rule
//! over mail already in the mailbox. Mach does not have one, for three reasons
//! that all point the same way.
//!
//! The API has no such parameter. `users.settings.filters.create` acts on mail
//! that arrives after it; the checkbox in Gmail's UI is Gmail's own client
//! doing a search and a bulk modify afterwards. Implementing it here means Mach
//! writing that loop.
//!
//! That loop is a different action with a different blast radius. Creating a
//! filter changes what happens to mail that does not exist yet, and is
//! therefore reversible in the only sense that matters — delete the rule and
//! nothing further happens. A bulk modify over an unbounded set of existing
//! conversations is not reversible by deleting the rule, and "archive
//! everything from this sender" over eleven years of mail is a very different
//! thing to consent to than "file this sender's mail from now on".
//!
//! And it would make the approval prompt lie. The owner is shown one sentence
//! and one button; folding a second, larger, differently-shaped action behind
//! that button is exactly the failure the gate exists to prevent.
//!
//! The capability is not lost. `search_threads` and the archive, trash and
//! label commands are already there, already undoable, and already go through
//! the same gate — so "and clear out the ones already in my inbox" is a second
//! step the owner watches happen and can take back with ⌘Z.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::db::{queries, Db};
use crate::google::types::{Filter, FilterAction, FilterCriteria};
use crate::google::GoogleError;

use super::error::CommandError;
use super::CommandDispatcher;

/// What Mach is doing when it needs `gmail.settings.basic`, for the sentence in
/// [`CommandError::MissingScope`].
const FILTER_ACTION: &str = "manage Gmail filters";

/// Gmail's user id for "whoever this token belongs to".
const ME: &str = "me";

// ---------------------------------------------------------------------------
// the scope notices
// ---------------------------------------------------------------------------

/// Accounts whose grant turned out to be too narrow.
///
/// # Why this is here and not in `AppState`
///
/// It is the state's to report — it belongs in `needsReauthorization`, beside
/// the accounts whose Keychain entry is gone, because from the owner's point of
/// view both mean "sign in again". But it is *discovered* on the path a request
/// fails on, and two callers walk that path: the preferences window, which has
/// the `AppState`, and an agent tool, which has a [`CommandDispatcher`] and
/// nothing else.
///
/// Putting it on the dispatcher gives both the same set, because both hold the
/// same `Arc<CommandDispatcher>`. `AppState::status_payload` reads it back out.
#[derive(Debug, Default)]
pub struct ScopeNotices {
    missing: Mutex<BTreeSet<String>>,
}

impl ScopeNotices {
    pub fn record(&self, email: &str) {
        lock(&self.missing).insert(email.to_string());
    }

    pub fn clear(&self, email: &str) {
        lock(&self.missing).remove(email);
    }

    /// Every account whose grant is missing a scope, in address order.
    pub fn emails(&self) -> Vec<String> {
        lock(&self.missing).iter().cloned().collect()
    }
}

/// A poisoned mutex here means another call panicked while holding a set of
/// strings. Recovering is strictly better than making every later call fail.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// the shape that leaves this module
// ---------------------------------------------------------------------------

/// One filter, on one account, with the sentence that says what it does.
///
/// `description` is computed here rather than in the window or by the model, so
/// the line the owner approves in the agent drawer and the line listed in
/// Preferences → Mail are the same line, produced by the same code, from the
/// same label names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountFilter {
    pub account_id: i64,
    pub account_email: String,
    pub id: String,
    pub criteria: FilterCriteria,
    pub action: FilterAction,
    pub description: String,
}

// ---------------------------------------------------------------------------
// the operations
// ---------------------------------------------------------------------------

impl CommandDispatcher {
    /// The accounts' filters, live from Google.
    ///
    /// `account_id` of `None` asks every account, and one account's failure
    /// fails the call — a list that silently omits a mailbox would read as
    /// "you have no filters there", which is the wrong answer to act on.
    pub async fn list_filters(
        &self,
        account_id: Option<i64>,
    ) -> Result<Vec<AccountFilter>, CommandError> {
        let mut out = Vec::new();
        for (id, email) in self.filter_accounts(account_id)? {
            let gmail = self.clients.gmail(id)?;
            let filters = gmail
                .filters_list(&self.user_id_or_me())
                .await
                .map_err(|e| self.scope_aware(id, &email, e))?;
            let names = self.label_names(id)?;
            out.extend(
                filters
                    .into_iter()
                    .map(|filter| self.account_filter(id, &email, filter, &names)),
            );
        }
        Ok(out)
    }

    /// Create one filter on one account.
    ///
    /// Refuses two shapes Google would accept and nobody means: a filter with
    /// no criteria, which matches every message that ever arrives, and one with
    /// no action, which is a rule that does nothing.
    pub async fn create_filter(
        &self,
        account_id: i64,
        filter: Filter,
    ) -> Result<AccountFilter, CommandError> {
        if filter.matches_everything() {
            return Err(CommandError::Invalid {
                message: "a filter needs something to match on — from, to, subject, a search \
                          query, or has-attachment"
                    .into(),
            });
        }
        if filter.does_nothing() {
            return Err(CommandError::Invalid {
                message: "a filter needs something to do — a label to add or remove, or an \
                          address to forward to"
                    .into(),
            });
        }

        let email = self.account_email(account_id)?;
        let gmail = self.clients.gmail(account_id)?;
        let created = gmail
            .filters_create(&self.user_id_or_me(), &filter)
            .await
            .map_err(|e| self.scope_aware(account_id, &email, e))?;
        let names = self.label_names(account_id)?;
        Ok(self.account_filter(account_id, &email, created, &names))
    }

    /// Delete one filter by the id Google assigned it.
    pub async fn delete_filter(
        &self,
        account_id: i64,
        filter_id: &str,
    ) -> Result<(), CommandError> {
        let email = self.account_email(account_id)?;
        let gmail = self.clients.gmail(account_id)?;
        gmail
            .filters_delete(&self.user_id_or_me(), filter_id)
            .await
            .map_err(|e| self.scope_aware(account_id, &email, e))
    }

    /// The plain-words sentence for a filter that has not been created yet —
    /// what an approval prompt has to show.
    pub fn describe_filter(&self, account_id: i64, filter: &Filter) -> String {
        let names = self.label_names(account_id).unwrap_or_default();
        describe(filter, &names)
    }

    /// The one account, or every account, as (id, email).
    fn filter_accounts(&self, account_id: Option<i64>) -> Result<Vec<(i64, String)>, CommandError> {
        match account_id {
            Some(id) => Ok(vec![(id, self.account_email(id)?)]),
            None => Ok(self
                .db
                .read(queries::list_accounts)?
                .into_iter()
                .map(|a| (a.id, a.email))
                .collect()),
        }
    }

    fn account_email(&self, account_id: i64) -> Result<String, CommandError> {
        self.db
            .read(|conn| crate::db::command_queries::account_by_id(conn, account_id))?
            .map(|a| a.email)
            .ok_or(CommandError::UnknownAccount { account_id })
    }

    /// Gmail label id → the name a human calls it.
    fn label_names(&self, account_id: i64) -> Result<BTreeMap<String, String>, CommandError> {
        Ok(label_names(&self.db, account_id)?)
    }

    fn user_id_or_me(&self) -> String {
        if self.user_id.is_empty() {
            ME.to_string()
        } else {
            self.user_id.clone()
        }
    }

    fn account_filter(
        &self,
        account_id: i64,
        email: &str,
        filter: Filter,
        names: &BTreeMap<String, String>,
    ) -> AccountFilter {
        AccountFilter {
            account_id,
            account_email: email.to_string(),
            description: describe(&filter, names),
            id: filter.id,
            criteria: filter.criteria,
            action: filter.action,
        }
    }

    /// Turn a Google error into a command error, and remember the one kind that
    /// the owner has to act on.
    ///
    /// The record is what makes the account show up as needing authorization
    /// rather than the failure being a sentence in one window that nothing else
    /// hears about.
    fn scope_aware(&self, account_id: i64, email: &str, error: GoogleError) -> CommandError {
        if error.is_insufficient_scope() {
            self.scope_notices.record(email);
            return CommandError::MissingScope {
                account_id,
                email: email.to_string(),
                action: FILTER_ACTION,
            };
        }
        CommandError::Invalid {
            message: error.to_string(),
        }
    }
}

fn label_names(db: &Db, account_id: i64) -> Result<BTreeMap<String, String>, crate::db::DbError> {
    Ok(db
        .read(|conn| queries::list_labels(conn, Some(account_id)))?
        .into_iter()
        .map(|label| (label.gmail_label_id, label.name))
        .collect())
}

// ---------------------------------------------------------------------------
// the sentence
// ---------------------------------------------------------------------------

/// A filter in plain words: what it matches, then what happens to it.
///
/// # Why a sentence and not the JSON
///
/// This is the text the owner is asked to approve, and `{"criteria":{"from":
/// "no-reply@…"},"action":{"removeLabelIds":["INBOX","UNREAD"]}}` is not a
/// question anybody can answer. `removeLabelIds: ["INBOX"]` in particular does
/// not read as "skip the inbox" to anyone who has not worked on Gmail's API,
/// and `addLabelIds: ["TRASH"]` — the destructive one — reads least like what
/// it is.
///
/// Label ids are resolved to names where the local store knows them, and shown
/// as the id where it does not, which is honest rather than empty.
pub fn describe(filter: &Filter, names: &BTreeMap<String, String>) -> String {
    format!(
        "{}. {}.",
        matches_clause(&filter.criteria),
        effects_clause(&filter.action, names)
    )
}

fn matches_clause(criteria: &FilterCriteria) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(from) = present(&criteria.from) {
        parts.push(format!("from {from}"));
    }
    if let Some(to) = present(&criteria.to) {
        parts.push(format!("to {to}"));
    }
    if let Some(subject) = present(&criteria.subject) {
        parts.push(format!("with \u{201c}{subject}\u{201d} in the subject"));
    }
    if let Some(query) = present(&criteria.query) {
        parts.push(format!("matching {query}"));
    }
    if let Some(negated) = present(&criteria.negated_query) {
        parts.push(format!("not matching {negated}"));
    }
    if criteria.has_attachment == Some(true) {
        parts.push("with an attachment".to_string());
    }
    if parts.is_empty() {
        // `create_filter` refuses this shape, so it can only be a filter Google
        // already holds. Saying so beats an empty sentence.
        return "Every message".to_string();
    }
    format!("Mail {}", join(&parts))
}

fn effects_clause(action: &FilterAction, names: &BTreeMap<String, String>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut labelled: Vec<String> = Vec::new();

    for id in &action.add_label_ids {
        match id.as_str() {
            // The two the whole feature is about.
            "TRASH" => parts.push("is deleted".to_string()),
            "SPAM" => parts.push("is marked as spam".to_string()),
            "STARRED" => parts.push("is starred".to_string()),
            "IMPORTANT" => parts.push("is marked important".to_string()),
            "UNREAD" => parts.push("is left unread".to_string()),
            other => labelled.push(label_name(other, names)),
        }
    }
    if !labelled.is_empty() {
        parts.push(format!("is labelled {}", join(&labelled)));
    }

    let mut unlabelled: Vec<String> = Vec::new();
    for id in &action.remove_label_ids {
        match id.as_str() {
            "INBOX" => parts.push("skips the inbox".to_string()),
            "UNREAD" => parts.push("is marked as read".to_string()),
            "SPAM" => parts.push("is never marked as spam".to_string()),
            "IMPORTANT" => parts.push("is never marked important".to_string()),
            "STARRED" => parts.push("is unstarred".to_string()),
            other => unlabelled.push(label_name(other, names)),
        }
    }
    if !unlabelled.is_empty() {
        parts.push(format!("loses the label {}", join(&unlabelled)));
    }

    if let Some(forward) = present(&action.forward) {
        parts.push(format!("is forwarded to {forward}"));
    }

    if parts.is_empty() {
        return "Nothing happens to it".to_string();
    }
    // "Mail from x. It skips the inbox and is deleted." — the subject of the
    // second sentence is the mail named by the first.
    format!("It {}", join(&parts))
}

fn label_name(id: &str, names: &BTreeMap<String, String>) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.to_string())
}

fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// `a`, `a and b`, `a, b and c` — the shape a person would have written.
fn join(parts: &[String]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        2 => format!("{} and {}", parts[0], parts[1]),
        _ => {
            let (last, rest) = parts.split_last().expect("length is at least three");
            format!("{} and {}", rest.join(", "), last)
        }
    }
}
