//! Unified provider configuration (shared by client and daemon).
//!
//! Replaces the two near-identical structs that previously lived separately
//! in `auto-ai-client::config::ProviderConfig` and
//! `auto-ai-daemon::config::ProviderEntry`. The only field unique to the
//! daemon is `max_concurrency`, modelled as `Option<usize>` so the client
//! (which doesn't care about it) simply leaves it `None`.

use serde::{Deserialize, Serialize};

use crate::tier::ModelDefinition;

/// One LLM provider's configuration.
///
/// `models` are [`ModelDefinition`]s (each tagged with a [`crate::ModelTier`]),
/// not bare strings — so a profession's tier can be resolved to a concrete
/// model at request time. `max_concurrency` is only meaningful to the daemon.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider type: `"anthropic"` | `"openai"` | `"zhipu"`.
    pub kind: String,
    /// API base URL.
    pub base_url: String,
    /// API key as a direct string (alternative to `key_env`).
    pub api_key: Option<String>,
    /// Environment variable name holding the API key (alternative to `api_key`).
    pub key_env: Option<String>,
    /// Available models, each tagged with its capability tier.
    pub models: Vec<ModelDefinition>,
    /// Daemon-only: per-provider concurrency cap. `None` on the client side.
    pub max_concurrency: Option<usize>,
}

impl ProviderConfig {
    /// Resolve the API key: direct string takes precedence, else read the env
    /// var named by `key_env`. `None` if neither is available.
    ///
    /// For providers that don't require authentication (e.g. local Ollama),
    /// returns a placeholder `"no-key-needed"` instead of None, so the daemon
    /// doesn't reject the provider for having no key.
    ///
    /// **Limitation (review-002)**: the placeholder conflates "no key" with
    /// "has key". A cleaner design would add an explicit `auth_required: bool`
    /// field so providers can declare no-auth status directly, and the daemon
    /// would skip the `Authorization` header for them. Deferred — current
    /// behavior is functionally correct for no-auth providers.
    pub fn resolve_key(&self) -> Option<String> {
        if let Some(key) = &self.api_key {
            if key.is_empty() {
                return Some("no-key-needed".into());
            }
            return Some(key.clone());
        }
        if let Some(env_name) = &self.key_env {
            return std::env::var(env_name).ok().or_else(|| Some("no-key-needed".into()));
        }
        // No api_key and no key_env → placeholder for local/no-auth providers.
        Some("no-key-needed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProviderConfig {
        ProviderConfig {
            kind: "openai".into(),
            base_url: String::new(),
            api_key: None,
            key_env: None,
            models: vec![],
            max_concurrency: None,
        }
    }

    #[test]
    fn resolve_key_direct_over_env() {
        let mut pc = sample();
        pc.api_key = Some("sk-xxx".into());
        pc.key_env = Some("SHOULD_BE_IGNORED".into());
        assert_eq!(pc.resolve_key(), Some("sk-xxx".into()));
    }

    #[test]
    fn resolve_key_from_env() {
        std::env::set_var("TEST_AI_CONFIG_KEY", "env-val");
        let mut pc = sample();
        pc.key_env = Some("TEST_AI_CONFIG_KEY".into());
        assert_eq!(pc.resolve_key(), Some("env-val".into()));
        std::env::remove_var("TEST_AI_CONFIG_KEY");
    }

    #[test]
    fn resolve_key_placeholder_when_nothing_set() {
        // No api_key and no key_env: return a placeholder so no-auth providers
        // (e.g. local Ollama) aren't rejected by the daemon. A cleaner design
        // with an explicit `auth_required` field is tracked in review-002/003.
        let pc = sample();
        assert_eq!(pc.resolve_key(), Some("no-key-needed".into()));
    }

    #[test]
    fn resolve_key_placeholder_when_empty_api_key() {
        let mut pc = sample();
        pc.api_key = Some(String::new());
        assert_eq!(pc.resolve_key(), Some("no-key-needed".into()));
    }
}
