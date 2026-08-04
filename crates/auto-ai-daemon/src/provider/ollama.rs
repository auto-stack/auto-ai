//! Ollama provider — a thin wrapper over [`OpenAiProvider`].
//!
//! [Ollama](https://ollama.com) exposes an OpenAI-compatible `/v1/chat/completions`
//! endpoint, so it can be served by delegating to the OpenAI provider. This
//! module exists so configs can use `kind : ollama` explicitly (more
//! discoverable than `kind : openai` for a local Ollama instance) and so the
//! no-auth behavior is obvious.
//!
//! The daemon already supports no-auth providers via `auth_required : false`
//! (and auto-infers it for localhost URLs), so this wrapper adds no special
//! key handling — Ollama simply doesn't need one.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::openai::OpenAiProvider;
use super::AiProvider;
use ai_config::{CompletionRequest, CompletionResponse};
use crate::LlmError;

/// Ollama provider — delegates everything to an [`OpenAiProvider`] pointed at
/// the Ollama OpenAI-compatible endpoint.
pub struct OllamaProvider {
    inner: OpenAiProvider,
}

impl OllamaProvider {
    /// Create a new Ollama provider.
    ///
    /// `base_url` should point at the OpenAI-compatible root, e.g.
    /// `http://localhost:11434/v1`. No API key is needed (Ollama ignores it);
    /// a placeholder is supplied for the inner OpenAI client.
    pub fn new(name: String, base_url: String, models: Vec<String>) -> Self {
        Self {
            inner: OpenAiProvider::new(name, base_url, "no-key-needed".to_string(), models),
        }
    }
}

#[async_trait]
impl AiProvider for OllamaProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn models(&self) -> Vec<String> {
        self.inner.models()
    }

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        self.inner.complete(req).await
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_delta: Arc<dyn Fn(super::StreamDelta) + Send + Sync>,
        cancel: CancellationToken,
    ) -> Result<CompletionResponse, LlmError> {
        self.inner.complete_stream(req, on_delta, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_models_pass_through() {
        let p = OllamaProvider::new(
            "local-ollama".into(),
            "http://localhost:11434/v1".into(),
            vec!["qwen2.5-coder:7b".into(), "ornith-9b".into()],
        );
        assert_eq!(p.name(), "local-ollama");
        assert_eq!(p.models(), vec!["qwen2.5-coder:7b", "ornith-9b"]);
    }
}
