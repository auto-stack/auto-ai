//! AutoOS AI agent layer (Layer 3 + 4 of the AI stack).
//!
//! Built on top of [`auto_ai_client`] (Layer 2 — provider/daemon plumbing and
//! native tool-calling). This crate adds three things:
//!
//! - **[`agent`]** — an autonomous ReAct loop that drives an LLM to a goal by
//!   interleaving reasoning and tool calls.
//! - **[`profession`]** + the built-in **[`professions`]** — a library of
//!   "Professions" (system prompts + model/temperature/tool policy), ported
//!   from AutoForge's relay souls.
//! - **[`workflow`]** — a multi-step orchestration engine that chains Agents
//!   together, with a [`relay`] target abstraction.
//!
//! `.at` (Atom) config files for custom Professions/Workflows are parsed with
//! the shared [`auto_atom`] parser (see [`config`] in later phases).
//!
//! Design doc: `docs/auto-ai-agent-design.md`.

pub mod agent;
pub mod error;
pub mod memory;
pub mod profession;
pub mod tool;

pub use agent::{Agent, AgentResult, ToolCallRecord};
pub use error::{AgentError, ToolError};
pub use memory::Memory;
pub use profession::Profession;
pub use tool::{Tool, ToolRegistry};
