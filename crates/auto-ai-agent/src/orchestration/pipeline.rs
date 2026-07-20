//! Pipeline Engine — the deterministic state machine that executes flows.
//!
//! Pure Rust code: zero LLM tokens are spent on orchestration. The engine's
//! `advance() / submit_handoff() / resolve_gate()` triad drives an external
//! loop (the PipelineDriver).
//!
//! (Plan 008 Phase 3 — moved from musk relay/pipeline.rs. Changes:
//! - `profession_id` → `role_id` (matching FlowStep rename)
//! - removed `agent_config_id` (musk-specific)
//! - removed `Edit` variant from GateDecision (simplify for generic use)
//! - TokenUsage fields adapted to match generic HandoffDocument)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::budget::{BudgetAction, BudgetTracker, TokenBudget};
use super::flow::{ExitRouting, FlowSpec, FlowStep, GateType};
use super::handoff::HandoffDocument;

/// Execution mode controlling human-gate behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineMode {
    /// Autonomous — human gates still pause for approval.
    Auto,
    /// Human reviews every step.
    Interactive,
}

impl Default for PipelineMode {
    fn default() -> Self {
        PipelineMode::Auto
    }
}

/// Result of advancing the pipeline — tells the caller what to do next.
#[derive(Debug, Clone, PartialEq)]
pub enum AdvanceResult {
    /// Run the agent for this step, then call `submit_handoff()`.
    ExecuteStep { step_id: String, role_id: String },
    /// Pause for human approval at a gate.
    WaitForHuman { step_id: String },
    /// Flow completed successfully.
    Completed,
    /// Flow failed.
    Failed { error: String },
    /// Loop reached max iterations — manual resume required.
    Paused { step_id: String, reason: String },
}

/// Decision a human makes at a gate.
#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    /// Approve and continue.
    Approve,
    /// Reject and redraft the same step, with feedback.
    Reject { feedback: String },
}

/// Record of a completed step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_id: String,
    pub role_id: String,
    pub handoff: Option<HandoffDocument>,
    pub started_at: u64,
    pub completed_at: u64,
    pub iteration: u32,
}

/// The pipeline engine state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineEngine {
    pub flow: FlowSpec,
    pub current_step: usize,
    pub status: PipelineStatus,
    pub run_id: String,
    pub step_history: Vec<StepRecord>,
    pub loop_counters: HashMap<String, u32>,
    pub pending_gate: Option<PendingGate>,
    pub gate_feedback: HashMap<String, Vec<String>>,
    pub gate_resolved_for_step: Option<String>,
    #[serde(default)]
    pub resumed_step_id: Option<String>,
    pub cumulative_tokens: u64,
    pub budget_tracker: BudgetTracker,
    pub mode: PipelineMode,
}

/// Current state of the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PipelineStatus {
    Idle,
    Running { step_id: String, role_id: String, started_at: u64 },
    WaitingForHuman { step_id: String, since: u64 },
    Completed,
    Failed { error: String },
    Paused { at_step: usize },
}

impl PipelineStatus {
    pub fn to_status_str(&self) -> String {
        match self {
            PipelineStatus::Idle => "idle".into(),
            PipelineStatus::Running { .. } => "running".into(),
            PipelineStatus::WaitingForHuman { .. } => "waiting_approval".into(),
            PipelineStatus::Completed => "completed".into(),
            PipelineStatus::Failed { .. } => "failed".into(),
            PipelineStatus::Paused { .. } => "paused".into(),
        }
    }
}

/// A gate awaiting human resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingGate {
    pub step_id: String,
    pub since: u64,
}

enum NextStep {
    Index(usize),
    Complete,
    Error(String),
    Pause { reason: String, resume_step_id: String },
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl PipelineEngine {
    pub fn new(flow: FlowSpec, run_id: impl Into<String>) -> Self {
        Self::with_budget(flow, run_id, TokenBudget::default())
    }

    pub fn with_budget(
        flow: FlowSpec,
        run_id: impl Into<String>,
        run_budget: TokenBudget,
    ) -> Self {
        Self {
            flow,
            current_step: 0,
            status: PipelineStatus::Idle,
            run_id: run_id.into(),
            step_history: Vec::new(),
            loop_counters: HashMap::new(),
            pending_gate: None,
            gate_feedback: HashMap::new(),
            gate_resolved_for_step: None,
            resumed_step_id: None,
            cumulative_tokens: 0,
            budget_tracker: BudgetTracker::new(run_budget),
            mode: PipelineMode::Auto,
        }
    }

    /// Advance the pipeline by one logical action.
    pub fn advance(&mut self) -> AdvanceResult {
        match &self.status {
            PipelineStatus::Completed => return AdvanceResult::Completed,
            PipelineStatus::Failed { error } => {
                return AdvanceResult::Failed { error: error.clone() }
            }
            PipelineStatus::WaitingForHuman { .. } => {
                return AdvanceResult::Failed {
                    error: "Cannot advance while waiting for gate. Call resolve_gate() first.".into(),
                };
            }
            PipelineStatus::Paused { at_step } => {
                let step = &self.flow.steps[*at_step];
                return AdvanceResult::Paused {
                    step_id: step.id.clone(),
                    reason: format!("Paused at '{}'. Call resume() to continue.", step.id),
                };
            }
            _ => {}
        }

        if self.current_step >= self.flow.steps.len() {
            self.status = PipelineStatus::Completed;
            return AdvanceResult::Completed;
        }

        let step = &self.flow.steps[self.current_step];
        let now = now_secs();

        // Human gate.
        if step.gate == GateType::Human
            && self.gate_resolved_for_step.as_ref() != Some(&step.id)
        {
            self.status = PipelineStatus::WaitingForHuman {
                step_id: step.id.clone(),
                since: now,
            };
            self.pending_gate = Some(PendingGate {
                step_id: step.id.clone(),
                since: now,
            });
            return AdvanceResult::WaitForHuman {
                step_id: step.id.clone(),
            };
        }

        self.status = PipelineStatus::Running {
            step_id: step.id.clone(),
            role_id: step.role_id.clone(),
            started_at: now,
        };
        self.resumed_step_id = None;

        AdvanceResult::ExecuteStep {
            step_id: step.id.clone(),
            role_id: step.role_id.clone(),
        }
    }

    /// Submit the result of an agent turn to continue the pipeline.
    pub fn submit_handoff(&mut self, mut handoff: HandoffDocument) -> AdvanceResult {
        let now = now_secs();

        let (step_id, started_at) = match &self.status {
            PipelineStatus::Running { step_id, started_at, .. } => (step_id.clone(), *started_at),
            _ => {
                self.status = PipelineStatus::Failed {
                    error: "submit_handoff called but no step is running".into(),
                };
                return self.advance();
            }
        };

        self.gate_resolved_for_step = None;

        let role_id = self.flow.steps[self.current_step].role_id.clone();
        let exit = self.flow.steps[self.current_step].exit.clone();

        // Handoff target auto-correction.
        let expected_role = match &exit {
            ExitRouting::Next => {
                let next_idx = self.current_step + 1;
                self.flow.steps.get(next_idx).map(|s| s.role_id.clone())
            }
            ExitRouting::Loop { target_step_id, .. } => {
                self.flow.get_step_index(target_step_id).map(|idx| self.flow.steps[idx].role_id.clone())
            }
        };
        if let Some(expected) = expected_role {
            if handoff.to != expected {
                tracing::warn!("Handoff target '{}' != expected '{}'; correcting.", handoff.to, expected);
                self.gate_feedback
                    .entry(step_id.clone())
                    .or_default()
                    .push(format!("[AUTO-CORRECTION] target '{}' corrected to '{}'.", handoff.to, expected));
                handoff.to = expected;
            }
        }

        self.step_history.push(StepRecord {
            step_id: step_id.clone(),
            role_id: role_id.clone(),
            handoff: Some(handoff.clone()),
            started_at,
            completed_at: now,
            iteration: *self.loop_counters.get(&step_id).unwrap_or(&0),
        });

        // Budget tracking.
        self.cumulative_tokens += handoff.token_usage.step_tokens;
        self.budget_tracker.record(&role_id, handoff.token_usage.step_tokens, 0);

        // Budget check is advisory by design (Plan 008 recheck): a LimitReached
        // signal logs a warning but does NOT halt the run. The default 100M
        // limit is a runaway guard, not a billing control. Callers needing
        // hard enforcement must act on BudgetAction::LimitReached themselves.
        match self.budget_tracker.check(&role_id) {
            crate::orchestration::budget::BudgetAction::LimitReached => {
                tracing::warn!(
                    "PIPELINE BUDGET LIMIT REACHED: {} tokens spent (limit: {}). \
                     Task continues — budget is advisory (see BudgetAction docs).",
                    self.budget_tracker.cumulative, self.budget_tracker.run_budget.limit
                );
            }
            crate::orchestration::budget::BudgetAction::Warning { remaining } => {
                tracing::warn!(
                    "pipeline budget warning: {} tokens remaining (step '{}')",
                    remaining, role_id
                );
            }
            _ => {}
        }

        let next = self.resolve_next_step(&step_id, &exit);
        match next {
            NextStep::Index(idx) => { self.current_step = idx; self.advance() }
            NextStep::Complete => { self.current_step = self.flow.steps.len(); self.status = PipelineStatus::Completed; AdvanceResult::Completed }
            NextStep::Error(msg) => { self.status = PipelineStatus::Failed { error: msg.clone() }; AdvanceResult::Failed { error: msg } }
            NextStep::Pause { reason, resume_step_id } => {
                if let Some(idx) = self.flow.get_step_index(&resume_step_id) { self.current_step = idx; }
                self.status = PipelineStatus::Paused { at_step: self.current_step };
                AdvanceResult::Paused { step_id: self.flow.steps[self.current_step].id.clone(), reason }
            }
        }
    }

    /// Resolve a pending human gate.
    pub fn resolve_gate(&mut self, decision: GateDecision) -> AdvanceResult {
        let pending = match self.pending_gate.take() {
            Some(g) => g,
            None => return AdvanceResult::Failed { error: "No pending gate".into() },
        };
        match decision {
            GateDecision::Approve => {
                self.gate_resolved_for_step = Some(pending.step_id.clone());
                self.status = PipelineStatus::Idle;
                self.advance()
            }
            GateDecision::Reject { feedback } => {
                self.gate_feedback.entry(pending.step_id.clone()).or_default().push(feedback);
                self.gate_resolved_for_step = Some(pending.step_id.clone());
                self.status = PipelineStatus::Idle;
                self.advance()
            }
        }
    }

    fn resolve_next_step(&mut self, step_id: &str, exit: &ExitRouting) -> NextStep {
        match exit {
            ExitRouting::Next => {
                let next = self.current_step + 1;
                if next >= self.flow.steps.len() { NextStep::Complete } else { NextStep::Index(next) }
            }
            ExitRouting::Loop { target_step_id, max_iterations } => {
                let count = self.loop_counters.entry(step_id.to_string()).or_insert(0);
                *count += 1;
                if *count >= *max_iterations {
                    NextStep::Pause { reason: format!("Loop max ({}) reached at '{}'", max_iterations, step_id), resume_step_id: target_step_id.clone() }
                } else {
                    self.flow.get_step_index(target_step_id).map(NextStep::Index).unwrap_or_else(|| NextStep::Error(format!("Loop target '{}' not found", target_step_id)))
                }
            }
        }
    }

    pub fn pause(&mut self) {
        if matches!(self.status, PipelineStatus::Running { .. }) {
            self.status = PipelineStatus::Paused { at_step: self.current_step };
        }
    }

    pub fn resume(&mut self) -> Option<AdvanceResult> {
        if matches!(self.status, PipelineStatus::Paused { .. }) {
            for step in &self.flow.steps { self.loop_counters.insert(step.id.clone(), 0); }
            if let Some(step) = self.flow.steps.get(self.current_step) { self.resumed_step_id = Some(step.id.clone()); }
            self.status = PipelineStatus::Idle;
            Some(self.advance())
        } else { None }
    }

    pub fn rerun(&mut self) -> Option<AdvanceResult> {
        if matches!(self.status, PipelineStatus::Failed { .. }) {
            let step_id = self.flow.steps.get(self.current_step)?.id.clone();
            self.loop_counters.insert(step_id.clone(), 0);
            self.gate_feedback.remove(&step_id);
            self.gate_resolved_for_step = None;
            self.status = PipelineStatus::Idle;
            Some(self.advance())
        } else { None }
    }

    pub fn current_role_id(&self) -> Option<&str> {
        self.flow.steps.get(self.current_step).map(|s| s.role_id.as_str())
    }

    pub fn current_step_id(&self) -> Option<&str> {
        self.flow.steps.get(self.current_step).map(|s| s.id.as_str())
    }

    pub fn feedback_for(&self, step_id: &str) -> Vec<String> {
        self.gate_feedback.get(step_id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_step_flow() -> FlowSpec {
        let mut f = FlowSpec::new("test");
        f.add_step(FlowStep::new("a", "assistant"));
        f.add_step(FlowStep::new("b", "coder"));
        f
    }

    fn handoff(from: &str, to: &str) -> HandoffDocument {
        let mut h = HandoffDocument::new(from, to);
        h.summary = "done".into();
        h
    }

    #[test]
    fn advance_executes_then_completes() {
        let mut eng = PipelineEngine::new(two_step_flow(), "run-1");
        let r = eng.advance();
        assert!(matches!(r, AdvanceResult::ExecuteStep { ref step_id, .. } if step_id == "a"));
        let r = eng.submit_handoff(handoff("assistant", "coder"));
        assert!(matches!(r, AdvanceResult::ExecuteStep { ref step_id, .. } if step_id == "b"));
        let r = eng.submit_handoff(handoff("coder", "reviewer"));
        assert_eq!(r, AdvanceResult::Completed);
        assert_eq!(eng.step_history.len(), 2);
    }

    #[test]
    fn human_gate_pauses_then_resolves() {
        let mut f = FlowSpec::new("gated");
        f.add_step(FlowStep::new("advise", "assistant").with_gate(GateType::Human));
        f.add_step(FlowStep::new("code", "coder"));
        let mut eng = PipelineEngine::new(f, "run-2");
        let r = eng.advance();
        assert!(matches!(r, AdvanceResult::WaitForHuman { .. }));
        let r = eng.advance();
        assert!(matches!(r, AdvanceResult::Failed { .. }));
        let r = eng.resolve_gate(GateDecision::Approve);
        assert!(matches!(r, AdvanceResult::ExecuteStep { ref step_id, .. } if step_id == "advise"));
    }

    #[test]
    fn gate_reject_redrafts_with_feedback() {
        let mut f = FlowSpec::new("gated");
        f.add_step(FlowStep::new("advise", "assistant").with_gate(GateType::Human));
        let mut eng = PipelineEngine::new(f, "run-3");
        eng.advance();
        let r = eng.resolve_gate(GateDecision::Reject { feedback: "needs detail".into() });
        assert!(matches!(r, AdvanceResult::ExecuteStep { .. }));
        assert!(eng.feedback_for("advise").iter().any(|s| s.contains("needs detail")));
    }

    #[test]
    fn handoff_target_auto_corrected() {
        let mut eng = PipelineEngine::new(two_step_flow(), "run-4");
        eng.advance();
        let r = eng.submit_handoff(handoff("assistant", "wrong-target"));
        assert!(matches!(r, AdvanceResult::ExecuteStep { .. }));
        assert!(eng.feedback_for("a").iter().any(|s| s.contains("AUTO-CORRECTION")));
    }

    #[test]
    fn loop_pauses_at_max() {
        let mut f = FlowSpec::new("loop");
        f.add_step(FlowStep::new("test", "tester"));
        f.add_step(FlowStep::new("code", "coder").with_exit(ExitRouting::Loop { target_step_id: "test".into(), max_iterations: 2 }));
        let mut eng = PipelineEngine::new(f, "run-5");
        eng.advance();
        eng.submit_handoff(handoff("tester", "coder"));
        eng.submit_handoff(handoff("coder", "tester"));
        eng.submit_handoff(handoff("tester", "coder"));
        let r = eng.submit_handoff(handoff("coder", "tester"));
        assert!(matches!(r, AdvanceResult::Paused { .. }));
    }

    #[test]
    fn budget_exceeded_logs_but_continues() {
        // Budget is advisory by design (Plan 008 recheck): LimitReached signals
        // the threshold but the pipeline continues — the default limit is a
        // runaway guard, not a billing control.
        let mut eng = PipelineEngine::with_budget(two_step_flow(), "run-6", TokenBudget::new(100));
        eng.advance();
        let mut h = handoff("assistant", "coder");
        h.token_usage.step_tokens = 200;
        let r = eng.submit_handoff(h);
        // Advisory: continues to next step rather than failing.
        assert!(!matches!(r, AdvanceResult::Failed { .. }));
        // Should continue to next step.
        assert!(matches!(r, AdvanceResult::ExecuteStep { .. }) | matches!(r, AdvanceResult::Completed));
    }

    #[test]
    fn rerun_from_failure() {
        let mut eng = PipelineEngine::new(two_step_flow(), "run-7");
        eng.status = PipelineStatus::Failed { error: "boom".into() };
        eng.current_step = 1;
        let r = eng.rerun();
        assert!(matches!(r, Some(AdvanceResult::ExecuteStep { .. })));
    }
}
