//! The typed command layer — Mach's entire write path.
//!
//! Every action the app can take is a [`Command`]. The UI dispatches them; it
//! never calls Gmail or Calendar itself. When the agent arrives, its tools are
//! these same commands — [`Command::catalogue`] already describes them as data,
//! and every one of them already knows how to undo itself. That dual role is
//! the reason this is a layer with a vocabulary rather than a folder of helper
//! functions.
//!
//! # The contract
//!
//! Every command, without exception:
//!
//! 1. **writes locally first**, in one transaction, and commits — the UI
//!    repaints from SQLite before a single byte goes to Google;
//! 2. **then** performs the remote call;
//! 3. **reverts** the local write if that call fails, exactly, including the
//!    unread flag and any snooze row;
//! 4. **returns its inverse**, narrowed to the ids that actually changed, so
//!    undo is a value rather than a branch in the UI.
//!
//! Step 1 before step 2 is the speed thesis made concrete. Step 3 is what stops
//! the store from quietly disagreeing with Gmail: `users.history.list` only
//! reports changes that happened, so a local write Google never accepted would
//! be invisible to incremental sync and would survive until a full resync.
//! There is no durable outbox yet, so a failure is always reverted — even a
//! retriable one, whose retry budget has already been spent inside
//! [`RetryPolicy`](crate::google::RetryPolicy). [`CommandFailure::retriable`]
//! tells the caller it is worth dispatching the same command again.
//!
//! # Layout
//!
//! | module | what lives there |
//! |---|---|
//! | [`types`] | the [`Command`] enum and [`CommandResult`] |
//! | [`error`] | [`CommandError`] (did not run) and [`CommandFailure`] (ran, refused) |
//! | [`clients`] | how a command reaches the right account's API client |
//! | [`mail`] | the label-delta engine, batching, snooze |
//! | [`calendar`] | RSVP, and the event write path (create/update/delete/move) |
//! | [`catalogue`] | the self-describing schema |
//!
//! `mail`'s module docs carry the batching and snooze decisions in full.

pub mod calendar;
pub mod catalogue;
pub mod clients;
pub mod error;
pub mod mail;
pub mod types;

use std::sync::Arc;

pub use catalogue::{CommandSpec, ParamSpec, ParamType};
pub use clients::{AccountClients, GoogleClients};
pub use error::{CommandError, CommandFailure, FailureKind};
pub use types::{
    Command, CommandResult, EventDraft, EventPatch, EventScope, ThreadLabelState,
};

use crate::db::command_queries;
use crate::db::Db;

/// Executes commands against the local store and Google.
///
/// Cheap to clone the pieces of: `Db` is an `Arc` inside and the client factory
/// is behind an `Arc`, so this drops into `tauri::State` and can be shared with
/// the agent later without ceremony.
pub struct CommandDispatcher {
    pub(crate) db: Db,
    pub(crate) clients: Arc<dyn GoogleClients>,
    /// Gmail addresses the authorised account as `me`; kept configurable
    /// because the API takes an address here and tests are clearer when they
    /// can see which one.
    pub(crate) user_id: String,
    pub(crate) snooze_label_name: String,
    pub(crate) max_batch_message_ids: usize,
}

impl CommandDispatcher {
    /// Build a dispatcher, ensuring the command layer's own tables exist.
    pub fn new(db: Db, clients: Arc<dyn GoogleClients>) -> Result<Self, CommandError> {
        db.write(command_queries::ensure_command_schema)?;
        Ok(CommandDispatcher {
            db,
            clients,
            user_id: "me".to_string(),
            snooze_label_name: mail::DEFAULT_SNOOZE_LABEL.to_string(),
            max_batch_message_ids: mail::DEFAULT_MAX_BATCH_MESSAGE_IDS,
        })
    }

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = user_id.into();
        self
    }

    /// The Gmail label that marks a snoozed thread. Defaults to
    /// [`mail::DEFAULT_SNOOZE_LABEL`].
    pub fn with_snooze_label_name(mut self, name: impl Into<String>) -> Self {
        self.snooze_label_name = name.into();
        self
    }

    /// How many Gmail message ids may ride in one `batchModify`. Google's cap
    /// is 1000; lowering it is how the partial-failure behaviour is tested.
    pub fn with_max_batch_message_ids(mut self, max: usize) -> Self {
        self.max_batch_message_ids = max.max(1);
        self
    }

    /// Run a command.
    ///
    /// `Err` means the command never ran and nothing was written — an unknown
    /// id, an unconfigured account, a missing snooze label. A remote refusal is
    /// **not** an `Err`: it comes back as `Ok` with `ok: false` and a
    /// [`CommandFailure`] naming the ids that were rolled back, because the
    /// caller needs the per-id breakdown that a plain error cannot carry.
    pub async fn execute(&self, command: Command) -> Result<CommandResult, CommandError> {
        match command {
            Command::Rsvp { event_id, response } => {
                calendar::execute_rsvp(self, event_id, response).await
            }
            Command::CreateEvent {
                account_id,
                calendar_id,
                draft,
            } => calendar::execute_create(self, account_id, &calendar_id, &draft).await,
            Command::UpdateEvent {
                event_id,
                patch,
                scope,
            } => calendar::execute_update(self, event_id, &patch, scope).await,
            Command::DeleteEvent { event_id, scope } => {
                calendar::execute_delete(self, event_id, scope).await
            }
            Command::MoveEvent {
                event_id,
                account_id,
                calendar_id,
            } => calendar::execute_move(self, event_id, account_id, &calendar_id).await,
            other => mail::execute(self, &other).await,
        }
    }

    /// Run several commands in order, stopping at the first that could not run.
    ///
    /// The inverses come back in reverse order, which is the order they must be
    /// applied in to undo the whole batch.
    pub async fn execute_all(
        &self,
        commands: Vec<Command>,
    ) -> Result<Vec<CommandResult>, CommandError> {
        let mut out = Vec::with_capacity(commands.len());
        for command in commands {
            out.push(self.execute(command).await?);
        }
        Ok(out)
    }

    /// The undo stack for a run of results: every inverse, newest first.
    pub fn undo_stack(results: &[CommandResult]) -> Vec<Command> {
        results
            .iter()
            .rev()
            .filter_map(|r| r.undo.clone())
            .collect()
    }

    pub fn db(&self) -> &Db {
        &self.db
    }
}
