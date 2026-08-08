//! `mach-plugin.json` — the schema, and what makes one invalid.
//!
//! The manifest has two halves and the split is the whole design.
//!
//! **`capabilities`** is what the plugin asks the *user* to grant. It is
//! enforced at runtime, not merely advertised: dispatching a command kind the
//! manifest did not name is refused, every time, by name.
//!
//! **`contributes`** is what the plugin adds to the *app*. It is static, so the
//! host can populate ⌘K and the keymap without executing a line of plugin code
//! — VS Code's activation-events idea reduced to its useful core.
//!
//! # Two things this file is the only enforcer of
//!
//! **The `machApi` gate.** A manifest declares a major version and nothing
//! finer. The host refuses a major it does not implement, naming both, and says
//! so in the plugin list rather than crashing.
//!
//! **The proposed-API gate**, taken wholesale from VS Code because it is the
//! cheapest thing in the design that enforces stability rather than promising
//! it: `machApiProposed` works only in a development install. A published
//! plugin that declares one is refused at install time. There is no deprecation
//! window inside the gate, and that is the point.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The `machApi` majors this host implements.
///
/// More than one may be live at once — Zed's version-gated dispatch, which is
/// what turns a major bump from a flag day into a migration with no deadline.
pub const SUPPORTED_API_MAJORS: &[&str] = &["1"];

/// Capabilities that exist but are not promised. Usable only from a dev install.
pub const PROPOSED_APIS: &[&str] = &["calendarOverlay", "messageAnnotation", "syncHook"];

/// Everything a plugin may ask to read. Anything else is a typo, and is
/// reported as one rather than silently granting nothing.
pub const READ_SCOPES: &[&str] = &[
    "threads.metadata",
    "threads",
    "calendar",
    "labels",
    "accounts",
];

/// Surfaces a plugin may occupy. `sidebar` and `row-badge` are v1.1; declaring
/// one parses but contributes nothing yet.
pub const UI_SURFACES: &[&str] = &["palette", "reading-pane", "sidebar", "row-badge"];

/// Events a plugin may subscribe to.
pub const EVENT_NAMES: &[&str] = &["mail:arrived", "thread:opened", "command:executed"];

/// Where an action is offered.
pub const ACTION_CONTEXTS: &[&str] = &["threads", "global", "calendar", "reading-pane"];

// ===========================================================================
// The document
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Major only: `"1"`.
    pub mach_api: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default = "default_main")]
    pub main: String,
    /// Unstable APIs, usable only from a development install.
    #[serde(default)]
    pub mach_api_proposed: Vec<String>,
    /// Tier 1 (`sandbox`) or tier 2 (`process`). Tier 2 parses and is refused
    /// at load with a sentence, so a tier-2 manifest is a *known* refusal
    /// rather than a schema error — see [`Runtime`].
    #[serde(default)]
    pub runtime: Runtime,
    /// Tier 2 only. Declared here so the install prompt can show it before the
    /// tier ships, and so a tier-1 plugin that declares one is refused.
    #[serde(default)]
    pub network_access: Option<NetworkAccess>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub contributes: Contributes,
}

fn default_main() -> String {
    "main.js".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// The iframe-plus-worker sandbox. No network, ever.
    #[default]
    Sandbox,
    /// A subprocess speaking JSON-RPC over stdio, with a declared host
    /// allowlist. Designed, not shipped: see `docs/plugins.md` §2 "Tier 2".
    Process,
}

/// Figma's control, which is the best-designed instance of this anywhere.
///
/// Specific hosts, optionally path-scoped; `["none"]` is legal and encouraged;
/// and a wildcard **cannot be declared without a written justification** that
/// the install prompt shows verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkAccess {
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub ui: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub store: bool,
    /// **Opt-out, not opt-in.** Owner's decision, 2026-08-08: "the agent should
    /// have access to everything". Absent means every contributed action is an
    /// agent tool; `false` means none; a list names the subset.
    ///
    /// The consequence is not optional and is enforced elsewhere: a plugin tool
    /// is attributed in the transcript, and it inherits the *strictest* approval
    /// policy of any command the plugin may dispatch. A plugin cannot widen its
    /// own authority by describing itself persuasively.
    #[serde(default)]
    pub agent: AgentGrant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentGrant {
    /// `true` (or absent) — every action. `false` — none.
    All(bool),
    /// Exactly these action ids.
    Only(Vec<String>),
}

impl Default for AgentGrant {
    fn default() -> Self {
        AgentGrant::All(true)
    }
}

impl AgentGrant {
    pub fn allows(&self, action_id: &str) -> bool {
        match self {
            AgentGrant::All(all) => *all,
            AgentGrant::Only(ids) => ids.iter().any(|id| id == action_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Contributes {
    #[serde(default)]
    pub actions: Vec<ActionContribution>,
    #[serde(default)]
    pub views: Vec<ViewContribution>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionContribution {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub keywords: Option<String>,
    /// A keybinding, in the app's own syntax: `"alt+f"`, `"shift+h"`, `"g i"`.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default = "default_context")]
    pub context: String,
    /// The sentence the *model* reads when this becomes a tool. Falls back to
    /// the title, which is usually worse and always safe.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub params: Vec<ActionParam>,
}

fn default_context() -> String {
    "threads".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Number,
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewContribution {
    pub id: String,
    /// `reading-pane` in v1. `sidebar` parses and renders nothing yet.
    pub surface: String,
}

// ===========================================================================
// Validation
// ===========================================================================

/// Where a plugin came from. The proposed-API gate turns on this and nothing
/// else, which is what makes it a build-time error rather than a guideline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallKind {
    /// `mach plugin install ./path` — the author's own working copy.
    Development,
    /// A git URL or a directory copied into the plugin folder. Published.
    Published,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "camelCase")]
pub enum ManifestError {
    #[error("mach-plugin.json is not valid JSON: {0}")]
    Malformed(String),
    #[error("{0}")]
    Invalid(String),
    #[error(
        "this plugin needs mach plugin API {wanted}, and this build of Mach implements {supported}"
    )]
    UnsupportedApi { wanted: String, supported: String },
    #[error(
        "{id} declares the proposed API {api:?}. Proposed APIs work only in a development \
         install (mach plugin install ./path) and are refused everywhere else — there is no \
         compatibility promise inside the gate, which is the point."
    )]
    ProposedApiInPublishedInstall { id: String, api: String },
    #[error("{id} declares the unknown proposed API {api:?}")]
    UnknownProposedApi { id: String, api: String },
    #[error(
        "{id} declares \"runtime\": \"process\" (tier 2). Subprocess plugins are designed but \
         not shipped: see docs/plugins.md §2."
    )]
    Tier2NotShipped { id: String },
    #[error(
        "{id} declares networkAccess with a wildcard but no reasoning. A wildcard host \
         allowlist needs a written justification, and the install prompt shows it verbatim."
    )]
    WildcardWithoutReasoning { id: String },
    #[error(
        "{id} runs in the sandbox and declares networkAccess. Sandboxed plugins have no \
         network at all — that is enforced by CSP, not by politeness. Remove it, or declare \
         \"runtime\": \"process\"."
    )]
    NetworkInSandbox { id: String },
}

/// Parse and validate. `known_commands` is the command catalogue's `kind`
/// strings — passing it in keeps this module from depending on the command
/// layer, and keeps "is `trash` a real command" a single source of truth.
pub fn parse(
    source: &str,
    install: InstallKind,
    known_commands: &[&str],
) -> Result<PluginManifest, ManifestError> {
    let manifest: PluginManifest =
        serde_json::from_str(source).map_err(|e| ManifestError::Malformed(e.to_string()))?;
    validate(&manifest, install, known_commands)?;
    Ok(manifest)
}

pub fn validate(
    manifest: &PluginManifest,
    install: InstallKind,
    known_commands: &[&str],
) -> Result<(), ManifestError> {
    let id = manifest.id.clone();

    if !is_plugin_id(&id) {
        return Err(ManifestError::Invalid(format!(
            "\"{id}\" is not a usable plugin id — use lowercase letters, digits and dashes, \
             because the id is also the origin the plugin runs on (plugin://{id}/)"
        )));
    }
    if manifest.name.trim().is_empty() {
        return Err(ManifestError::Invalid(format!("{id} has no name")));
    }
    if !is_version(&manifest.version) {
        return Err(ManifestError::Invalid(format!(
            "{id} has version {:?}, which is not a semver version like 1.0.0",
            manifest.version
        )));
    }
    if !SUPPORTED_API_MAJORS.contains(&manifest.mach_api.as_str()) {
        return Err(ManifestError::UnsupportedApi {
            wanted: manifest.mach_api.clone(),
            supported: SUPPORTED_API_MAJORS.join(", "),
        });
    }
    if manifest.main.contains("..") || manifest.main.starts_with('/') {
        return Err(ManifestError::Invalid(format!(
            "{id} points main at {:?}, which leaves its own directory",
            manifest.main
        )));
    }

    // The gate.
    for api in &manifest.mach_api_proposed {
        if !PROPOSED_APIS.contains(&api.as_str()) {
            return Err(ManifestError::UnknownProposedApi {
                id: id.clone(),
                api: api.clone(),
            });
        }
        if install != InstallKind::Development {
            return Err(ManifestError::ProposedApiInPublishedInstall {
                id: id.clone(),
                api: api.clone(),
            });
        }
    }

    // The network declaration is checked *before* the tier-2 refusal, so a
    // malformed allowlist is reported as malformed rather than hidden behind
    // "tier 2 is not shipped" — the author has to fix it either way, and will
    // fix it now rather than when the tier lands.
    if let Some(net) = &manifest.network_access {
        if net.allowed_domains.iter().any(|d| d == "*")
            && net
                .reasoning
                .as_deref()
                .map(|r| !r.trim().is_empty())
                != Some(true)
        {
            return Err(ManifestError::WildcardWithoutReasoning { id });
        }
        let asks_for_something = net
            .allowed_domains
            .iter()
            .any(|d| !d.trim().is_empty() && d != "none");
        if asks_for_something && manifest.runtime == Runtime::Sandbox {
            return Err(ManifestError::NetworkInSandbox { id });
        }
    }

    // Tier 2, designed and not shipped. The refusal names the tier so the
    // message is actionable rather than "unknown runtime".
    if manifest.runtime == Runtime::Process {
        return Err(ManifestError::Tier2NotShipped { id });
    }

    for scope in &manifest.capabilities.read {
        if !READ_SCOPES.contains(&scope.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "{id} asks to read {scope:?}, which is not a thing Mach can grant. \
                 Try one of: {}",
                READ_SCOPES.join(", ")
            )));
        }
    }
    for kind in &manifest.capabilities.commands {
        if !known_commands.contains(&kind.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "{id} asks to dispatch {kind:?}, which is not a command Mach has. \
                 Plugins compose the command layer; they cannot add to it."
            )));
        }
    }
    for surface in &manifest.capabilities.ui {
        if !UI_SURFACES.contains(&surface.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "{id} asks for the UI surface {surface:?}, which does not exist"
            )));
        }
    }
    for event in &manifest.capabilities.events {
        if !EVENT_NAMES.contains(&event.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "{id} subscribes to {event:?}, which is not an event Mach emits"
            )));
        }
    }

    let mut seen = BTreeSet::new();
    for action in &manifest.contributes.actions {
        if !is_action_id(&action.id) {
            return Err(ManifestError::Invalid(format!(
                "{id} contributes an action with the unusable id {:?}",
                action.id
            )));
        }
        if !seen.insert(action.id.clone()) {
            return Err(ManifestError::Invalid(format!(
                "{id} contributes two actions called {:?}",
                action.id
            )));
        }
        if action.title.trim().is_empty() {
            return Err(ManifestError::Invalid(format!(
                "{id}'s action {:?} has no title, so nothing could offer it",
                action.id
            )));
        }
        if !ACTION_CONTEXTS.contains(&action.context.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "{id}'s action {:?} declares context {:?}; expected one of {}",
                action.id,
                action.context,
                ACTION_CONTEXTS.join(", ")
            )));
        }
    }

    // An action the agent may call still has to be an action.
    if let AgentGrant::Only(ids) = &manifest.capabilities.agent {
        for granted in ids {
            if !seen.contains(granted) {
                return Err(ManifestError::Invalid(format!(
                    "{id} grants the agent an action called {granted:?}, which it does not \
                     contribute"
                )));
            }
        }
    }

    for view in &manifest.contributes.views {
        if !UI_SURFACES.contains(&view.surface.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "{id}'s view {:?} names the surface {:?}, which does not exist",
                view.id, view.surface
            )));
        }
    }

    Ok(())
}

/// Also the origin's host component, which is why it is this strict.
pub fn is_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

fn is_action_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split(['.', '-', '+']).collect();
    parts.len() >= 3
        && parts[..3]
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

impl PluginManifest {
    /// Which of this plugin's actions the agent may call.
    pub fn agent_actions(&self) -> Vec<&ActionContribution> {
        self.contributes
            .actions
            .iter()
            .filter(|a| self.capabilities.agent.allows(&a.id))
            .collect()
    }
}

// ===========================================================================
// The install prompt
// ===========================================================================

/// One line the user actually reads before saying yes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentLine {
    pub text: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Worth knowing.
    Note,
    /// A meaningfully bigger ask.
    Warning,
    /// The sandbox does not hold for this. Tier 2 only.
    Danger,
}

/// The install prompt, as sentences.
///
/// With tier 2 approved this is **the entire security control**, so it is
/// written as consequences rather than as API names — Chrome MV3's most
/// transferable habit — and the network justification is reproduced verbatim
/// rather than summarised, because summarising it is how consent becomes a
/// formality.
pub fn consent_lines(manifest: &PluginManifest) -> Vec<ConsentLine> {
    let mut lines = Vec::new();
    let note = |text: String| ConsentLine {
        text,
        severity: Severity::Note,
    };
    let warn = |text: String| ConsentLine {
        text,
        severity: Severity::Warning,
    };

    for scope in &manifest.capabilities.read {
        lines.push(match scope.as_str() {
            "threads" => warn(
                "Read your mail, including the text of messages.".to_string(),
            ),
            "threads.metadata" => note(
                "Read who your mail is from, its subjects and labels — but not the \
                 messages themselves."
                    .to_string(),
            ),
            "calendar" => note("Read your calendar: titles, times and who is invited.".to_string()),
            "labels" => note("See the names of your labels.".to_string()),
            "accounts" => note("See which Google accounts you have connected.".to_string()),
            other => note(format!("Read {other}.")),
        });
    }

    if !manifest.capabilities.commands.is_empty() {
        let verbs: Vec<String> = manifest
            .capabilities
            .commands
            .iter()
            .map(|k| command_phrase(k).to_string())
            .collect();
        lines.push(warn(format!(
            "Act on your mailbox: {}. Everything it does is undoable with ⌘Z and is \
             recorded against this plugin's name.",
            join_and(&verbs)
        )));
    }

    if manifest.capabilities.store {
        lines.push(note(
            "Keep a small amount of its own data, private to this plugin.".to_string(),
        ));
    }

    if manifest.capabilities.ui.iter().any(|s| s == "palette") {
        lines.push(note("Add entries to ⌘K and the keyboard.".to_string()));
    }
    if manifest.capabilities.ui.iter().any(|s| s == "reading-pane") {
        lines.push(note(
            "Show a line of its own above the conversation you are reading.".to_string(),
        ));
    }

    let agent_actions = manifest.agent_actions();
    if !agent_actions.is_empty() {
        lines.push(warn(format!(
            "Be used by the assistant. {} can be called in a sentence, and this plugin's \
             own description text becomes something the assistant reads — anything it can \
             do, the assistant can ask it to do. Actions that reach another human still \
             stop for your confirmation.",
            join_and(
                &agent_actions
                    .iter()
                    .map(|a| format!("\u{201c}{}\u{201d}", a.title))
                    .collect::<Vec<_>>()
            )
        )));
    }

    match manifest.runtime {
        Runtime::Sandbox => lines.push(note(
            "It cannot reach the network at all, cannot see the app, and never gets your \
             Google sign-in."
                .to_string(),
        )),
        Runtime::Process => {
            let hosts = manifest
                .network_access
                .as_ref()
                .map(|n| n.allowed_domains.clone())
                .unwrap_or_default();
            lines.push(ConsentLine {
                text: format!(
                    "This plugin runs outside Mach's sandbox. It can send anything it reads \
                     to {}. Only install it if you trust the author.",
                    if hosts.is_empty() {
                        "a server".to_string()
                    } else {
                        join_and(&hosts)
                    }
                ),
                severity: Severity::Danger,
            });
            if let Some(reasoning) = manifest
                .network_access
                .as_ref()
                .and_then(|n| n.reasoning.as_deref())
            {
                lines.push(ConsentLine {
                    // Verbatim, in the author's own words, quoted so it is
                    // visibly theirs rather than ours.
                    text: format!("The author's reason, in their words: \u{201c}{reasoning}\u{201d}"),
                    severity: Severity::Danger,
                });
            }
        }
    }

    lines
}

/// A command kind, as the thing it does to your mailbox.
fn command_phrase(kind: &str) -> &'static str {
    match kind {
        "archive" => "archive conversations",
        "unarchive" => "move conversations back to the inbox",
        "markRead" => "mark conversations read or unread",
        "star" => "star conversations",
        "label" => "add and remove labels",
        "trash" => "move conversations to the trash",
        "untrash" => "take conversations out of the trash",
        "snooze" => "snooze conversations",
        "unsnooze" => "wake snoozed conversations",
        "rsvp" => "reply to calendar invitations on your behalf",
        "createEvent" => "create calendar events, which emails the guests",
        "updateEvent" => "change calendar events, which emails the guests",
        "deleteEvent" => "cancel calendar events, which emails the guests",
        "moveEvent" => "move events between calendars",
        _ => "act on your mailbox",
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[&str] = &[
        "archive", "label", "trash", "snooze", "star", "markRead", "rsvp", "createEvent",
    ];

    fn quick_file() -> String {
        r#"{
          "id": "quick-file",
          "name": "Quick File",
          "version": "1.0.0",
          "machApi": "1",
          "description": "Pick a label, apply it, archive.",
          "author": "you <you@example.com>",
          "main": "main.js",
          "capabilities": {
            "read": ["labels"],
            "commands": ["label", "archive"],
            "ui": ["palette"],
            "store": true
          },
          "contributes": {
            "actions": [
              { "id": "file", "title": "File to label…", "keywords": "move folder sort",
                "key": "alt+f", "context": "threads" }
            ]
          }
        }"#
        .to_string()
    }

    #[test]
    fn parses_the_worked_example() {
        let manifest = parse(&quick_file(), InstallKind::Published, COMMANDS).unwrap();
        assert_eq!(manifest.id, "quick-file");
        assert_eq!(manifest.contributes.actions[0].key.as_deref(), Some("alt+f"));
        assert!(manifest.capabilities.store);
    }

    /// Owner's decision, 2026-08-08: the agent gets everything unless told not to.
    #[test]
    fn agent_access_is_opt_out() {
        let manifest = parse(&quick_file(), InstallKind::Published, COMMANDS).unwrap();
        assert_eq!(manifest.agent_actions().len(), 1);

        let opted_out = quick_file().replace("\"store\": true", "\"store\": true, \"agent\": false");
        let manifest = parse(&opted_out, InstallKind::Published, COMMANDS).unwrap();
        assert!(manifest.agent_actions().is_empty());
    }

    #[test]
    fn refuses_an_api_major_it_does_not_implement() {
        let source = quick_file().replace("\"machApi\": \"1\"", "\"machApi\": \"7\"");
        let error = parse(&source, InstallKind::Published, COMMANDS).unwrap_err();
        assert!(matches!(error, ManifestError::UnsupportedApi { .. }));
        // Both versions named, so the message is actionable.
        assert!(error.to_string().contains('7') && error.to_string().contains('1'));
    }

    #[test]
    fn a_proposed_api_works_only_in_a_development_install() {
        let source = quick_file().replace(
            "\"machApi\": \"1\"",
            "\"machApi\": \"1\", \"machApiProposed\": [\"calendarOverlay\"]",
        );
        assert!(parse(&source, InstallKind::Development, COMMANDS).is_ok());
        let error = parse(&source, InstallKind::Published, COMMANDS).unwrap_err();
        assert!(matches!(
            error,
            ManifestError::ProposedApiInPublishedInstall { .. }
        ));
    }

    #[test]
    fn an_invented_proposed_api_is_refused_even_in_development() {
        let source = quick_file().replace(
            "\"machApi\": \"1\"",
            "\"machApi\": \"1\", \"machApiProposed\": [\"rootShell\"]",
        );
        assert!(matches!(
            parse(&source, InstallKind::Development, COMMANDS).unwrap_err(),
            ManifestError::UnknownProposedApi { .. }
        ));
    }

    #[test]
    fn a_command_that_does_not_exist_cannot_be_asked_for() {
        let source = quick_file().replace("\"label\", \"archive\"", "\"label\", \"dropDatabase\"");
        let error = parse(&source, InstallKind::Published, COMMANDS).unwrap_err();
        assert!(error.to_string().contains("dropDatabase"));
    }

    #[test]
    fn a_sandboxed_plugin_cannot_declare_a_network() {
        let source = quick_file().replace(
            "\"machApi\": \"1\"",
            "\"machApi\": \"1\", \"networkAccess\": { \"allowedDomains\": [\"api.example.com\"] }",
        );
        assert!(matches!(
            parse(&source, InstallKind::Published, COMMANDS).unwrap_err(),
            ManifestError::NetworkInSandbox { .. }
        ));
    }

    #[test]
    fn a_wildcard_host_needs_a_written_justification() {
        let source = quick_file().replace(
            "\"machApi\": \"1\"",
            "\"machApi\": \"1\", \"runtime\": \"process\", \
             \"networkAccess\": { \"allowedDomains\": [\"*\"] }",
        );
        assert!(matches!(
            parse(&source, InstallKind::Published, COMMANDS).unwrap_err(),
            ManifestError::WildcardWithoutReasoning { .. }
        ));
    }

    #[test]
    fn tier_two_parses_and_is_refused_by_name() {
        let source = quick_file().replace(
            "\"machApi\": \"1\"",
            "\"machApi\": \"1\", \"runtime\": \"process\", \
             \"networkAccess\": { \"allowedDomains\": [\"api.example.com\"], \
             \"reasoning\": \"Looks up sender reputation.\" }",
        );
        let error = parse(&source, InstallKind::Published, COMMANDS).unwrap_err();
        assert!(matches!(error, ManifestError::Tier2NotShipped { .. }));
        assert!(error.to_string().contains("tier 2"));
    }

    #[test]
    fn the_id_has_to_be_usable_as_an_origin() {
        for bad in ["Quick File", "quick_file", "../evil", "", "-lead"] {
            let source = quick_file().replace("\"id\": \"quick-file\"", &format!("\"id\": {bad:?}"));
            assert!(
                parse(&source, InstallKind::Published, COMMANDS).is_err(),
                "{bad:?} was accepted as a plugin id"
            );
        }
    }

    #[test]
    fn an_unknown_field_is_a_typo_and_says_so() {
        let source = quick_file().replace("\"store\": true", "\"stoer\": true");
        assert!(matches!(
            parse(&source, InstallKind::Published, COMMANDS).unwrap_err(),
            ManifestError::Malformed(_)
        ));
    }

    #[test]
    fn the_prompt_says_what_reading_mail_means_in_english() {
        let source = quick_file().replace("\"read\": [\"labels\"]", "\"read\": [\"threads\"]");
        let manifest = parse(&source, InstallKind::Published, COMMANDS).unwrap();
        let lines = consent_lines(&manifest);
        let body = lines
            .iter()
            .find(|l| l.text.contains("text of messages"))
            .expect("the bigger ask has to be said out loud");
        assert_eq!(body.severity, Severity::Warning);
        // And the consequence of a command, not the command's name.
        assert!(lines.iter().any(|l| l.text.contains("add and remove labels")));
        assert!(lines.iter().any(|l| l.text.contains("cannot reach the network")));
    }

    #[test]
    fn the_prompt_reproduces_a_network_justification_verbatim() {
        let manifest = PluginManifest {
            id: "reputation".into(),
            name: "Reputation".into(),
            version: "1.0.0".into(),
            mach_api: "1".into(),
            description: String::new(),
            author: String::new(),
            homepage: None,
            main: "main.js".into(),
            mach_api_proposed: vec![],
            runtime: Runtime::Process,
            network_access: Some(NetworkAccess {
                allowed_domains: vec!["reputation.example.org/lookup".into()],
                reasoning: Some("Checks whether a sender has been seen before.".into()),
            }),
            capabilities: Capabilities::default(),
            contributes: Contributes::default(),
        };
        let lines = consent_lines(&manifest);
        assert!(lines.iter().any(|l| l.severity == Severity::Danger
            && l.text.contains("outside Mach's sandbox")
            && l.text.contains("reputation.example.org/lookup")));
        assert!(lines
            .iter()
            .any(|l| l.text.contains("Checks whether a sender has been seen before.")));
    }
}
