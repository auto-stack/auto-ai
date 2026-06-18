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
    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_delta: Arc<dyn Fn(String) + Send + Sync>,
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
            let key = pc
                .resolve_key()
                .ok_or_else(|| LlmError::NoApiKey(name.clone()))?;
            let provider: Arc<dyn AiProvider> = match pc.kind.as_str() {
                "anthropic" => Arc::new(AnthropicProvider::new(
                    name.clone(),
                    pc.base_url.clone(),
                    key,
                    pc.models.clone(),
                )),
                "openai" | _ => Arc::new(OpenAiProvider::new(
                    name.clone(),
                    pc.base_url.clone(),
                    key,
                    pc.models.clone(),
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
