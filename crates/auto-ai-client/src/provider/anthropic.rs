//! Anthropic Claude provider.
//!
//! Uses the Anthropic Messages API (`/v1/messages`) with SSE streaming.
//! Ported from AutoForge's `provider/claude.rs`.

use std::sync::Arc;

use async_trait::async_trait;

use super::AiProvider;
use crate::sse::SseParser;
use crate::types::*;
use crate::ClientError;

pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: String,
    models_list: Vec<String>,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(name: String, base_url: String, api_key: String, models: Vec<String>) -> Self {
        Self {
            name,
            base_url,
            api_key,
            models_list: models,
            client: reqwest::Client::new(),
        }
    }

    fn url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{}/v1/messages", base)
    }

    fn build_body(&self, req: &CompletionRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": content_blocks_to_anthropic(&m.content),
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens.unwrap_or(4096),
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

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
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
            .map_err(ClientError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api(format!("{}: {}", status, text)));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ClientError::Api(format!("parse response: {}", e)))?;

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

        let usage = json.get("usage").map(|u| Usage {
            input_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
        });

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
        })
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_delta: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
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
            .map_err(ClientError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::Api(format!("{}: {}", status, text)));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut content = String::new();

        while let Some(chunk_result) = stream.next().await {
            let bytes = chunk_result.map_err(|e| ClientError::Http(e.to_string()))?;
            let data_events = parser.push(&bytes);

            for data in data_events {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(text) = json["delta"]["text"].as_str().map(|s| s.to_string()) {
                        content.push_str(&text);
                        on_delta(text);
                    }
                }
            }
        }

        // Flush remaining.
        for data in parser.finish() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(text) = json["delta"]["text"].as_str().map(|s| s.to_string()) {
                    content.push_str(&text);
                    on_delta(text);
                }
            }
        }

        Ok(CompletionResponse {
            content,
            tool_calls: Vec::new(),
            stop_reason: None,
            usage: None,
            model: req.model.clone(),
            error: None,
        })
    }
}

// ── Anthropic wire-format adapters ──────────────────────────────────────────

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
        );
        let req = CompletionRequest::single("claude-3-5-sonnet-20241022", "hi");
        let body = p.build_body(&req);
        assert_eq!(body["model"], "claude-3-5-sonnet-20241022");
        assert_eq!(body["max_tokens"], 4096); // default
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn url_construction() {
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com/".into(), "k".into(), vec![]);
        assert_eq!(p.url(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn build_body_includes_tools() {
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com".into(), "k".into(), vec![]);
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
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com".into(), "k".into(), vec![]);
        let req = CompletionRequest::single("claude-3-5-sonnet-20241022", "hi");
        let body = p.build_body(&req);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_serializes_tool_result_block() {
        let p = AnthropicProvider::new("a".into(), "https://api.anthropic.com".into(), "k".into(), vec![]);
        let mut req = CompletionRequest::single("claude-3-5-sonnet-20241022", "hi");
        req.messages.push(Message::tool_result("call_1", "42"));
        let body = p.build_body(&req);
        let last = &body["messages"].as_array().unwrap().last().unwrap()["content"][0];
        assert_eq!(last["type"], "tool_result");
        assert_eq!(last["tool_use_id"], "call_1");
        assert_eq!(last["content"], "42");
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
}
