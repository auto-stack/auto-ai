//! Shared OpenAI chat-completions wire-format translation.
//!
//! The daemon's [`crate::provider::OpenAiProvider`] speaks OpenAI's
//! `/v1/chat/completions` format. This module centralizes the translation of
//! our provider-agnostic [`ContentBlock`]s into OpenAI's message shapes (plain
//! text, assistant `tool_calls`, and `role:"tool"` results).
//!
//! Migrated from `auto-ai-client` (Task 6).

use ai_config::{ContentBlock, ToolDefinition};
use serde_json::Value as JsonValue;

/// One tool result extracted from a message's content blocks, ready to become
/// an OpenAI `role:"tool"` message.
pub(crate) struct OpenAiToolResult {
    pub tool_call_id: String,
    pub content: String,
}

/// Classifies a message's content blocks into one of OpenAI's three shapes.
pub(crate) enum OpenAiMsg {
    /// Plain text message (user/assistant/system): a single `content` string.
    Text { role: String, content: String },
    /// Assistant message that requested one or more tools.
    AssistantWithTools {
        text: String,
        tool_calls: Vec<JsonValue>,
    },
    /// A user message composed entirely of tool results → one or more
    /// `role:"tool"` messages.
    ToolResults(Vec<OpenAiToolResult>),
}

/// Translate our content blocks into the OpenAI message shape, given the
/// message's `role`. Tool-use always yields an assistant message; tool results
/// always split into one or more `role:"tool"` messages; everything else is a
/// plain text message in the caller's role.
pub(crate) fn openai_content(role: &str, blocks: &[ContentBlock]) -> OpenAiMsg {
    // Pure tool-result message?
    if !blocks.is_empty()
        && blocks
            .iter()
            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
    {
        let results = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => Some(OpenAiToolResult {
                    tool_call_id: tool_use_id.clone(),
                    content: content.clone(),
                }),
                _ => None,
            })
            .collect();
        return OpenAiMsg::ToolResults(results);
    }

    // Assistant message carrying tool_use blocks?
    let has_tool_use = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    if has_tool_use {
        let text: String = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        let tool_calls: Vec<JsonValue> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => Some(serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": input.to_string(),
                    }
                })),
                _ => None,
            })
            .collect();
        return OpenAiMsg::AssistantWithTools { text, tool_calls };
    }

    // Pure text (or any block we can coerce to text): emit a plain message.
    let content: String = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    OpenAiMsg::Text {
        role: role.to_string(),
        content,
    }
}

/// Translate our [`ToolDefinition`] to OpenAI's tool object.
pub(crate) fn tool_to_openai(t: &ToolDefinition) -> JsonValue {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        }
    })
}

/// Parse OpenAI's `tool_calls` array into our [`ai_config::ToolCall`] list.
pub(crate) fn parse_openai_tool_calls(arr: &[JsonValue]) -> Vec<ai_config::ToolCall> {
    arr.iter()
        .filter_map(|tc| {
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                return None;
            }
            let input = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}"))
                .unwrap_or(serde_json::json!({}));
            Some(ai_config::ToolCall {
                id: tc["id"].as_str().unwrap_or("").into(),
                name,
                input,
            })
        })
        .collect()
}
