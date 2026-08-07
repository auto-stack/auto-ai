//! Hand-written glue for provider.at.
//!
//! `ProviderRegistry::from_daemon_config` needs to construct the concrete
//! providers (OpenAiProvider / AnthropicProvider / OllamaProvider), which are
//! Phase-4 transpilation targets (provider/openai.rs, anthropic.rs, ollama.rs).
//! Until those are transpiled, `build_registry` returns `Err(NoProvider)` — the
//! daemon's HTTP layer (Phase 3) will surface this as a 503 at startup if no
//! providers can be built. Phase 4 will replace this body with the real
//! provider-construction dispatch (mirrors the rust-ref's `build()` fn).
//!
//! (Plan 025 Phase 2: glue stub for a Phase-4 dependency. Same pattern as
//! tier_router_glue.rs — hand-written .rs that the .at source calls via a
//! use.rust bridge.)

use crate::error::LlmError;
use crate::provider::ProviderRegistry;
use ai_config::DaemonConfig;

/// Build a ProviderRegistry from a daemon config. Phase-4 stub: returns
/// NoProvider until the concrete providers are transpiled.
pub fn build_registry(_config: &DaemonConfig) -> Result<ProviderRegistry, LlmError> {
    Err(LlmError::NoProvider)
}
