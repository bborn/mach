//! Any other agent, plugged in — the `command` backend.
//!
//! Claude Code is the default because it is what this app's owner has. It is
//! not meant to be the only possibility, and the way to make that true is not to
//! grow a matrix of vendors inside Mach: it is to write down what a program has
//! to do to be Mach's brain, and then run any program that does it.
//!
//! # The contract
//!
//! Mach spawns the configured program **once per message** and gives it:
//!
//! | channel | what it carries |
//! |---|---|
//! | stdin | the message: the `<context>` block, the conversation so far on a follow-up, and the owner's sentence. Closed after writing, so a read to EOF terminates. |
//! | `MACH_SYSTEM_PROMPT` | who you are, what time it is, whose mail this is |
//! | `MACH_MCP_CONFIG` | path to an MCP config file (`{"mcpServers":{"mach":{…}}}`, the Claude Code spelling) — the tools |
//! | `MACH_MCP_URL` / `MACH_MCP_TOKEN` | the same server, for a program that speaks MCP itself and would rather not parse a config file |
//! | `MACH_SESSION_ID` | stable across the messages of one session |
//! | cwd | a scratch directory owned by Mach |
//! | stdout | **the answer**, as plain text. Streamed to the drawer as it arrives. |
//! | stderr | diagnostics. Shown only if the program fails. |
//! | exit code | `0` is an answer; anything else fails the session with the first line of stderr. |
//!
//! There is no protocol to implement and nothing to serialise. A three-line
//! shell script satisfies this, and so does a program that never touches the
//! MCP server at all — it will simply be an agent that can talk but not act,
//! which is a legitimate thing to want and an honest thing to get.
//!
//! # What it cannot do
//!
//! It cannot reach the mailbox except through the MCP server, and that server
//! answers to [`ToolGate`](super::gate::ToolGate) — so a plugged-in agent gets
//! precisely the tools the built-in one gets, with the same approval rule,
//! enforced in this process. "Configure any agent you like" does not mean
//! "configure any agent to send mail unattended".
//!
//! The token in `MACH_MCP_TOKEN` is good for one session and dies with it.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::google::BoxFuture;

use super::brain::{Brain, BrainIo};
use super::error::AgentError;
use super::mcp::McpServer;

/// How often the run loop notices that the session was closed.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(150);

pub struct CommandBrain {
    program: PathBuf,
    args: Vec<String>,
}

impl CommandBrain {
    pub fn new(program: PathBuf, args: Vec<String>) -> CommandBrain {
        CommandBrain { program, args }
    }
}

impl Brain for CommandBrain {
    fn drive<'a>(&'a self, io: BrainIo) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(self.run(io))
    }
}

impl CommandBrain {
    async fn run(&self, mut io: BrainIo) -> Result<(), AgentError> {
        let server = McpServer::start(
            Arc::clone(&io.gate),
            tokio::runtime::Handle::current(),
            &io.workspace,
            &io.session_id,
        )?;

        let mut message = io.first_message.clone();

        loop {
            if io.is_cancelled() {
                return Ok(());
            }
            self.turn(&io, &server, &message).await?;

            match io.idle().await {
                Some(next) => {
                    io.ui.user_text(&next);
                    // A stateless program cannot remember the last exchange, so
                    // Mach hands it back. A stateful one can key off
                    // `MACH_SESSION_ID` and ignore this.
                    message = format!("{}\n\n{next}", io.ui.transcript());
                }
                None => return Ok(()),
            }
        }
    }

    async fn turn(
        &self,
        io: &BrainIo,
        server: &McpServer,
        message: &str,
    ) -> Result<(), AgentError> {
        let endpoint = server.endpoint();
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .current_dir(&io.workspace)
            .env("MACH_SYSTEM_PROMPT", &io.system)
            .env("MACH_MCP_CONFIG", server.config_path())
            .env("MACH_MCP_URL", &endpoint.url)
            .env("MACH_MCP_TOKEN", &endpoint.token)
            .env("MACH_SESSION_ID", &io.session_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                AgentError::MissingApiKey(format!(
                    "Mach could not start the agent command {}: {e}. Fix it in \
                     Preferences → Agent, or choose a different backend.",
                    self.program.display()
                ))
            })?;

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
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_string(&mut collected).await;
            }
            collected
        });

        // Read bytes rather than lines: a program that streams a sentence
        // without a trailing newline should still appear in the drawer as it
        // types, and a line-oriented reader would hold it until it finished.
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| AgentError::transport("the agent command produced no output"))?,
        );
        let mut answer = String::new();
        let mut buffer = [0u8; 4096];

        loop {
            let read = tokio::select! {
                read = stdout.read(&mut buffer) => read,
                _ = tokio::time::sleep(CANCEL_POLL) => {
                    if io.is_cancelled() {
                        let _ = child.start_kill();
                        return Ok(());
                    }
                    continue;
                }
            };
            match read {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buffer[..n]).to_string();
                    io.ui.delta(&chunk);
                    answer.push_str(&chunk);
                }
                Err(e) => {
                    return Err(AgentError::transport(format!(
                        "could not read the agent command's output: {e}"
                    )))
                }
            }
        }

        let status = child.wait().await;
        let stderr = errors.await.unwrap_or_default();

        match status {
            Ok(status) if status.success() => {
                if !answer.trim().is_empty() {
                    io.ui.agent_text(answer.trim());
                }
                Ok(())
            }
            Ok(status) => Err(AgentError::Api {
                status: 200,
                message: format!(
                    "the agent command exited with {}: {}",
                    status.code().unwrap_or(-1),
                    first_line(&stderr)
                ),
            }),
            Err(e) => Err(AgentError::transport(format!(
                "the agent command could not be waited on: {e}"
            ))),
        }
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
