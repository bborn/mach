//! Step 0: run the plugin sandbox conformance probe in the **real WKWebView**.
//!
//! `docs/plugins.md` §2 rests on a claim about the WebView — that a `blob:`
//! worker inside an iframe on a custom-protocol origin inherits the document's
//! policy container, so `connect-src 'none'` actually holds. HTML §7.1.7
//! requires it; WebKit is not obliged to agree, and if it does not, the tier-1
//! design collapses and the answer is QuickJS-in-WASM. That is a different
//! build, so this runs first and nothing else starts until it passes.
//!
//! # Why this is a separate binary
//!
//! Someone is using this machine. Launching Mach to look at it takes the
//! keyboard away from them: a new Tauri window activates on macOS whether you
//! wanted it to or not. So this binary
//!
//!   * sets the activation policy to **Accessory** before anything is shown, so
//!     the process never becomes the active application and never appears in the
//!     Dock; and
//!   * creates its one window **hidden**, and never shows it.
//!
//! It is otherwise the real thing: the real protocol handler, the real guest,
//! the real worker, the real frontend, and the real `plugin_conformance` IPC
//! command writing the real report file.
//!
//! ```sh
//! MACH_DATA_DIR=.qa/plugin-probe/data cargo run --bin plugin_probe
//!
//! # …and, with a plugin directory, run worked example 1 through the real
//! # iframe afterwards:
//! MACH_PLUGIN_DEMO=$PWD/plugins/quick-file cargo run --bin plugin_probe
//! ```
//!
//! Exits 0 when every escape attempt was blocked, 1 otherwise, and prints the
//! report as JSON either way.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mach_lib::{config, ipc, plugins};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// How long the probe may take. The canary waits out several 1.5s network
/// timeouts in sequence, and a hidden window's timers can be throttled.
const DEADLINE: Duration = Duration::from_secs(90);

fn main() {
    let data_dir = match std::env::var_os("MACH_DATA_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::temp_dir().join("mach-plugin-probe"),
    };
    std::fs::create_dir_all(&data_dir).expect("could not create the probe data directory");
    let report_path = data_dir.join("plugins").join("conformance.json");
    let _ = std::fs::remove_file(&report_path);

    let app_config = config::AppConfig::load(config::database_path(&data_dir));

    let mut context = tauri::generate_context!();
    // The configured window would be created before `setup` runs, visible, and
    // would steal focus. This binary builds its own.
    for window in context.config_mut().app.windows.iter_mut() {
        window.create = false;
    }

    tauri::Builder::default()
        .register_uri_scheme_protocol(plugins::SCHEME, |_ctx, request| {
            let uri = request.uri().to_string();
            let response = plugins::protocol::respond(&request);
            eprintln!("[probe] plugin:// {uri} -> {}", response.status());
            response
        })
        // The probe is headless, so the only way to know the page is alive is
        // to say so. Without this, "no report" cannot be told apart from "the
        // hidden window never ran a line of JavaScript".
        .on_page_load(|webview, payload| {
            eprintln!("[probe] page {:?} {}", payload.event(), payload.url());
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                let _ = webview.eval(FORWARD_CONSOLE);
            }
        })
        .invoke_handler(tauri::generate_handler![
            probe_log,
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
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let state = ipc::bootstrap(app_config.clone()).map_err(|e| e.to_string())?;
            state
                .plugins
                .set_sink(Arc::new(ipc::plugins::TauriInvokeSink {
                    app: app.handle().clone(),
                }));
            app.manage(state);

            // Labelled "main" so the capability in `capabilities/default.json`
            // applies to it — the label is what capabilities are keyed on.
            WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("mach plugin conformance")
                .visible(false)
                .inner_size(900.0, 600.0)
                // The frontend renders the probe instead of the app when it
                // sees this. A flag rather than a query string, so the URL is
                // byte-for-byte the one the app itself loads.
                .initialization_script(&format!(
                    "globalThis.__MACH_PLUGIN_PROBE__ = true; globalThis.__MACH_PLUGIN_DEMO__ = {};",
                    match std::env::var("MACH_PLUGIN_DEMO") {
                        Ok(path) if !path.is_empty() => format!("{path:?}"),
                        _ => "false".to_string(),
                    }
                ))
                .build()?;

            let handle = app.handle().clone();
            std::thread::spawn(move || watch(&report_path, &handle));
            Ok(())
        })
        .run(context)
        .expect("the probe could not start");
}

/// A hidden window has no console anyone can read, so it forwards its own.
const FORWARD_CONSOLE: &str = r#"
(() => {
  const send = (m) => { try { window.__TAURI_INTERNALS__.invoke('probe_log', { message: String(m) }); } catch (e) {} };
  send('flag=' + String(globalThis.__MACH_PLUGIN_PROBE__) + ' ipc=' + typeof window.__TAURI_INTERNALS__);
  for (const level of ['log', 'warn', 'error']) {
    const original = console[level];
    console[level] = (...args) => { send(level + ': ' + args.map(String).join(' ')); original(...args); };
  }
  window.addEventListener('error', (e) => send('error: ' + e.message + ' @ ' + e.filename));
  window.addEventListener('unhandledrejection', (e) => send('rejection: ' + (e.reason?.message ?? e.reason)));
})();
"#;

#[tauri::command]
fn probe_log(message: String) {
    eprintln!("[page] {message}");
}

/// Wait for the frontend to write its verdict, print it, and stop.
fn watch(report_path: &std::path::Path, app: &tauri::AppHandle) {
    let started = Instant::now();
    while started.elapsed() < DEADLINE {
        if let Ok(body) = std::fs::read_to_string(report_path) {
            println!("{body}");
            let ok = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("ok").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            app.exit(if ok { 0 } else { 1 });
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    eprintln!(
        "the probe produced no report within {}s — the sandbox could not be stood up, which \
         counts as a failure",
        DEADLINE.as_secs()
    );
    app.exit(2);
}
