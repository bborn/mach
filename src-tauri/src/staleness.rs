//! "Is the app you are looking at the app you just built?"
//!
//! Twice in one day, working code looked broken because the running process
//! predated the binary on disk. The frontend hot-reloads through Vite, so UI
//! changes appear instantly and give every impression that the whole app is
//! current; the Rust half only changes when the process restarts. The result is
//! a *mismatched pair* — a new frontend calling commands an old backend does
//! not have — and it fails in ways that look like ordinary bugs. The first
//! time it cost an afternoon of chasing a calendar save path that was already
//! fixed; the second time it hid an entire migration, so preferences silently
//! had nowhere to persist to.
//!
//! Neither time was there any way to *see* it. That is the actual defect, and
//! it is what this fixes.
//!
//! # Why the executable's timestamp, and not a build id
//!
//! The obvious approach is to stamp a version into the binary at compile time
//! and compare it with one baked into the frontend bundle. It does not work
//! here. `build.rs` does not necessarily re-run when only Rust sources change,
//! so a compile-time stamp goes stale exactly when it matters, and the
//! frontend's "build" under a dev server is a moving target with no meaningful
//! identity at all.
//!
//! The thing actually being asked is much narrower: *has the file I was
//! launched from been replaced since I started?* That is one `stat` against a
//! path the process already knows, it needs no build plumbing, and it is
//! precisely true — `cargo build` rewrites the executable, so a changed mtime
//! means a newer build is sitting there waiting for a restart.
//!
//! In a shipped bundle this is inert: nothing rewrites the executable of an
//! installed app, so it reports current forever.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

/// What the UI needs to say "restart to pick up a newer build".
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildStatus {
    /// Modification time of the executable this process was launched from,
    /// captured at startup. Milliseconds since the epoch.
    pub running_build_ms: i64,
    /// Modification time of that same path right now.
    pub on_disk_build_ms: i64,
    /// Whether a newer build is waiting.
    pub stale: bool,
}

/// The running process's own executable and the mtime it had at startup.
///
/// Captured once, because the whole comparison depends on remembering what we
/// were launched from *before* anything replaced it.
#[derive(Debug, Clone)]
pub struct BuildWatch {
    exe: Option<PathBuf>,
    launched_with_ms: i64,
}

impl BuildWatch {
    /// Records the executable's current mtime. Call once, at startup.
    pub fn capture() -> BuildWatch {
        let exe = std::env::current_exe().ok();
        let launched_with_ms = exe.as_deref().and_then(mtime_ms).unwrap_or(0);
        BuildWatch { exe, launched_with_ms }
    }

    /// Re-stats the executable and reports whether it has been replaced.
    ///
    /// A missing or unreadable file reports *not* stale. The executable being
    /// unstattable is not evidence that a newer one exists, and a false
    /// "restart me" is worse than silence — it teaches you to ignore the
    /// banner, which is the one thing this must not do.
    pub fn status(&self) -> BuildStatus {
        let on_disk_build_ms = self
            .exe
            .as_deref()
            .and_then(mtime_ms)
            .unwrap_or(self.launched_with_ms);

        BuildStatus {
            running_build_ms: self.launched_with_ms,
            on_disk_build_ms,
            // Strictly newer. Equal is the normal case and must never nag.
            stale: on_disk_build_ms > self.launched_with_ms,
        }
    }
}

fn mtime_ms(path: &std::path::Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Now, in milliseconds — for tests that need to write a plausible mtime.
#[cfg(test)]
fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before the epoch")
            .as_millis(),
    )
    .expect("time fits in i64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A file standing in for the executable, so the test can rewrite it.
    fn temp_exe(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mach-staleness-{name}-{}", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"binary").expect("write");
        path
    }

    fn watching(path: &std::path::Path) -> BuildWatch {
        BuildWatch {
            exe: Some(path.to_path_buf()),
            launched_with_ms: mtime_ms(path).expect("mtime"),
        }
    }

    #[test]
    fn an_untouched_executable_is_never_stale() {
        let exe = temp_exe("untouched");
        let watch = watching(&exe);

        let status = watch.status();
        assert!(!status.stale);
        assert_eq!(status.running_build_ms, status.on_disk_build_ms);

        std::fs::remove_file(&exe).ok();
    }

    #[test]
    fn a_rebuilt_executable_is_stale() {
        // The actual scenario: `cargo build` rewrote the file under a process
        // that is still running the old one.
        let exe = temp_exe("rebuilt");
        let watch = watching(&exe);

        // Long enough that the new mtime is distinguishable on any filesystem
        // whose timestamps are only second-resolution.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&exe, b"rebuilt binary").expect("rebuild");

        let status = watch.status();
        assert!(status.stale, "a newer binary on disk must be reported");
        assert!(status.on_disk_build_ms > status.running_build_ms);

        std::fs::remove_file(&exe).ok();
    }

    #[test]
    fn an_older_file_on_disk_is_not_stale() {
        // Restoring an older binary is not a reason to nag about restarting
        // into it, and `>` rather than `!=` is what makes that true. Faked by
        // claiming to have launched from a future build rather than by
        // back-dating the file, which needs no platform-specific call.
        let exe = temp_exe("older");
        let watch = BuildWatch {
            exe: Some(exe.clone()),
            launched_with_ms: now_ms() + 600_000,
        };

        assert!(!watch.status().stale);
        std::fs::remove_file(&exe).ok();
    }

    #[test]
    fn a_missing_executable_reports_current_rather_than_stale() {
        let exe = temp_exe("missing");
        let watch = watching(&exe);
        std::fs::remove_file(&exe).expect("remove");

        let status = watch.status();
        assert!(!status.stale, "an unreadable path is not evidence of a newer build");
        assert_eq!(status.on_disk_build_ms, status.running_build_ms);
    }

    #[test]
    fn a_process_with_no_executable_path_is_inert() {
        let watch = BuildWatch { exe: None, launched_with_ms: now_ms() };
        assert!(!watch.status().stale);
    }
}
