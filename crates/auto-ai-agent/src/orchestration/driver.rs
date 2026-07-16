//! Pipeline Driver — the orchestration loop that bridges PipelineEngine ↔ Agent.
//!
//! Apps implement [`AgentFactory`] to tell the driver how to build an agent for
//! each step (with their own tools, context, safety rules). The driver then:
//! 1. Advances the pipeline engine.
//! 2. On `ExecuteStep` → builds an agent via the factory → runs it → constructs
//!    a `HandoffDocument` from the result → submits it to the engine.
//! 3. On `WaitForHuman` → emits a gate event (the app decides how to handle it).
//! 4. Loops until Completed / Failed / Paused.
//!
//! (Plan 008 Phase 5)

use std::sync::Arc;

use super::flow::FlowSpec;
use super::handoff::HandoffDocument;
use super::pipeline::{AdvanceResult, GateDecision, PipelineEngine};

use crate::agent::{Agent, AgentResult, StreamEvent};
use crate::error::AgentError;

/// Events emitted by the pipeline driver during execution.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// A step started executing.
    StepStarted { step_id: String, role_id: String },
    /// A text delta from the current step's agent (streaming).
    Delta { text: String },
    /// A tool call result from the current step's agent.
    Tool { tool: String, result: String },
    /// A step completed and its handoff was submitted.
    StepCompleted { step_id: String, handoff: HandoffDocument },
    /// A human gate is waiting for approval.
    GateWaiting { step_id: String },
    /// The pipeline completed successfully.
    Completed,
    /// The pipeline failed.
    Failed { error: String },
    /// A loop reached max iterations — paused.
    Paused { step_id: String, reason: String },
    /// Token budget warning.
    BudgetWarning { remaining: u64 },
}

/// Factory trait: apps implement this to build agents for each pipeline step.
///
/// The factory receives the role name (e.g. "coder") and the previous step's
/// handoff (if any). It returns a fully-configured `Agent` ready to run.
pub trait AgentFactory: Send + Sync {
    fn build_agent(
        &self,
        role_id: &str,
        handoff: Option<&HandoffDocument>,
    ) -> Result<Agent, String>;
}

/// The orchestration driver. Owns a `PipelineEngine` + an `AgentFactory`.
pub struct PipelineDriver<F: AgentFactory> {
    engine: PipelineEngine,
    factory: F,
    /// Optional gate handler. When a human gate is encountered, this is called
    /// to decide the gate outcome. If `None`, gates are auto-approved.
    gate_handler: Option<Box<dyn Fn(&str) -> GateDecision + Send + Sync>>,
}

impl<F: AgentFactory> PipelineDriver<F> {
    /// Create a new driver from a flow spec, a factory, and a task.
    pub fn new(flow: FlowSpec, factory: F, task: &str) -> Self {
        let run_id = format!("pipeline-{}", now_secs());
        let engine = PipelineEngine::new(flow, run_id);
        // The task is the "user message" for the first step. We store it as
        // a synthetic initial handoff so the factory can inject it.
        let _ = task; // The factory receives role_id + handoff=None for step 0;
                      // it should use the task directly (passed separately).
        Self { engine, factory, gate_handler: None }
    }

    /// Set a custom gate handler. When a human gate is encountered during
    /// `drive()`, this handler is called with the step_id to decide whether
    /// to Approve or Reject (with feedback).
    ///
    /// If no handler is set, gates are auto-approved.
    pub fn with_gate_handler(
        mut self,
        handler: Box<dyn Fn(&str) -> GateDecision + Send + Sync>,
    ) -> Self {
        self.gate_handler = Some(handler);
        self
    }

    /// Run the pipeline to completion (or until a gate/pause/failure).
    ///
    /// `on_event` receives pipeline events for the app to display/log.
    /// `task` is the user's original request, passed to the first agent.
    pub async fn drive(
        &mut self,
        task: &str,
        on_event: Arc<dyn Fn(PipelineEvent) + Send + Sync>,
    ) -> Result<(), AgentError> {
        let mut last_handoff: Option<HandoffDocument> = None;

        loop {
            let result = self.engine.advance();

            match result {
                AdvanceResult::ExecuteStep { step_id, role_id } => {
                    on_event(PipelineEvent::StepStarted {
                        step_id: step_id.clone(),
                        role_id: role_id.clone(),
                    });

                    // Build the agent for this step.
                    let mut agent = self
                        .factory
                        .build_agent(&role_id, last_handoff.as_ref())
                        .map_err(AgentError::Config)?;

                    // Construct the input: original task for step 0, or
                    // the handoff render for subsequent steps.
                    let input = if let Some(h) = &last_handoff {
                        h.render()
                    } else {
                        task.to_string()
                    };

                    // Run the agent with streaming events.
                    let collected = Arc::new(std::sync::Mutex::new(String::new()));
                    let tool_calls: Arc<std::sync::Mutex<Vec<(String, String)>>> =
                        Arc::new(std::sync::Mutex::new(Vec::new()));
                    let event_cb = on_event.clone();
                    let col_clone = collected.clone();
                    let tc_clone = tool_calls.clone();

                    let stream_cb: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |ev| {
                        match ev {
                            StreamEvent::Delta { text } => {
                                col_clone.lock().unwrap().push_str(&text);
                                event_cb(PipelineEvent::Delta { text });
                            }
                            StreamEvent::Tool { tool, result, .. } => {
                                tc_clone.lock().unwrap().push((tool.clone(), result.clone()));
                                event_cb(PipelineEvent::Tool { tool, result });
                            }
                            StreamEvent::Done { .. } | StreamEvent::Error { .. } => {}
                        }
                    });

                    let agent_result = agent.run_stream(&input, stream_cb).await?;

                    // Build handoff from the result.
                    let content = collected.lock().unwrap().clone();
                    let handoff = self.build_handoff(&step_id, &role_id, &agent_result, &content);
                    on_event(PipelineEvent::StepCompleted {
                        step_id: step_id.clone(),
                        handoff: handoff.clone(),
                    });

                    last_handoff = Some(handoff);

                    // Submit to engine — it will route to the next step.
                    let next = self.engine.submit_handoff(last_handoff.clone().unwrap());
                    match next {
                        AdvanceResult::Completed => {
                            on_event(PipelineEvent::Completed);
                            return Ok(());
                        }
                        AdvanceResult::Failed { error } => {
                            on_event(PipelineEvent::Failed { error: error.clone() });
                            return Err(AgentError::Config(error));
                        }
                        AdvanceResult::Paused { step_id, reason } => {
                            on_event(PipelineEvent::Paused { step_id, reason });
                            return Ok(()); // Paused is not an error — app can resume.
                        }
                        AdvanceResult::WaitForHuman { step_id } => {
                            on_event(PipelineEvent::GateWaiting {
                                step_id: step_id.clone(),
                            });
                            let decision = if let Some(ref handler) = self.gate_handler {
                                handler(&step_id)
                            } else {
                                GateDecision::Approve
                            };
                            let gate_result = self.engine.resolve_gate(decision);
                            if let AdvanceResult::Failed { error } = gate_result {
                                on_event(PipelineEvent::Failed { error: error.clone() });
                                return Err(AgentError::Config(
                                    "gate resolution failed".into(),
                                ));
                            }
                            // Continue the loop — the next advance() will execute the step.
                        }
                        AdvanceResult::ExecuteStep { .. } => {
                            // Engine already advanced to the next step — continue loop.
                        }
                    }
                }
                AdvanceResult::Completed => {
                    on_event(PipelineEvent::Completed);
                    return Ok(());
                }
                AdvanceResult::Failed { error } => {
                    on_event(PipelineEvent::Failed { error: error.clone() });
                    return Err(AgentError::Config(error));
                }
                AdvanceResult::Paused { step_id, reason } => {
                    on_event(PipelineEvent::Paused { step_id, reason });
                    return Ok(());
                }
                AdvanceResult::WaitForHuman { step_id } => {
                    on_event(PipelineEvent::GateWaiting {
                        step_id: step_id.clone(),
                    });
                    let decision = if let Some(ref handler) = self.gate_handler {
                        handler(&step_id)
                    } else {
                        GateDecision::Approve
                    };
                    let gate_result = self.engine.resolve_gate(decision);
                    if let AdvanceResult::Failed { error } = gate_result {
                        on_event(PipelineEvent::Failed { error });
                        return Err(AgentError::Config("gate resolution failed".into()));
                    }
                }
            }
        }
    }

    /// Construct a HandoffDocument from an agent's result.
    fn build_handoff(
        &self,
        _step_id: &str,
        role_id: &str,
        result: &AgentResult,
        content: &str,
    ) -> HandoffDocument {
        let mut h = HandoffDocument::new(role_id, "next");
        h.summary = content.chars().take(200).collect::<String>();
        // Extract work product from tool calls (files written).
        for tc in &result.tool_calls {
            if tc.tool == "write_file" || tc.tool == "edit_file" {
                // Extract path from args if possible.
                h.work_product.push(super::handoff::WorkProduct {
                    path: tc.args.to_string(),
                    description: tc.tool.clone(),
                    lines: None,
                });
            }
        }
        h.token_usage.step_tokens = result.total_tokens as u64;
        h
    }

    /// Access the underlying engine (for pause/resume/rerun).
    pub fn engine(&self) -> &PipelineEngine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut PipelineEngine {
        &mut self.engine
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
