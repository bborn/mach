//! Where Mach finds its database and its Google credentials.
//!
//! Two things are resolved here and nowhere else:
//!
//!  * **The database path.** Chosen by the caller from Tauri's path API (the OS
//!    app-data directory), never hardcoded — `database_path` only appends the
//!    file name so the convention lives in one place.
//!  * **The OAuth client.** `MACH_GOOGLE_CLIENT_ID` / `MACH_GOOGLE_CLIENT_SECRET`,
//!    exactly as [`crate::auth`] documents them.
//!
//! # Missing credentials are a state, not a crash
//!
//! A fresh checkout has no `.env.local` and no exported variables. The app still
//! has to boot: the store opens, the window paints, and the UI is told *why* it
//! cannot add an account. So configuration failure is captured as
//! [`AppConfig::configuration_error`] and surfaced through `sync_status()`
//! rather than raised. Nothing in this module panics or exits.
//!
//! # `.env.local` in development
//!
//! In a debug build the repository's `.env.local` is loaded if it can be found,
//! which is what makes `cargo tauri dev` work without a shell wrapper. Release
//! builds never read it — a shipped app must not pick up a file that happens to
//! sit next to it. `dotenvy` does not overwrite variables that are already set,
//! so an explicit export always wins.

use std::path::{Path, PathBuf};

use crate::auth::{AuthError, ClientConfig, ENV_CLIENT_ID, ENV_CLIENT_SECRET};

/// The SQLite file inside the app-data directory.
pub const DATABASE_FILE_NAME: &str = "mach.sqlite3";

/// The development-only credentials file, looked for at the repository root.
pub const DOTENV_FILE_NAME: &str = ".env.local";

/// How far up the tree to look for [`DOTENV_FILE_NAME`]. `src-tauri` → repo root
/// is one hop; the extra levels cover a target directory or a worktree layout.
const DOTENV_SEARCH_DEPTH: usize = 5;

/// The database file inside `dir`.
pub fn database_path(dir: impl AsRef<Path>) -> PathBuf {
    dir.as_ref().join(DATABASE_FILE_NAME)
}

/// Everything the app needs before it can open anything.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_path: PathBuf,
    /// `None` when the OAuth client is not configured. The app still runs; it
    /// just cannot authorize or sync.
    pub client: Option<ClientConfig>,
    /// A sentence the UI can render verbatim. `Some` exactly when `client` is
    /// `None`.
    pub configuration_error: Option<String>,
}

impl AppConfig {
    /// Read the process environment (loading `.env.local` first in development).
    pub fn load(database_path: impl Into<PathBuf>) -> Self {
        load_development_dotenv();
        Self::from_values(
            database_path,
            std::env::var(ENV_CLIENT_ID).ok(),
            std::env::var(ENV_CLIENT_SECRET).ok(),
        )
    }

    /// The env-independent core of [`Self::load`], so "not configured" is
    /// testable without mutating a shared process environment.
    pub fn from_values(
        database_path: impl Into<PathBuf>,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Self {
        let database_path = database_path.into();

        // A desktop OAuth client always ships with a secret and Google's token
        // endpoint expects it, so an absent secret is treated as unconfigured
        // rather than discovered later as a 401 with no explanation.
        let secret_present = client_secret
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());

        match ClientConfig::from_values(client_id, client_secret) {
            Ok(client) if secret_present => AppConfig {
                database_path,
                client: Some(client),
                configuration_error: None,
            },
            Ok(_) => AppConfig {
                database_path,
                client: None,
                configuration_error: Some(not_configured_message(&AuthError::MissingConfig(
                    ENV_CLIENT_SECRET,
                ))),
            },
            Err(error) => AppConfig {
                database_path,
                client: None,
                configuration_error: Some(not_configured_message(&error)),
            },
        }
    }

    pub fn is_configured(&self) -> bool {
        self.client.is_some()
    }
}

/// The sentence shown when Mach cannot authorize anything.
pub fn not_configured_message(error: &AuthError) -> String {
    format!(
        "Google sign-in is not configured — {error}. \
         Set {ENV_CLIENT_ID} and {ENV_CLIENT_SECRET} in the environment \
         (or in {DOTENV_FILE_NAME} for development) and restart Mach."
    )
}

/// The nearest ancestor of `start` containing [`DOTENV_FILE_NAME`].
pub fn find_dotenv_from(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    for _ in 0..DOTENV_SEARCH_DEPTH {
        let current = dir?;
        let candidate = current.join(DOTENV_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = current.parent();
    }
    None
}

#[cfg(debug_assertions)]
fn load_development_dotenv() {
    // The crate directory is where a `cargo`-driven run starts; the working
    // directory is where a launched binary starts. Either can be the way in.
    let roots = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ];
    for root in roots {
        if let Some(path) = find_dotenv_from(&root) {
            // A failure here is not worth failing a boot over: the variables may
            // already be exported, and `AppConfig` reports the outcome either way.
            let _ = dotenvy::from_path(&path);
            return;
        }
    }
}

#[cfg(not(debug_assertions))]
fn load_development_dotenv() {}
