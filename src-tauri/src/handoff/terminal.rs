//! Which terminal a `terminal`-mode handoff opens in.
//!
//! # One choice, not one per target
//!
//! A person has one terminal. Targets are a small table of four fields each,
//! and a fifth field holding the same word in every row would be a column that
//! only ever disagrees with itself by accident — rename the app, and every row
//! has to be visited. So this is a single value in the `preferences` table,
//! under [`TERMINAL_APP_KEY`], written by the frontend and read here. The same
//! shape `agentBackend` already has: a setting the window writes and only Rust
//! reads.
//!
//! # What the value is
//!
//! Exactly the argument `open -a` is given: an application name (`iTerm`) or a
//! bundle path (`/Users/x/Applications/Ghostty.app`). Empty means "whatever
//! macOS opens a `.command` with", which is what Mach did before this existed
//! and is therefore the default.
//!
//! # Offering what is there
//!
//! [`installed`] looks for the bundles in [`KNOWN`] in the directories a Mac
//! actually keeps applications in, and reports the ones it finds. A hardcoded
//! menu of eight names would offer this machine seven terminals it does not
//! have and hide the one it does — and it would still be wrong for the person
//! who keeps Ghostty in `~/Applications`, which is why the value is free text
//! underneath and the menu is only a shortcut into it.
//!
//! A name that resolves to nothing is not silently ignored: `open -a` exits
//! non-zero without opening anything, and [`super::plan::open_in_terminal`]
//! turns that into a sentence naming the app.

use std::path::PathBuf;

use serde::Serialize;

/// Where the choice is stored. Read through `ipc::prefs::get`.
///
/// Letters only, so `ipc::prefs::is_valid_key` accepts it and the frontend can
/// write it through `set_preference` like any other preference.
pub const TERMINAL_APP_KEY: &str = "handoffTerminalApp";

/// The override that predates the setting, kept working.
///
/// It was the only way in for as long as there was no control, so anything
/// that sets it — a launch agent, a `.env` a wrapper script exports — must
/// keep meaning what it meant. It wins over the stored value rather than
/// seeding it: an environment variable is the more specific statement, and the
/// dialog says so rather than showing a menu that would be a lie.
pub const TERMINAL_APP_ENV: &str = "MACH_HANDOFF_TERMINAL_APP";

/// The terminals worth looking for, by bundle name, in the order they are
/// offered. `Terminal` first because it is the one every Mac has.
///
/// `kitty` is lower-case because its bundle is: this list is matched against
/// the filesystem, so it spells the names the way the applications do.
pub const KNOWN: &[&str] = &[
    "Terminal",
    "iTerm",
    "Ghostty",
    "WezTerm",
    "kitty",
    "Alacritty",
    "Warp",
    "Hyper",
];

/// One terminal that is actually on this Mac.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminal {
    /// What `open -a` is given, and what the menu says.
    pub name: String,
    /// The bundle it was found at.
    pub path: String,
}

/// Where applications live, in the order a duplicate should be resolved.
///
/// `/Applications` first because that is where an app the user installed is;
/// the system copies come last so that a hand-installed build wins over one
/// Apple ships. `~/Applications` is included because a single-user install of
/// anything downloaded lands there.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        let home = home.trim_end_matches('/');
        if !home.is_empty() {
            dirs.push(PathBuf::from(format!("{home}/Applications")));
        }
    }
    dirs.push(PathBuf::from("/System/Applications"));
    // Where Terminal.app has lived since Catalina.
    dirs.push(PathBuf::from("/System/Applications/Utilities"));
    dirs
}

/// The known terminals present in `dirs`, in [`KNOWN`] order.
///
/// Takes the directories rather than finding them, so a test can point it at a
/// tree it made and assert about what came back — the answer on a real Mac
/// depends on what is installed, which is not a thing to assert.
pub fn detect_in(dirs: &[PathBuf]) -> Vec<Terminal> {
    let mut found = Vec::new();
    for name in KNOWN {
        for dir in dirs {
            let bundle = dir.join(format!("{name}.app"));
            if bundle.is_dir() {
                found.push(Terminal {
                    name: (*name).to_string(),
                    path: bundle.to_string_lossy().into_owned(),
                });
                break;
            }
        }
    }
    found
}

/// The known terminals on this Mac.
pub fn installed() -> Vec<Terminal> {
    detect_in(&search_dirs())
}

/// The environment override, if one is set to something.
pub fn forced() -> Option<String> {
    std::env::var(TERMINAL_APP_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Which app to open the launcher with: the override, then the stored value,
/// then nobody — which is the bare `open` that goes to whatever macOS has
/// registered for `.command`.
pub fn chosen(stored: Option<&str>) -> Option<String> {
    forced().or_else(|| {
        stored
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_stored_and_nothing_forced_means_the_system_default() {
        std::env::remove_var(TERMINAL_APP_ENV);
        assert_eq!(chosen(None), None);
        assert_eq!(chosen(Some("   ")), None);
    }

    #[test]
    fn a_directory_with_no_terminals_offers_none() {
        let empty = vec![PathBuf::from("/nonexistent/mach-test/Applications")];
        assert_eq!(detect_in(&empty), Vec::new());
    }
}
