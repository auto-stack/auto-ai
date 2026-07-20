//! OpenAI-compatible provider (works with OpenAI, Zhipu GLM, Moonshot, etc.).
//!
//! Uses the standard OpenAI `/v1/chat/completions` API format with SSE streaming.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::AiProvider;
use crate::sse::SseParser;
use ai_config::*;
use crate::LlmError;

pub struct OpenAiProvider {
    name: String,
    base_url: String,
    api_key: String,
    models_list: Vec<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
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
        format!("{}/chat/completions", base)
    }

    fn build_body(&self, req: &CompletionRequest) -> serde_json::Value {
        // Translate each message. OpenAI has no content-block array: text blocks
        // become a string `content`, `ToolUse` becomes `tool_calls`, and
        // `ToolResult` becomes a separate `role:"tool"` message (which we emit
        // as its own entry in the messages array below).
        let mut messages: Vec<serde_json::Value> = Vec::new();
        for m in &req.messages {
            match crate::format::openai_content(&m.role, &m.content) {
                crate::format::OpenAiMsg::Text { role, content } => {
                    messages.push(serde_json::json!({ "role": role, "content": content }));
                }
                crate::format::OpenAiMsg::AssistantWithTools { text, tool_calls } => {
                    let mut obj = serde_json::json!({ "role": "assistant" });
                    if !text.is_empty() {
                        obj["content"] = serde_json::json!(text);
                    }
                    obj["tool_calls"] = serde_json::Value::Array(tool_calls);
                    messages.push(obj);
                }
                crate::format::OpenAiMsg::ToolResults(results) => {
                    // Each tool result is its own role:"tool" message in OpenAI.
                    for r in results {
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": r.tool_call_id,
                            "content": r.content,
                        }));
                    }
                }
            }
        }

        let mut body = serde_json::json!({
            "model": req.model,
            "stream": false,
        });

        if let Some(sys) = &req.system_prompt {
            // Prepend system message.
            let mut all_msgs = vec![serde_json::json!({ "role": "system", "content": sys })];
            all_msgs.extend(messages);
            body["messages"] = serde_json::Value::Array(all_msgs);
        } else {
            body["messages"] = serde_json::Value::Array(messages);
        }

        if !req.tools.is_empty() {
            body["tools"] = serde_json::Value::Array(
                req.tools
                    .iter()
                    .map(crate::format::tool_to_openai)
                    .collect(),
            );
        }
        if let Some(n) = req.max_tokens {
            body["max_tokens"] = serde_json::json!(n);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        body
    }
}

#[async_trait]
impl AiProvider for OpenAiProvider {
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
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(LlmError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("{}: {}", status, text)));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LlmError::Api(format!("parse response: {}", e)))?;

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        // OpenAI encodes tool invocations as a `tool_calls` array on the message.
        let tool_calls: Vec<ToolCall> = json["choices"][0]["message"]["tool_calls"]
            .as_array()
            .map(|arr| crate::format::parse_openai_tool_calls(arr))
            .unwrap_or_default();

        let stop_reason = json["choices"][0]["finish_reason"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let usage = json.get("usage").map(|u| {
            Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
            }
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
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<CompletionResponse, LlmError> {
        let mut body = self.build_body(req);
        body["stream"] = serde_json::json!(true);

        let resp = self
            .client
            .post(self.url())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(LlmError::from)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("{}: {}", status, text)));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut content = String::new();

        // Accumulate tool_calls from SSE delta chunks (Plan 006).
        #[derive(Default)]
        struct AccumToolCall {
            id: String,
            name: String,
            arguments: String,
        }
        let mut tool_call_accum: Vec<AccumToolCall> = Vec::new();
        let mut finish_reason: Option<String> = None;

        let process_json = |json: &serde_json::Value,
                            content: &mut String,
                            tool_call_accum: &mut Vec<AccumToolCall>,
                            finish_reason: &mut Option<String>,
                            on_delta: &Arc<dyn Fn(String) + Send + Sync>| {
            if let Some(delta) = json["choices"][0]["delta"]["content"].as_str() {
                content.push_str(delta);
                on_delta(delta.to_string());
            }
            if let Some(finish) = json["choices"][0]["finish_reason"].as_str() {
                *finish_reason = Some(finish.to_string());
            }
            // Parse tool_calls deltas (incremental by index).
            if let Some(tcs) = json["choices"][0]["delta"]["tool_calls"].as_array() {
                for tc in tcs {
                    let index = tc["index"].as_u64().map(|v| v as usize).unwrap_or(0);
                    while tool_call_accum.len() <= index {
                        tool_call_accum.push(AccumToolCall::default());
                    }
                    let accum = &mut tool_call_accum[index];
                    if let Some(id) = tc["id"].as_str() {
                        accum.id = id.to_string();
                    }
                    if let Some(name) = tc["function"]["name"].as_str() {
                        accum.name = name.to_string();
                    }
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        accum.arguments.push_str(args);
                    }
                }
            }
        };

        // Idle timeout for the upstream SSE stream: if no chunk arrives within
        // this window, abort (the upstream is stuck, don't hold the permit).
        const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

        loop {
            // Race the next chunk against cancellation and an idle timeout.
            // This ensures a client disconnect (cancel fired) or a stuck
            // upstream (idle timeout) stops pulling tokens promptly.
            let chunk_result = tokio::select! {
                biased; // poll cancel first so a cancel always wins.
                _ = cancel.cancelled() => {
                    tracing::info!("openai stream cancelled by caller");
                    break;
                }
                r = tokio::time::timeout(SSE_IDLE_TIMEOUT, stream.next()) => match r {
                    Ok(Some(chunk)) => chunk.map_err(|e| LlmError::Http(e.to_string()))?,
                    Ok(None) => break, // upstream stream ended
                    Err(_) => {
                        tracing::warn!("openai stream idle timeout ({}s), aborting", SSE_IDLE_TIMEOUT.as_secs());
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
                        &mut tool_call_accum,
                        &mut finish_reason,
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
                    &mut tool_call_accum,
                    &mut finish_reason,
                    &on_delta,
                );
            }
        }

        // Convert accumulated tool_calls into ToolCall structs.
        let tool_calls: Vec<ToolCall> = tool_call_accum
            .into_iter()
            .filter(|tc| !tc.name.is_empty())
            .map(|tc| {
                tracing::debug!(
                    "streaming tool_call: name='{}' id='{}' args_len={}",
                    tc.name, tc.id, tc.arguments.len()
                );
                let input = if tc.arguments.is_empty() {
                    tracing::warn!("streaming: tool_call '{}' has empty arguments", tc.name);
                    serde_json::Value::Object(serde_json::Map::new())
                } else {
                    match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                "streaming: failed to parse tool_call arguments for '{}': {} (args len={}, first 200: '{}')",
                                tc.name, e, tc.arguments.len(), &tc.arguments[..tc.arguments.len().min(200)]
                            );
                            // The arguments might be truncated or malformed.
                            // Try to extract known fields (path, content, cmd) heuristically.
                            extract_fields_heuristic(&tc.arguments)
                        }
                    }
                };
                tracing::debug!(
                    "streaming tool_call parsed: name='{}' input keys={:?}",
                    tc.name,
                    input.as_object().map(|m| m.keys().collect::<Vec<_>>()).unwrap_or_default()
                );
                ToolCall {
                    id: tc.id,
                    name: tc.name,
                    input,
                }
            })
            .collect();

        Ok(CompletionResponse {
            content,
            tool_calls,
            stop_reason: finish_reason,
            usage: None,
            model: req.model.clone(),
            error: None,
        })
    }
}

/// Heuristic extraction of known fields from malformed/truncated JSON arguments.
///
/// When streaming large tool_call arguments (e.g. a full HTML file in write_file),
/// the accumulated JSON string can be truncated or have escaped characters that
/// break serde_json::from_str. This function tries to extract the most important
/// fields (path, content, cmd, old_string, new_string) using simple string matching.
fn extract_fields_heuristic(raw: &str) -> serde_json::Value {
    let mut obj = serde_json::Map::new();

    // Try to find "path" field.
    if let Some(path) = extract_json_string_field(raw, "path") {
        obj.insert("path".into(), serde_json::Value::String(path));
    }
    // Try to find "content" field.
    if let Some(content) = extract_json_string_field(raw, "content") {
        obj.insert("content".into(), serde_json::Value::String(content));
    }
    // Try to find "cmd" field.
    if let Some(cmd) = extract_json_string_field(raw, "cmd") {
        obj.insert("cmd".into(), serde_json::Value::String(cmd));
    }
    // Try to find "old_string" field.
    if let Some(old) = extract_json_string_field(raw, "old_string") {
        obj.insert("old_string".into(), serde_json::Value::String(old));
    }
    // Try to find "new_string" field.
    if let Some(new) = extract_json_string_field(raw, "new_string") {
        obj.insert("new_string".into(), serde_json::Value::String(new));
    }
    // Try to find "task" field.
    if let Some(task) = extract_json_string_field(raw, "task") {
        obj.insert("task".into(), serde_json::Value::String(task));
    }
    // Try to find "flow" field.
    if let Some(flow) = extract_json_string_field(raw, "flow") {
        obj.insert("flow".into(), serde_json::Value::String(flow));
    }

    if obj.is_empty() {
        // Last resort: store raw.
        obj.insert("_raw".into(), serde_json::Value::String(raw.to_string()));
    }

    tracing::warn!(
        "heuristic extraction from {} bytes → {} fields: {:?}",
        raw.len(),
        obj.len(),
        obj.keys().collect::<Vec<_>>()
    );

    serde_json::Value::Object(obj)
}

/// Extract a string field value from a (possibly malformed) JSON string.
/// Looks for `"field_name":` followed by a quoted string value.
fn extract_json_string_field(raw: &str, field: &str) -> Option<String> {
    // Pattern: "field": "value" — but value may contain escaped quotes.
    let patterns = [
        format!("\"{field}\":\""),
        format!("\"{field}\" : \""),
        format!("\"{field}\": \""),
    ];

    for pat in &patterns {
        if let Some(start) = raw.find(pat.as_str()) {
            let value_start = start + pat.len();
            let bytes = raw.as_bytes();
            let mut i = value_start;
            let mut result = String::new();
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    // Escape sequence.
                    match bytes[i + 1] {
                        b'"' => result.push('"'),
                        b'\\' => result.push('\\'),
                        b'n' => result.push('\n'),
                        b't' => result.push('\t'),
                        b'r' => result.push('\r'),
                        b'/' => result.push('/'),
                        _ => {
                            result.push('\\');
                            result.push(bytes[i + 1] as char);
                        }
                    }
                    i += 2;
                } else if bytes[i] == b'"' {
                    // End of string.
                    return Some(result);
                } else {
                    // Collect the character (handle UTF-8 by pushing the raw byte).
                    // This is safe because we're looking for an unescaped " to end.
                    let ch_start = i;
                    let ch_len = utf8_char_len(bytes[i]);
                    if ch_start + ch_len <= bytes.len() {
                        if let Ok(s) = std::str::from_utf8(&bytes[ch_start..ch_start + ch_len]) {
                            result.push_str(s);
                        }
                    }
                    i += ch_len;
                }
            }
            // Reached end of input without closing quote (truncated).
            tracing::warn!(
                "extract_json_string_field '{}': string appears truncated (len={})",
                field, result.len()
            );
            return Some(result);
        }
    }
    None
}

/// Get the byte length of a UTF-8 character from its leading byte.
fn utf8_char_len(byte: u8) -> usize {
    if byte < 0x80 { 1 }
    else if byte < 0xC0 { 1 } // continuation byte (shouldn't happen at start)
    else if byte < 0xE0 { 2 }
    else if byte < 0xF0 { 3 }
    else { 4 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_body_basic() {
        let p = OpenAiProvider::new("test".into(), "https://api.test.com/v1".into(), "key".into(), vec![]);
        let req = CompletionRequest::single("gpt-4o", "hello");
        let body = p.build_body(&req);
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn build_body_with_system() {
        let p = OpenAiProvider::new("test".into(), "https://api.test.com/v1".into(), "key".into(), vec![]);
        let req = CompletionRequest::single("gpt-4o", "hello").with_system("be nice");
        let body = p.build_body(&req);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be nice");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn url_construction() {
        let p = OpenAiProvider::new("z".into(), "https://api.test.com/v1/".into(), "k".into(), vec![]);
        assert_eq!(p.url(), "https://api.test.com/v1/chat/completions");
    }

    #[test]
    fn build_body_includes_tools() {
        let p = OpenAiProvider::new("test".into(), "https://api.test.com/v1".into(), "key".into(), vec![]);
        let tool = ToolDefinition::new("get_weather", "weather", serde_json::json!({"type":"object","properties":{}}));
        let req = CompletionRequest::single("gpt-4o", "hi").with_tools(vec![tool]);
        let body = p.build_body(&req);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn build_body_omits_tools_when_empty() {
        let p = OpenAiProvider::new("test".into(), "https://api.test.com/v1".into(), "key".into(), vec![]);
        let req = CompletionRequest::single("gpt-4o", "hi");
        let body = p.build_body(&req);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_serializes_tool_result_as_role_tool() {
        let p = OpenAiProvider::new("test".into(), "https://api.test.com/v1".into(), "key".into(), vec![]);
        let mut req = CompletionRequest::single("gpt-4o", "hi");
        req.messages.push(Message::tool_result("call_1", "42"));
        let body = p.build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        // [user "hi", tool result]
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["role"], "tool");
        assert_eq!(msgs[1]["tool_call_id"], "call_1");
        assert_eq!(msgs[1]["content"], "42");
    }

    #[test]
    fn parse_openai_tool_calls() {
        // Simulate OpenAI's response carrying a tool_calls array.
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"a.txt\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 },
            "model": "gpt-4o"
        });

        let tool_calls: Vec<ToolCall> = json["choices"][0]["message"]["tool_calls"]
            .as_array()
            .map(|arr| {
                arr.iter().filter_map(|tc| {
                    let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    if name.is_empty() { return None; }
                    let input = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}"))
                        .unwrap_or(serde_json::json!({}));
                    Some(ToolCall {
                        id: tc["id"].as_str().unwrap_or("").into(),
                        name,
                        input,
                    })
                }).collect()
            })
            .unwrap_or_default();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "read_file");
        assert_eq!(tool_calls[0].input["path"], "a.txt");
        assert_eq!(json["choices"][0]["finish_reason"].as_str(), Some("tool_calls"));
    }

    #[test]
    fn heuristic_extracts_path_and_content() {
        // Simulate a truncated/malformed JSON (missing closing brace).
        let raw = r#"{"path":"game.html","content":"<!DOCTYPE html><html>"#;
        let result = super::extract_fields_heuristic(raw);
        assert_eq!(result["path"].as_str(), Some("game.html"));
        assert!(result["content"].as_str().unwrap().contains("<!DOCTYPE"));
    }

    #[test]
    fn heuristic_extracts_from_escaped_content() {
        // Content with escaped quotes and newlines.
        let raw = r#"{"path":"test.txt","content":"line1\nline2\"quoted\""}"#;
        let result = super::extract_fields_heuristic(raw);
        assert_eq!(result["path"].as_str(), Some("test.txt"));
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("line1"));
        assert!(content.contains("quoted"));
    }

    #[test]
    fn heuristic_extracts_from_truncated_content() {
        // Content that was cut off mid-stream (no closing quote).
        let raw = r#"{"path":"big.html","content":"<html><head><title>Game</title><style>body{margin:0}"#;
        let result = super::extract_fields_heuristic(raw);
        assert_eq!(result["path"].as_str(), Some("big.html"));
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("<html>"));
        assert!(content.contains("margin:0"));
    }

    #[test]
    fn heuristic_extracts_cmd_field() {
        let raw = r#"{"cmd":"echo hello"}"#;
        let result = super::extract_fields_heuristic(raw);
        assert_eq!(result["cmd"].as_str(), Some("echo hello"));
    }

    #[test]
    fn heuristic_returns_raw_when_no_known_fields() {
        let raw = r#"{"unknown_field":"value"}"#;
        let result = super::extract_fields_heuristic(raw);
        assert!(result["_raw"].is_string());
    }
}
