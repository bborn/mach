//! Where plugins live on disk, and what the app remembers about them.
//!
//! ```text
//!   <data dir>/plugins/
//!     state.json              what was approved, and when
//!     quick-file/
//!       mach-plugin.json
//!       main.js
//! ```
//!
//! # The approval record is content-addressed
//!
//! `state.json` stores the SHA-256 of the manifest and of `main.js` as they
//! were when the user said yes. Two things fall out of that, both bought with
//! someone else's incident:
//!
//! - **A changed hash with an unchanged version number is reported as
//!   suspicious.** VS Code's `ahban.shiba` passed review and shipped ransomware
//!   months later in an update; review at submission does not survive updates.
//! - **A capability diff stops the update.** An update that adds
//!   `read: ["threads"]` to a plugin that had `read: ["labels"]` needs a fresh
//!   yes, in English, showing the difference.
//!
//! Neither detects a malicious update on its own. What actually carries the
//! weight is that a tier-1 plugin that turns malicious overnight still has no
//! network, no tokens and no DOM: the design goal is not "detect the bad
//! update", it is "make the bad update boring".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{self, InstallKind, ManifestError, PluginManifest};

pub const MANIFEST_FILE: &str = "mach-plugin.json";
const STATE_FILE: &str = "state.json";

/// Bounded because it is read into memory, imported as a blob and parsed on
/// every activation. A plugin is one pre-bundled file, not an application.
pub const MAX_MAIN_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Manifest(#[from] ManifestError),
    #[error("there is no plugin called {0}")]
    Unknown(String),
    #[error("{id} is already installed at version {version}; use update instead")]
    AlreadyInstalled { id: String, version: String },
    #[error("{0} does not look like a plugin directory — it has no {MANIFEST_FILE}")]
    NotAPlugin(String),
    #[error("{id}'s {file} is {size} bytes, and the limit is {limit}")]
    TooBig {
        id: String,
        file: String,
        size: usize,
        limit: usize,
    },
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        StoreError::Io(error.to_string())
    }
}

/// What the user agreed to, and against which bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRecord {
    pub version: String,
    pub manifest_sha256: String,
    pub main_sha256: String,
    pub approved_at: i64,
    pub install: InstallKind,
    /// Where it came from: a git URL, or an absolute path for a dev install.
    #[serde(default)]
    pub source: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The manifest **as approved**, kept whole rather than hashed.
    ///
    /// A hash answers "did this change"; the capability diff has to answer
    /// "did this ask for more", and that needs the old document. It is a few
    /// kilobytes, and it is the difference between stopping an update and
    /// merely noticing one.
    pub approved_manifest: PluginManifest,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    #[serde(default)]
    plugins: BTreeMap<String, ApprovalRecord>,
}

/// Why a plugin is not running, when it is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state", content = "detail")]
pub enum PluginStatus {
    Ready,
    Disabled,
    /// `--safe-mode`. Nothing is uninstalled; nothing runs.
    SafeMode,
    /// The manifest no longer parses, or asks for something that does not exist.
    Invalid(String),
    /// The bytes changed without the version changing.
    ChangedWithoutVersionBump,
    /// The capability set grew since it was approved.
    NeedsReapproval(Vec<String>),
}

/// One installed plugin, as the UI and the agent see it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub id: String,
    pub manifest: PluginManifest,
    pub status: PluginStatus,
    pub approval: ApprovalRecord,
    pub directory: String,
}

impl InstalledPlugin {
    pub fn is_runnable(&self) -> bool {
        self.status == PluginStatus::Ready
    }
}

/// The plugin directory, plus the one flag that turns everything off.
pub struct PluginStore {
    root: PathBuf,
    safe_mode: bool,
    now: fn() -> i64,
}

impl PluginStore {
    pub fn new(data_dir: &Path, safe_mode: bool) -> Self {
        PluginStore {
            root: data_dir.join("plugins"),
            safe_mode,
            now: crate::ipc::compose::now_ms,
        }
    }

    pub fn with_clock(mut self, now: fn() -> i64) -> Self {
        self.now = now;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn safe_mode(&self) -> bool {
        self.safe_mode
    }

    /// Read a candidate directory without installing it — what the install
    /// prompt is built from. Nothing is executed and nothing is copied.
    pub fn inspect(
        &self,
        directory: &Path,
        install: InstallKind,
        known_commands: &[&str],
    ) -> Result<(PluginManifest, String, String), StoreError> {
        let manifest_path = directory.join(MANIFEST_FILE);
        if !manifest_path.exists() {
            return Err(StoreError::NotAPlugin(directory.display().to_string()));
        }
        let source = std::fs::read_to_string(&manifest_path)?;
        let manifest = manifest::parse(&source, install, known_commands)?;

        let main_path = directory.join(&manifest.main);
        let main = std::fs::read_to_string(&main_path).map_err(|e| {
            StoreError::Io(format!(
                "{} names main {:?}, and it could not be read: {e}",
                manifest.id, manifest.main
            ))
        })?;
        if main.len() > MAX_MAIN_BYTES {
            return Err(StoreError::TooBig {
                id: manifest.id.clone(),
                file: manifest.main.clone(),
                size: main.len(),
                limit: MAX_MAIN_BYTES,
            });
        }
        Ok((manifest, source, main))
    }

    /// Copy a directory in and record the approval. The caller has already
    /// shown the prompt and been told yes; this does not ask.
    pub fn install(
        &self,
        directory: &Path,
        install: InstallKind,
        known_commands: &[&str],
    ) -> Result<InstalledPlugin, StoreError> {
        let (manifest, manifest_source, main) = self.inspect(directory, install, known_commands)?;

        let mut state = self.read_state();
        if let Some(existing) = state.plugins.get(&manifest.id) {
            return Err(StoreError::AlreadyInstalled {
                id: manifest.id.clone(),
                version: existing.version.clone(),
            });
        }

        let target = self.root.join(&manifest.id);
        std::fs::create_dir_all(&target)?;
        std::fs::write(target.join(MANIFEST_FILE), &manifest_source)?;
        std::fs::write(target.join(&manifest.main), &main)?;

        let record = ApprovalRecord {
            version: manifest.version.clone(),
            manifest_sha256: digest(&manifest_source),
            main_sha256: digest(&main),
            approved_at: (self.now)(),
            install,
            source: directory.display().to_string(),
            enabled: true,
            approved_manifest: manifest.clone(),
        };
        state.plugins.insert(manifest.id.clone(), record.clone());
        self.write_state(&state)?;

        Ok(InstalledPlugin {
            id: manifest.id.clone(),
            status: if self.safe_mode {
                PluginStatus::SafeMode
            } else {
                PluginStatus::Ready
            },
            manifest,
            approval: record,
            directory: target.display().to_string(),
        })
    }

    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        let mut state = self.read_state();
        if state.plugins.remove(id).is_none() {
            return Err(StoreError::Unknown(id.to_string()));
        }
        let directory = self.root.join(id);
        if directory.exists() {
            std::fs::remove_dir_all(directory)?;
        }
        self.write_state(&state)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), StoreError> {
        let mut state = self.read_state();
        let record = state
            .plugins
            .get_mut(id)
            .ok_or_else(|| StoreError::Unknown(id.to_string()))?;
        record.enabled = enabled;
        self.write_state(&state)
    }

    /// Everything installed, whether or not it can run — a plugin that cannot
    /// run is exactly the thing the user needs to see.
    pub fn list(&self, known_commands: &[&str]) -> Vec<InstalledPlugin> {
        let state = self.read_state();
        state
            .plugins
            .iter()
            .filter_map(|(id, record)| self.load(id, record, known_commands))
            .collect()
    }

    pub fn get(&self, id: &str, known_commands: &[&str]) -> Option<InstalledPlugin> {
        let state = self.read_state();
        let record = state.plugins.get(id)?;
        self.load(id, record, known_commands)
    }

    /// The module source for one plugin. Read at activation, never cached here:
    /// a dev install is expected to change under the app's feet.
    pub fn read_main(&self, plugin: &InstalledPlugin) -> Result<String, StoreError> {
        let path = PathBuf::from(&plugin.directory).join(&plugin.manifest.main);
        Ok(std::fs::read_to_string(path)?)
    }

    fn load(
        &self,
        id: &str,
        record: &ApprovalRecord,
        known_commands: &[&str],
    ) -> Option<InstalledPlugin> {
        let directory = self.root.join(id);
        let manifest_source = std::fs::read_to_string(directory.join(MANIFEST_FILE)).ok()?;

        let manifest = match manifest::parse(&manifest_source, record.install, known_commands) {
            Ok(manifest) => manifest,
            Err(error) => {
                // Enough of a stub to be listed and removed. A plugin that
                // cannot be named cannot be uninstalled from the UI.
                return Some(InstalledPlugin {
                    id: id.to_string(),
                    manifest: stub_manifest(id, &record.version),
                    status: PluginStatus::Invalid(error.to_string()),
                    approval: record.clone(),
                    directory: directory.display().to_string(),
                });
            }
        };

        let main = std::fs::read_to_string(directory.join(&manifest.main)).unwrap_or_default();
        let status = self.status_of(record, &manifest, &manifest_source, &main);

        Some(InstalledPlugin {
            id: id.to_string(),
            manifest,
            status,
            approval: record.clone(),
            directory: directory.display().to_string(),
        })
    }

    fn status_of(
        &self,
        record: &ApprovalRecord,
        manifest: &PluginManifest,
        manifest_source: &str,
        main: &str,
    ) -> PluginStatus {
        if self.safe_mode {
            return PluginStatus::SafeMode;
        }
        if !record.enabled {
            return PluginStatus::Disabled;
        }
        // A dev install is expected to change on every save; that is what
        // installing from a path is for. Everything else is held to its bytes.
        if record.install == InstallKind::Development {
            return PluginStatus::Ready;
        }

        // Asking for more is the serious one, and it is checked first: an
        // update that both bumped its version *and* widened its grant must stop
        // on the grant, not pass because the version moved.
        let widened = capability_diff(&record.approved_manifest, manifest);
        if !widened.is_empty() {
            return PluginStatus::NeedsReapproval(widened);
        }

        let manifest_changed = digest(manifest_source) != record.manifest_sha256;
        let main_changed = digest(main) != record.main_sha256;
        if (manifest_changed || main_changed) && manifest.version == record.version {
            return PluginStatus::ChangedWithoutVersionBump;
        }
        PluginStatus::Ready
    }

    fn read_state(&self) -> State {
        std::fs::read_to_string(self.root.join(STATE_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_state(&self, state: &State) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.root)?;
        let body = serde_json::to_string_pretty(state)
            .map_err(|e| StoreError::Io(format!("could not write the plugin state: {e}")))?;
        std::fs::write(self.root.join(STATE_FILE), body)?;
        Ok(())
    }
}

/// The capability grants an update added, in English. Empty means the update is
/// within what was already approved.
///
/// Only *growth* stops an update. Narrowing is always allowed, because a plugin
/// asking for less is the outcome this whole system is trying to encourage.
pub fn capability_diff(old: &PluginManifest, new: &PluginManifest) -> Vec<String> {
    let mut out = Vec::new();
    let added = |label: &str, before: &[String], after: &[String]| -> Vec<String> {
        after
            .iter()
            .filter(|item| !before.contains(item))
            .map(|item| format!("{label}: {item}"))
            .collect()
    };
    out.extend(added(
        "reads",
        &old.capabilities.read,
        &new.capabilities.read,
    ));
    out.extend(added(
        "dispatches",
        &old.capabilities.commands,
        &new.capabilities.commands,
    ));
    out.extend(added("occupies", &old.capabilities.ui, &new.capabilities.ui));
    out.extend(added(
        "subscribes to",
        &old.capabilities.events,
        &new.capabilities.events,
    ));
    if new.capabilities.store && !old.capabilities.store {
        out.push("keeps its own data".to_string());
    }
    if new.runtime != old.runtime {
        out.push("runs outside the sandbox, with a network".to_string());
    }
    // An update that widens what the assistant may call is a real change, even
    // though the ceiling is still the command set.
    let old_agent: Vec<&str> = old.agent_actions().iter().map(|a| a.id.as_str()).collect();
    for action in new.agent_actions() {
        if !old_agent.contains(&action.id.as_str()) {
            out.push(format!("lets the assistant call: {}", action.title));
        }
    }
    out
}

pub fn digest(bytes: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes.as_bytes());
    hex::encode(hasher.finalize())
}

fn stub_manifest(id: &str, version: &str) -> PluginManifest {
    PluginManifest {
        id: id.to_string(),
        name: id.to_string(),
        version: version.to_string(),
        mach_api: "1".to_string(),
        description: String::new(),
        author: String::new(),
        homepage: None,
        main: "main.js".to_string(),
        mach_api_proposed: Vec::new(),
        runtime: Default::default(),
        network_access: None,
        capabilities: Default::default(),
        contributes: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const COMMANDS: &[&str] = &["archive", "label", "trash"];

    fn scratch(name: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mach-plugin-store-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_plugin(dir: &Path, id: &str, version: &str, extra_caps: &str) -> PathBuf {
        let source = dir.join(id);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join(MANIFEST_FILE),
            format!(
                r#"{{"id":"{id}","name":"{id}","version":"{version}","machApi":"1",
                   "capabilities":{{"commands":["archive"]{extra_caps}}},
                   "contributes":{{"actions":[{{"id":"go","title":"Go"}}]}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(source.join("main.js"), "export const actions = {};").unwrap();
        source
    }

    #[test]
    fn installs_lists_and_removes() {
        let scratch = scratch("roundtrip");
        let store = PluginStore::new(&scratch.join("data"), false);
        let source = write_plugin(&scratch, "quick-file", "1.0.0", "");

        let installed = store
            .install(&source, InstallKind::Published, COMMANDS)
            .unwrap();
        assert_eq!(installed.status, PluginStatus::Ready);

        let list = store.list(COMMANDS);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "quick-file");
        assert!(store.read_main(&list[0]).unwrap().contains("actions"));

        store.set_enabled("quick-file", false).unwrap();
        assert_eq!(store.list(COMMANDS)[0].status, PluginStatus::Disabled);

        store.remove("quick-file").unwrap();
        assert!(store.list(COMMANDS).is_empty());
    }

    #[test]
    fn safe_mode_disables_everything_without_uninstalling_it() {
        let scratch = scratch("safe");
        let data = scratch.join("data");
        let source = write_plugin(&scratch, "quick-file", "1.0.0", "");
        PluginStore::new(&data, false)
            .install(&source, InstallKind::Published, COMMANDS)
            .unwrap();

        let safe = PluginStore::new(&data, true);
        assert_eq!(safe.list(COMMANDS)[0].status, PluginStatus::SafeMode);
        // And it is still installed: safe mode is not uninstall.
        assert_eq!(safe.list(COMMANDS).len(), 1);
    }

    /// The `ahban.shiba` property: same version, different bytes, reported.
    #[test]
    fn changed_bytes_under_the_same_version_are_suspicious() {
        let scratch = scratch("hash");
        let data = scratch.join("data");
        let source = write_plugin(&scratch, "quick-file", "1.0.0", "");
        let store = PluginStore::new(&data, false);
        store
            .install(&source, InstallKind::Published, COMMANDS)
            .unwrap();

        std::fs::write(
            data.join("plugins/quick-file/main.js"),
            "export const actions = { evil() {} };",
        )
        .unwrap();

        assert_eq!(
            store.list(COMMANDS)[0].status,
            PluginStatus::ChangedWithoutVersionBump
        );
    }

    #[test]
    fn a_development_install_is_expected_to_change() {
        let scratch = scratch("dev");
        let data = scratch.join("data");
        let source = write_plugin(&scratch, "quick-file", "1.0.0", "");
        let store = PluginStore::new(&data, false);
        store
            .install(&source, InstallKind::Development, COMMANDS)
            .unwrap();
        std::fs::write(data.join("plugins/quick-file/main.js"), "// edited").unwrap();
        assert_eq!(store.list(COMMANDS)[0].status, PluginStatus::Ready);
    }

    #[test]
    fn a_capability_diff_is_reported_in_english() {
        let scratch = scratch("diff");
        let old = manifest::parse(
            &std::fs::read_to_string(write_plugin(&scratch, "a", "1.0.0", "").join(MANIFEST_FILE))
                .unwrap(),
            InstallKind::Published,
            COMMANDS,
        )
        .unwrap();
        let new = manifest::parse(
            &std::fs::read_to_string(
                write_plugin(&scratch, "b", "1.1.0", ",\"read\":[\"threads\"]").join(MANIFEST_FILE),
            )
            .unwrap(),
            InstallKind::Published,
            COMMANDS,
        )
        .unwrap();

        let diff = capability_diff(&old, &new);
        assert_eq!(diff, vec!["reads: threads".to_string()]);
        // Narrowing is not a diff worth stopping for.
        assert!(capability_diff(&new, &old).is_empty());
    }

    #[test]
    fn a_directory_that_is_not_a_plugin_says_so() {
        let scratch = scratch("empty");
        let store = PluginStore::new(&scratch.join("data"), false);
        assert!(matches!(
            store.install(&scratch, InstallKind::Published, COMMANDS),
            Err(StoreError::NotAPlugin(_))
        ));
    }
}
