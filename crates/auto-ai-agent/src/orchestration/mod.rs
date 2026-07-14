//! Multi-agent orchestration primitives — shared across all AutoOS apps.
//!
//! (Plan 008 — Orchestration Down)
//!
//! This module provides the generic building blocks for multi-agent pipelines:
//! - [`handoff::HandoffDocument`] — structured inter-agent message (token-efficient)
//! - [`budget::BudgetTracker`] — per-step + per-run token budget enforcement
//! - [`flow`] — FlowSpec / FlowStep / ExitRouting / GateType definitions
//! - [`pipeline::PipelineEngine`] — deterministic state machine (advance/gate/pause)
//! - [`driver`] — PipelineDriver + AgentFactory trait (parameterized orchestration loop)
//!
//! Apps (musk, auto-ai-cli, future) consume these to run multi-role pipelines
//! without reimplementing the state machine, budget, or handoff logic.

pub mod budget;
pub mod flow;
pub mod handoff;
pub mod pipeline;

pub use budget::{BudgetAction, BudgetStrategy, BudgetTracker, TokenBudget};
pub use flow::{ExitRouting, FlowSpec, FlowStep, GateDecision as FlowGateDecision, GateType};
pub use handoff::{
    ContextPointers, Decision, HandoffDocument, Question, TokenUsage, WorkProduct,
};
pub use pipeline::{
    AdvanceResult, GateDecision, PendingGate, PipelineEngine, PipelineMode, PipelineStatus,
    StepRecord,
};
