//! Provider trait + registry + concrete provider implementations.
//!
//! Migrated from `auto-ai-client` (Task 6): the daemon owns all LLM provider
//! knowledge. Providers translate canonical [`ai_config::CompletionRequest`]s
//! to their wire format and parse responses back into canonical
//! [`ai_config::CompletionResponse`]s.

pub mod openai;
pub mod anthropic;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;

use std::collections::HashMap;
use std::sync::Arc;

use ai_config::{ClientConfig, CompletionRequest, CompletionResponse, DaemonConfig};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::LlmError;

/// Trait that every LLM provider implements.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Provider name (e.g. "zhipu", "anthropic").
    fn name(&self) -> &str;

    /// Available models.
    fn models(&self) -> Vec<String>;

    /// Non-streaming completion.
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Streaming completion. Calls `on_delta` for each text chunk.
    ///
    /// `cancel` lets the caller (e.g. when the SSE client disconnects) abort
    /// the upstream fetch early, so a dropped connection doesn't keep pulling
    /// tokens (and spending quota) until the upstream stream ends naturally.
    /// Honored at each SSE chunk boundary.
    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_delta: Arc<dyn Fn(String) + Send + Sync>,
        cancel: CancellationToken,
    ) -> Result<CompletionResponse, LlmError>;
}

/// Registry of configured providers.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn AiProvider>>,
    default_name: String,
}

impl ProviderRegistry {
    /// Build a registry from a client-side config.
    pub fn from_config(config: &ClientConfig) -> Result<Self, LlmError> {
        Self::build(&config.providers, &config.default_provider)
    }

    /// Build a registry from a daemon-side config.
    pub fn from_daemon_config(config: &DaemonConfig) -> Result<Self, LlmError> {
        Self::build(&config.providers, &config.default_provider)
    }

    fn build(
        providers: &HashMap<String, ai_config::ProviderConfig>,
        default_name: &str,
    ) -> Result<Self, LlmError> {
        let mut registry: HashMap<String, Arc<dyn AiProvider>> = HashMap::new();

        for (name, pc) in providers {
            // Resolve the API key. For auth_required providers this fails fast
            // (None → NoApiKey). For no-auth providers (auth_required=false, e.g.
            // local Ollama) a placeholder is returned. As a backward-compat
            // safety net: if an old config file omits auth_required (defaults to
            // true) but the provider points at localhost and has no key, infer
            // no-auth rather than hard-failing (W1 follow-up).
            let key = match pc.resolve_key() {
                Some(k) => k,
                None => {
                    let is_local = pc.base_url.contains("localhost")
                        || pc.base_url.contains("127.0.0.1");
                    if is_local {
                        tracing::warn!(
                            "provider '{}' has no API key but targets a local URL ({}); \
                             treating as no-auth (set auth_required : false to silence this).",
                            name, pc.base_url
                        );
                        "no-key-needed".to_string()
                    } else {
                        return Err(LlmError::NoApiKey(name.clone()));
                    }
                }
            };
            // Providers only need the model id list (not the full tier metadata).
            let model_ids: Vec<String> = pc.models.iter().map(|m| m.id.clone()).collect();
            let provider: Arc<dyn AiProvider> = match pc.kind.as_str() {
                "anthropic" => Arc::new(AnthropicProvider::new(
                    name.clone(),
                    pc.base_url.clone(),
                    key,
                    model_ids.clone(),
                )),
                "openai" | _ => Arc::new(OpenAiProvider::new(
                    name.clone(),
                    pc.base_url.clone(),
                    key,
                    model_ids.clone(),
                )),
            };
            registry.insert(name.clone(), provider);
        }

        if registry.is_empty() {
            return Err(LlmError::NoProvider);
        }

        Ok(Self {
            providers: registry,
            default_name: default_name.to_string(),
        })
    }

    pub fn default_provider(&self) -> Result<&Arc<dyn AiProvider>, LlmError> {
        self.providers
            .get(&self.default_name)
            .ok_or(LlmError::NoProvider)
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn AiProvider>> {
        self.providers.get(name)
    }

    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    pub fn models_for(&self, provider: &str) -> Vec<String> {
        self.providers
            .get(provider)
            .map(|p| p.models())
            .unwrap_or_default()
    }
}
