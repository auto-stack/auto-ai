// Auto-assembled by retranspile.sh (Plan 025 Phase 1).
// lib stub — Phase 1 modules appended below as they come online.

// ── extern-crate shim (a2r emits `use crate::ai_config::...`).
pub mod ai_config {
    pub use ::ai_config::*;
}

pub mod config;
pub mod format;
pub mod sse;
pub mod tier_router;
pub mod tier_router_glue;
pub mod tracker;
