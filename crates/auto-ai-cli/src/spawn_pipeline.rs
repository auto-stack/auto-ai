//! spawn_pipeline tool — lets the assistant start a multi-agent pipeline
//! from within a chat session. The assistant classifies the task complexity
//! (NORMAL / SUPERPOWERS / RELAY) per its soul instructions, then calls this
//! tool to delegate to a pipeline.
//!
//! (Plan 009 — Execution Modes)

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use auto_ai_agent::{
    Agent, AgentFactory, Client, FlowSpec, FlowStep, PipelineDriver, PipelineEvent,
    Tool, ToolError, HandoffDocument,
};

/// Build a flow spec for a named mode.
pub fn flow_for(mode: &str) -> Option<FlowSpec> {
    match mode {
        "superpowers" => {
            let mut f = FlowSpec::new("superpowers");
            f.add_step(FlowStep::new("brainstorm", "assistant"));
            f.add_step(FlowStep::new("plan", "assistant"));
            f.add_step(FlowStep::new("execute", "coder"));
            f.add_step(FlowStep::new("review", "reviewer"));
            Some(f)
        }
        "relay" => {
            let mut f = FlowSpec::new("relay");
            f.add_step(FlowStep::new("design", "architect"));
            f.add_step(FlowStep::new("implement", "coder"));
            f.add_step(FlowStep::new("test", "tester"));
            f.add_step(FlowStep::new("review", "reviewer"));
            Some(f)
        }
        _ => None,
    }
}

/// AgentFactory for the CLI pipeline — builds an agent with the standard
/// tool set for each step's role.
pub struct CliAgentFactory {
    client: Arc<dyn Client>,
}

impl CliAgentFactory {
    pub fn new(client: Arc<dyn Client>) -> Self {
        Self { client }
    }
}

impl AgentFactory for CliAgentFactory {
    fn build_agent(
        &self,
        role_id: &str,
        handoff: Option<&auto_ai_agent::HandoffDocument>,
    ) -> Result<Agent, String> {
        let role = auto_ai_agent::load_builtin(role_id)
            .ok_or_else(|| format!("unknown role '{role_id}'"))?;
        let mut agent = Agent::new(crate::OwnedRole(role), self.client.clone());
        agent.register_tool(crate::tools::ReadFile);
        agent.register_tool(crate::tools::WriteFile);
        agent.register_tool(crate::tools::EditFile);
        agent.register_tool(crate::tools::ListDir);
        agent.register_tool(crate::tools::Search);
        agent.register_tool(crate::tools::RunCommand);
        // If there's a handoff, inject it as context.
        if let Some(h) = handoff {
            agent = Agent::with_context(agent, h.render());
        }
        Ok(agent)
    }
}

/// The spawn_pipeline tool. Registered on the assistant agent in chat mode.
/// When the assistant decides a task needs SUPERPOWERS or RELAY mode, it
/// calls this tool with the flow name and task description.
pub struct SpawnPipeline {
    client: Arc<dyn Client>,
}

impl SpawnPipeline {
    pub fn new(client: Arc<dyn Client>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for SpawnPipeline {
    fn name(&self) -> &str {
        "spawn_pipeline"
    }
    fn description(&self) -> &str {
        "Start a multi-agent pipeline for complex tasks. Use this when the task \
         needs multiple steps across multiple files. \
         flow='superpowers': brainstorm→plan→execute→review (medium tasks, 2-6 files). \
         flow='relay': design→implement→test→review (complex multi-module tasks). \
         The pipeline runs to completion and returns a summary."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "flow": {
                    "type": "string",
                    "description": "'superpowers' or 'relay'"
                },
                "task": {
                    "type": "string",
                    "description": "the task for the pipeline to execute"
                }
            },
            "required": ["flow", "task"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let flow_name = args["flow"]
            .as_str()
            .ok_or_else(|| ToolError::Args("missing 'flow' argument".into()))?;
        let task = args["task"]
            .as_str()
            .ok_or_else(|| ToolError::Args("missing 'task' argument".into()))?;

        let flow = flow_for(flow_name)
            .ok_or_else(|| ToolError::Args(format!(
                "unknown flow '{flow_name}'. Use 'superpowers' or 'relay'."
            )))?;

        eprintln!("\n  ┌─ pipeline: {} ─────────────────", flow_name);
        eprintln!("  │ task: {task}");
        eprintln!("  │");

        let factory = CliAgentFactory::new(self.client.clone());
        let mut driver = PipelineDriver::new(flow, factory, task);

        let mut final_summary = String::new();
        let result = driver
            .drive(
                task,
                Arc::new(|ev: PipelineEvent| {
                    match ev {
                        PipelineEvent::StepStarted { step_id, role_id } => {
                            eprintln!("  │ ▶ step '{step_id}' (role: {role_id})");
                        }
                        PipelineEvent::Delta { text } => {
                            eprint!("{text}");
                        }
                        PipelineEvent::Tool { tool, result } => {
                            let preview: String = result.chars().take(60).collect();
                            eprintln!("\n  │   [tool] {tool} → {preview}…");
                        }
                        PipelineEvent::StepCompleted { step_id, handoff } => {
                            eprintln!("\n  │ ✓ step '{step_id}' done");
                            if !handoff.summary.is_empty() {
                                final_summary_clone(&handoff.summary);
                            }
                        }
                        PipelineEvent::Completed => {
                            eprintln!("  │");
                            eprintln!("  └─ pipeline complete ──────────");
                        }
                        PipelineEvent::Failed { error } => {
                            eprintln!("  │ ✗ pipeline failed: {error}");
                        }
                        PipelineEvent::Paused { step_id, reason } => {
                            eprintln!("  │ ⏸ paused at '{step_id}': {reason}");
                        }
                        PipelineEvent::GateWaiting { step_id } => {
                            eprintln!("  │ ⏸ gate waiting at '{step_id}' (auto-approving…)");
                        }
                        PipelineEvent::BudgetWarning { remaining } => {
                            eprintln!("  │ ⚠ budget warning: {remaining} tokens remaining");
                        }
                    }
                }),
            )
            .await;

        // Extract final summary from the engine's step history.
        let summary = driver
            .engine()
            .step_history
            .last()
            .and_then(|r| r.handoff.as_ref())
            .map(|h| h.summary.clone())
            .unwrap_or_else(|| "Pipeline completed.".into());

        match result {
            Ok(_) => Ok(format!(
                "Pipeline '{flow_name}' completed successfully.\n\nSummary:\n{summary}"
            )),
            Err(e) => Ok(format!(
                "Pipeline '{flow_name}' encountered an error: {e}\n\nPartial summary:\n{summary}"
            )),
        }
    }
}

// Helper to avoid Arc<Mutex> for the final summary (printed to stderr, not
// returned through the closure).
fn final_summary_clone(_s: &str) {
    // The summary is retrieved from the engine's history after drive() returns.
    // This function is a no-op placeholder; the actual extraction happens below.
}
