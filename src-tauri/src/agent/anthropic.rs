//! The Anthropic Messages API as a brain — the original loop, unchanged in
//! behaviour, now sitting behind the same gate as everything else.
//!
//! This is the backend for people who would rather spend an API key than
//! install a CLI, and it is the one the tests drive, because a scripted
//! [`ModelTransport`] can replay the exact bytes the API sends without a
//! network. Nothing about the loop changed when the seam went in: the same
//! streaming, the same single `user` message carrying every tool result, the
//! same turn ceiling. The only difference is that the policy check and the
//! approval wait moved out of here and into [`ToolGate`], which is what makes
//! "approval is enforced by Mach, not by the model's host" true for *all three*
//! backends rather than for this one by accident.

use std::sync::Arc;

use serde_json::Value;

use crate::google::BoxFuture;

use super::brain::{Brain, BrainIo};
use super::config::AgentConfig;
use super::error::AgentError;
use super::gate::GateResult;
use super::session::SessionStatus;
use super::wire::{
    self, AssistantTurn, ModelTransport, SseDecoder, StreamSignal, ToolDefinition, TurnAccumulator,
    TurnRequest,
};

/// How many model turns one session may take before it is stopped.
///
/// A session that has called forty tools without answering is stuck, and the
/// owner is paying for it. Generous enough that "search, read three threads,
/// draft, schedule" never comes close.
const MAX_TURNS: usize = 24;

pub struct AnthropicBrain {
    config: AgentConfig,
    transport: Arc<dyn ModelTransport>,
}

impl AnthropicBrain {
    pub fn new(config: AgentConfig, transport: Arc<dyn ModelTransport>) -> AnthropicBrain {
        AnthropicBrain { config, transport }
    }
}

impl Brain for AnthropicBrain {
    fn drive<'a>(&'a self, io: BrainIo) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(self.run(io))
    }
}

impl AnthropicBrain {
    async fn run(&self, mut io: BrainIo) -> Result<(), AgentError> {
        let tool_defs: Vec<ToolDefinition> = io.gate.definitions();
        let mut messages = vec![wire::user_text(io.first_message.clone())];

        for _ in 0..MAX_TURNS {
            if io.is_cancelled() {
                return Ok(());
            }
            io.ui.set_status(SessionStatus::Running);

            let request = TurnRequest {
                system: io.system.clone(),
                messages: messages.clone(),
                tools: tool_defs.clone(),
            };
            let turn = self.stream_turn(&io, &request).await?;

            if io.is_cancelled() {
                return Ok(());
            }

            if turn.is_refusal() {
                return Err(AgentError::Api {
                    status: 200,
                    message: "the model declined this request".to_string(),
                });
            }

            messages.push(wire::assistant_message(&turn.content));

            let text = turn.text();
            if !text.trim().is_empty() {
                io.ui.agent_text(text.trim());
            }

            if !turn.wants_tools() {
                match io.idle().await {
                    Some(next) => {
                        io.ui.user_text(&next);
                        messages.push(wire::user_text(next));
                        continue;
                    }
                    None => return Ok(()),
                }
            }

            let results = match self.run_tools(&io, &turn).await? {
                Some(results) => results,
                // The session was closed while a tool was parked on the owner.
                None => return Ok(()),
            };
            if io.is_cancelled() {
                return Ok(());
            }
            messages.push(wire::tool_results_message(results));
        }

        Err(AgentError::invalid(
            "the agent kept working without reaching an answer, so it was stopped",
        ))
    }

    /// One model turn, streamed. Emits deltas as they arrive.
    async fn stream_turn(
        &self,
        io: &BrainIo,
        request: &TurnRequest,
    ) -> Result<AssistantTurn, AgentError> {
        let mut rx = match self
            .transport
            .send(wire::build_call(&self.config, request, self.config.fallbacks))
            .await
        {
            Ok(rx) => rx,
            // An account without the fallback beta must still get an agent.
            Err(error) if self.config.fallbacks && wire::is_fallback_rejection(&error) => {
                self.transport
                    .send(wire::build_call(&self.config, request, false))
                    .await?
            }
            Err(error) => return Err(error),
        };

        let mut decoder = SseDecoder::new();
        let mut accumulator = TurnAccumulator::new();

        while let Some(chunk) = rx.recv().await {
            if io.is_cancelled() {
                break;
            }
            for payload in decoder.push(&chunk?) {
                for signal in accumulator.apply(&payload)? {
                    match signal {
                        StreamSignal::TextDelta(text) => io.ui.delta(&text),
                        StreamSignal::ToolStarted { id, name } => {
                            // Attributed the moment it starts, before the gate
                            // has even seen it: the owner has to be able to see
                            // *which third party* is touching their mailbox, not
                            // just that something is.
                            let summary = io.gate.running_summary(&name);
                            io.ui.tool_running(&id, &name, &summary);
                        }
                        StreamSignal::Done => {}
                    }
                }
            }
        }

        Ok(accumulator.finish())
    }

    /// Every tool the turn asked for, through the gate, in order.
    ///
    /// `None` means the session was closed mid-flight — the caller returns
    /// rather than sending a half-finished set of results back to a model
    /// nobody is reading.
    async fn run_tools(
        &self,
        io: &BrainIo,
        turn: &AssistantTurn,
    ) -> Result<Option<Vec<Value>>, AgentError> {
        let mut results = Vec::new();

        for call in turn.tool_uses() {
            if io.is_cancelled() {
                return Ok(Some(results));
            }
            match io.gate.run(&call.id, &call.name, &call.input).await {
                result @ (GateResult::Ok(_) | GateResult::Refused(_)) => {
                    let (content, is_error) = result
                        .as_tool_result()
                        .expect("ok and refused always have a tool result");
                    results.push(wire::tool_result(&call.id, &content, is_error));
                }
                GateResult::Closed => return Ok(None),
                GateResult::Fatal(error) => return Err(error),
            }
        }

        Ok(Some(results))
    }
}
