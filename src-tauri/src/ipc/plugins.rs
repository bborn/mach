//! Plugins: the invoke surface.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `plugin_sandbox` | — | the worker shim, the canary, and whether the sandbox is verified |
//! | `plugin_conformance` | `report` | records the verdict; plugins stay dead until it passes |
//! | `plugin_list` | — | everything installed, runnable or not |
//! | `plugin_inspect` | `path`, `dev?` | the manifest and the consent lines, without installing |
//! | `plugin_install` | `path`, `dev?` | installs a directory the user has already approved |
//! | `plugin_remove` / `plugin_set_enabled` | `id` | uninstall, or turn off without uninstalling |
//! | `plugin_source` | `id` | `main.js`, for the frontend to hand to the sandbox |
//! | `plugin_invoke_result` | `requestId`, … | the frontend answering the agent's bridge |
//!
//! Every handler is a wrapper, for the same reason the ones in
//! [`super::commands`] are: a `#[tauri::command]` can only really be driven by
//! standing up an application, so no decision lives inside one. The decisions
//! are in [`crate::plugins`], where `tests/plugins.rs` can reach them.
//!
//! # `plugin_source` hands out code, and that is fine
//!
//! It returns the plugin's own `main.js` as text so the frontend can pass it to
//! the guest, which imports it from a `blob:` URL. The alternative — serving it
//! over `plugin://` — would mean the guest fetching its own module, and
//! `connect-src 'none'` exists precisely to make sure the guest cannot fetch
//! anything. The module has to arrive by `postMessage`, so it has to come
//! through here first.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::plugins::manifest::{consent_lines, ConsentLine, InstallKind};
use crate::plugins::protocol;
use crate::plugins::runtime::{ConformanceReport, InvokeRequest, InvokeSink};
use crate::plugins::{InstalledPlugin, PluginManifest};

use super::error::IpcError;
use super::state::AppState;

/// The channel the agent's bridge asks the webview for work on.
pub const PLUGIN_INVOKE_EVENT: &str = "plugin-invoke";

/// Emits invoke requests to the webview.
pub struct TauriInvokeSink {
    pub app: AppHandle,
}

impl InvokeSink for TauriInvokeSink {
    fn request(&self, request: &InvokeRequest) {
        // A failed emit means the window is gone; the caller's timeout turns
        // that into a tool error the model can recover from.
        let _ = self.app.emit(PLUGIN_INVOKE_EVENT, request);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxAssets {
    /// The worker shim, as text. Handed to the guest over `postMessage`.
    pub worker_source: String,
    /// The conformance plugin, loaded through the same channel a plugin is.
    pub canary_source: String,
    pub csp: String,
    pub sandbox: String,
    pub verified: bool,
    pub safe_mode: bool,
    /// Known weaknesses of the boundary on platforms Mach does not ship on.
    pub platform_limits: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCandidate {
    pub manifest: PluginManifest,
    pub consent: Vec<ConsentLine>,
    /// The capability lines an *update* would add. Empty for a fresh install.
    pub added_capabilities: Vec<String>,
    pub already_installed: bool,
    pub install: InstallKind,
}

#[tauri::command]
pub fn plugin_sandbox(state: State<'_, AppState>) -> SandboxAssets {
    SandboxAssets {
        worker_source: protocol::WORKER_JS.to_string(),
        canary_source: protocol::CANARY_JS.to_string(),
        csp: protocol::GUEST_CSP.to_string(),
        sandbox: protocol::GUEST_SANDBOX.to_string(),
        verified: state.plugins.sandbox_verified(),
        safe_mode: state.plugins.store().safe_mode(),
        platform_limits: protocol::PLATFORM_LIMITS
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

#[tauri::command]
pub fn plugin_conformance(
    state: State<'_, AppState>,
    report: ConformanceReport,
) -> Result<(), IpcError> {
    // Written to the data directory as well as held in memory: the probe is
    // evidence, and evidence that only exists inside a running window cannot be
    // read after the window is gone. `scripts/qa` and CI both read this file.
    let path = state.plugins.store().root().join("conformance.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string_pretty(&report) {
        let _ = std::fs::write(&path, body);
    }

    if report.ok {
        println!(
            "[plugins] sandbox verified: {} escape attempts, all blocked (guest origin {})",
            report.rows.len(),
            report.guest_origin
        );
    } else {
        eprintln!(
            "[plugins] SANDBOX CONFORMANCE FAILED — plugins will not load. Not blocked: {:?}{}",
            report.failures,
            report
                .error
                .as_ref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        );
    }
    state.plugins.record_conformance(report);
    Ok(())
}

#[tauri::command]
pub fn plugin_list(state: State<'_, AppState>) -> Vec<InstalledPlugin> {
    state.plugins.installed()
}

#[tauri::command]
pub fn plugin_inspect(
    state: State<'_, AppState>,
    path: String,
    dev: Option<bool>,
) -> Result<InstallCandidate, IpcError> {
    let install = install_kind(dev);
    let commands = state.plugins.command_kinds();
    let (manifest, _, _) = state
        .plugins
        .store()
        .inspect(&PathBuf::from(&path), install, &commands)
        .map_err(plugin_error)?;

    let existing = state.plugins.store().get(&manifest.id, &commands);
    let added = existing
        .as_ref()
        .map(|p| crate::plugins::store::capability_diff(&p.approval.approved_manifest, &manifest))
        .unwrap_or_default();

    Ok(InstallCandidate {
        consent: consent_lines(&manifest),
        added_capabilities: added,
        already_installed: existing.is_some(),
        install,
        manifest,
    })
}

#[tauri::command]
pub fn plugin_install(
    state: State<'_, AppState>,
    path: String,
    dev: Option<bool>,
) -> Result<InstalledPlugin, IpcError> {
    let install = install_kind(dev);
    let commands = state.plugins.command_kinds();
    state
        .plugins
        .store()
        .install(&PathBuf::from(&path), install, &commands)
        .map_err(plugin_error)
}

#[tauri::command]
pub fn plugin_remove(state: State<'_, AppState>, id: String) -> Result<(), IpcError> {
    state.plugins.store().remove(&id).map_err(plugin_error)
}

#[tauri::command]
pub fn plugin_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), IpcError> {
    state
        .plugins
        .store()
        .set_enabled(&id, enabled)
        .map_err(plugin_error)
}

#[tauri::command]
pub fn plugin_source(state: State<'_, AppState>, id: String) -> Result<String, IpcError> {
    let commands = state.plugins.command_kinds();
    let plugin = state
        .plugins
        .store()
        .get(&id, &commands)
        .ok_or_else(|| invalid(format!("there is no plugin called {id}")))?;
    if !plugin.is_runnable() {
        return Err(invalid(format!(
            "{id} is installed but not runnable: {}",
            describe_status(&plugin)
        )));
    }
    state
        .plugins
        .store()
        .read_main(&plugin)
        .map_err(plugin_error)
}

#[tauri::command]
pub fn plugin_invoke_result(
    state: State<'_, AppState>,
    request_id: u64,
    ok: bool,
    value: Option<Value>,
    error: Option<String>,
) {
    let result = if ok {
        Ok(value.unwrap_or(Value::Null))
    } else {
        Err(error.unwrap_or_else(|| "the plugin failed".to_string()))
    };
    state.plugins.resolve(request_id, result);
}

// ---------------------------------------------------------------------------

fn install_kind(dev: Option<bool>) -> InstallKind {
    if dev.unwrap_or(false) {
        InstallKind::Development
    } else {
        InstallKind::Published
    }
}

fn describe_status(plugin: &InstalledPlugin) -> String {
    use crate::plugins::PluginStatus;
    match &plugin.status {
        PluginStatus::Ready => "ready".to_string(),
        PluginStatus::Disabled => "it is disabled".to_string(),
        PluginStatus::SafeMode => "Mach is in safe mode".to_string(),
        PluginStatus::Invalid(detail) => format!("its manifest is invalid — {detail}"),
        PluginStatus::ChangedWithoutVersionBump => {
            "its files changed without its version changing, which needs a fresh look".to_string()
        }
        PluginStatus::NeedsReapproval(added) => {
            format!("it now asks for more than you approved: {}", added.join(", "))
        }
    }
}

fn plugin_error(error: crate::plugins::StoreError) -> IpcError {
    invalid(error.to_string())
}

fn invalid(message: impl Into<String>) -> IpcError {
    IpcError::Command(crate::commands::CommandError::Invalid {
        message: message.into(),
    })
}

/// Build the runtime at boot. Separate so `bootstrap` stays a list of joins.
pub fn runtime(data_dir: &std::path::Path, safe_mode: bool) -> Arc<crate::plugins::PluginRuntime> {
    let store = Arc::new(crate::plugins::PluginStore::new(data_dir, safe_mode));
    let commands = crate::commands::Command::catalogue()
        .iter()
        .map(|spec| spec.kind.to_string())
        .collect();
    Arc::new(crate::plugins::PluginRuntime::new(store, commands))
}
