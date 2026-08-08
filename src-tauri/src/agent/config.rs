//! Where the agent finds its credential and how the model is configured.
//!
//! Mirrors [`crate::config`] rather than editing it: the same `.env.local`
//! search in a debug build, the same "missing credentials are a state, not a
//! crash" rule. Nothing here panics, and nothing here is read at boot — the key
//! is resolved when a session actually starts, so adding it to `.env.local`
//! costs a relaunch and nothing else.
//!
//! | variable | meaning | default |
//! |---|---|---|
//! | `ANTHROPIC_API_KEY` | the credential (`x-api-key`) | — required |
//! | `ANTHROPIC_AUTH_TOKEN` | an OAuth bearer, used only when the key is absent | — |
//! | `MACH_AGENT_MODEL` | model id | `claude-opus-5` |
//! | `MACH_AGENT_EFFORT` | `low`/`medium`/`high`/`xhigh`/`max` | `medium` |
//! | `MACH_AGENT_MAX_TOKENS` | output ceiling (thinking included) | `32000` |
//! | `MACH_AGENT_BASE_URL` | API base, for a stub during QA | `https://api.anthropic.com` |
//!
//! # Why `medium` effort
//!
//! These sessions are conversational and their tools are narrow and typed — a
//! handful of local reads and one undoable command. The reasoning that matters
//! is "which thread, which Tuesday", not a long-horizon plan, and the owner is
//! waiting with the drawer open. `medium` is the fastest setting that still
//! reliably chains read → compose → schedule; `MACH_AGENT_EFFORT` moves it.

use super::error::AgentError;

pub const ENV_API_KEY: &str = "ANTHROPIC_API_KEY";
pub const ENV_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
pub const ENV_MODEL: &str = "MACH_AGENT_MODEL";
pub const ENV_EFFORT: &str = "MACH_AGENT_EFFORT";
pub const ENV_MAX_TOKENS: &str = "MACH_AGENT_MAX_TOKENS";
pub const ENV_BASE_URL: &str = "MACH_AGENT_BASE_URL";

/// The current Opus. Not a date-suffixed snapshot — the alias is the id.
pub const DEFAULT_MODEL: &str = "claude-opus-5";
pub const DEFAULT_EFFORT: &str = "medium";
pub const DEFAULT_MAX_TOKENS: u32 = 32_000;
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The Messages API version header. Fixed, not configurable.
pub const API_VERSION: &str = "2023-06-01";

/// Server-side refusal fallback. Claude Opus 5's safety classifiers can decline
/// a request with a 200 and `stop_reason: "refusal"`; `fallbacks: "default"`
/// re-runs it on Anthropic's recommended substitute inside the same call
/// instead of handing the owner a dead session. Dropped automatically on a 400
/// that names it, so an account without the beta still works.
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// How the request authenticates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// `x-api-key: …`
    ApiKey(String),
    /// `Authorization: Bearer …` plus the OAuth beta header.
    BearerToken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub credential: Credential,
    pub model: String,
    pub effort: String,
    pub max_tokens: u32,
    pub base_url: String,
    /// Whether to send the server-side fallback beta on the first attempt.
    pub fallbacks: bool,
}

impl AgentConfig {
    /// Read the process environment, loading `.env.local` first in development.
    pub fn load() -> Result<AgentConfig, AgentError> {
        load_development_dotenv();
        Self::from_values(
            std::env::var(ENV_API_KEY).ok(),
            std::env::var(ENV_AUTH_TOKEN).ok(),
            std::env::var(ENV_MODEL).ok(),
            std::env::var(ENV_EFFORT).ok(),
            std::env::var(ENV_MAX_TOKENS).ok(),
            std::env::var(ENV_BASE_URL).ok(),
        )
    }

    /// The env-independent core of [`Self::load`], so "not configured" is
    /// testable without mutating a shared process environment.
    pub fn from_values(
        api_key: Option<String>,
        auth_token: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        max_tokens: Option<String>,
        base_url: Option<String>,
    ) -> Result<AgentConfig, AgentError> {
        let credential = match non_empty(api_key) {
            Some(key) => Credential::ApiKey(key),
            None => match non_empty(auth_token) {
                Some(token) => Credential::BearerToken(token),
                None => return Err(AgentError::MissingApiKey(not_configured_message())),
            },
        };

        Ok(AgentConfig {
            credential,
            model: non_empty(model).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            effort: non_empty(effort).unwrap_or_else(|| DEFAULT_EFFORT.to_string()),
            max_tokens: non_empty(max_tokens)
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_MAX_TOKENS),
            base_url: non_empty(base_url)
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            fallbacks: true,
        })
    }

    pub fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }
}

/// The sentence shown when the agent has no credential. Names the variable and
/// the file, exactly as [`crate::config::not_configured_message`] does for
/// Google — a feature that cannot run must say what to set.
pub fn not_configured_message() -> String {
    format!(
        "The agent is not configured — {ENV_API_KEY} is not set. \
         Add it to the environment (or to {} for development) and restart Mach.",
        crate::config::DOTENV_FILE_NAME
    )
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty()).map(|v| v.trim().to_string())
}

/// Load `.env.local` at most once per process, from the same roots
/// [`crate::config`] searches. `dotenvy` never overwrites a variable that is
/// already set, so an explicit export always wins.
#[cfg(debug_assertions)]
fn load_development_dotenv() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let roots = [
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        ];
        for root in roots {
            if let Some(path) = crate::config::find_dotenv_from(&root) {
                let _ = dotenvy::from_path(&path);
                return;
            }
        }
    });
}

#[cfg(not(debug_assertions))]
fn load_development_dotenv() {}
