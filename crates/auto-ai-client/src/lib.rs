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
    ) -> Result<CompletionResponse, ClientError> {
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
        // Collect tool_calls + metadata from the done event (Plan 006).
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut stop_reason: Option<String> = None;
        let mut usage: Option<Usage> = None;
        let mut model = String::new();
        let mut error_msg: Option<String> = None;

        while let Some(chunk_result) = stream.next().await {
            let bytes = chunk_result.map_err(|e| ClientError::Http(e.to_string()))?;
            for data_line in sse.push(&bytes) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data_line) {
                    if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                        full.push_str(text);
                    }
                    // Parse error event from SSE stream (Plan 008 — error propagation).
                    if value.get("type").and_then(|t| t.as_str()) == Some("error") {
                        error_msg = value
                            .get("message")
                            .and_then(|m| m.as_str())
                            .map(String::from);
                    }
                    // Parse done event for tool_calls + usage + model.
                    if value.get("type").and_then(|t| t.as_str()) == Some("done") {
                        if let Some(tcs) = value.get("tool_calls").and_then(|t| t.as_array()) {
                            tool_calls = tcs.iter().map(|tc| ToolCall {
                                id: tc["id"].as_str().unwrap_or("").to_string(),
                                name: tc["name"].as_str().unwrap_or("").to_string(),
                                input: tc["input"].clone(),
                            }).collect();
                        }
                        stop_reason = value.get("stop_reason").and_then(|t| t.as_str()).map(String::from);
                        model = value.get("model").and_then(|t| t.as_str()).unwrap_or("").to_string();
                        if let Some(u) = value.get("usage") {
                            usage = serde_json::from_value(u.clone()).ok();
                        }
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
                // Parse error event from SSE stream (Plan 008 — error propagation).
                if value.get("type").and_then(|t| t.as_str()) == Some("error") {
                    error_msg = value
                        .get("message")
                        .and_then(|m| m.as_str())
                        .map(String::from);
                }
                if value.get("type").and_then(|t| t.as_str()) == Some("done") {
                    if let Some(tcs) = value.get("tool_calls").and_then(|t| t.as_array()) {
                        tool_calls = tcs.iter().map(|tc| ToolCall {
                            id: tc["id"].as_str().unwrap_or("").to_string(),
                            name: tc["name"].as_str().unwrap_or("").to_string(),
                            input: tc["input"].clone(),
                        }).collect();
                    }
                    stop_reason = value.get("stop_reason").and_then(|t| t.as_str()).map(String::from);
                    model = value.get("model").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    if let Some(u) = value.get("usage") {
                        usage = serde_json::from_value(u.clone()).ok();
                    }
                }
                on_event(value);
            }
        }

        Ok(CompletionResponse {
            content: full,
            tool_calls,
            stop_reason,
            usage,
            model,
            error: error_msg,
        })
    }

    /// Always true now (the client is daemon-only).
    pub fn is_daemon_mode(&self) -> bool {
        true
    }
}

impl Default for AiClient {
    fn default() -> Self {
        // Honor $AAID_URL instead of hardcoding the default port, so tests
        // and callers that set the env var aren't surprised (see plan 011 L6).
        Self::with_url(crate::daemon::daemon_url())
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
