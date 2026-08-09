//! Claude Code as the brain — the default, and the one that needs no key.
//!
//! The owner already pays for Claude Code. Making him also hold an
//! `ANTHROPIC_API_KEY` to reach the same model inside his own mail client is
//! asking him to pay twice for one thing, and it was the reason ⌘K said "the
//! agent is not configured" on a machine with a perfectly good agent on it.
//!
//! So: spawn `claude`, headless, and give it Mach's tools.
//!
//! ```text
//!   ⌘K ──► session ──► claude --print --output-format stream-json
//!                          │            --mcp-config <0600 file>
//!                          │            --tools "" --allowedTools mcp__mach
//!                          │
//!                          │  tool call over loopback HTTP
//!                          ▼
//!                       McpServer ──► ToolGate ──► CommandDispatcher
//!                     (in this app)   (approval)   (typed · undoable)
//! ```
//!
//! # What the CLI is allowed to do
//!
//! Nothing except Mach's tools. `--tools ""` empties the built-in set — no Bash,
//! no Read, no Write, no WebFetch — so "reply to this email" cannot become a
//! side trip through the filesystem. `--allowedTools mcp__mach` then permits
//! exactly the one MCP server this app is serving, and `--strict-mcp-config`
//! stops any MCP server the owner has configured for his own coding work from
//! being loaded into a session about his mail. `--setting-sources ""` keeps his
//! `SessionStart` hooks, output styles and memory files out of it: a mail client
//! that fired a developer's shell hooks every time he pressed ⌘K would be a
//! genuinely astonishing thing to have built.
//!
//! Even if all of that were undone — even with
//! `--dangerously-skip-permissions`, which Mach never passes — the CLI still
//! could not send an email without the owner clicking, because the approval
//! gate is on *this* side of the MCP call. The CLI's permission system decides
//! whether the CLI may ask; [`ToolGate`](super::gate::ToolGate) decides whether
//! anything happens.
//!
//! # Authentication is the CLI's business
//!
//! Mach passes no credential and strips none: the child inherits the
//! environment, so `claude` authenticates exactly as it does when the owner runs
//! it in a terminal — the subscription, normally, or an API key if he has one
//! exported. That is the whole point. Mach does not need to know.
//!
//! # One process per message
//!
//! A message spawns a process, the process answers, the process exits. Follow-up
//! messages `--resume` the session id the first run reported, so the
//! conversation continues with its history intact. The alternative —
//! `--input-format stream-json` and one long-lived child — buys lower latency on
//! the second message and costs a process that has to be kept alive, watched,
//! and killed correctly across every path a session can end on. For a drawer
//! where a person types one sentence at a time, that is a bad trade.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::google::BoxFuture;

use super::brain::{Brain, BrainIo};
use super::error::AgentError;
use super::mcp::{McpServer, SERVER_NAME};

/// How long the CLI may spend on one MCP tool call before it gives up on us.
///
/// Long, because the thing it is usually waiting for is a human reading an
/// approval prompt. Fifteen minutes is "he went to make coffee"; anything that
/// takes longer than that has been abandoned and the session can be re-asked.
const TOOL_TIMEOUT_MS: &str = "900000";

/// How long the CLI waits for the tool server to come up. It is in this
/// process and already listening, so this only needs to survive a loaded
/// machine.
const STARTUP_TIMEOUT_MS: &str = "20000";

/// How often the run loop notices that the session was closed.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(150);

pub struct ClaudeCliBrain {
    exe: PathBuf,
    model: Option<String>,
}

impl ClaudeCliBrain {
    pub fn new(exe: PathBuf, model: Option<String>) -> ClaudeCliBrain {
        ClaudeCliBrain { exe, model }
    }
}

impl Brain for ClaudeCliBrain {
    fn drive<'a>(&'a self, io: BrainIo) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(self.run(io))
    }
}

impl ClaudeCliBrain {
    async fn run(&self, mut io: BrainIo) -> Result<(), AgentError> {
        // The tool port and the CLI's session are the same lifetime: the server
        // is dropped — listener closed, token file deleted — when this returns,
        // whichever way it returns.
        let workspace = io.workspace.clone();
        let server = McpServer::start(
            Arc::clone(&io.gate),
            tokio::runtime::Handle::current(),
            &workspace,
            &io.session_id,
        )?;

        let mut message = io.first_message.clone();
        let mut resume: Option<String> = None;

        loop {
            if io.is_cancelled() {
                return Ok(());
            }
            let outcome = self.turn(&mut io, &server, &workspace, &message, resume.as_deref()).await?;
            // Only adopt a session id if the run reported one; a failed launch
            // must not make the next message resume a conversation that does not
            // exist.
            if let Some(id) = outcome {
                resume = Some(id);
            }

            match io.idle().await {
                Some(next) => {
                    io.ui.user_text(&next);
                    message = next;
                }
                None => return Ok(()),
            }
        }
    }

    /// One run of the CLI, from spawn to exit. Returns its session id.
    async fn turn(
        &self,
        io: &mut BrainIo,
        server: &McpServer,
        workspace: &std::path::Path,
        message: &str,
        resume: Option<&str>,
    ) -> Result<Option<String>, AgentError> {
        let mut command = Command::new(&self.exe);
        command
            .current_dir(workspace)
            .args(["--print", "--verbose"])
            .args(["--output-format", "stream-json"])
            // Token-by-token, so the drawer fills in as it thinks rather than
            // arriving in one lump at the end.
            .arg("--include-partial-messages")
            // Mach's prompt, not Claude Code's. `--system-prompt` replaces the
            // coding-agent framing outright: this is a mail client, and the
            // model should be told so rather than left to infer it.
            .args(["--system-prompt", &io.system])
            // No built-in tools at all. See the module docs.
            .args(["--tools", ""])
            .args(["--mcp-config", &server.config_path().to_string_lossy()])
            .arg("--strict-mcp-config")
            .args(["--setting-sources", ""])
            .arg("--disable-slash-commands")
            // Mach's own tools may run without the CLI asking; Mach asks.
            .args(["--allowedTools", &format!("mcp__{SERVER_NAME}")])
            .env("MCP_TIMEOUT", STARTUP_TIMEOUT_MS)
            .env("MCP_TOOL_TIMEOUT", TOOL_TIMEOUT_MS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if let Some(model) = &self.model {
            command.args(["--model", model]);
        }
        if let Some(session) = resume {
            command.args(["--resume", session]);
        }

        let mut child = command.spawn().map_err(|e| {
            AgentError::MissingApiKey(format!(
                "Mach could not start Claude Code at {}: {e}. Check that it is still installed, \
                 or choose a different agent backend in Preferences → Agent.",
                self.exe.display()
            ))
        })?;

        // The prompt goes on stdin rather than in the argument vector: it is the
        // owner's mail, it can be long, and an argument vector is public.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(message.as_bytes())
                .await
                .map_err(|e| AgentError::transport(format!("could not send the prompt: {e}")))?;
            drop(stdin);
        }

        let stderr = child.stderr.take();
        let errors = tokio::spawn(async move {
            let mut collected = String::new();
            if let Some(stderr) = stderr {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    collected.push_str(&line);
                    collected.push('\n');
                }
            }
            collected
        });

        let result = self.read_stream(io, &mut child).await;
        let status = child.wait().await;
        let stderr = errors.await.unwrap_or_default();

        let session_id = match result {
            Ok(state) => {
                if let Some(message) = state.failure {
                    return Err(AgentError::Api { status: 200, message });
                }
                state.session_id
            }
            Err(error) => return Err(error),
        };

        // A non-zero exit that produced no `result` message is a launch failure
        // — a bad model name, a broken install — and stderr is the only place
        // that says which.
        if let Ok(status) = status {
            if !status.success() && session_id.is_none() {
                return Err(AgentError::transport(format!(
                    "Claude Code exited with {}: {}",
                    status.code().unwrap_or(-1),
                    first_line(&stderr)
                )));
            }
        }

        Ok(session_id)
    }

    /// Read the CLI's NDJSON until it stops, or until the session is closed.
    async fn read_stream(
        &self,
        io: &mut BrainIo,
        child: &mut Child,
    ) -> Result<RunState, AgentError> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::transport("Claude Code produced no output stream"))?;
        let mut lines = BufReader::new(stdout).lines();
        let mut state = RunState::default();

        loop {
            let next = tokio::select! {
                line = lines.next_line() => line,
                _ = tokio::time::sleep(CANCEL_POLL) => {
                    if io.is_cancelled() {
                        // ⌘W has to mean it. The child owns a model call and an
                        // MCP connection; killing it releases both.
                        let _ = child.start_kill();
                        return Ok(state);
                    }
                    continue;
                }
            };

            match next {
                Ok(Some(line)) => self.apply(io, &line, &mut state),
                Ok(None) => return Ok(state),
                Err(e) => {
                    return Err(AgentError::transport(format!(
                        "could not read Claude Code's output: {e}"
                    )))
                }
            }
        }
    }

    /// One line of the CLI's stream, applied.
    ///
    /// Unparseable lines are ignored rather than fatal: the CLI is allowed to
    /// print a warning to stdout (it does, when stdin is slow), and a session
    /// that died because of a diagnostic would be a poor trade.
    fn apply(&self, io: &BrainIo, line: &str, state: &mut RunState) {
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        match event.get("type").and_then(Value::as_str).unwrap_or_default() {
            "system" => {
                if event.get("subtype").and_then(Value::as_str) == Some("init") {
                    state.session_id = event
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    // If the tool server did not connect, the model is about to
                    // answer questions about mail with no way to read any. Say
                    // so now rather than letting it improvise.
                    if let Some(problem) = mcp_problem(&event) {
                        state.failure = Some(problem);
                    }
                }
            }

            // The Anthropic stream events, verbatim — the CLI forwards them
            // untouched, which is why this can hand them to the accumulator the
            // API backend already uses instead of inventing a second decoder.
            "stream_event" => {
                if let Some(text) = text_delta(event.get("event")) {
                    io.ui.delta(&text);
                }
            }

            // The completed turn. Emitting it as an entry is what clears the
            // streaming buffer in the drawer, so the final text is authoritative
            // over the tokens that built it.
            "assistant" => {
                let text = assistant_text(&event);
                if !text.trim().is_empty() {
                    io.ui.agent_text(text.trim());
                }
            }

            "result" => {
                if event.get("is_error").and_then(Value::as_bool) == Some(true) {
                    state.failure = Some(result_error(&event));
                }
            }

            _ => {}
        }
    }
}

/// What one run of the CLI produced.
#[derive(Debug, Default)]
struct RunState {
    session_id: Option<String>,
    /// Set when the run reported a failure the owner has to hear about.
    failure: Option<String>,
}

/// The MCP server's status from the `init` event, when it is bad news.
fn mcp_problem(event: &Value) -> Option<String> {
    let servers = event.get("mcp_servers")?.as_array()?;
    let mach = servers
        .iter()
        .find(|s| s.get("name").and_then(Value::as_str) == Some(SERVER_NAME))?;
    match mach.get("status").and_then(Value::as_str) {
        Some("connected") => None,
        Some(other) => Some(format!(
            "Claude Code could not reach Mach's tools ({other}), so it has no way to read or \
             change your mail. Nothing was done."
        )),
        None => None,
    }
}

fn text_delta(event: Option<&Value>) -> Option<String> {
    let event = event?;
    if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    delta
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

fn assistant_text(event: &Value) -> String {
    let Some(content) = event
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return String::new();
    };
    content
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

/// The sentence in a failed `result` message. The CLI puts it in different
/// places depending on how it failed, so this looks in all of them before
/// falling back to the subtype.
fn result_error(event: &Value) -> String {
    for key in ["result", "error", "message"] {
        if let Some(text) = event.get(key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
    }
    match event.get("subtype").and_then(Value::as_str) {
        Some("error_max_turns") => {
            "Claude Code kept working without reaching an answer, so it was stopped.".to_string()
        }
        Some(other) => format!("Claude Code failed: {other}"),
        None => "Claude Code failed.".to_string(),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .chars()
        .take(300)
        .collect()
}
