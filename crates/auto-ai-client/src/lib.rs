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

    /// Send a **streaming** completion request. Sets `stream: true` and reads
    /// the daemon's SSE response, invoking `on_event` for each parsed event.
    ///
    /// Each event is the JSON object the daemon emitted as a `data:` line:
    /// `{ "type": "delta", "text": "..." }`, `{ "type": "done", ... }`, or
    /// `{ "type": "error", "message": "..." }`. The caller decides how to
    /// handle each type (the agent's `run_stream` turns these into typed
    /// events).
    ///
    /// Returns the accumulated full text (all deltas concatenated) on success.
    pub async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_event: impl Fn(serde_json::Value) + Send + 'static,
    ) -> Result<String, ClientError> {
        let mut req = req.clone();
        req.stream = true;

        let resp = self
            .http
            .post(format!("{}/v1/chat/completions", self.url))
            .header("Content-Type", "application/json")
            .header("X-App-Name", "auto-ai-client")
            .json(&req)
            .send()
            .await
            .map_err(ClientError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api(format!("daemon {status}: {text}")));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut sse = SseBuffer::new();
        let mut full = String::new();

        while let Some(chunk_result) = stream.next().await {
            let bytes = chunk_result.map_err(|e| ClientError::Http(e.to_string()))?;
            for data_line in sse.push(&bytes) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data_line) {
                    if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                        full.push_str(text);
                    }
                    on_event(value);
                }
            }
        }
        for data_line in sse.finish() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data_line) {
                if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                    full.push_str(text);
                }
                on_event(value);
            }
        }

        Ok(full)
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

/// Minimal SSE line buffer: accumulates bytes, yields complete `data:` payloads.
/// (The client's former `sse.rs` moved to the daemon; this is a small inline
/// reader for the daemon's `text/event-stream` response.)
struct SseBuffer {
    buf: String,
}
impl SseBuffer {
    fn new() -> Self {
        Self { buf: String::new() }
    }
    /// Feed bytes, returning any complete `data:` payloads found. A payload is
    /// the text after `data: ` up to a blank line (`\n\n`).
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(idx) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..idx + 2).collect();
            for line in block.lines() {
                if let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
                    let data = data.trim();
                    if data == "[DONE]" {
                        continue;
                    }
                    if !data.is_empty() {
                        out.push(data.to_string());
                    }
                }
            }
        }
        out
    }
    /// Flush any trailing payload without a final `\n\n`.
    fn finish(&mut self) -> Vec<String> {
        if self.buf.is_empty() {
            return Vec::new();
        }
        let block = std::mem::take(&mut self.buf);
        block
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
                    .map(str::trim)
                    .filter(|d| !d.is_empty() && *d != "[DONE]")
                    .map(str::to_string)
            })
            .collect()
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
