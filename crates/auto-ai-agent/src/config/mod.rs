//! `.at` (Atom) configuration parsing for Professions, using the shared
//! [`auto_atom`] parser (the same one AutoForge uses).
//!
//! Format (one root `profession { … }` block per file):
//!
//! ```text
//! profession {
//!     name : "coder"
//!     model : "glm-4.5"
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

pub mod profession_config;

pub use profession_config::{
    load_profession, parse_at_profession, ConfigProfession, ProfessionConfig,
};
