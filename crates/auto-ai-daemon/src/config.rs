//! Daemon configuration — delegates parsing to `ai-config`.
//!
//! The `DaemonConfig` struct and `.at` parsing live in the shared `ai-config`
//! crate so client and daemon agree on the provider layout. This module just
//! adds the daemon-specific load entry points (file discovery + env fallback)
//! on top of it.

use std::collections::HashMap;

// Re-export the shared config types so existing `crate::config::DaemonConfig`
// references keep resolving, now pointing at the single source of truth.
pub use ai_config::loader::{parse_daemon_config, DaemonConfig};

use ai_config::tier::{ModelDefinition, ModelTier};

/// Default per-provider concurrency cap when a provider doesn't set
/// `max_concurrency`.
const DEFAULT_CONCURRENCY: usize = 4;

/// Load daemon config from `~/.config/autoos/ai-daemon.at`, else from env.
pub fn load() -> DaemonConfig {
    if let Some(cfg) = load_from_file() {
        return cfg;
    }
    load_from_env()
}

fn load_from_file() -> Option<DaemonConfig> {
    let path = dirs::home_dir()?.join(".config/autoos/ai-daemon.at");
    let content = std::fs::read_to_string(&path).ok()?;
    parse_daemon_config(&content).ok()
}

/// Env-var fallback (Forge-compatible): ZHIPU_API_KEY / ANTHROPIC_API_KEY /
/// OPENAI_API_KEY, each with a default concurrency cap of 4.
fn load_from_env() -> DaemonConfig {
    let mut providers = HashMap::new();

    if let Ok(key) = std::env::var("ZHIPU_API_KEY") {
        providers.insert(
            "zhipu".into(),
            provider_env("openai", "https://open.bigmodel.cn/api/paas/v4", key, vec![
                ModelDefinition::new("glm-4.6", ModelTier::Mid),
                ModelDefinition::new("glm-4-flash", ModelTier::Min),
            ]),
        );
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY").or_else(|_| std::env::var("ANTHROPIC_AUTH_TOKEN")) {
        providers.insert(
            "anthropic".into(),
            provider_env("anthropic", &std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into()), key, vec![
                ModelDefinition::new("claude-3-5-sonnet-20241022", ModelTier::Mid),
            ]),
        );
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        providers.insert(
            "openai".into(),
            provider_env("openai", &std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into()), key, vec![
                ModelDefinition::new("gpt-4o", ModelTier::Mid),
            ]),
        );
    }

    let default_provider = providers.keys().next().cloned().unwrap_or_default();
    let default_model = providers
        .get(&default_provider)
        .and_then(|p| p.models.first().map(|m| m.id.clone()))
        .unwrap_or_default();

    DaemonConfig {
        listen_addr: "127.0.0.1:17654".into(),
        idle_timeout_min: 10,
        log_level: "info".into(),
        providers,
        default_provider,
        default_model,
        tier_routing: ai_config::loader::TierRouting::default(),
    }
}

fn provider_env(kind: &str, base_url: &str, key: String, models: Vec<ModelDefinition>) -> ai_config::ProviderConfig {
    ai_config::ProviderConfig {
        kind: kind.into(),
        base_url: base_url.into(),
        api_key: Some(key),
        key_env: None,
        models,
        max_concurrency: Some(DEFAULT_CONCURRENCY),
    }
}

/// Compatibility alias for callers that still name the provider entry type.
pub type ProviderEntry = ai_config::ProviderConfig;
