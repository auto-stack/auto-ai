//! AutoOS AI agent layer (Layer 3 + 4 of the AI stack).
//!
//! Built on top of [`auto_ai_client`] (Layer 2 — provider/daemon plumbing and
//! native tool-calling). This crate adds two things:
//!
//! - **[`agent`]** — an autonomous ReAct loop that drives an LLM to a goal by
//!   interleaving reasoning and tool calls.
//! - **built-in **builtin_roles**** — a library of
//!   "Roles" (system prompts + model/temperature/tool policy), ported
//!   from AutoForge's relay souls.
//!
//! `.at` (Atom) config files for custom Professions are parsed with
//! the shared [`auto_atom`] parser (see [`config`] in later phases).
//!
//! Design doc: `docs/auto-ai-agent-design.md`.

pub mod agent;
pub mod compaction;
pub mod config;
pub mod error;
pub mod memory;
pub mod orchestration;
pub mod role_def;
pub mod builtin_roles;
pub mod relay;
pub mod roles;
pub mod skill;
pub mod tool;
pub mod validate;

pub use agent::{Agent, AgentResult, Client, StreamEvent, ToolCallRecord};
pub use compaction::{compact, estimate_tokens, find_cut_point, should_compact, CompactionSettings};
pub use config::{load_role, parse_at_role, parse_tier_field, serialize_at_role, ConfigRole, RoleConfig};
pub use error::{AgentError, ToolError};
pub use memory::Memory;
pub use role_def::Role;
// Re-export ModelTier so downstream crates (musk, …) can name the tier type
// without depending on ai_config directly.
pub use ai_config::ModelTier;
pub use builtin_roles::{load_builtin, builtin_names, Assistant, Architect, Coder, Documenter, Reviewer, Runner, Tester, Translator};
pub use roles::{RoleDetail, RoleRegistry, RoleSummary};
pub use skill::{Skill, SkillRegistry, SkillTool};
pub use tool::{Tool, ToolRegistry};
pub use validate::{load_client_config, validate_role_model};
pub use orchestration::{
    BudgetAction, BudgetStrategy, BudgetTracker, TokenBudget,
    ContextPointers, Decision, HandoffDocument, Question, TokenUsage, WorkProduct,
    // Flow types (Phase 8)
    FlowSpec, FlowStep, GateType, ExitRouting,
    // Pipeline types (Phase 8)
    GateDecision, PipelineEngine, PipelineStatus, PipelineMode,
    AdvanceResult, StepRecord,
    // Driver types (Phase 8)
    AgentFactory, PipelineDriver, PipelineEvent,
};
