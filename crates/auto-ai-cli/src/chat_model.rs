//! Chat data model for the TUI — messages, tool blocks, and the chat log.
//!
//! (Plan 010 Phase 1)

use serde_json::Value;

/// A tool call block in an assistant message — can be collapsed/expanded.
#[derive(Debug, Clone)]
pub struct ToolBlock {
    pub tool: String,
    pub args_summary: String,
    pub result: String,
    pub collapsed: bool,
}

impl ToolBlock {
    pub fn new(tool: &str, args: &Value, result: &str) -> Self {
        let args_summary = format_args_summary(tool, args);
        Self {
            tool: tool.into(),
            args_summary,
            result: result.into(),
            collapsed: true, // default collapsed
        }
    }
}

fn format_args_summary(tool: &str, args: &Value) -> String {
    match tool {
        "run_command" => args.get("cmd")
            .and_then(|c| c.as_str())
            .map(|c| c.chars().take(60).collect::<String>())
            .unwrap_or_default(),
        "write_file" | "edit_file" | "read_file" => args.get("path")
            .and_then(|p| p.as_str())
            .map(|p| p.to_string())
            .unwrap_or_default(),
        "spawn_pipeline" => {
            let flow = args.get("flow").and_then(|f| f.as_str()).unwrap_or("?");
            format!("flow={flow}")
        }
        _ => String::new(),
    }
}

/// An assistant message: streaming text + tool blocks.
#[derive(Debug, Clone, Default)]
pub struct AssistantMsg {
    pub text: String,
    pub tools: Vec<ToolBlock>,
}

/// One line in the chat log.
#[derive(Debug, Clone)]
pub enum ChatLine {
    User(String),
    Assistant(AssistantMsg),
    System(String),
    Error(String),
    Divider(String),
}

/// The full chat history + current streaming state.
#[derive(Debug, Clone, Default)]
pub struct ChatLog {
    pub lines: Vec<ChatLine>,
    /// Index of the current assistant message being streamed (if any).
    pub streaming_idx: Option<usize>,
}

impl ChatLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new assistant message (for streaming).
    pub fn start_assistant(&mut self) {
        self.lines.push(ChatLine::Assistant(AssistantMsg::default()));
        self.streaming_idx = Some(self.lines.len() - 1);
    }

    /// Append text to the current streaming assistant message.
    pub fn append_delta(&mut self, text: &str) {
        if let Some(idx) = self.streaming_idx {
            if let ChatLine::Assistant(msg) = &mut self.lines[idx] {
                msg.text.push_str(text);
            }
        }
    }

    /// Add a tool block to the current streaming assistant message.
    pub fn add_tool(&mut self, tool: &str, args: &Value, result: &str) {
        if let Some(idx) = self.streaming_idx {
            if let ChatLine::Assistant(msg) = &mut self.lines[idx] {
                msg.tools.push(ToolBlock::new(tool, args, result));
            }
        }
    }

    /// Finish the current assistant message.
    pub fn finish_assistant(&mut self) {
        self.streaming_idx = None;
    }

    /// Add a user message.
    pub fn add_user(&mut self, text: &str) {
        self.lines.push(ChatLine::User(text.into()));
    }

    /// Add a system/error message.
    pub fn add_system(&mut self, text: &str) {
        self.lines.push(ChatLine::System(text.into()));
    }

    pub fn add_error(&mut self, text: &str) {
        self.lines.push(ChatLine::Error(text.into()));
    }

    pub fn add_divider(&mut self, text: &str) {
        self.lines.push(ChatLine::Divider(text.into()));
    }

    /// Toggle the last tool block's collapsed state in the current/last assistant message.
    pub fn toggle_last_tool(&mut self) {
        // Find the last assistant message with tools.
        for line in self.lines.iter_mut().rev() {
            if let ChatLine::Assistant(msg) = line {
                if !msg.tools.is_empty() {
                    let last = msg.tools.len() - 1;
                    msg.tools[last].collapsed = !msg.tools[last].collapsed;
                    return;
                }
            }
        }
    }
}
