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
    /// Plan 028: quota/billing exhaustion — switching provider candidates
    /// won't help, so the fallback loop must NOT consume the chain on these.
    pub fn is_quota_exhausted(&self) -> bool {
        match self {
            LlmError::Upstream { status, message, .. } => {
                if *status == 402 {
                    return true;
                }
                let lower = message.to_lowercase();
                lower.contains("insufficient_quota")
                    || lower.contains("quota_exceeded")
                    || lower.contains("billing")
                    || lower.contains("exceeded your current quota")
            }
            _ => false,
        }
    }

    /// Plan 031: the request overflowed the model's context window. The
    /// daemon itself doesn't recover (compaction lives in the agent); this
    /// classification exists so callers/tests can recognize the class and so
    /// the error text keeps flowing to the client un-mangled. Conservative,
    /// mirroring the agent's `compaction::is_context_overflow`.
    pub fn is_context_overflow(&self) -> bool {
        let message = match self {
            LlmError::Upstream { message, .. } => message,
            LlmError::Api(m) | LlmError::Http(m) | LlmError::Timeout(m) => m,
            _ => return false,
        };
        let lower = message.to_lowercase();
        if ["rate limit", "too many requests", "throttling"]
            .iter()
            .any(|m| lower.contains(m))
        {
            return false;
        }
        [
            "prompt is too long",
            "request_too_large",
            "exceeds the context window",
            "maximum context length",
            "context_length_exceeded",
            "exceeds the available context size",
            "exceeded max context length",
            "greater than the context length",
        ]
        .iter()
        .any(|m| lower.contains(m))
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_exhausted_detection() {
        assert!(LlmError::Upstream { status: 402, message: "payment required".into(), retryable: false }.is_quota_exhausted());
        assert!(LlmError::Upstream { status: 400, message: "insufficient_quota: you exceeded".into(), retryable: false }.is_quota_exhausted());
        assert!(LlmError::Upstream { status: 429, message: "quota_exceeded for today".into(), retryable: true }.is_quota_exhausted());
        assert!(LlmError::Upstream { status: 400, message: "billing hard limit reached".into(), retryable: false }.is_quota_exhausted());
        assert!(!LlmError::Upstream { status: 500, message: "internal".into(), retryable: true }.is_quota_exhausted());
        assert!(!LlmError::Upstream { status: 400, message: "bad parameter".into(), retryable: false }.is_quota_exhausted());
        assert!(!LlmError::RateLimited.is_quota_exhausted());
        assert!(!LlmError::Http("conn".into()).is_quota_exhausted());
    }

    #[test]
    fn retryable_classification() {
        assert!(LlmError::RateLimited.is_retryable());
        assert!(LlmError::Timeout("t".into()).is_retryable());
        assert!(LlmError::Http("connection refused".into()).is_retryable());
        assert!(LlmError::Upstream { status: 503, message: "unavailable".into(), retryable: true }.is_retryable());
        assert!(!LlmError::Upstream { status: 400, message: "invalid request".into(), retryable: false }.is_retryable());
        assert!(!LlmError::Api("bad body".into()).is_retryable());
        assert!(!LlmError::NoProvider.is_retryable());
        assert!(!LlmError::NoApiKey("p".into()).is_retryable());
    }

    #[test]
    fn from_upstream_status_routing() {
        assert!(matches!(LlmError::from_upstream_status(reqwest::StatusCode::TOO_MANY_REQUESTS, "x".into()), LlmError::RateLimited));
        match LlmError::from_upstream_status(reqwest::StatusCode::BAD_GATEWAY, "x".into()) {
            LlmError::Upstream { status: 502, retryable: true, .. } => {}
            other => panic!("expected retryable 502 Upstream, got {other:?}"),
        }
        match LlmError::from_upstream_status(reqwest::StatusCode::BAD_REQUEST, "x".into()) {
            LlmError::Upstream { status: 400, retryable: false, .. } => {}
            other => panic!("expected non-retryable 400 Upstream, got {other:?}"),
        }
    }

    #[test]
    fn context_overflow_classification() {
        // The three provider families (Plan 031), via the daemon variant.
        let of = |msg: &str| LlmError::Upstream { status: 400, message: msg.into(), retryable: false };
        assert!(of("prompt is too long: 90000 tokens > 32000 maximum").is_context_overflow());
        assert!(of("{\"type\":\"request_too_large\"}").is_context_overflow());
        assert!(of("This model's maximum context length is 8192 tokens").is_context_overflow());
        assert!(of("the request exceeds the available context size").is_context_overflow());
        assert!(LlmError::Api("context_length_exceeded".into()).is_context_overflow());
        // Rate limiting / unknown errors are NOT overflow.
        assert!(!of("rate limit exceeded").is_context_overflow());
        assert!(!of("invalid api key").is_context_overflow());
        assert!(!LlmError::RateLimited.is_context_overflow());
        assert!(!LlmError::NoProvider.is_context_overflow());
    }
}
