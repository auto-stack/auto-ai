//! AutoOS shared AI client (Plan 325).
//!
//! Extracted from AutoForge's provider layer. Provides a unified `AiClient` API
//! for all AutoOS apps to call LLM services.
//!
//! Two modes:
//! - **Daemon mode**: routes through `aaid` for shared concurrency. Auto-starts
//!   the daemon if not running (ssh-agent model).
//! - **Direct mode**: calls the LLM API directly (fallback when daemon unavailable).

pub mod config;
pub mod daemon;
pub mod provider;
pub mod sse;
pub mod types;

pub use provider::{AiProvider, ProviderRegistry};
pub use types::*;

use crate::sse::SseParser;

/// Which mode the client is operating in.
enum ClientMode {
    /// Route through aaid daemon (shared concurrency, key vault).
    Daemon { url: String, http: reqwest::Client },
    /// Call LLM API directly.
    Direct { registry: ProviderRegistry },
}

/// The main client. Apps create one of these and call `complete()`.
pub struct AiClient {
    mode: ClientMode,
}

impl AiClient {
    /// Create a new client. Auto-discovers the daemon (lazy-start if needed).
    /// Falls back to direct mode if daemon is unavailable.
    pub fn new() -> Result<Self, ClientError> {
        // Try daemon mode first.
        if let Some(url) = daemon::ensure_daemon() {
            tracing::info!("ai-client: daemon mode ({})", url);
            return Ok(Self {
                mode: ClientMode::Daemon {
                    url,
                    http: reqwest::Client::new(),
                },
            });
        }

        // Fallback: direct mode.
        tracing::info!("ai-client: direct mode (daemon not available)");
        let config = config::ClientConfig::load();
        let registry = ProviderRegistry::from_config(&config)?;
        Ok(Self {
            mode: ClientMode::Direct { registry },
        })
    }

    /// Create a client with an explicit config (direct mode, for testing).
    pub fn with_config(config: config::ClientConfig) -> Result<Self, ClientError> {
        let registry = ProviderRegistry::from_config(&config)?;
        Ok(Self {
            mode: ClientMode::Direct { registry },
        })
    }

    /// Force direct mode (skip daemon discovery).
    pub fn direct() -> Result<Self, ClientError> {
        let config = config::ClientConfig::load();
        let registry = ProviderRegistry::from_config(&config)?;
        Ok(Self {
            mode: ClientMode::Direct { registry },
        })
    }

    /// Send a completion request (non-streaming).
    pub async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
        match &self.mode {
            ClientMode::Daemon { url, http } => {
                self.daemon_complete(http, url, req, false).await
            }
            ClientMode::Direct { registry } => {
                let provider = registry.default_provider()?;
                provider.complete(req).await
            }
        }
    }

    /// Send a streaming completion.
    pub async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_delta: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<CompletionResponse, ClientError> {
        let cb = std::sync::Arc::new(on_delta);
        match &self.mode {
            ClientMode::Daemon { url, http } => {
                self.daemon_complete_stream(http, url, req, cb).await
            }
            ClientMode::Direct { registry } => {
                let provider = registry.default_provider()?;
                provider.complete_stream(req, cb).await
            }
        }
    }

    /// List available providers.
    pub fn providers(&self) -> Vec<&str> {
        match &self.mode {
            ClientMode::Direct { registry } => registry.provider_names(),
            ClientMode::Daemon { .. } => vec!["daemon"], // Daemon manages providers.
        }
    }

    /// List available models for a provider.
    pub fn models(&self, _provider: &str) -> Vec<String> {
        match &self.mode {
            ClientMode::Direct { registry } => registry.models_for(_provider),
            ClientMode::Daemon { .. } => vec![], // Daemon manages models.
        }
    }

    /// Whether the client is in daemon mode.
    pub fn is_daemon_mode(&self) -> bool {
        matches!(self.mode, ClientMode::Daemon { .. })
    }

    // ── Daemon mode HTTP helpers ──────────────────────────────────────────

    async fn daemon_complete(
        &self,
        http: &reqwest::Client,
        url: &str,
        req: &CompletionRequest,
        stream: bool,
    ) -> Result<CompletionResponse, ClientError> {
        let body = self.build_daemon_body(req, stream);

        let resp = http
            .post(format!("{}/v1/chat/completions", url))
            .header("Content-Type", "application/json")
            .header("X-App-Name", "auto-ai-client")
            .json(&body)
            .send()
            .await
            .map_err(ClientError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api(format!("daemon {}: {}", status, text)));
        }

        let json: serde_json::Value =
            resp.json().await.map_err(|e| ClientError::Api(format!("parse: {}", e)))?;

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let usage = json.get("usage").map(|u| Usage {
            input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
        });

        let model = json["model"].as_str().unwrap_or(&req.model).to_string();

        Ok(CompletionResponse { content, usage, model, error: None })
    }

    async fn daemon_complete_stream(
        &self,
        http: &reqwest::Client,
        url: &str,
        req: &CompletionRequest,
        on_delta: std::sync::Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        let body = self.build_daemon_body(req, true);

        let resp = http
            .post(format!("{}/v1/chat/completions", url))
            .header("Content-Type", "application/json")
            .header("X-App-Name", "auto-ai-client")
            .json(&body)
            .send()
            .await
            .map_err(ClientError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api(format!("daemon {}: {}", status, text)));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut content = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| ClientError::Http(e.to_string()))?;
            for data in parser.push(&bytes) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(delta) = json["choices"][0]["delta"]["content"]
                        .as_str()
                        .map(|s| s.to_string())
                    {
                        content.push_str(&delta);
                        on_delta(delta);
                    }
                }
            }
        }

        for data in parser.finish() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(delta) = json["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(|s| s.to_string())
                {
                    content.push_str(&delta);
                    on_delta(delta);
                }
            }
        }

        Ok(CompletionResponse {
            content,
            usage: None,
            model: req.model.clone(),
            error: None,
        })
    }

    /// Build the OpenAI-compatible request body for the daemon.
    fn build_daemon_body(&self, req: &CompletionRequest, stream: bool) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = req.messages.iter().map(|m| {
            serde_json::json!({ "role": m.role, "content": m.content })
        }).collect();

        let mut all_msgs = Vec::new();
        if let Some(sys) = &req.system_prompt {
            all_msgs.push(serde_json::json!({ "role": "system", "content": sys }));
        }
        all_msgs.extend(messages);

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": all_msgs,
            "stream": stream,
        });
        if let Some(n) = req.max_tokens {
            body["max_tokens"] = serde_json::json!(n);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        body
    }
}

/// Unified error type.
#[derive(Debug)]
pub enum ClientError {
    NoApiKey(String),
    NoProvider,
    Http(String),
    Sse(String),
    Api(String),
    Config(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoApiKey(p) => write!(f, "no API key for provider '{}'", p),
            Self::NoProvider => write!(f, "no provider configured"),
            Self::Http(e) => write!(f, "HTTP error: {}", e),
            Self::Sse(e) => write!(f, "SSE parse error: {}", e),
            Self::Api(e) => write!(f, "API error: {}", e),
            Self::Config(e) => write!(f, "config error: {}", e),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_error_display() {
        assert!(format!("{}", ClientError::NoProvider).contains("no provider"));
        assert!(format!("{}", ClientError::NoApiKey("zhipu".into())).contains("zhipu"));
    }

    #[test]
    fn build_daemon_body_basic() {
        let client = AiClient {
            mode: ClientMode::Daemon {
                url: "http://test".into(),
                http: reqwest::Client::new(),
            },
        };
        let req = CompletionRequest::single("glm-4.5", "hello");
        let body = client.build_daemon_body(&req, false);
        assert_eq!(body["model"], "glm-4.5");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn build_daemon_body_with_system() {
        let client = AiClient {
            mode: ClientMode::Daemon {
                url: "http://test".into(),
                http: reqwest::Client::new(),
            },
        };
        let req = CompletionRequest::single("glm-4.5", "hi").with_system("be nice");
        let body = client.build_daemon_body(&req, true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn is_daemon_mode_check() {
        let daemon_client = AiClient {
            mode: ClientMode::Daemon {
                url: "http://test".into(),
                http: reqwest::Client::new(),
            },
        };
        assert!(daemon_client.is_daemon_mode());
    }
}
