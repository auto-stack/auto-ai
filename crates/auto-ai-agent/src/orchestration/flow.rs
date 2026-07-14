//! Flow Specification — declarative pipeline definitions.
//!
//! A flow is an ordered list of steps that the [`PipelineEngine`] executes.
//! The orchestrator is pure Rust state-machine code — zero LLM tokens are
//! spent deciding what to do next.
//!
//! (Plan 008 Phase 2 — moved from musk relay/flow.rs. The type definitions
//! are generic; app-specific built-in flow instances stay in the app.)

use serde::{Deserialize, Serialize};

/// A flow is an ordered list of steps with routing logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowSpec {
    pub id: String,
    pub steps: Vec<FlowStep>,
}

impl FlowSpec {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            steps: Vec::new(),
        }
    }

    pub fn add_step(&mut self, step: FlowStep) -> &mut Self {
        self.steps.push(step);
        self
    }

    pub fn get_step(&self, step_id: &str) -> Option<&FlowStep> {
        self.steps.iter().find(|s| s.id == step_id)
    }

    pub fn get_step_index(&self, step_id: &str) -> Option<usize> {
        self.steps.iter().position(|s| s.id == step_id)
    }
}

/// A single step in a flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowStep {
    pub id: String,
    /// Which role/profession executes this step (e.g. "coder", "reviewer").
    pub role_id: String,
    /// Gate type controlling whether a step needs human approval.
    #[serde(default)]
    pub gate: GateType,
    /// Max LLM turns before forced handoff (overrides role default).
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// How to route after this step completes.
    #[serde(default)]
    pub exit: ExitRouting,
    /// Optional per-step token budget (overrides role default).
    #[serde(default)]
    pub token_budget: Option<u64>,
}

impl FlowStep {
    pub fn new(id: impl Into<String>, role_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role_id: role_id.into(),
            gate: GateType::Auto,
            max_turns: None,
            exit: ExitRouting::Next,
            token_budget: None,
        }
    }

    pub fn with_gate(mut self, gate: GateType) -> Self {
        self.gate = gate;
        self
    }

    pub fn with_exit(mut self, exit: ExitRouting) -> Self {
        self.exit = exit;
        self
    }

    pub fn with_budget(mut self, budget: u64) -> Self {
        self.token_budget = Some(budget);
        self
    }
}

/// Gate type controlling whether a step needs human approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateType {
    /// Proceed automatically.
    Auto,
    /// Pause for human approval before executing.
    Human,
}

impl Default for GateType {
    fn default() -> Self {
        GateType::Auto
    }
}

/// Routing logic after a step completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitRouting {
    /// Go to the next step in sequence.
    Next,
    /// Loop back to a target step (e.g., coder→tester iteration).
    Loop {
        /// Step to return to.
        target_step_id: String,
        /// Max iterations before breaking to next (or Paused).
        max_iterations: u32,
    },
}

impl Default for ExitRouting {
    fn default() -> Self {
        ExitRouting::Next
    }
}

/// Decision a human makes at a gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDecision {
    /// Approve — continue to next step.
    Approve,
    /// Reject — redraft the same step with feedback.
    Reject { feedback: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_flow() {
        let mut flow = FlowSpec::new("test");
        flow.add_step(FlowStep::new("design", "architect"));
        flow.add_step(FlowStep::new("code", "coder").with_exit(ExitRouting::Loop {
            target_step_id: "design".into(),
            max_iterations: 2,
        }));
        assert_eq!(flow.steps.len(), 2);
        assert_eq!(flow.get_step("code").unwrap().role_id, "coder");
        assert!(matches!(
            &flow.get_step("code").unwrap().exit,
            ExitRouting::Loop { max_iterations: 2, .. }
        ));
    }

    #[test]
    fn gate_human_flow() {
        let mut flow = FlowSpec::new("gated");
        flow.add_step(FlowStep::new("review", "reviewer").with_gate(GateType::Human));
        assert_eq!(flow.get_step("review").unwrap().gate, GateType::Human);
    }
}
