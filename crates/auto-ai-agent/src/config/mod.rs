//! `.at` (Atom) configuration parsing for Roles, using the shared
//! [`auto_atom`] parser (the same one AutoForge uses).
//!
//! Format (one root `role { … }` block per file):
//!
//! ```text
//! role {
//!     name : "coder"
//!     model : "glm-4.6"
//!     temperature : 0.2
//!     max_turns : 15
//!     system_prompt : "you are a coder..."
//!     system_prompt_append : "extra guidance"
//!     tools : [read_file, write_file, run_command]
//!     inherit : "coder"
//! }
//! ```
//!
//! See `docs/auto-ai-agent-design.md` §4.4 for the inherit/merge semantics.

pub mod role_config;

pub use role_config::{
    load_role, parse_at_role, parse_tier_field, serialize_at_role, ConfigRole,
    RoleConfig,
};
