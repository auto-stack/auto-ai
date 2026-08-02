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
    /// Whether this provider requires an API key for authentication. Defaults
    /// to `true`. Set to `false` for local/no-auth providers (e.g. Ollama) —
    /// `resolve_key` then returns a placeholder so the daemon can skip the
    /// Authorization header, instead of failing with NoApiKey. (review-003 W1)
    #[serde(default = "default_auth_required")]
    pub auth_required: bool,
}

fn default_auth_required() -> bool {
    true
}

impl ProviderConfig {
    /// Resolve the API key: direct string takes precedence, else read the env
    /// var named by `key_env`. `None` if neither is available.
    ///
    /// Behavior depends on [`ProviderConfig::auth_required`]:
    /// - `auth_required = true` (default): no key and no key_env → `None`,
    ///   so the daemon fails fast with a clear "no API key" error instead of
    ///   sending a bogus header upstream.
    /// - `auth_required = false` (local/no-auth providers like Ollama): returns
    ///   a `"no-key-needed"` placeholder so the daemon can skip the
    ///   `Authorization` header rather than rejecting the provider.
    pub fn resolve_key(&self) -> Option<String> {
        if let Some(key) = &self.api_key {
            if key.is_empty() {
                // An explicitly-empty api_key: treat like "no key".
                if !self.auth_required {
                    return Some("no-key-needed".into());
                }
                return None;
            }
            return Some(key.clone());
        }
        if let Some(env_name) = &self.key_env {
            return std::env::var(env_name).ok().or_else(|| {
                if self.auth_required { None } else { Some("no-key-needed".into()) }
            });
        }
        // No api_key and no key_env.
        if self.auth_required { None } else { Some("no-key-needed".into()) }
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
            auth_required: true,
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
    fn resolve_key_none_when_auth_required_and_nothing_set() {
        // auth_required=true (default) and no key/key_env → None (fail-fast),
        // so the daemon reports "no API key" rather than sending a bogus header.
        let pc = sample();
        assert_eq!(pc.resolve_key(), None);
    }

    #[test]
    fn resolve_key_none_when_auth_required_and_empty_api_key() {
        let mut pc = sample();
        pc.api_key = Some(String::new());
        assert_eq!(pc.resolve_key(), None);
    }

    #[test]
    fn resolve_key_placeholder_when_no_auth_and_nothing_set() {
        // auth_required=false (local/no-auth provider like Ollama): return a
        // placeholder so the daemon can skip the Authorization header.
        let mut pc = sample();
        pc.auth_required = false;
        assert_eq!(pc.resolve_key(), Some("no-key-needed".into()));
    }

    #[test]
    fn resolve_key_placeholder_when_no_auth_and_empty_api_key() {
        let mut pc = sample();
        pc.auth_required = false;
        pc.api_key = Some(String::new());
        assert_eq!(pc.resolve_key(), Some("no-key-needed".into()));
    }
}
