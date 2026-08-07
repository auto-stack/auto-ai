// Auto-assembled by retranspile.sh (Plan 025 Phase 1+2).
// lib stub — Phase 1/2 modules appended below as they come online.

// ── extern-crate shim (a2r emits `use crate::ai_config::...`).
pub mod ai_config {
    pub use ::ai_config::*;
}

pub mod config;
pub mod error;
pub mod format;
pub mod pool;
pub mod provider;
pub mod provider_glue;
pub mod server;
pub mod server_glue;
pub mod services;
pub mod sse;
pub mod tier_router;
pub mod tier_router_glue;
pub mod tracker;
