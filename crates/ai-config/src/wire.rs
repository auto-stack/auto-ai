//! Canonical wire types for the AutoOS AI stack.
//!
//! These are the provider-agnostic, neutral-format types exchanged between
//! `auto-ai-client` (which sends canonical [`CompletionRequest`]s) and
//! `auto-ai-daemon` (which receives them and translates to a concrete
//! provider's format). Defining them once here means the client, daemon, and
//! agent crates all share one source of truth — no provider-specific shapes
//! leak across the client↔daemon boundary.
//!
//! Messages carry an ordered list of [`ContentBlock`]s so the same type can
//! represent plain text, assistant tool-use requests, and user tool results.
//! This is what makes the native tool-calling ReAct loop in `auto-ai-agent`
//! possible without a separate protocol.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// A single message in a conversation.
///
/// `content` is a list of [`ContentBlock`]s rather than a flat string so the
/// same message can carry text, tool-use requests (assistant), and tool
/// results (user). The convenience constructors [`Message::user`],
/// [`Message::assistant`], [`Message::system`] wrap a single `Text` block; use
/// [`Message::tool_result`] when feeding a tool's output back to the model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String, // "user" | "assistant" | "system"
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// A user-role message that returns the result of a tool call. The model
    /// matches it to the prior assistant `ToolUse` block via `tool_use_id`.
    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error: false,
            }],
        }
    }

    /// Like [`Message::tool_result`] but flagged as an error.
    pub fn tool_error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error: true,
            }],
        }
    }

    /// Convenience: the concatenated text of all `Text` blocks in this message
    /// (ignoring tool-use / tool-result blocks). Handy for logging & tests.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// One block within a message's content.
///
/// Mirrors the Anthropic content-block model, which the OpenAI provider
/// translates to/from its `tool_calls` array.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// A span of plain text.
    Text {
        text: String,
    },
    /// The model requested a tool invocation (assistant role).
    ToolUse {
        id: String,
        name: String,
        input: JsonValue,
    },
    /// The caller's response to a prior `ToolUse` (user role).
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

impl ContentBlock {
    pub fn text(t: impl Into<String>) -> Self {
        ContentBlock::Text { text: t.into() }
    }
}

/// A tool the model is allowed to call.
///
/// `parameters` is a JSON-schema fragment (the `parameters` field of an
/// OpenAI function definition, or the `input_schema` of an Anthropic tool).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonValue,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A tool invocation the model emitted in its response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: JsonValue,
}

/// A completion request (provider-agnostic).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: Option<usize>,
    pub temperature: Option<f64>,
    pub system_prompt: Option<String>,
    /// Tools the model may call. Empty by default.
    pub tools: Vec<ToolDefinition>,
}

impl CompletionRequest {
    /// Simple single-turn request: one user message.
    pub fn single(model: &str, prompt: &str) -> Self {
        Self {
            model: model.to_string(),
            messages: vec![Message::user(prompt)],
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            tools: Vec::new(),
        }
    }

    /// With a system prompt.
    pub fn with_system(mut self, system: &str) -> Self {
        self.system_prompt = Some(system.to_string());
        self
    }

    /// With max output tokens.
    pub fn with_max_tokens(mut self, n: usize) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// With temperature.
    pub fn with_temperature(mut self, t: f64) -> Self {
        self.temperature = Some(t);
        self
    }

    /// With a set of callable tools.
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }
}

/// A completion response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// The full text response (all chunks joined for streaming).
    pub content: String,
    /// Tool invocations the model requested, if any.
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped (`"end_turn"`, `"tool_use"`, `"stop"`, ...).
    pub stop_reason: Option<String>,
    /// Token usage (if reported by the API).
    pub usage: Option<Usage>,
    /// Model that produced the response.
    pub model: String,
    /// Error message (if any). Content may still be partial.
    pub error: Option<String>,
}

impl CompletionResponse {
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// True when the model wants to call at least one tool.
    pub fn wants_tool(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// Token usage statistics.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl Usage {
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        assert_eq!(Message::user("hi").role, "user");
        assert_eq!(Message::assistant("hello").role, "assistant");
        assert_eq!(Message::system("be nice").role, "system");
    }

    #[test]
    fn message_text_helper() {
        let m = Message::user("hello world");
        assert_eq!(m.text(), "hello world");
    }

    #[test]
    fn message_tool_result() {
        let m = Message::tool_result("call_1", "42");
        assert_eq!(m.role, "user");
        match &m.content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "call_1");
                assert_eq!(content, "42");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn message_tool_error_flags_error() {
        let m = Message::tool_error("call_1", "boom");
        match &m.content[0] {
            ContentBlock::ToolResult { is_error, .. } => assert!(*is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn completion_request_builder() {
        let req = CompletionRequest::single("glm-4.5", "hello")
            .with_system("you are helpful")
            .with_max_tokens(100)
            .with_temperature(0.7);
        assert_eq!(req.model, "glm-4.5");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.system_prompt.as_deref(), Some("you are helpful"));
        assert_eq!(req.max_tokens, Some(100));
        assert_eq!(req.temperature, Some(0.7));
        assert!(req.tools.is_empty());
    }

    #[test]
    fn completion_request_with_tools() {
        let tool = ToolDefinition::new(
            "echo",
            "echo back",
            serde_json::json!({ "type": "object", "properties": {} }),
        );
        let req = CompletionRequest::single("m", "hi").with_tools(vec![tool]);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "echo");
    }

    #[test]
    fn response_wants_tool() {
        let r = CompletionResponse {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                input: serde_json::json!({}),
            }],
            stop_reason: Some("tool_use".into()),
            usage: None,
            model: "m".into(),
            error: None,
        };
        assert!(r.wants_tool());
    }

    #[test]
    fn usage_total() {
        let u = Usage {
            input_tokens: 100,
            output_tokens: 50,
        };
        assert_eq!(u.total_tokens(), 150);
    }
}
