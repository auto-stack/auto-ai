//! Unified provider configuration (shared by client and daemon).
//!
//! Replaces the two near-identical structs that previously lived separately
//! in `auto-ai-client::config::ProviderConfig` and
//! `auto-ai-daemon::config::ProviderEntry`. The only field unique to the
//! daemon is `max_concurrency`, modelled as `Option<usize>` so the client
//! (which doesn't care about it) simply leaves it `None`.

use serde::{Deserialize, Serialize};

/// One LLM provider's configuration.
///
/// `max_concurrency` is only meaningful to the daemon (its per-provider
/// concurrency cap); client-side code ignores it and leaves it `None`.
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
    /// Available models for this provider.
    pub models: Vec<String>,
    /// Daemon-only: per-provider concurrency cap. `None` on the client side.
    pub max_concurrency: Option<usize>,
}

impl ProviderConfig {
    /// Resolve the API key: direct string takes precedence, else read the env
    /// var named by `key_env`. `None` if neither is available.
    pub fn resolve_key(&self) -> Option<String> {
        if let Some(key) = &self.api_key {
            return Some(key.clone());
        }
        if let Some(env_name) = &self.key_env {
            return std::env::var(env_name).ok();
        }
        None
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
    fn resolve_key_none_when_nothing_set() {
        let pc = sample();
        assert_eq!(pc.resolve_key(), None);
    }
}
