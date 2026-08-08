//! Mach — a fast Google-only mail and calendar client.
//!
//! The invariant that defines this codebase: the UI never waits on Google.
//! Everything renders from local SQLite; the network is a background sync loop
//! that writes into it. See `docs/superpowers/specs/`.
//!
//! # Boot order
//!
//! ```text
//!   app data dir ──► Db::open (migrations) ──► TokenManager (Keychain)
//!                                  │                    │
//!                                  ├── CommandDispatcher┤
//!                                  └── SyncEngine ──────┘
//!                                          │
//!            restore accounts ─────────────┤
//!                                          ▼
//!                                   start the loop, emit
//! ```
//!
//! Two things about that order are deliberate. The database path comes from
//! Tauri's path API rather than a constant, so the store lands where macOS
//! expects it and a sandboxed build keeps working. And the sync loop starts
//! *after* accounts are restored — starting it first would put a pass on the
//! wire for accounts whose credentials had not been checked yet, and a failed
//! pass would be the first thing the user sees.
//!
//! Missing Google credentials are not fatal: the app boots, the store opens, and
//! `sync_status()` reports `configured: false` with a sentence explaining what
//! to set. See [`config`].

pub mod auth;
pub mod commands;
pub mod config;
pub mod db;
pub mod google;
pub mod ipc;
pub mod plugins;
pub mod render;
pub mod sync;

use std::sync::Arc;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        // One origin per plugin: `plugin://<id>/`. The handler serves two
        // static files and, crucially, the Content-Security-Policy as a
        // *response header* — see `plugins::protocol`.
        .register_uri_scheme_protocol(plugins::SCHEME, |_ctx, request| {
            plugins::protocol::respond(&request)
        })
        .setup(|app| {
            // MACH_DATA_DIR gives an agent (or a second window) its own store,
            // so QA cannot mutate the mailbox someone is actually reading.
            // Unset — the normal case — resolves to the OS app-data dir.
            let data_dir = match std::env::var_os("MACH_DATA_DIR") {
                Some(dir) => std::path::PathBuf::from(dir),
                None => app.path().app_data_dir().map_err(|e| {
                    format!("could not resolve the application data directory: {e}")
                })?,
            };
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| format!("could not create {}: {e}", data_dir.display()))?;
            let app_config = config::AppConfig::load(config::database_path(&data_dir));

            // Inside `block_on` so the HTTP transport is constructed with a
            // Tokio context available, exactly as it will be used.
            let state = tauri::async_runtime::block_on(async { ipc::bootstrap(app_config) })
                .map_err(|e| e.to_string())?;

            let sync = Arc::clone(&state.sync);
            let start_sync = state.should_start_sync();

            // The agent's bridge needs somewhere to send invoke requests, and
            // that is the window. Wired after `bootstrap` because it needs the
            // handle, and before `manage` because the state is moved.
            state
                .plugins
                .set_sink(Arc::new(ipc::plugins::TauriInvokeSink {
                    app: app.handle().clone(),
                }));
            app.manage(state);

            // The bridge starts the loop and then forwards its progress for the
            // life of the app. It has to run after `manage`, because it reads
            // the state it is reporting on.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(ipc::events::run(handle, sync, start_sync));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::commands::list_accounts,
            ipc::commands::list_labels,
            ipc::commands::list_threads,
            ipc::commands::get_thread,
            ipc::commands::search_threads,
            ipc::commands::list_calendars,
            ipc::commands::list_events,
            ipc::commands::execute_command,
            ipc::commands::command_catalogue,
            ipc::commands::sync_status,
            ipc::commands::sync_now,
            ipc::commands::begin_add_account,
            ipc::commands::complete_add_account,
            ipc::commands::remove_account,
            ipc::feedback::submit_feedback,
            ipc::feedback::capture_window,
            ipc::render::render_message_body,
            ipc::render::open_external,
            ipc::compose::send_message,
            ipc::agent::agent_start,
            ipc::agent::agent_sessions,
            ipc::agent::agent_send,
            ipc::plugins::plugin_sandbox,
            ipc::plugins::plugin_conformance,
            ipc::plugins::plugin_list,
            ipc::plugins::plugin_inspect,
            ipc::plugins::plugin_install,
            ipc::plugins::plugin_remove,
            ipc::plugins::plugin_set_enabled,
            ipc::plugins::plugin_source,
            ipc::plugins::plugin_invoke_result,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
