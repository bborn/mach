//! Where the door is, and the file that says so.
//!
//! The address is not a constant. `mcp.rs` binds `127.0.0.1:0` so that nothing
//! is guessable and two instances never collide, and the door keeps that; what
//! is stable is the **path of the file**, under the instance's own data
//! directory, and that is enough for a command line. A fixed port number would
//! have bought nothing except a collision between the owner's app and every QA
//! instance on the machine.
//!
//! # The pid is in the file, and it is load-bearing
//!
//! [`Door`](super::door::Door)'s `Drop` removes this file, and a process that
//! is killed hard does not run `Drop`. So a stale file with a live-looking port
//! in it is a normal thing to find after a crash — and a CLI that trusted it
//! would either hang on a connect to nothing, or, far worse, reach whatever
//! process has since been handed that port number. Recording the pid means the
//! reader can ask the kernel whether the writer is still there before it says a
//! word to the socket, and can say "Mach is not running" rather than something
//! about connection refused.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file, inside the instance's data directory.
pub const ENDPOINT_FILE: &str = "cli-door.json";

/// Where the CLI should talk, and what it must say to be heard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    /// The app process that opened it. See the module doc.
    pub pid: u32,
    /// The build that wrote the file, so a CLI from another checkout can say so
    /// instead of failing on a field it does not understand.
    pub version: String,
}

impl Endpoint {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, super::door::PATH)
    }

    /// Whether the process that wrote this file is still alive.
    ///
    /// `kill(pid, 0)` asks the kernel about a process without touching it: it
    /// succeeds when the process exists and we may signal it, and fails with
    /// `ESRCH` when it does not. `EPERM` — the pid exists and belongs to
    /// somebody else — counts as alive here, because the useful answer is "that
    /// number is taken", and the token check is what actually decides whether
    /// the thing on the port is Mach.
    #[cfg(unix)]
    pub fn writer_is_alive(&self) -> bool {
        // Safety: `kill` with signal 0 sends nothing. It reads process-table
        // state and returns; there is no memory involved and no process is
        // affected.
        if unsafe { libc::kill(self.pid as libc::pid_t, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(not(unix))]
    pub fn writer_is_alive(&self) -> bool {
        true
    }
}

/// The path of the endpoint file inside `data_dir`.
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(ENDPOINT_FILE)
}

/// Write it with an owner-only mode.
///
/// Created with the mode already set rather than written and then chmod-ed: the
/// window between the two is small, but it is a window in which a bearer token
/// that can send mail is world-readable. Same reasoning and same shape as
/// `agent::mcp::write_config` and `qa::write_endpoint`; this is the third
/// caller and the pattern is by now the house style rather than a coincidence.
pub fn write(data_dir: &Path, endpoint: &Endpoint) -> Result<PathBuf, String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("could not create {}: {e}", data_dir.display()))?;
    let path = path_in(data_dir);
    let body = serde_json::to_string(endpoint).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, body)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }

    Ok(path)
}

/// Read it back, or say why not.
pub fn read(data_dir: &Path) -> Result<Endpoint, String> {
    let path = path_in(data_dir);
    let body = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    serde_json::from_str(&body).map_err(|e| format!("{} is not an endpoint file: {e}", path.display()))
}

/// The data directory this machine's Mach uses, without a Tauri application to
/// ask.
///
/// `MACH_DATA_DIR` first, for the same reason everything else in the codebase
/// honours it: a QA instance has its own store and its own door, and a CLI run
/// against one must not reach across to the owner's. Otherwise it is the
/// platform's app-data directory with the bundle identifier on the end — the
/// same path `app.path().app_data_dir()` resolves to, restated here because
/// there is no `app` in a command-line process and Tauri's resolver needs one.
pub fn default_data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("MACH_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set, so there is nowhere to look for the store".to_string())?;

    #[cfg(target_os = "macos")]
    let dir = home
        .join("Library")
        .join("Application Support")
        .join(BUNDLE_IDENTIFIER);

    #[cfg(not(target_os = "macos"))]
    let dir = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join(BUNDLE_IDENTIFIER);

    Ok(dir)
}

/// The identifier from `tauri.conf.json`. Duplicated because that file is JSON
/// consumed by the bundler at build time and there is no way to read a constant
/// out of it at run time; `tests/cli.rs` asserts the two still agree.
pub const BUNDLE_IDENTIFIER: &str = "com.mach.mail";
