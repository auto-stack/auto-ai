//! Hand-written glue for the provider modules.
//!
//! Two responsibilities (Plan 025 Phase 4):
//! 1. `build_registry` — construct the concrete providers (OpenAi/Anthropic/
//!    Ollama) from a daemon config. Mirrors rust-ref provider/mod.rs:84-146.
//!    (The .at-side `ProviderRegistry::from_daemon_config` delegates here.)
//! 2. `openai_complete_stream` / `anthropic_complete_stream` — the streaming
//!    completion path. Uses `tokio::select!` for cancel + idle-timeout racing,
//!    which has no `.at` syntax, so the whole select!-loop lives here. The
//!    transpiled providers' `complete_stream` methods delegate to these.
//!
//! (Plan 025 Phase 4: glue for the select! blocker + provider construction.
//! Same pattern as tier_router_glue.rs / server_glue.rs.)

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::anthropic::AnthropicProvider;
use crate::error::LlmError;
use crate::openai::OpenAiProvider;
use crate::ollama::OllamaProvider;
use crate::provider::{AiProvider, ProviderRegistry, StreamDelta};
use crate::sse::SseParser;

use ai_config::{DaemonConfig, ProviderConfig};

// ═══════════════════════════════════════════════════════════════════════════
// build_registry — construct concrete providers from config
// ═══════════════════════════════════════════════════════════════════════════

/// Build a ProviderRegistry from a daemon config. Mirrors rust-ref
/// provider/mod.rs:84-146: resolve each provider's API key, then dispatch by
/// `kind` (anthropic / ollama / openai) to the concrete provider constructor.
pub fn build_registry(config: &DaemonConfig) -> Result<ProviderRegistry, LlmError> {
    let providers = collect_provider_entries(&config.providers, &config.default_provider)?;
    Ok(ProviderRegistry::from_entries(providers, &config.default_provider))
}

/// One (name, provider) pair built from a config entry.
type ProviderEntry = (String, Arc<dyn AiProvider>);

/// Walk the config's providers (HashMap — no .at iteration), resolve each key,
/// and build the concrete provider by kind. Errors on a missing required key.
fn collect_provider_entries(
    providers: &HashMap<String, ProviderConfig>,
    _default: &str,
) -> Result<Vec<ProviderEntry>, LlmError> {
    let mut out: Vec<ProviderEntry> = Vec::new();
    for (name, pc) in providers {
        // Resolve the API key. For auth_required providers this fails fast
        // (None → NoApiKey). For no-auth (localhost) a placeholder is returned.
        let key = match pc.resolve_key() {
            Some(k) => k,
            None => {
                let is_local =
                    pc.base_url.contains("localhost") || pc.base_url.contains("127.0.0.1");
                if is_local {
                    "no-key-needed".to_string()
                } else {
                    return Err(LlmError::NoApiKey(name.clone()));
                }
            }
        };
        // Providers only need the model id list (not the full tier metadata).
        let model_ids: Vec<String> = pc.models.iter().map(|m| m.id.clone()).collect();
        let provider: Arc<dyn AiProvider> = match pc.kind.as_str() {
            "anthropic" => Arc::new(AnthropicProvider::new(
                name.clone(),
                pc.base_url.clone(),
                key,
                model_ids.clone(),
            )),
            "ollama" => Arc::new(OllamaProvider::new(
                name.clone(),
                pc.base_url.clone(),
                model_ids.clone(),
            )),
            "openai" | _ => Arc::new(OpenAiProvider::new(
                name.clone(),
                pc.base_url.clone(),
                key,
                model_ids.clone(),
            )),
        };
        out.push((name.clone(), provider));
    }
    if out.is_empty() {
        return Err(LlmError::NoProvider);
    }
    Ok(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// Streaming completion (tokio::select! — no .at syntax)
// ═══════════════════════════════════════════════════════════════════════════
//
// Both providers' complete_stream share the same structure: POST with
// stream:true, read the SSE byte stream, race each chunk against cancellation
// + an idle timeout, parse deltas. The wire-format parsing differs (OpenAI vs
// Anthropic event shapes), hence two fns. Mirrors rust-ref openai.rs:174-374
// and anthropic.rs:155-358.
//
// These use a real reqwest::Client (the transpiled providers use the global
// `http` module for non-streaming, but streaming needs resp.bytes_stream() +
// futures::StreamExt, which only reqwest provides).

/// OpenAI streaming completion. Delegated by OpenAiProvider::complete_stream.
#[allow(clippy::too_many_arguments)]
pub async fn openai_complete_stream(
    provider: &OpenAiProvider,
    req: &ai_config::CompletionRequest,
    on_delta: Arc<dyn Fn(StreamDelta) + Send + Sync>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<ai_config::CompletionResponse, LlmError> {
    use futures::StreamExt;

    let mut body = provider.build_body(req.clone());
    body["stream"] = serde_json::json!(true);
    body["stream_options"] = serde_json::json!({ "include_usage": true });

    let client = reqwest::Client::new();
    let resp = client
        .post(provider.url())
        .header("Authorization", format!("Bearer {}", provider.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(LlmError::from)?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(LlmError::from_upstream_status(status, &text));
    }

    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut content = String::new();

    #[derive(Default)]
    struct AccumToolCall {
        id: String,
        name: String,
        arguments: String,
    }
    let mut tool_call_accum: Vec<AccumToolCall> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut usage: Option<ai_config::Usage> = None;

    const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    loop {
        let chunk_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            r = tokio::time::timeout(SSE_IDLE_TIMEOUT, stream.next()) => match r {
                Ok(Some(chunk)) => chunk.map_err(|e| LlmError::Http(e.to_string()))?,
                Ok(None) => break,
                Err(_) => return Err(LlmError::Http(format!(
                    "upstream idle timeout ({}s)", SSE_IDLE_TIMEOUT.as_secs()
                ))),
            }
        };
        let data_events = parser.push(std::str::from_utf8(&chunk_result).unwrap_or(""));
        for data in data_events {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(delta) = json["choices"][0]["delta"]["content"].as_str() {
                    content.push_str(delta);
                    on_delta(StreamDelta::Text(delta.to_string()));
                }
                if let Some(r) = json["choices"][0]["delta"]["reasoning_content"]
                    .as_str()
                    .or_else(|| json["choices"][0]["delta"]["reasoning"].as_str())
                {
                    on_delta(StreamDelta::Reasoning(r.to_string()));
                }
                if let Some(finish) = json["choices"][0]["finish_reason"].as_str() {
                    finish_reason = Some(finish.to_string());
                }
                if let Some(u) = json.get("usage") {
                    usage = Some(ai_config::Usage {
                        input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                        output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                        cache_read_tokens: u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0) as u32,
                        cache_write_tokens: 0,
                    });
                }
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
            }
        }
    }

    for data in parser.finish() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(delta) = json["choices"][0]["delta"]["content"].as_str() {
                content.push_str(delta);
                on_delta(StreamDelta::Text(delta.to_string()));
            }
            if let Some(finish) = json["choices"][0]["finish_reason"].as_str() {
                finish_reason = Some(finish.to_string());
            }
            if let Some(u) = json.get("usage") {
                usage = Some(ai_config::Usage {
                    input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                    output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                    cache_read_tokens: u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0) as u32,
                    cache_write_tokens: 0,
                });
            }
        }
    }

    let tool_calls: Vec<ai_config::ToolCall> = tool_call_accum
        .into_iter()
        .filter(|tc| !tc.name.is_empty())
        .map(|tc| {
            let input = if tc.arguments.is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&tc.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
            };
            ai_config::ToolCall {
                id: tc.id,
                name: tc.name,
                input,
            }
        })
        .collect();

    Ok(ai_config::CompletionResponse {
        content,
        tool_calls,
        stop_reason: finish_reason,
        usage,
        model: req.model.clone(),
        error: None,
    })
}

/// Anthropic streaming completion. Delegated by AnthropicProvider::complete_stream.
#[allow(clippy::too_many_arguments)]
pub async fn anthropic_complete_stream(
    provider: &AnthropicProvider,
    req: &ai_config::CompletionRequest,
    on_delta: Arc<dyn Fn(StreamDelta) + Send + Sync>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<ai_config::CompletionResponse, LlmError> {
    use futures::StreamExt;

    let mut body = provider.build_body(req.clone());
    body["stream"] = serde_json::json!(true);

    let client = reqwest::Client::new();
    let resp = client
        .post(provider.url())
        .header("x-api-key", &provider.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(LlmError::from)?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(LlmError::from_upstream_status(status, &text));
    }

    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut content = String::new();

    #[derive(Default)]
    struct ToolBlock {
        id: String,
        name: String,
        input_json: String,
    }
    let mut tool_blocks: Vec<ToolBlock> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut usage: Option<ai_config::Usage> = None;

    const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    loop {
        let chunk_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            r = tokio::time::timeout(SSE_IDLE_TIMEOUT, stream.next()) => match r {
                Ok(Some(chunk)) => chunk.map_err(|e| LlmError::Http(e.to_string()))?,
                Ok(None) => break,
                Err(_) => return Err(LlmError::Http(format!(
                    "upstream idle timeout ({}s)", SSE_IDLE_TIMEOUT.as_secs()
                ))),
            }
        };
        let data_events = parser.push(std::str::from_utf8(&chunk_result).unwrap_or(""));
        for data in data_events {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                let event_type = json["type"].as_str().unwrap_or("");
                match event_type {
                    "content_block_delta" => {
                        if let Some(text) = json["delta"]["text"].as_str() {
                            content.push_str(text);
                            on_delta(StreamDelta::Text(text.to_string()));
                        }
                        if let Some(partial) = json["delta"]["partial_json"].as_str() {
                            let index = json["index"].as_u64().map(|v| v as usize).unwrap_or(0);
                            while tool_blocks.len() <= index {
                                tool_blocks.push(ToolBlock::default());
                            }
                            tool_blocks[index].input_json.push_str(partial);
                        }
                        let reasoning = json["delta"]["thinking"]
                            .as_str()
                            .or_else(|| json["delta"]["reasoning_content"].as_str());
                        if let Some(r) = reasoning {
                            on_delta(StreamDelta::Reasoning(r.to_string()));
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
                        if let Some(u) = json.get("message").and_then(|m| m.get("usage")) {
                            usage = Some(ai_config::Usage {
                                input_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
                                output_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                                cache_read_tokens: u["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32,
                                cache_write_tokens: u["cache_creation_input_tokens"].as_u64().unwrap_or(0) as u32,
                            });
                        }
                    }
                    "message_delta" => {
                        if let Some(stop) = json["delta"]["stop_reason"].as_str() {
                            stop_reason = Some(stop.to_string());
                        }
                        if let Some(u) = json.get("usage") {
                            let out = u["output_tokens"].as_u64().unwrap_or(0) as u32;
                            match &mut usage {
                                Some(prev) => prev.output_tokens = out,
                                None => {
                                    usage = Some(ai_config::Usage {
                                        input_tokens: 0,
                                        output_tokens: out,
                                        cache_read_tokens: 0,
                                        cache_write_tokens: 0,
                                    })
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    for data in parser.finish() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
            let event_type = json["type"].as_str().unwrap_or("");
            if event_type == "content_block_delta" {
                if let Some(text) = json["delta"]["text"].as_str() {
                    content.push_str(text);
                    on_delta(StreamDelta::Text(text.to_string()));
                }
            }
            if event_type == "message_delta" {
                if let Some(stop) = json["delta"]["stop_reason"].as_str() {
                    stop_reason = Some(stop.to_string());
                }
            }
        }
    }

    let tool_calls: Vec<ai_config::ToolCall> = tool_blocks
        .into_iter()
        .filter(|tb| !tb.name.is_empty())
        .map(|tb| {
            let input = serde_json::from_str(&tb.input_json)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            ai_config::ToolCall {
                id: tb.id,
                name: tb.name,
                input,
            }
        })
        .collect();

    Ok(ai_config::CompletionResponse {
        content,
        tool_calls,
        stop_reason,
        usage,
        model: req.model.clone(),
        error: None,
    })
}
