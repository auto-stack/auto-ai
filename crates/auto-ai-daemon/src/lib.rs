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
pub mod tier_router;
pub mod tracker;

pub use config::DaemonConfig;
pub use pool::ConcurrencyManager;
pub use provider::{AiProvider, ProviderRegistry};
pub use server::AppState;
pub use tier_router::{TierCandidate, TierRouter};
pub use tracker::UsageTracker;

/// Error from an LLM API call (used by the provider layer).
///
/// The structured variants (`RateLimited` / `Timeout` / `Upstream`) let the
/// router decide whether to fall back to the next provider candidate: rate
/// limits, timeouts, and 5xx are retryable on another provider; 4xx (other
/// than 429) indicate a request-shape problem that fallback won't fix.
#[derive(Debug)]
pub enum LlmError {
    /// A transport-level failure (connection refused, DNS, TLS, …).
    Http(String),
    /// A retryable rate limit (HTTP 429).
    RateLimited,
    /// A request/connection timeout.
    Timeout(String),
    /// An upstream HTTP response with a non-success status. `retryable` is
    /// true for 5xx (transient server faults); false for 4xx other than 429
    /// (client/parameter errors that fallback cannot fix).
    Upstream { status: u16, message: String, retryable: bool },
    /// A successful HTTP response whose body couldn't be parsed/understood.
    Api(String),
    NoProvider,
    NoApiKey(String),
}

impl LlmError {
    /// Whether falling back to another provider candidate is reasonable.
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::RateLimited | LlmError::Timeout(_) => true,
            LlmError::Upstream { retryable, .. } => *retryable,
            // Http (transport) is also retryable on another provider.
            LlmError::Http(_) => true,
            LlmError::Api(_) | LlmError::NoProvider | LlmError::NoApiKey(_) => false,
        }
    }

    /// Classify an upstream non-success HTTP status into the right variant:
    /// 429 → `RateLimited`; 5xx → retryable `Upstream`; other 4xx →
    /// non-retryable `Upstream`. Used by both providers' status-check path.
    pub fn from_upstream_status(status: reqwest::StatusCode, body: String) -> Self {
        let code = status.as_u16();
        if code == 429 {
            LlmError::RateLimited
        } else {
            LlmError::Upstream {
                status: code,
                message: body,
                retryable: status.is_server_error(),
            }
        }
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Http(e) => write!(f, "HTTP error: {e}"),
            LlmError::RateLimited => write!(f, "rate limited (HTTP 429)"),
            LlmError::Timeout(e) => write!(f, "timeout: {e}"),
            LlmError::Upstream { status, message, .. } => {
                write!(f, "upstream error ({status}): {message}")
            }
            LlmError::Api(e) => write!(f, "API error: {e}"),
            LlmError::NoProvider => write!(f, "no provider configured"),
            LlmError::NoApiKey(p) => write!(f, "no API key for provider '{p}'"),
        }
    }
}

impl std::error::Error for LlmError {}

impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        // Classify timeouts so the router can treat them as retryable.
        if e.is_timeout() {
            return Self::Timeout(e.to_string());
        }
        Self::Http(e.to_string())
    }
}
