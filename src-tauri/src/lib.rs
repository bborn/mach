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
pub mod notify;
pub mod plugins;
pub mod render;
pub mod shell;
pub mod staleness;
pub mod sync;

use std::sync::Arc;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();
    // A QA instance builds its own window in `setup`, unfocused. The configured
    // one would exist before `setup` ran and would already have taken focus.
    shell::suppress_configured_window(&mut context);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        // The save panel for attachments. Registered here rather than lazily on
        // first use; nothing `dialog:` is granted to JavaScript, so the only
        // reachable operation is "save *this* attachment" via Rust.
        .plugin(tauri_plugin_dialog::init())
        // Banners and the Dock badge. Wraps `tauri-plugin-notification` rather
        // than being it: the wrapper is what holds the `AppHandle` the sync
        // loop needs, subscribes to `threads-changed` for the badge, and sees
        // the `Reopen` a notification click produces. See `notify::host`.
        .plugin(notify::init())
        // One origin per plugin: `plugin://<id>/`. The handler serves two
        // static files and, crucially, the Content-Security-Policy as a
        // *response header* — see `plugins::protocol`.
        .register_uri_scheme_protocol(plugins::SCHEME, |_ctx, request| {
            plugins::protocol::respond(&request)
        })
        .setup(|app| {
            // Before anything is shown: a QA instance must never take focus.
            shell::apply_qa_policy(app)?;

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

            // Credentials are checked *after* launch, on a thread that is not
            // this one. Reading the Keychain here would deadlock the window into
            // existence — see `ipc::state::restore_accounts_into` for the stack.
            let creds = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let state = creds.state::<ipc::state::AppState>();
                ipc::state::restore_accounts_into(&state);
                // Tell the UI, so an account needing attention says so without
                // waiting for the first sync pass to report it.
                ipc::events::emit_sync_status(&creds, &state.status_payload());
            });

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
            ipc::prefs::get_preferences,
            ipc::prefs::set_preference,
            ipc::notify::notification_state,
            ipc::notify::notification_request_permission,
            ipc::notify::notification_pending_open,
            ipc::feedback::submit_feedback,
            ipc::feedback::capture_window,
            ipc::render::render_message_body,
            ipc::render::open_external,
            ipc::attachments::attachment_open,
            ipc::attachments::attachment_save,
            ipc::attachments::attachment_inline_image,
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
        .menu(shell::build_menu)
        .on_menu_event(|app, event| shell::on_menu_event(app, event.id().as_ref()))
        // Closing hides the window rather than destroying it, so ⌘W stops
        // leaving a live process with no way back to it. See `shell`.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if shell::intercept_close(window) {
                    api.prevent_close();
                }
            }
        })
        .build(context)
        .expect("error while running tauri application")
        .run(|app, event| {
            // A Dock click on an app whose window is hidden. Without this the
            // window that ⌘W hid would be unreachable for the life of the
            // process.
            if let tauri::RunEvent::Reopen { .. } = event {
                shell::reopen(app);
            }
        });
}
