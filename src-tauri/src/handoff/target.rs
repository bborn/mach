//! A handoff target: four fields, a list of them, and where the list is kept.
//!
//! # Why this has its own key and its own commands
//!
//! Targets are stored in the same `preferences` table as everything else, under
//! [`TARGETS_KEY`], but they are read and written by [`load`] and [`save`]
//! rather than through `set_preference`. Two reasons, and the second is the real
//! one. The key has a dot in it, which `ipc::prefs::is_valid_key` refuses on
//! purpose — that validator exists so that the settings table cannot be used as
//! a blob store by anything that can reach the command. And the shape here is a
//! list of records with meaning, so the round trip has to validate; a preference
//! is a value, and this is closer to a small table.
//!
//! Nothing else in the app reads this key. `prefs.ts` builds its object from a
//! fixed list of names, so an extra row is invisible to it.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::Result as DbResult;

use super::template;
use super::HandoffError;

/// Where the list lives in the `preferences` table.
pub const TARGETS_KEY: &str = "mach.handoffTargets";

/// A list long enough that nobody will reach it, short enough that a runaway
/// write cannot fill the settings table.
pub const MAX_TARGETS: usize = 64;

/// The `run` template of the seeded first target.
///
/// Zero configuration should still do something useful, and this is the thing
/// he actually does: open a Claude session in a directory with a sentence to
/// work on.
pub const SEED_RUN: &str = "claude \"{{prompt}}\"";

/// What Mach does with the process once it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandoffMode {
    /// Launch it in his terminal and let go. A live session he takes over.
    Terminal,
    /// Run it, capture what it printed, show that once. For commands that
    /// return — `ty task create …` — not for watching work happen.
    Inline,
    /// Run it on a pty inside the window and talk to it there. The same live
    /// session `Terminal` gives, without leaving the app. See [`super::session`].
    Session,
}

impl HandoffMode {
    pub fn as_str(self) -> &'static str {
        match self {
            HandoffMode::Terminal => "terminal",
            HandoffMode::Inline => "inline",
            HandoffMode::Session => "session",
        }
    }
}

/// One place a handoff can go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffTarget {
    /// Stable across renames, because the palette's recall and the "has this
    /// ever run" record are both keyed on it.
    pub id: String,
    pub name: String,
    /// The working directory the command runs in.
    pub dir: String,
    /// The command line, with `{{placeholders}}`. See [`super::template`].
    pub run: String,
    pub mode: HandoffMode,
    /// When this target last launched something, unix ms.
    ///
    /// Not statistics — Mach does not track handoffs. It is the answer to one
    /// question: has this configuration ever actually run? A target that never
    /// has gets a confirmation the first time; see `ipc::handoff`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
}

impl HandoffTarget {
    /// The first target, named after a directory he picked.
    pub fn seed(dir: &str) -> HandoffTarget {
        HandoffTarget {
            id: new_id(),
            name: name_from_dir(dir),
            dir: dir.to_string(),
            run: SEED_RUN.to_string(),
            mode: HandoffMode::Terminal,
            last_run_at: None,
        }
    }

    /// Whether this target has ever launched anything.
    pub fn is_unproven(&self) -> bool {
        self.last_run_at.is_none()
    }
}

/// `~/Projects/offerlab` → `offerlab`. The last component that is one.
pub fn name_from_dir(dir: &str) -> String {
    let cleaned = dir.trim().trim_end_matches('/');
    let last = cleaned.rsplit('/').find(|part| !part.is_empty());
    match last {
        Some(part) if !part.is_empty() && part != "~" => part.to_string(),
        _ => "Handoff".to_string(),
    }
}

/// Ids are opaque and only have to be unique within one list.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("t{nanos:x}{n:x}")
}

/// Everything wrong with a target, as a sentence, or `Ok`.
///
/// Checked on save rather than only on launch, because a target that cannot
/// work should say so while he is looking at the field that is wrong. The one
/// thing deliberately *not* checked here is whether `dir` exists: directories
/// come and go, and refusing to save a target because a checkout is on the
/// other disk today would be worse than failing at launch, which says so
/// clearly and costs nothing.
pub fn validate(target: &HandoffTarget) -> Result<(), HandoffError> {
    if target.name.trim().is_empty() {
        return Err(HandoffError::BadTarget("give the target a name".into()));
    }
    if target.dir.trim().is_empty() {
        return Err(HandoffError::BadTarget(
            "give the target a directory to run in".into(),
        ));
    }

    let tokens = template::tokenize(&target.run)?;
    let program = tokens.first().map(String::as_str).unwrap_or_default();
    if program.is_empty() {
        return Err(HandoffError::BadTemplate(
            "there is no command to run — the run field is empty".into(),
        ));
    }
    // The terminal path hands argv to `env`, which reads a leading `NAME=value`
    // as an assignment rather than as the program. Refusing here means the two
    // modes can never disagree about what a template means.
    if program.contains('=') {
        return Err(HandoffError::BadTemplate(format!(
            "{program:?} is not a program name — a command cannot start with an assignment"
        )));
    }
    if program.contains("{{") {
        return Err(HandoffError::BadTemplate(
            "the program to run cannot itself be a placeholder".into(),
        ));
    }
    Ok(())
}

/// Validate a whole list, and give every target an id if it arrived without one.
pub fn normalize(mut targets: Vec<HandoffTarget>) -> Result<Vec<HandoffTarget>, HandoffError> {
    if targets.len() > MAX_TARGETS {
        return Err(HandoffError::BadTarget(format!(
            "that is {} targets; the limit is {MAX_TARGETS}",
            targets.len()
        )));
    }
    let mut seen: Vec<String> = Vec::with_capacity(targets.len());
    for target in &mut targets {
        target.name = target.name.trim().to_string();
        target.dir = target.dir.trim().to_string();
        target.run = target.run.trim().to_string();
        if target.id.trim().is_empty() || seen.contains(&target.id) {
            target.id = new_id();
        }
        seen.push(target.id.clone());
        validate(target)?;
    }
    Ok(targets)
}

// ===========================================================================
// The stored list
// ===========================================================================

/// The saved targets, or an empty list.
///
/// A row that will not parse is treated as absent rather than fatal, for the
/// same reason `ipc::prefs::all` skips one: losing the handoff list is a
/// nuisance, and taking the app's settings surface down with it is not.
pub fn load(conn: &Connection) -> DbResult<Vec<HandoffTarget>> {
    let stored = crate::ipc::prefs::get(conn, TARGETS_KEY)?;
    Ok(stored
        .and_then(|value| serde_json::from_value::<Vec<HandoffTarget>>(value).ok())
        .unwrap_or_default())
}

pub fn save(conn: &Connection, targets: &[HandoffTarget], now_ms: i64) -> DbResult<()> {
    let value = serde_json::to_value(targets).unwrap_or(serde_json::Value::Array(Vec::new()));
    crate::ipc::prefs::set(conn, TARGETS_KEY, &value, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_is_named_after_its_directory() {
        assert_eq!(HandoffTarget::seed("~/Projects/offerlab").name, "offerlab");
        assert_eq!(HandoffTarget::seed("/Users/x/mach/").name, "mach");
        assert_eq!(HandoffTarget::seed("/").name, "Handoff");
    }

    #[test]
    fn a_seed_validates() {
        validate(&HandoffTarget::seed("/tmp")).expect("the seeded target must be usable");
    }
}
