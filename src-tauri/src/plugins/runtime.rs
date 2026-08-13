//! The runtime: what is installed, whether the sandbox is trustworthy, and the
//! bridge that lets the agent call an action that runs in the webview.
//!
//! # Why there is a bridge at all
//!
//! Plugin code runs in an iframe, which only the frontend can address; the agent
//! runs in Rust. So a plugin tool call travels: model → session → this bridge →
//! a Tauri event → the plugin host in the webview → the sandbox → back. The
//! alternative — a second plugin runtime in Rust — is the thing §2 spent a page
//! rejecting, and it would mean every host API written twice.
//!
//! The bridge is a request table and a timeout, and nothing else. It is
//! deliberately not a queue: a plugin tool call the frontend cannot answer
//! within [`INVOKE_TIMEOUT_MS`] is a failed tool call the model can recover
//! from, not a backlog to work through later.
//!
//! # The conformance gate
//!
//! [`PluginRuntime::runnable`] returns nothing at all until the frontend has
//! reported a *passing* conformance run. Not "assume it passed and check
//! later": the whole security argument is a claim about the WebView, and an
//! unverified claim is not a boundary. That also means the agent is never
//! offered a plugin tool it could not actually run.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use super::manifest::PluginManifest;
use super::store::{InstalledPlugin, PluginStore};

/// How long the agent waits for the webview to run a plugin action.
///
/// Longer than the sandbox's own per-call timeout, because the frontend may be
/// asking the *user* something — `mach.ask.pick` is a host-rendered dialog, and
/// a person is slower than a worker.
pub const INVOKE_TIMEOUT_MS: u64 = 120_000;

/// What the frontend is asked to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeRequest {
    pub request_id: u64,
    pub plugin_id: String,
    pub action: String,
    pub input: Value,
    /// Who asked. Today always `agent`; the palette and the keymap call the
    /// frontend host directly and never come through here.
    pub source: String,
}

/// Where an invoke request goes. Tauri in production, a channel in the tests.
pub trait InvokeSink: Send + Sync {
    fn request(&self, request: &InvokeRequest);
}

/// The verdict from the last conformance run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceReport {
    pub ok: bool,
    pub at: i64,
    #[serde(default)]
    pub app_origin: String,
    #[serde(default)]
    pub guest_origin: String,
    #[serde(default)]
    pub rows: Vec<ConformanceRow>,
    /// The positive control: the host fetching what the guest was refused. A
    /// run whose control failed proves nothing and is not a pass.
    #[serde(default)]
    pub control: Option<ConformanceControl>,
    #[serde(default)]
    pub failures: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl ConformanceReport {
    /// Does the evidence in this report actually support a pass?
    ///
    /// `ok` arrives as a boolean the frontend computed. That is fine as far as
    /// it goes — the frontend is not the adversary here, a plugin is, and a
    /// plugin has no way to reach IPC — but it means the gate that decides
    /// whether untrusted code runs is a claim rather than a derivation, and the
    /// rest of the report is the evidence for it sitting right there unread.
    ///
    /// So this re-derives the verdict from the rows, in Rust, using the same
    /// three rules the probe describes:
    ///
    /// 1. **No attempt succeeded.** An `allowed` row is an escape that worked.
    /// 2. **Something was attempted.** An empty run proves nothing, and "no
    ///    rows" is what a probe that crashed early looks like.
    /// 3. **The control succeeded.** Every check is a negative, so an unplugged
    ///    network passes all of them. Without the positive control a verified
    ///    sandbox and a dead network are the same report.
    ///
    /// Rule 3 is the one worth having on this side. A report can arrive with
    /// `ok: true` and `control: { succeeded: false }` — the frontend would have
    /// caught that, but the field that decides is the one Rust reads.
    pub fn evidence_supports_a_pass(&self) -> bool {
        let nothing_escaped = self.rows.iter().all(|row| !row.allowed);
        let something_was_tried = !self.rows.is_empty();
        let control_held = self.control.as_ref().is_some_and(|c| c.succeeded);
        nothing_escaped && something_was_tried && control_held && self.failures.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceControl {
    pub name: String,
    pub succeeded: bool,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceRow {
    pub scope: String,
    pub name: String,
    pub allowed: bool,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("{0}")]
    Refused(String),
    #[error("the plugin host did not answer within {INVOKE_TIMEOUT_MS}ms")]
    TimedOut,
    #[error("there is no window to run plugins in")]
    NoHost,
}

/// Everything the app knows about plugins, in one place.
pub struct PluginRuntime {
    store: Arc<PluginStore>,
    conformance: Mutex<Option<ConformanceReport>>,
    sink: Mutex<Option<Arc<dyn InvokeSink>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next: AtomicU64,
    /// Command dispatch times, per plugin, for the rate limit.
    rate: Mutex<HashMap<String, Vec<i64>>>,
    /// The command catalogue's `kind` strings, so the store can validate a
    /// manifest without depending on the command layer.
    commands: Vec<String>,
}

impl PluginRuntime {
    pub fn new(store: Arc<PluginStore>, commands: Vec<String>) -> Self {
        PluginRuntime {
            store,
            conformance: Mutex::new(None),
            sink: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            rate: Mutex::new(HashMap::new()),
            commands,
        }
    }

    pub fn store(&self) -> &Arc<PluginStore> {
        &self.store
    }

    pub fn command_kinds(&self) -> Vec<&str> {
        self.commands.iter().map(String::as_str).collect()
    }

    pub fn set_sink(&self, sink: Arc<dyn InvokeSink>) {
        *lock(&self.sink) = Some(sink);
    }

    pub fn record_conformance(&self, report: ConformanceReport) {
        *lock(&self.conformance) = Some(report);
    }

    pub fn conformance(&self) -> Option<ConformanceReport> {
        lock(&self.conformance).clone()
    }

    /// Whether the sandbox has been shown to hold, in this window, this boot.
    pub fn sandbox_verified(&self) -> bool {
        lock(&self.conformance).as_ref().map(|r| r.ok).unwrap_or(false)
    }

    /// Everything installed, runnable or not — what the plugin list renders.
    pub fn installed(&self) -> Vec<InstalledPlugin> {
        self.store.list(&self.command_kinds())
    }

    /// The plugins that may actually do anything. Empty until the sandbox is
    /// verified, and empty in safe mode.
    pub fn runnable(&self) -> Vec<InstalledPlugin> {
        if !self.sandbox_verified() {
            return Vec::new();
        }
        self.installed().into_iter().filter(|p| p.is_runnable()).collect()
    }

    pub fn manifest(&self, id: &str) -> Option<PluginManifest> {
        self.store.get(id, &self.command_kinds()).map(|p| p.manifest)
    }

    /// Ask the webview to run one action, and wait for the answer.
    pub async fn invoke(
        &self,
        plugin_id: &str,
        action: &str,
        input: Value,
        source: &str,
    ) -> Result<Value, BridgeError> {
        let plugin = self
            .runnable()
            .into_iter()
            .find(|p| p.id == plugin_id)
            .ok_or_else(|| {
                BridgeError::Refused(format!(
                    "{plugin_id} is not installed, not enabled, or the plugin sandbox has not \
                     been verified in this window"
                ))
            })?;
        if !plugin.manifest.contributes.actions.iter().any(|a| a.id == action) {
            return Err(BridgeError::Refused(format!(
                "{plugin_id} has no action called {action:?}"
            )));
        }

        let sink = lock(&self.sink).clone().ok_or(BridgeError::NoHost)?;
        let request_id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(request_id, tx);

        sink.request(&InvokeRequest {
            request_id,
            plugin_id: plugin_id.to_string(),
            action: action.to_string(),
            input,
            source: source.to_string(),
        });

        let answer = tokio::time::timeout(
            std::time::Duration::from_millis(INVOKE_TIMEOUT_MS),
            rx,
        )
        .await;

        match answer {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(BridgeError::Refused(message)),
            // The sender was dropped: the window went away mid-call.
            Ok(Err(_)) => Err(BridgeError::NoHost),
            Err(_) => {
                lock(&self.pending).remove(&request_id);
                Err(BridgeError::TimedOut)
            }
        }
    }

    /// The frontend answering an [`InvokeRequest`]. Unknown ids are dropped —
    /// they are a call that already timed out, not an error worth surfacing.
    pub fn resolve(&self, request_id: u64, result: Result<Value, String>) {
        if let Some(tx) = lock(&self.pending).remove(&request_id) {
            let _ = tx.send(result);
        }
    }

    /// The second capability check, on the side of the wall that matters.
    ///
    /// The frontend host refuses an undeclared command before it ever reaches
    /// IPC, and that is the check whose error message a plugin author reads.
    /// This one exists because the frontend is not the trust boundary: the
    /// command layer is. It costs one map lookup on a path that is about to do
    /// a network round trip anyway.
    ///
    /// Also where the rate limit lives, for the same reason — one choke point,
    /// per source.
    pub fn authorize_command(&self, plugin_id: &str, kind: &str, now: i64) -> Result<(), String> {
        let manifest = self.manifest(plugin_id).ok_or_else(|| {
            format!("{plugin_id} is not installed, so it cannot dispatch {kind}")
        })?;
        if !self.sandbox_verified() {
            return Err(format!(
                "the plugin sandbox has not been verified in this window, so {plugin_id} \
                 cannot dispatch anything"
            ));
        }
        if !manifest.capabilities.commands.iter().any(|c| c == kind) {
            return Err(format!("{plugin_id} did not declare commands: [\"{kind}\"]"));
        }
        self.spend(plugin_id, now)
    }

    /// A rolling window, per plugin. Generous enough that filing a whole
    /// mailbox is fine, small enough that a runaway loop stops being one.
    fn spend(&self, plugin_id: &str, now: i64) -> Result<(), String> {
        let mut buckets = lock(&self.rate);
        let bucket = buckets.entry(plugin_id.to_string()).or_default();
        bucket.retain(|at| now - *at < RATE_WINDOW_MS);
        if bucket.len() >= RATE_LIMIT {
            return Err(format!(
                "{plugin_id} has dispatched {RATE_LIMIT} commands in the last minute and is \
                 being rate limited"
            ));
        }
        bucket.push(now);
        Ok(())
    }
}

/// Commands one plugin may dispatch per [`RATE_WINDOW_MS`].
pub const RATE_LIMIT: usize = 120;
pub const RATE_WINDOW_MS: i64 = 60_000;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::manifest::InstallKind;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU32;

    struct Echo {
        runtime: Mutex<Option<Arc<PluginRuntime>>>,
    }

    impl InvokeSink for Echo {
        fn request(&self, request: &InvokeRequest) {
            let runtime = lock(&self.runtime).clone().unwrap();
            let id = request.request_id;
            let input = request.input.clone();
            tokio::spawn(async move { runtime.resolve(id, Ok(json!({ "echoed": input }))) });
        }
    }

    fn scratch(name: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mach-plugin-runtime-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn runtime_with_plugin(name: &str) -> Arc<PluginRuntime> {
        let scratch = scratch(name);
        let source = scratch.join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("mach-plugin.json"),
            r#"{"id":"quick-file","name":"Quick File","version":"1.0.0","machApi":"1",
               "capabilities":{"commands":["archive"]},
               "contributes":{"actions":[{"id":"file","title":"File"}]}}"#,
        )
        .unwrap();
        std::fs::write(source.join("main.js"), "export const actions = {};").unwrap();

        let store = Arc::new(PluginStore::new(&scratch.join("data"), false));
        store
            .install(&source, InstallKind::Published, &["archive"])
            .unwrap();
        Arc::new(PluginRuntime::new(store, vec!["archive".to_string()]))
    }

    fn passing() -> ConformanceReport {
        ConformanceReport {
            ok: true,
            at: 0,
            app_origin: "http://localhost:1420".into(),
            guest_origin: "plugin://quick-file".into(),
            rows: vec![],
            control: None,
            failures: vec![],
            error: None,
        }
    }

    /// The gate: no verified sandbox, no plugins, no plugin tools.
    #[tokio::test]
    async fn nothing_runs_until_the_sandbox_is_verified() {
        let runtime = runtime_with_plugin("gate");
        assert_eq!(runtime.installed().len(), 1);
        assert!(runtime.runnable().is_empty());

        runtime.record_conformance(ConformanceReport {
            ok: false,
            failures: vec!["worker: fetch (remote)".into()],
            ..passing()
        });
        assert!(runtime.runnable().is_empty(), "a failed probe must not let plugins run");

        runtime.record_conformance(passing());
        assert_eq!(runtime.runnable().len(), 1);
    }

    #[tokio::test]
    async fn an_action_the_manifest_does_not_contribute_is_refused() {
        let runtime = runtime_with_plugin("unknown-action");
        runtime.record_conformance(passing());
        runtime.set_sink(Arc::new(Echo {
            runtime: Mutex::new(Some(Arc::clone(&runtime))),
        }));

        let error = runtime
            .invoke("quick-file", "exfiltrate", json!({}), "agent")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no action called"));
    }

    #[tokio::test]
    async fn a_round_trip_reaches_the_frontend_and_comes_back() {
        let runtime = runtime_with_plugin("roundtrip");
        runtime.record_conformance(passing());
        runtime.set_sink(Arc::new(Echo {
            runtime: Mutex::new(Some(Arc::clone(&runtime))),
        }));

        let value = runtime
            .invoke("quick-file", "file", json!({ "n": 1 }), "agent")
            .await
            .unwrap();
        assert_eq!(value, json!({ "echoed": { "n": 1 } }));
    }

    /// Belt and braces: even if the frontend check were bypassed, the command
    /// layer refuses a kind the manifest did not name.
    #[test]
    fn an_undeclared_command_is_refused_on_the_rust_side_too() {
        let runtime = runtime_with_plugin("authorize");
        runtime.record_conformance(passing());

        assert!(runtime.authorize_command("quick-file", "archive", 0).is_ok());
        let error = runtime
            .authorize_command("quick-file", "trash", 0)
            .unwrap_err();
        assert!(error.contains("did not declare commands: [\"trash\"]"));

        let error = runtime.authorize_command("never-heard-of-it", "archive", 0).unwrap_err();
        assert!(error.contains("not installed"));
    }

    #[test]
    fn an_unverified_sandbox_cannot_dispatch_anything() {
        let runtime = runtime_with_plugin("unverified");
        let error = runtime.authorize_command("quick-file", "archive", 0).unwrap_err();
        assert!(error.contains("not been verified"));
    }

    #[test]
    fn a_runaway_plugin_is_rate_limited() {
        let runtime = runtime_with_plugin("rate");
        runtime.record_conformance(passing());
        for _ in 0..RATE_LIMIT {
            runtime.authorize_command("quick-file", "archive", 1_000).unwrap();
        }
        let error = runtime
            .authorize_command("quick-file", "archive", 1_000)
            .unwrap_err();
        assert!(error.contains("rate limited"));
        // The window rolls: a minute later it is fine again.
        assert!(runtime
            .authorize_command("quick-file", "archive", 1_000 + RATE_WINDOW_MS)
            .is_ok());
    }

    #[tokio::test]
    async fn without_a_window_there_is_nowhere_to_run() {
        let runtime = runtime_with_plugin("nohost");
        runtime.record_conformance(passing());
        assert!(matches!(
            runtime.invoke("quick-file", "file", json!({}), "agent").await,
            Err(BridgeError::NoHost)
        ));
    }
}
