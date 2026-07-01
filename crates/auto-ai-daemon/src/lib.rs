//! AutoOS AI daemon (`aaid`) — the single LLM gateway.
//!
//! All AutoOS apps route LLM requests through this daemon. It owns:
//! - All LLM API communication (provider request building + response parsing).
//! - Canonical↔provider shape conversion (OpenAI / Anthropic).
//! - Global concurrency pools (per-provider `Semaphore`).
//! - API key vault (apps never touch secrets).
//! - Cost/token tracking (per-app).
//!
//! Protocol: HTTP (axum) over TCP localhost. Apps use `auto-ai-client`, which
//! sends **canonical** `CompletionRequest`s and receives canonical
//! `CompletionResponse`s; the daemon translates to/from concrete providers.
//!
//! The provider/format/sse modules were migrated here from `auto-ai-client`
//! (Task 6) so that the client carries no provider knowledge.

pub mod config;
pub mod format;
pub mod pool;
pub mod provider;
pub mod server;
pub mod services;
pub mod sse;
pub mod tracker;

pub use config::DaemonConfig;
pub use pool::ConcurrencyManager;
pub use provider::{AiProvider, ProviderRegistry};
pub use server::AppState;
pub use tracker::UsageTracker;

/// Error from an LLM API call (used by the provider layer).
#[derive(Debug)]
pub enum LlmError {
    Http(String),
    Api(String),
    NoProvider,
    NoApiKey(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Http(e) => write!(f, "HTTP error: {e}"),
            LlmError::Api(e) => write!(f, "API error: {e}"),
            LlmError::NoProvider => write!(f, "no provider configured"),
            LlmError::NoApiKey(p) => write!(f, "no API key for provider '{p}'"),
        }
    }
}

impl std::error::Error for LlmError {}

impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.to_string())
    }
}
