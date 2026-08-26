//! The Tauri IPC surface: the one place React and Rust meet.
//!
//! # The contract
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `list_accounts` | — | `Account[]` |
//! | `list_labels` | `accountId?` | `Label[]` |
//! | `list_contacts` | — | `Contact[]` |
//! | `list_threads` | `query: ThreadQuery` | `ThreadPage` |
//! | `get_thread` | `threadId` | `ThreadDetail` |
//! | `search_threads` | `query`, `limit?` | `ThreadPage` |
//! | `list_calendars` | — | `Calendar[]` |
//! | `list_events` | `startMs`, `endMs` | `Event[]` |
//! | `execute_command` | `command: Command` | `CommandResult` |
//! | `command_catalogue` | — | `CommandSpec[]` |
//! | `sync_status` | — | `SyncStatus` |
//! | `sync_now` | `accountId?` | `ForcedPass` |
//! | `begin_add_account` | `email?` | `{ url, pendingId }` |
//! | `complete_add_account` | `pendingId` | `Account` |
//! | `remove_account` | `accountId` | `void` |
//! | `plugin_*` | see [`plugins`] | the plugin surface |
//! | `notification_*` | see [`notify`] | may we interrupt, and what to open |
//!
//! Everything is camelCase on the wire; every timestamp is unix milliseconds as
//! a number. Failures are [`IpcError`] — `{ kind, message }` — never a panic.
//!
//! # Layout
//!
//! | module | what lives there |
//! |---|---|
//! | [`commands`] | the `#[tauri::command]` handlers, and nothing else |
//! | [`reads`] | the read paths as plain functions over `&Db` |
//! | [`state`] | boot, the shared state, the add-account flow |
//! | [`events`] | the two push events |
//! | [`plugins`] | install, list, the sandbox assets, and the agent's bridge |
//! | [`types`] | the payload shapes |
//! | [`error`] | the one error that crosses the boundary |
//!
//! The split between `commands` and everything else is the load-bearing one: a
//! `#[tauri::command]` cannot be called without an application, so none of them
//! is allowed to hold a decision. `tests/ipc.rs` drives the plain functions.

pub mod agent;
pub mod attachments;
pub mod commands;
pub mod compose;
pub mod error;
pub mod feedback;
pub mod events;
pub mod handoff;
pub mod notify;
pub mod plugins;
pub mod prefs;
pub mod reads;
pub mod render;
pub mod state;
pub mod types;

pub use error::IpcError;
pub use state::{bootstrap, AppState};
pub use types::{
    Calendar, PendingAuthorizationHandle, SyncStatusPayload, ThreadDetail, ThreadPage, ThreadQuery,
};
