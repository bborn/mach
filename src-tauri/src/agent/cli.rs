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
//!
//! # The same door, without the session
//!
//! [`complete_once`] is `claude --print` with no tools, no MCP server and no
//! resume: one prompt in, one answer out. Reply suggestions need that shape and
//! nothing else, and before it existed they went out over
//! [`complete_as`](super::complete::complete_as) — the Anthropic HTTP path —
//! which on a machine with no API key failed instantly and said nothing. A
//! feature whose whole premise is "you already pay for Claude Code" cannot
//! reach the model only through the credential you do not have.
//!
//! It shares this module with [`ClaudeCliBrain`] on purpose: every flag above
//! that decides what the CLI may touch has to be decided the same way here, and
//! two files would eventually disagree about one of them.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::google::BoxFuture;

use super::brain::{Brain, BrainIo};
use super::complete::{usage_of, Completion, CompletionRequest, Cost};
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

// ===========================================================================
// One shot
// ===========================================================================

/// How long one unattended completion may take before the child is killed.
///
/// Nothing is on screen waiting for this, so the number is not a latency
/// budget — it is the point past which a wedged process is a wedged process. A
/// Sonnet answer of three short replies lands in seconds; two minutes is
/// generous for a loaded machine and short enough that a stuck child cannot
/// hold the one-at-a-time permit through a whole afternoon.
pub const ONE_SHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// One prompt, one answer, no session.
///
/// The flags are the session's minus everything a session needs and this does
/// not: no `--mcp-config` (and `--strict-mcp-config` so none of the owner's own
/// servers load either), no `--allowedTools`, no `--resume`. `--tools ""` and
/// `--setting-sources ""` carry the same meaning they do above, and matter more
/// here — this runs unattended, off an inbound email, with nobody watching.
///
/// `--no-session-persistence` is the one flag with no counterpart in a session:
/// a suggestion is thrown away if it is not used, and writing a transcript to
/// disk for every qualifying message that ever arrives would leave a growing
/// pile of files nobody will ever open.
///
/// The prompt goes on stdin for the same reason it does in a session: it is his
/// mail, and an argument vector is public. The *system* prompt does go in the
/// argument vector — it is Mach's own text, the same for every message.
pub async fn complete_once(
    exe: &Path,
    workspace: &Path,
    model: &str,
    request: &CompletionRequest,
) -> Result<Completion, AgentError> {
    tokio::fs::create_dir_all(workspace).await.map_err(|e| {
        AgentError::transport(format!(
            "Mach could not prepare {} for Claude Code: {e}",
            workspace.display()
        ))
    })?;

    let mut command = Command::new(exe);
    command
        .current_dir(workspace)
        .arg("--print")
        .args(["--output-format", "json"])
        .args(["--system-prompt", &request.system])
        .args(["--tools", ""])
        .arg("--strict-mcp-config")
        .args(["--setting-sources", ""])
        .arg("--disable-slash-commands")
        .arg("--no-session-persistence")
        .args(["--model", model])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|e| {
        AgentError::MissingApiKey(format!(
            "Mach could not start Claude Code at {}: {e}. Check that it is still installed, \
             or choose a different agent backend in Preferences → Agent.",
            exe.display()
        ))
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request.prompt.as_bytes())
            .await
            .map_err(|e| AgentError::transport(format!("could not send the prompt: {e}")))?;
        drop(stdin);
    }

    // `wait_with_output` owns the child, so the timeout branch drops it — and
    // `kill_on_drop` is what turns that into a dead process rather than an
    // orphan holding a model call open.
    let output = match tokio::time::timeout(ONE_SHOT_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(AgentError::transport(format!(
                "could not read Claude Code's output: {e}"
            )))
        }
        Err(_) => {
            return Err(AgentError::transport(format!(
                "Claude Code did not answer within {} seconds",
                ONE_SHOT_TIMEOUT.as_secs()
            )))
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    match one_shot_completion(&stdout) {
        Ok(completion) => Ok(completion),
        // A run that produced no readable result *and* exited badly failed to
        // launch — a model name it does not know, a broken install, an expired
        // login — and stderr is the only place that says which.
        Err(_) if !output.status.success() => Err(AgentError::transport(format!(
            "Claude Code exited with {}: {}",
            output.status.code().unwrap_or(-1),
            first_line(&String::from_utf8_lossy(&output.stderr))
        ))),
        Err(error) => Err(error),
    }
}

/// The answer out of `--output-format json`, and what the CLI said it cost.
///
/// One JSON document, in principle. In practice the CLI is allowed to print a
/// diagnostic to stdout before it, so a failed whole-buffer parse falls back to
/// reading the first complete value from the first `{` rather than giving up —
/// the same tolerance [`ClaudeCliBrain::apply`] extends to the stream.
///
/// # Where the dollars come from
///
/// `total_cost_usd` is on the result document, so this path does not price
/// tokens against a table the way the API path has to — it reads the number the
/// program that made the call arrived at. That is better data, because a table
/// goes stale the week a price moves and this does not.
///
/// It is still an estimate rather than an invoice: on a Claude subscription the
/// run draws down a quota and the figure is what the same tokens would have cost
/// on the API. That is the honest thing to show — it is the CLI's own number,
/// and nothing here makes one up.
pub fn one_shot_completion(stdout: &str) -> Result<Completion, AgentError> {
    let Some(event) = result_document(stdout) else {
        return Err(AgentError::Protocol(
            "Claude Code produced no result document".to_string(),
        ));
    };
    if event.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(AgentError::Api {
            status: 200,
            message: result_error(&event),
        });
    }
    match event.get("result").and_then(Value::as_str) {
        Some(text) => Ok(Completion {
            text: text.to_string(),
            cost: one_shot_cost(&event),
        }),
        None => Err(AgentError::Protocol(
            "Claude Code's result carried no text".to_string(),
        )),
    }
}

/// What the CLI said the run cost.
///
/// A negative or non-numeric `total_cost_usd` is absent rather than clamped: an
/// implausible figure is a version of the CLI this build does not understand,
/// and the count limit is what protects the owner when that happens.
pub fn one_shot_cost(event: &Value) -> Cost {
    let usd = event
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0);
    Cost {
        usd,
        ..Cost::of(&usage_of(event))
    }
}

/// The text alone, for callers with nothing to charge it to.
pub fn one_shot_text(stdout: &str) -> Result<String, AgentError> {
    one_shot_completion(stdout).map(|completion| completion.text)
}

fn result_document(stdout: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(stdout.trim()) {
        if value.is_object() {
            return Some(value);
        }
    }
    let start = stdout.find('{')?;
    serde_json::Deserializer::from_str(&stdout[start..])
        .into_iter::<Value>()
        .next()?
        .ok()
        .filter(Value::is_object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_field_is_the_answer() {
        let body = r#"{"type":"result","subtype":"success","is_error":false,
                       "result":"[{\"label\":\"Yes\",\"body\":\"Tuesday works.\"}]"}"#;
        assert_eq!(
            one_shot_text(body).unwrap(),
            r#"[{"label":"Yes","body":"Tuesday works."}]"#
        );
    }

    #[test]
    fn the_cli_reports_its_own_price_and_this_reads_it() {
        // The reason the API's price table is not the whole story: on the path
        // this app actually runs, the number comes back with the answer.
        let body = r#"{"type":"result","subtype":"success","is_error":false,
                       "result":"[]","total_cost_usd":0.0193,
                       "usage":{"input_tokens":2100,"output_tokens":380,
                                "cache_read_input_tokens":1024}}"#;
        let completion = one_shot_completion(body).unwrap();
        assert_eq!(completion.cost.usd, Some(0.0193));
        assert_eq!(completion.cost.input_tokens, Some(3124));
        assert_eq!(completion.cost.output_tokens, Some(380));
    }

    #[test]
    fn a_run_that_reported_no_price_reports_none_rather_than_free() {
        let body = r#"{"type":"result","result":"ok"}"#;
        assert_eq!(one_shot_completion(body).unwrap().cost, Cost::default());
        // And a figure that cannot be true is the same as no figure.
        let odd = r#"{"type":"result","result":"ok","total_cost_usd":-1}"#;
        assert_eq!(one_shot_completion(odd).unwrap().cost.usd, None);
    }

    #[test]
    fn a_diagnostic_before_the_document_is_stepped_over() {
        let body = "warning: something\n{\"type\":\"result\",\"result\":\"ok\"}\n";
        assert_eq!(one_shot_text(body).unwrap(), "ok");
    }

    #[test]
    fn a_failed_run_is_an_error_with_the_cli_s_own_sentence() {
        let body = r#"{"type":"result","is_error":true,"result":"Invalid model name"}"#;
        match one_shot_text(body) {
            Err(AgentError::Api { message, .. }) => assert_eq!(message, "Invalid model name"),
            other => panic!("expected an API error, got {other:?}"),
        }
    }

    #[test]
    fn nothing_readable_is_a_protocol_error_rather_than_an_empty_answer() {
        assert!(matches!(one_shot_text(""), Err(AgentError::Protocol(_))));
        assert!(matches!(
            one_shot_text("command not found"),
            Err(AgentError::Protocol(_))
        ));
        assert!(matches!(
            one_shot_text(r#"{"type":"result"}"#),
            Err(AgentError::Protocol(_))
        ));
    }
}
