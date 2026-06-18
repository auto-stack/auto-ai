//! AutoOS AI client — a thin daemon HTTP client.
//!
//! This client sends **canonical** [`CompletionRequest`]s to the `aaid`
//! daemon and receives canonical [`CompletionResponse`]s back. It carries no
//! provider knowledge and has no "direct" (LLM-direct) mode — the daemon owns
//! all LLM communication and canonical↔provider translation. The client's only
//! other responsibility is auto-discovering (and lazy-starting) the daemon via
//! [`daemon::ensure_daemon`].
//!
//! Canonical wire types (`Message`, `ContentBlock`, `CompletionRequest`, …)
//! are defined in the `ai-config` crate and re-exported here for convenience.
//!
//! Two modes of constructing a client:
//! - [`AiClient::new`] — discover/start the daemon, error if unreachable.
//! - [`AiClient::with_url`] — talk to a daemon at an explicit URL (testing).

pub mod daemon;
mod error;

// Canonical wire types — single source of truth in ai-config.
pub use ai_config::*;
pub use error::ClientError;

/// The daemon HTTP client. See the crate docs for the (canonical) wire format.
pub struct AiClient {
    url: String,
    http: reqwest::Client,
}

impl AiClient {
    /// Create a client. Auto-discovers the daemon (lazy-starting it if
    /// needed, ssh-agent model). Errors if the daemon can't be reached.
    pub fn new() -> Result<Self, ClientError> {
        let url = daemon::ensure_daemon().ok_or(ClientError::DaemonUnavailable)?;
        tracing::info!("ai-client: daemon at {}", url);
        Ok(Self {
            url,
            http: reqwest::Client::new(),
        })
    }

    /// Create a client pointed at an explicit daemon URL (for testing).
    pub fn with_url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            http: reqwest::Client::new(),
        }
    }

    /// The daemon URL this client talks to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Send a canonical completion request, receive a canonical response.
    pub async fn complete(
        &self,
        req: &CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        let resp = self
            .http
            .post(format!("{}/v1/chat/completions", self.url))
            .header("Content-Type", "application/json")
            .header("X-App-Name", "auto-ai-client")
            .json(req)
            .send()
            .await
            .map_err(ClientError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api(format!("daemon {status}: {text}")));
        }

        resp.json::<CompletionResponse>()
            .await
            .map_err(|e| ClientError::Api(format!("parse response: {e}")))
    }

    /// Always true now (the client is daemon-only).
    pub fn is_daemon_mode(&self) -> bool {
        true
    }
}

impl Default for AiClient {
    fn default() -> Self {
        Self::with_url("http://127.0.0.1:17654")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_error_display() {
        assert!(format!("{}", ClientError::DaemonUnavailable).contains("daemon unavailable"));
        assert!(format!("{}", ClientError::Api("boom".into())).contains("boom"));
    }

    #[test]
    fn with_url_sets_url() {
        let c = AiClient::with_url("http://example:1234");
        assert_eq!(c.url(), "http://example:1234");
        assert!(c.is_daemon_mode());
    }
}
