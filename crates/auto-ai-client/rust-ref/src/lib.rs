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

// Canonical wire types — single source of truth in ai-config. Re-export only
// what a client consumer needs (review-003 L1: a glob re-export also leaked
// daemon-only types like DaemonConfig/TierRouting through this thin client).
pub use ai_config::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ModelMeta, ToolCall,
    ToolDefinition, Usage,
};
pub use error::ClientError;

/// The daemon HTTP client. See the crate docs for the (canonical) wire format.
pub struct AiClient {
    url: String,
    http: reqwest::Client,
    /// Metadata of the model that served the most recent request (Plan 031).
    /// Refreshed from every response — including the SSE `done` tail frame —
    /// so it follows tier fallback's actual model swaps.
    last_model_meta: std::sync::Mutex<Option<ModelMeta>>,
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
            last_model_meta: std::sync::Mutex::new(None),
        })
    }

    /// Create a client pointed at an explicit daemon URL (for testing).
    pub fn with_url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            http: reqwest::Client::new(),
            last_model_meta: std::sync::Mutex::new(None),
        }
    }

    /// The daemon URL this client talks to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Metadata of the model that served the most recent request (Plan 031).
    /// `None` until the daemon reports it (or when talking to an old daemon
    /// that doesn't embed `model_meta`).
    pub fn last_model_meta(&self) -> Option<ModelMeta> {
        self.last_model_meta.lock().unwrap().clone()
    }

    fn remember_meta(&self, resp: &CompletionResponse) {
        if resp.model_meta.is_some() {
            *self.last_model_meta.lock().unwrap() = resp.model_meta.clone();
        }
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

        let resp = resp
            .json::<CompletionResponse>()
            .await
            .map_err(|e| ClientError::Api(format!("parse response: {e}")))?;
        self.remember_meta(&resp);
        Ok(resp)
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
        let mut model_meta: Option<ModelMeta> = None;
        let mut error_msg: Option<String> = None;

        // E-STREAM-HANG 根修（PLAN-099 解阻）：读循环自持空闲超时——上游断流/
        // 半开连接时 `stream.next()` 曾永久 pending（reqwest 默认无超时；运行
        // 侧 watchselect 的恢复面在同步桥停泊下不可达，preview-live-02 与
        // 099-e2e 双场景实证），run 整体冻结、会话永久 busy。现按读空闲窗
        // （env AUTO_AI_CLIENT_STREAM_IDLE_TIMEOUT_SECS，缺省 90s = daemon 侧
        // 30s 上游窗 ×3 余量）终结为 Http 错误——调用方按既有失败臂落盘可见
        // 错误并释放守卫。
        let idle_timeout = std::time::Duration::from_secs(
            std::env::var("AUTO_AI_CLIENT_STREAM_IDLE_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(90),
        );
        let mut chunk_count: u64 = 0;
        loop {
            let chunk_result = match tokio::time::timeout(idle_timeout, stream.next()).await {
                Err(_elapsed) => {
                    tracing::error!(
                        "llm stream idle timeout: no bytes for {:?} (chunks={})",
                        idle_timeout,
                        chunk_count
                    );
                    return Err(ClientError::Http(format!(
                        "llm stream idle timeout: no bytes for {:?} (chunks={})",
                        idle_timeout, chunk_count
                    )));
                }
                Ok(None) => break, // 上游 EOF（daemon 正常收尾）
                Ok(Some(r)) => r,
            };
            let bytes = chunk_result.map_err(|e| ClientError::Http(e.to_string()))?;
            chunk_count += 1;
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
                        if let Some(m) = value.get("model_meta") {
                            model_meta = serde_json::from_value(m.clone()).ok();
                        }
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

        let resp = CompletionResponse {
            content: full,
            tool_calls,
            stop_reason,
            usage,
            model,
            error: error_msg,
            model_meta,
        };
        self.remember_meta(&resp);
        Ok(resp)
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

    /// E-STREAM-HANG 根修钉测试（PLAN-099 解阻）：上游静默持连时读循环按
    /// 空闲窗终结为 Http 错误（不再永久 pending）。stub 发 2 个 SSE 块后
    /// 静默 30s；env 注入 1s 空闲窗，complete_stream 应在 ~1-2s 内返 Err。
    #[test]
    fn complete_stream_idle_timeout_on_silent_upstream() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            );
            for i in 0..2 {
                let ev = format!("data: {{\"type\":\"thinking\",\"thinking\":\"t{i}\"}}\n\n");
                let p = ev.as_bytes();
                let _ = s.write_all(format!("{:x}\r\n", p.len()).as_bytes());
                let _ = s.write_all(p);
                let _ = s.write_all(b"\r\n");
                let _ = s.flush();
            }
            // 静默持连 30s（测试用 env 短空闲窗，1s 后即应被客户端终结）。
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        std::env::set_var("AUTO_AI_CLIENT_STREAM_IDLE_TIMEOUT_SECS", "1");
        let c = AiClient::with_url(format!("http://127.0.0.1:{port}"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let t0 = std::time::Instant::now();
        let out = rt.block_on(c.complete_stream(
            &CompletionRequest {
                model: "m".into(),
                messages: vec![],
                max_tokens: None,
                temperature: None,
                system_prompt: None,
                tools: vec![],
                stream: true,
                preferred_provider: None,
                thinking_level: None,
                model_chain: vec![],
            },
            Box::new(|_v| {}),
        ));
        let elapsed = t0.elapsed();
        let err = out.err().expect("静默上游应终结为错误");
        assert!(
            format!("{err}").contains("idle timeout"),
            "错误应携带 idle timeout 语义: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "应在空闲窗附近终结（实测 {elapsed:?}）"
        );
    }

    /// PLAN-105 P1（UAT 87af4d4b 定罪形态）：send 阶段（连接/写/等响应头）
    /// 无超时——stub daemon accept 后持连不响应，complete_stream 曾永久
    /// pending（读循环的空闲超时只覆盖响应体阶段，send().await 本身无界；
    /// 099 根修补的是体读窗，此为同文件残余面）。env 注入 1s send 窗，
    /// 应在 ~1-2s（单次重试后）终结为 Http 错误。
    #[test]
    fn complete_stream_send_timeout_on_silent_daemon() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            // 持连不响应（send 阶段悬死形态），测试进程退出时随连接关闭。
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        std::env::set_var("AUTO_AI_CLIENT_SEND_TIMEOUT_SECS", "1");
        let c = AiClient::with_url(format!("http://127.0.0.1:{port}"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let t0 = std::time::Instant::now();
        let out = rt.block_on(c.complete_stream(
            &CompletionRequest {
                model: "m".into(),
                messages: vec![],
                max_tokens: None,
                temperature: None,
                system_prompt: None,
                tools: vec![],
                stream: true,
                preferred_provider: None,
                thinking_level: None,
                model_chain: vec![],
            },
            Box::new(|_v| {}),
        ));
        let elapsed = t0.elapsed();
        let err = out.err().expect("静默 daemon 的 send 阶段应终结为错误");
        assert!(
            format!("{err}").contains("send timeout"),
            "错误应携带 send timeout 语义: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "应在 send 窗（含单次重试）附近终结（实测 {elapsed:?}）"
        );
    }
}
