//! Anthropic Claude provider.
//!
//! Uses the Anthropic Messages API (`/v1/messages`) with SSE streaming.
//! Ported from AutoForge's `provider/claude.rs`.

/// PLAN-030 试用排查：provider 拒绝(4xx)时把请求体原文落盘，供离线定位
/// "messages 参数非法"(zhipu 1214) 的具体毒物。文件：
/// ~/.config/autoos/llm-rejects/<epoch_ms>-anthropic-<status>.json
/// （.at 源轨无 fs/tracing 原语，此为 rust 侧排查插桩——retranspile 时保留。）
fn dump_rejected_body(body: &serde_json::Value, status: u16) {
    use std::io::Write;
    let dir = dirs::home_dir()
        .map(|h| h.join(".config/autoos/llm-rejects"))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{ts}-anthropic-{status}.json"));
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = serde_json::to_string_pretty(body).map(|s| f.write_all(s.as_bytes()));
        tracing::error!("anthropic request rejected ({status}); body dumped to {}", path.display());
    }
}

use std::sync::Arc;

use async_trait::async_trait;

use super::AiProvider;
use crate::sse::SseParser;
use ai_config::*;
use crate::LlmError;

pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: String,
    models_list: Vec<String>,
    /// PLAN-064 gate (ProviderConfig.accepts_thinking_param): when false the
    /// request body never carries a `thinking` block, regardless of
    /// req.thinking_level — unverified upstreams stay byte-identical.
    accepts_thinking_param: bool,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(
        name: String,
        base_url: String,
        api_key: String,
        models: Vec<String>,
        accepts_thinking_param: bool,
    ) -> Self {
        Self {
            name,
            base_url,
            api_key,
            models_list: models,
            accepts_thinking_param,
            client: reqwest::Client::new(),
        }
    }

    fn url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{}/v1/messages", base)
    }

    fn build_body(&self, req: &CompletionRequest) -> serde_json::Value {
        // PLAN-030 试用修复（zhipu 1214 根因的纵深防御）：Anthropic messages
        // 首条必须 user——上游以 assistant 开头（如裁剪后历史）时垫一条
        // 合成 user。与 anthropic.at 源轨同步，retranspile 时保留。
        let mut messages: Vec<serde_json::Value> = Vec::new();
        if let Some(first) = req.messages.first() {
            if first.role != "user" {
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{ "type": "text", "text": "(continued)" }],
                }));
            }
        }
        messages.extend(req.messages.iter().map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": content_blocks_to_anthropic(&m.content),
            })
        }));

        // PLAN-064: thinking budget requires max_tokens > budget_tokens
        // (Anthropic contract). Lift max_tokens when the requested thinking
        // budget doesn't fit, keeping ≥1k answer headroom — this is the
        // architecture section's "必要时抬 max_tokens" clause, so the high/max
        // presets survive the default 4096 instead of clamping down to it.
        let mut max_tokens = req.max_tokens.unwrap_or(4096);

        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": max_tokens,
            "messages": messages,
        });

        if let Some(sys) = &req.system_prompt {
            body["system"] = serde_json::json!(sys);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = serde_json::Value::Array(
                req.tools.iter().map(tool_to_anthropic).collect(),
            );
        }

        // PLAN-064: thinking level injection, gated per provider
        // (ProviderConfig.accepts_thinking_param). Closed gate (default) →
        // no `thinking` key at all, whatever the request asks for.
        if self.accepts_thinking_param {
            if let Some(raw) = req.thinking_level.as_deref() {
                match super::parse_thinking_level(raw) {
                    Some(super::ThinkingLevel::Off) => {
                        body["thinking"] = serde_json::json!({ "type": "disabled" });
                    }
                    Some(level) => {
                        let budget = level.budget_tokens() as usize;
                        if max_tokens <= budget {
                            max_tokens = budget + 1024;
                            body["max_tokens"] = serde_json::json!(max_tokens);
                        }
                        body["thinking"] = serde_json::json!({
                            "type": "enabled",
                            "budget_tokens": budget,
                        });
                    }
                    None => {
                        tracing::warn!(
                            "provider '{}': unknown thinking_level '{raw}', skipping thinking injection",
                            self.name
                        );
                    }
                }
            }
        }
        body
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn models(&self) -> Vec<String> {
        self.models_list.clone()
    }

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let body = self.build_body(req);

        let resp = self
            .client
            .post(self.url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(LlmError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            dump_rejected_body(&body, status.as_u16());
            return Err(LlmError::from_upstream_status(status, text));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LlmError::Api(format!("parse response: {}", e)))?;

        // Anthropic returns content as an array of blocks.
        let mut content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(blocks) = json["content"].as_array() {
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(s) = b["text"].as_str() {
                            content.push_str(s);
                        }
                    }
                    Some("tool_use") => {
                        let id = b["id"].as_str().unwrap_or("").to_string();
                        let name = b["name"].as_str().unwrap_or("").to_string();
                        let input = b["input"].clone();
                        tool_calls.push(ToolCall { id, name, input });
                    }
                    _ => {}
                }
            }
        }

        let stop_reason = json["stop_reason"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let usage = json.get("usage").map(usage_from_json);

        let model = json["model"]
            .as_str()
            .unwrap_or(&req.model)
            .to_string();

        Ok(CompletionResponse {
            content,
            tool_calls,
            stop_reason,
            usage,
            model,
            error: None,
            model_meta: None,
        })
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_delta: Arc<dyn Fn(super::StreamDelta) + Send + Sync>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<CompletionResponse, LlmError> {
        let mut body = self.build_body(req);
        body["stream"] = serde_json::json!(true);

        let resp = self
            .client
            .post(self.url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(LlmError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            dump_rejected_body(&body, status.as_u16());
            return Err(LlmError::from_upstream_status(status, text));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut content = String::new();

        // Accumulate tool_use blocks from Anthropic SSE (Plan 006).
        // content_block_start declares id+name; content_block_delta delivers
        // input_json_delta fragments that we concatenate.
        let mut tool_blocks: Vec<ToolBlock> = Vec::new();
        let mut stop_reason: Option<String> = None;
        let mut usage: Option<Usage> = None;

        let process_json = |json: &serde_json::Value,
                            content: &mut String,
                            tool_blocks: &mut Vec<ToolBlock>,
                            stop_reason: &mut Option<String>,
                            usage: &mut Option<Usage>,
                            on_delta: &Arc<dyn Fn(super::StreamDelta) + Send + Sync>| {
            let event_type = json["type"].as_str().unwrap_or("");

            match event_type {
                "content_block_delta" => {
                    if let Some(text) = json["delta"]["text"].as_str() {
                        content.push_str(text);
                        on_delta(super::StreamDelta::Text(text.to_string()));
                    }
                    // Tool input JSON fragments.
                    if let Some(partial) = json["delta"]["partial_json"].as_str() {
                        let index = json["index"].as_u64().map(|v| v as usize).unwrap_or(0);
                        while tool_blocks.len() <= index {
                            tool_blocks.push(ToolBlock::default());
                        }
                        tool_blocks[index].input_json.push_str(partial);
                    }
                    // Reasoning/thinking deltas (standard Anthropic uses
                    // delta.thinking; some GLM/deepseek anthropic-compatible
                    // endpoints put it under delta.reasoning_content).
                    let reasoning = json["delta"]["thinking"]
                        .as_str()
                        .or_else(|| json["delta"]["reasoning_content"].as_str());
                    if let Some(r) = reasoning {
                        on_delta(super::StreamDelta::Reasoning(r.to_string()));
                    }
                }
                "content_block_start" => {
                    if json["content_block"]["type"] == "tool_use" {
                        let index = json["index"].as_u64().map(|v| v as usize).unwrap_or(0);
                        while tool_blocks.len() <= index {
                            tool_blocks.push(ToolBlock::default());
                        }
                        let block = &mut tool_blocks[index];
                        block.id = json["content_block"]["id"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        block.name = json["content_block"]["name"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                    }
                }
                "message_start" => {
                    // Anthropic reports input_tokens in the initial message_start.
                    if let Some(u) = json.get("message").and_then(|m| m.get("usage")) {
                        *usage = Some(usage_from_json(u));
                    }
                }
                "message_delta" => {
                    if let Some(stop) = json["delta"]["stop_reason"].as_str() {
                        *stop_reason = Some(stop.to_string());
                    }
                    // output_tokens is updated/finalized in message_delta.usage.
                    if let Some(u) = json.get("usage") {
                        let out = u["output_tokens"].as_u64().unwrap_or(0) as u32;
                        match usage {
                            Some(prev) => prev.output_tokens = out,
                            None => *usage = Some(Usage { input_tokens: 0, output_tokens: out, cache_read_tokens: 0, cache_write_tokens: 0 }),
                        }
                    }
                }
                _ => {}
            }
        };

        // Idle timeout for the upstream SSE stream: if no chunk arrives within
        // this window, abort (the upstream is stuck, don't hold the permit).
        const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

        loop {
            // Race the next chunk against cancellation and an idle timeout.
            let chunk_result = tokio::select! {
                biased; // poll cancel first so a cancel always wins.
                _ = cancel.cancelled() => {
                    tracing::info!("anthropic stream cancelled by caller");
                    break;
                }
                r = tokio::time::timeout(SSE_IDLE_TIMEOUT, stream.next()) => match r {
                    Ok(Some(chunk)) => chunk.map_err(|e| LlmError::Http(e.to_string()))?,
                    Ok(None) => break, // upstream stream ended
                    Err(_) => {
                        tracing::warn!("anthropic stream idle timeout ({}s), aborting", SSE_IDLE_TIMEOUT.as_secs());
                        return Err(LlmError::Http(format!(
                            "upstream idle timeout ({}s)", SSE_IDLE_TIMEOUT.as_secs()
                        )));
                    }
                }
            };
            let bytes = chunk_result;
            let data_events = parser.push(&bytes);
            for data in data_events {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                    process_json(
                        &json,
                        &mut content,
                        &mut tool_blocks,
                        &mut stop_reason,
                        &mut usage,
                        &on_delta,
                    );
                }
            }
        }

        // Flush remaining.
        for data in parser.finish() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                process_json(
                    &json,
                    &mut content,
                    &mut tool_blocks,
                    &mut stop_reason,
                    &mut usage,
                    &on_delta,
                );
            }
        }

        // Convert accumulated tool blocks into ToolCall structs (degraded
        // substitutions emit a warning delta — musk plan 073 T-03).
        let tool_calls: Vec<ToolCall> =
            tool_calls_from_blocks(tool_blocks, &|msg| {
                on_delta(super::StreamDelta::Warning(msg))
            });

        Ok(CompletionResponse {
            content,
            tool_calls,
            stop_reason,
            usage,
            model: req.model.clone(),
            error: None,
            model_meta: None,
        })
    }
}

// ── Anthropic wire-format adapters ──────────────────────────────────────────

/// Parse an Anthropic `usage` object (non-streaming body or the streaming
/// `message_start.message.usage` frame) into a canonical [`Usage`], keeping
/// the cache dimensions (Plan 028).
fn usage_from_json(u: &serde_json::Value) -> Usage {
    Usage {
        input_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
        output_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
        cache_read_tokens: u["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32,
        cache_write_tokens: u["cache_creation_input_tokens"].as_u64().unwrap_or(0) as u32,
    }
}

/// Translate our provider-agnostic content blocks into Anthropic's content
/// block array. Plain `Text` → `{type:"text"}`, and the user-side
/// `ToolResult` becomes Anthropic's `tool_result` block. (`ToolUse` here is an
/// *assistant* block and is emitted verbatim so prior turns round-trip.)
fn content_blocks_to_anthropic(blocks: &[ContentBlock]) -> serde_json::Value {
    let out: Vec<serde_json::Value> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => serde_json::json!({ "type": "text", "text": text }),
            ContentBlock::ToolUse { id, name, input } => serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            }),
        })
        .collect();
    serde_json::Value::Array(out)
}

/// Translate our [`ToolDefinition`] to Anthropic's tool object.
fn tool_to_anthropic(t: &ToolDefinition) -> serde_json::Value {
    serde_json::json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.parameters,
    })
}

/// A streamed tool_use block under accumulation (content_block_start declares
/// id+name; input_json_delta fragments are concatenated — Plan 006).
#[derive(Default)]
pub(crate) struct ToolBlock {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input_json: String,
}

/// Convert accumulated tool_use blocks into [`ToolCall`] structs.
///
/// Unparseable input JSON substitutes `{}` — fail-closed by design — and
/// invokes `on_warning` with a description so the client frame-stream can
/// surface the why (musk plan 073 T-03 / AC-02).
fn tool_calls_from_blocks(blocks: Vec<ToolBlock>, on_warning: &dyn Fn(String)) -> Vec<ToolCall> {
    blocks
        .into_iter()
        .filter(|tb| !tb.name.is_empty())
        .map(|tb| {
            let input = match serde_json::from_str::<serde_json::Value>(&tb.input_json) {
                Ok(v) => v,
                Err(e) => {
                    // Don't silently degrade to Null (downstream would run
                    // the tool with no args). Log, pass an empty object,
                    // and surface a warning frame to the client.
                    tracing::warn!(
                        "anthropic streaming: malformed tool_use input for '{}': {} \
                         (len={}, first 200: '{}') — passing empty object",
                        tb.name, e, tb.input_json.len(),
                        &tb.input_json[..tb.input_json.len().min(200)]
                    );
                    on_warning(format!(
                        "tool_use '{}' input failed to parse ({}); \
                         raw len={}, head '{}…' — empty input substituted",
                        tb.name, e, tb.input_json.len(),
                        &tb.input_json[..tb.input_json.len().min(80)]
                    ));
                    serde_json::Value::Object(serde_json::Map::new())
                }
            };
            ToolCall { id: tb.id, name: tb.name, input }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_body_anthropic() {
        let p = AnthropicProvider::new(
            "anthropic".into(),
            "https://api.anthropic.com".into(),
            "key".into(),
            vec!["claude-3-5-sonnet-20241022".into()],
            false,
        );
        let req = CompletionRequest::single("claude-3-5-sonnet-20241022", "hi");
        let body = p.build_body(&req);
        assert_eq!(body["model"], "claude-3-5-sonnet-20241022");
        assert_eq!(body["max_tokens"], 4096); // default
        assert_eq!(body["messages"][0]["role"], "user");
    }

    // ── PLAN-064: thinking level injection (gate matrix) ───────────────────

    #[test]
    fn thinking_gate_closed_never_injects() {
        // accepts_thinking_param=false (default): no thinking key whatever
        // the request asks for — unverified upstreams stay byte-identical.
        let p = AnthropicProvider::new(
            "deepseek".into(),
            "https://api.deepseek.com/anthropic".into(),
            "k".into(),
            vec![],
            false,
        );
        for level in ["off", "low", "high", "max"] {
            let req = CompletionRequest::single("m", "hi").with_thinking_level(level);
            let body = p.build_body(&req);
            assert!(body.get("thinking").is_none(), "level {level} leaked past closed gate");
        }
    }

    #[test]
    fn thinking_none_level_injects_nothing() {
        // thinking_level=None (pre-PLAN-064 client): no thinking key even on
        // an open gate.
        let p = AnthropicProvider::new("a".into(), "u".into(), "k".into(), vec![], true);
        let body = p.build_body(&CompletionRequest::single("m", "hi"));
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn thinking_off_injects_disabled() {
        let p = AnthropicProvider::new("a".into(), "u".into(), "k".into(), vec![], true);
        let req = CompletionRequest::single("m", "hi").with_thinking_level("off");
        let body = p.build_body(&req);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body["thinking"].get("budget_tokens").is_none());
        assert_eq!(body["max_tokens"], 4096); // untouched
    }

    #[test]
    fn thinking_low_injects_budget_within_default_max() {
        let p = AnthropicProvider::new("a".into(), "u".into(), "k".into(), vec![], true);
        let req = CompletionRequest::single("m", "hi").with_thinking_level("low");
        let body = p.build_body(&req);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 2048);
        assert_eq!(body["max_tokens"], 4096); // 2048 fits the default — no lift
    }

    #[test]
    fn thinking_high_lifts_max_tokens() {
        let p = AnthropicProvider::new("a".into(), "u".into(), "k".into(), vec![], true);
        let req = CompletionRequest::single("m", "hi").with_thinking_level("high");
        let body = p.build_body(&req);
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
        // budget must stay below max_tokens (anthropic contract): 8192 + 1024.
        assert_eq!(body["max_tokens"], 9216);
    }

    #[test]
    fn thinking_max_lifts_max_tokens() {
        let p = AnthropicProvider::new("a".into(), "u".into(), "k".into(), vec![], true);
        let req = CompletionRequest::single("m", "hi").with_thinking_level("max");
        let body = p.build_body(&req);
        assert_eq!(body["thinking"]["budget_tokens"], 32768);
        assert_eq!(body["max_tokens"], 33792);
    }

    #[test]
    fn thinking_unknown_level_warns_and_skips() {
        let p = AnthropicProvider::new("a".into(), "u".into(), "k".into(), vec![], true);
        let req = CompletionRequest::single("m", "hi").with_thinking_level("turbo");
        let body = p.build_body(&req);
        assert!(body.get("thinking").is_none());
        assert_eq!(body["max_tokens"], 4096);
    }

    #[test]
    fn thinking_level_case_insensitive() {
        let p = AnthropicProvider::new("a".into(), "u".into(), "k".into(), vec![], true);
        let req = CompletionRequest::single("m", "hi").with_thinking_level(" High ");
        let body = p.build_body(&req);
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
    }

    #[test]
    fn url_construction() {
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com/".into(), "k".into(), vec![], false);
        assert_eq!(p.url(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn build_body_includes_tools() {
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com".into(), "k".into(), vec![], false);
        let tool = ToolDefinition::new("get_weather", "weather", serde_json::json!({"type":"object","properties":{}}));
        let req = CompletionRequest::single("claude-3-5-sonnet-20241022", "hi").with_tools(vec![tool]);
        let body = p.build_body(&req);
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        // content blocks are now an array, not a bare string.
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn build_body_omits_tools_when_empty() {
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com".into(), "k".into(), vec![], false);
        let req = CompletionRequest::single("claude-3-5-sonnet-20241022", "hi");
        let body = p.build_body(&req);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_serializes_tool_result_block() {
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com".into(), "k".into(), vec![], false);
        let mut req = CompletionRequest::single("claude-3-5-sonnet-20241022", "hi");
        req.messages.push(Message::tool_result("call_1", "42"));
        let body = p.build_body(&req);
        let last = &body["messages"].as_array().unwrap().last().unwrap()["content"][0];
        assert_eq!(last["type"], "tool_result");
        assert_eq!(last["tool_use_id"], "call_1");
        assert_eq!(last["content"], "42");
    }

    #[test]
    fn usage_from_json_keeps_cache_dimensions() {
        // Non-streaming body shape (also message_start.message.usage).
        let u = serde_json::json!({
            "input_tokens": 1000,
            "output_tokens": 200,
            "cache_read_input_tokens": 700,
            "cache_creation_input_tokens": 250
        });
        let parsed = usage_from_json(&u);
        assert_eq!(parsed.input_tokens, 1000);
        assert_eq!(parsed.output_tokens, 200);
        assert_eq!(parsed.cache_read_tokens, 700);
        assert_eq!(parsed.cache_write_tokens, 250);
        // Missing fields default to 0 (older API versions without caching).
        let bare = usage_from_json(&serde_json::json!({"input_tokens": 5, "output_tokens": 2}));
        assert_eq!((bare.cache_read_tokens, bare.cache_write_tokens), (0, 0));
    }

    #[test]
    fn stream_usage_accumulates_message_start_then_delta() {
        // message_start seeds input + cache dims; message_delta finalizes
        // output_tokens without touching the rest (stream-path shapes).
        let start = serde_json::json!({
            "type": "message_start",
            "message": { "usage": {
                "input_tokens": 800, "output_tokens": 1,
                "cache_read_input_tokens": 500, "cache_creation_input_tokens": 100
            }}
        });
        let mut usage: Option<Usage> = None;
        if let Some(u) = start.get("message").and_then(|m| m.get("usage")) {
            usage = Some(usage_from_json(u));
        }
        let u = usage.as_mut().unwrap();
        let delta = serde_json::json!({"type": "message_delta", "usage": {"output_tokens": 342}});
        if let Some(du) = delta.get("usage") {
            u.output_tokens = du["output_tokens"].as_u64().unwrap_or(0) as u32;
        }
        assert_eq!(u.input_tokens, 800);
        assert_eq!(u.output_tokens, 342);
        assert_eq!(u.cache_read_tokens, 500);
        assert_eq!(u.cache_write_tokens, 100);
    }

    #[test]
    fn parse_tool_use_blocks() {
        // Simulate Anthropic's response: two tool_use blocks + a stop reason.
        let json = serde_json::json!({
            "content": [
                { "type": "text", "text": "calling tools" },
                { "type": "tool_use", "id": "c1", "name": "read_file", "input": { "path": "a.txt" } },
                { "type": "tool_use", "id": "c2", "name": "run_cmd",   "input": { "cmd": "ls" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 10, "output_tokens": 5 },
            "model": "claude-3-5-sonnet-20241022"
        });

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        for b in json["content"].as_array().unwrap() {
            match b["type"].as_str() {
                Some("text") => content.push_str(b["text"].as_str().unwrap_or("")),
                Some("tool_use") => tool_calls.push(ToolCall {
                    id: b["id"].as_str().unwrap_or("").into(),
                    name: b["name"].as_str().unwrap_or("").into(),
                    input: b["input"].clone(),
                }),
                _ => {}
            }
        }
        assert_eq!(content, "calling tools");
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].name, "read_file");
        assert_eq!(tool_calls[0].input["path"], "a.txt");
        assert_eq!(tool_calls[1].name, "run_cmd");
        assert_eq!(json["stop_reason"].as_str(), Some("tool_use"));
    }

    #[test]
    fn degraded_args_substitute_empty_and_warn() {
        // musk plan 073 AC-02: unparseable input_json substitutes `{}` AND
        // emits a warning; healthy blocks stay untouched.
        let warnings: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let w2 = warnings.clone();
        let blocks = vec![
            ToolBlock {
                id: "tu_1".into(),
                name: "read_file".into(),
                input_json: "{\"path\": \"a\"".into(), // truncated
            },
            ToolBlock {
                id: "tu_2".into(),
                name: "write_file".into(),
                input_json: "{\"path\":\"b.txt\",\"content\":\"x\"}".into(),
            },
        ];
        let calls = tool_calls_from_blocks(blocks, &move |msg| {
            w2.lock().unwrap().push(msg);
        });
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].input, serde_json::json!({}));
        assert_eq!(calls[1].input["path"], "b.txt");
        let ws = warnings.lock().unwrap();
        assert_eq!(ws.len(), 1);
        assert!(ws[0].contains("read_file"), "warn mentions tool: {}", ws[0]);
    }
}
