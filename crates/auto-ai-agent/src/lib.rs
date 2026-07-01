//! AutoOS AI agent layer (Layer 3 + 4 of the AI stack).
//!
//! Built on top of [`auto_ai_client`] (Layer 2 — provider/daemon plumbing and
//! native tool-calling). This crate adds three things:
//!
//! - **[`agent`]** — an autonomous ReAct loop that drives an LLM to a goal by
//!   interleaving reasoning and tool calls.
//! - **built-in **builtin_roles**** — a library of
//!   "Roles" (system prompts + model/temperature/tool policy), ported
//!   from AutoForge's relay souls.
//! - **[`workflow`]** — a multi-step orchestration engine that chains Agents
//!   together, with a [`relay`] target abstraction.
//!
//! `.at` (Atom) config files for custom Professions/Workflows are parsed with
//! the shared [`auto_atom`] parser (see [`config`] in later phases).
//!
//! Design doc: `docs/auto-ai-agent-design.md`.

pub mod agent;
pub mod config;
pub mod error;
pub mod memory;
pub mod role_def;
pub mod builtin_roles;
pub mod relay;
pub mod roles;
pub mod skill;
pub mod tool;
pub mod validate;
pub mod workflow;
pub mod workflow_validator;

pub use agent::{Agent, AgentResult, Client, StreamEvent, ToolCallRecord};
pub use config::{load_role, parse_at_role, parse_tier_field, serialize_at_role, ConfigRole, RoleConfig};
pub use error::{AgentError, ToolError};
pub use memory::Memory;
pub use role_def::Role;
// Re-export ModelTier so downstream crates (musk, …) can name the tier type
// without depending on ai_config directly.
pub use ai_config::ModelTier;
pub use builtin_roles::{load_builtin, builtin_names, Assistant, Architect, Coder, Documenter, Reviewer, Runner, Tester, Translator};
pub use relay::RelayTarget;
pub use roles::{RoleDetail, RoleRegistry, RoleSummary};
pub use skill::{Skill, SkillRegistry, SkillTool};
pub use tool::{Tool, ToolRegistry};
pub use validate::{load_client_config, validate_role_model};
pub use workflow::{parse_at_workflow, Workflow, WorkflowContext, WorkflowEvent, WorkflowResult, WorkflowStep};
