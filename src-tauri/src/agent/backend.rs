//! Which brain answers, and how Mach works that out on its own.
//!
//! The agent used to have exactly one way to think: `POST /v1/messages` with an
//! `ANTHROPIC_API_KEY`. That is a fine way, and it is still here — but it is the
//! wrong *default* for the person this app was built for, who already pays for
//! Claude Code and had it installed the whole time. Asking him for a second
//! credential to reach the same model is asking him to pay twice.
//!
//! So the brain is a choice, and the choice makes itself:
//!
//! | backend | what it is | what it costs |
//! |---|---|---|
//! | [`Backend::ClaudeCli`] | the `claude` executable, driven headless | nothing new — his subscription |
//! | [`Backend::AnthropicApi`] | the Messages API, as before | an API key |
//! | [`Backend::Command`] | any program that satisfies `docs/agent-backends.md` | whatever it is |
//!
//! # Detection
//!
//! [`resolve`] answers in one pass, and the order is the point:
//!
//! 1. an explicit preference wins, and if it cannot be satisfied it *fails*
//!    rather than silently falling through to something the owner did not pick;
//! 2. otherwise, if a `claude` executable can be found, that is the answer;
//! 3. otherwise, if there is an API credential, that is the answer;
//! 4. otherwise the error names both remedies, because "not configured" without
//!    an instruction is the bug this whole module exists to fix.
//!
//! # Why the executable is found by looking, not by asking a shell
//!
//! `claude` in the owner's interactive shell is a *function* wrapping the real
//! binary, so `zsh -ic 'which claude'` reports something that is not a path and
//! cannot be spawned. Worse, a Tauri app launched from Finder inherits the
//! launchd `PATH` — `/usr/bin:/bin:/usr/sbin:/sbin` — which contains no
//! developer tooling at all, so even an honest `PATH` search misses the
//! installer's default location. [`find_claude`] therefore searches `PATH` *and*
//! the handful of places the official installers actually write to, and
//! `MACH_CLAUDE_BIN` overrides the lot.

use std::path::{Path, PathBuf};

use super::config::{AgentConfig, ENV_API_KEY, ENV_AUTH_TOKEN};
use super::error::AgentError;

/// Point Mach at a specific `claude`. Also how the tests avoid depending on
/// whether the machine running them has one.
pub const ENV_CLAUDE_BIN: &str = "MACH_CLAUDE_BIN";

/// The preference keys. Alphanumeric camelCase, because that is the alphabet
/// [`crate::ipc::prefs::is_valid_key`] allows — the `mach.` namespace lives in
/// the store's own scoping, not in the key text.
pub const PREF_BACKEND: &str = "agentBackend";
pub const PREF_MODEL: &str = "agentModel";
pub const PREF_COMMAND: &str = "agentCommand";

/// The places Claude Code's installers put the executable, in the order they
/// are worth trying. Home-relative paths are expanded against `$HOME`.
const WELL_KNOWN: &[&str] = &[
    "~/.local/bin/claude",
    "~/.claude/local/claude",
    "/opt/homebrew/bin/claude",
    "/usr/local/bin/claude",
];

// ===========================================================================
// What the owner asked for
// ===========================================================================

/// The stored preference, before it has met reality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendChoice {
    /// Work it out. The default, and the one that must need no configuration.
    #[default]
    Auto,
    ClaudeCli,
    AnthropicApi,
    Command,
}

impl BackendChoice {
    /// Parse the stored string. Anything unrecognised is [`BackendChoice::Auto`]
    /// — a preference row written by a newer build, or by hand, must not leave
    /// the agent unable to start.
    pub fn parse(raw: Option<&str>) -> BackendChoice {
        match raw.unwrap_or("").trim() {
            "claudeCli" => BackendChoice::ClaudeCli,
            "anthropicApi" => BackendChoice::AnthropicApi,
            "command" => BackendChoice::Command,
            _ => BackendChoice::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BackendChoice::Auto => "auto",
            BackendChoice::ClaudeCli => "claudeCli",
            BackendChoice::AnthropicApi => "anthropicApi",
            BackendChoice::Command => "command",
        }
    }
}

/// Everything the owner can say about the brain, as read from the preferences
/// table. All optional: the zero value is "decide for me".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendPrefs {
    pub choice: BackendChoice,
    /// A model id or alias. Meaning depends on the backend: `opus` is a fine
    /// answer for the CLI and a meaningless one for the Messages API.
    pub model: Option<String>,
    /// The command line for [`BackendChoice::Command`], as typed. Split on
    /// whitespace, so a path with a space in it needs quoting — see
    /// [`split_command`].
    pub command: Option<String>,
}

impl BackendPrefs {
    /// Read the three keys out of the preferences store.
    ///
    /// A store that will not open, or a row of the wrong type, means "no
    /// preference" rather than an error: the agent's defaults are correct
    /// answers to both.
    pub fn load(db: &crate::db::Db) -> BackendPrefs {
        let read = |key: &str| -> Option<String> {
            db.read(|conn| crate::ipc::prefs::get(conn, key))
                .ok()
                .flatten()
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        BackendPrefs {
            choice: BackendChoice::parse(read(PREF_BACKEND).as_deref()),
            model: read(PREF_MODEL),
            command: read(PREF_COMMAND),
        }
    }
}

// ===========================================================================
// What it resolved to
// ===========================================================================

/// A brain, resolved: everything needed to start one, with nothing left to look
/// up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    ClaudeCli {
        exe: PathBuf,
        /// `None` leaves the CLI on whatever model it is configured for, which
        /// is the right default: it is his CLI and his choice.
        model: Option<String>,
    },
    AnthropicApi(Box<AgentConfig>),
    Command {
        program: PathBuf,
        args: Vec<String>,
    },
}

impl Backend {
    /// The stable tag the UI and the tests use.
    pub fn kind(&self) -> &'static str {
        match self {
            Backend::ClaudeCli { .. } => "claudeCli",
            Backend::AnthropicApi(_) => "anthropicApi",
            Backend::Command { .. } => "command",
        }
    }

    /// One line for the session header — which brain answered, and on what.
    pub fn label(&self) -> String {
        match self {
            Backend::ClaudeCli { model: Some(model), .. } => format!("Claude Code ({model})"),
            Backend::ClaudeCli { .. } => "Claude Code".to_string(),
            Backend::AnthropicApi(config) => format!("Anthropic API ({})", config.model),
            Backend::Command { program, .. } => program
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| program.to_string_lossy().to_string()),
        }
    }
}

/// Which of the three could run right now, for the preferences dialog and for
/// the sentence a failed resolution prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    /// The `claude` executable, if there is one.
    pub claude: Option<PathBuf>,
    /// Whether `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` is set.
    pub api_key: bool,
}

impl Availability {
    /// Look at the machine. Cheap — two `stat`s and an environment read — so
    /// this runs per session rather than being cached into staleness.
    pub fn probe() -> Availability {
        Availability {
            claude: find_claude(),
            api_key: env_non_empty(ENV_API_KEY) || env_non_empty(ENV_AUTH_TOKEN),
        }
    }
}

/// Turn a preference and a machine into a backend, or into a sentence saying
/// what to do about it.
///
/// `config` is the pinned [`AgentConfig`] a test (or anything that wants a
/// specific model without exporting a variable) supplied; when it is present the
/// API backend needs no environment at all.
pub fn resolve(
    prefs: &BackendPrefs,
    available: &Availability,
    config: Option<&AgentConfig>,
) -> Result<Backend, AgentError> {
    match prefs.choice {
        BackendChoice::ClaudeCli => match &available.claude {
            Some(exe) => Ok(claude_cli(exe, prefs)),
            None => Err(AgentError::MissingApiKey(no_claude_message())),
        },

        BackendChoice::AnthropicApi => api_backend(prefs, config),

        BackendChoice::Command => command_backend(prefs),

        BackendChoice::Auto => {
            if let Some(exe) = &available.claude {
                return Ok(claude_cli(exe, prefs));
            }
            if config.is_some() || available.api_key {
                return api_backend(prefs, config);
            }
            Err(AgentError::MissingApiKey(nothing_available_message()))
        }
    }
}

fn claude_cli(exe: &Path, prefs: &BackendPrefs) -> Backend {
    Backend::ClaudeCli {
        exe: exe.to_path_buf(),
        model: prefs.model.clone(),
    }
}

fn api_backend(prefs: &BackendPrefs, config: Option<&AgentConfig>) -> Result<Backend, AgentError> {
    let mut config = match config {
        Some(pinned) => pinned.clone(),
        None => AgentConfig::load()?,
    };
    // The preference is the owner speaking directly, so it outranks the
    // environment variable — which is usually a leftover from a shell.
    if let Some(model) = &prefs.model {
        config.model = model.clone();
    }
    Ok(Backend::AnthropicApi(Box::new(config)))
}

fn command_backend(prefs: &BackendPrefs) -> Result<Backend, AgentError> {
    let line = prefs
        .command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| AgentError::MissingApiKey(no_command_message()))?;

    let (program, args) = split_command(line)
        .ok_or_else(|| AgentError::MissingApiKey(no_command_message()))?;
    Ok(Backend::Command { program, args })
}

// ===========================================================================
// Finding things
// ===========================================================================

/// The `claude` executable, or `None`.
///
/// `MACH_CLAUDE_BIN` first — including the empty string, which is how a test (or
/// an owner who wants the API path) says "pretend there is none". Then `PATH`,
/// then the installer locations, because the app's `PATH` is very often not the
/// shell's.
pub fn find_claude() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(ENV_CLAUDE_BIN) {
        let trimmed = explicit.trim();
        if trimmed.is_empty() {
            return None;
        }
        let path = PathBuf::from(trimmed);
        return is_executable(&path).then_some(path);
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("claude");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    let home = std::env::var("HOME").ok();
    for entry in WELL_KNOWN {
        let candidate = match (entry.strip_prefix("~/"), &home) {
            (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
            (Some(_), None) => continue,
            (None, _) => PathBuf::from(entry),
        };
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// A regular file with an execute bit. The directory check matters: `PATH`
/// entries pointing at a *directory* named `claude` are not unheard of, and
/// spawning one is a confusing failure much later.
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Split a command line the way a person means it, not the way a shell would.
///
/// Whitespace separates arguments; single and double quotes group them. There
/// is deliberately no variable expansion, no globbing, no pipes and no `&&` —
/// this string names a program to spawn, and anything that made it a shell
/// fragment would make "which program is the agent" a question with no answer.
pub fn split_command(line: &str) -> Option<(PathBuf, Vec<String>)> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for ch in line.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, c @ ('\'' | '"')) => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        parts.push(current);
    }

    let mut parts = parts.into_iter();
    let program = parts.next()?;
    if program.is_empty() {
        return None;
    }
    Some((PathBuf::from(program), parts.collect()))
}

fn env_non_empty(key: &str) -> bool {
    std::env::var(key).map(|v| !v.trim().is_empty()).unwrap_or(false)
}

// ===========================================================================
// The sentences
// ===========================================================================

/// What to say when nothing at all is available.
///
/// The old text named a variable and stopped. This one names both ways out and
/// puts the cheap one first, because the owner of this app already has a Claude
/// subscription and the API key is the answer he does *not* want.
pub fn nothing_available_message() -> String {
    format!(
        "The agent has no brain to think with. Either install Claude Code — \
         `curl -fsSL https://claude.com/install.sh | bash`, then relaunch Mach — \
         which uses the Claude subscription you already have and needs no key; \
         or set {ENV_API_KEY} in the environment (or in {}) to use the Anthropic API. \
         Mach picks Claude Code automatically when it can find it.",
        crate::config::DOTENV_FILE_NAME
    )
}

fn no_claude_message() -> String {
    format!(
        "Preferences ask for Claude Code, but no `claude` executable could be found. \
         Install it with `curl -fsSL https://claude.com/install.sh | bash`, or point \
         {ENV_CLAUDE_BIN} at the binary, or choose a different agent backend in \
         Preferences → Agent."
    )
}

fn no_command_message() -> String {
    "Preferences ask for a custom agent command, but none is set. Put the command \
     in Preferences → Agent — it must satisfy the contract in docs/agent-backends.md \
     — or choose a different backend."
        .to_string()
}
