//! Chat data model for the TUI — an ordered list of content blocks.
//!
//! A single assistant turn produces *multiple* blocks streamed over time:
//!   💭 思考 (reasoning before a tool call)
//!     → ⏳/📁 工具 (one or more tool calls, each with a running→done lifecycle)
//!   … (repeat for multi-step ReAct turns)
//!   → ● 回答 (the final answer)
//!
//! Each block has a uniform UI shape (icon + title + body, blank line between
//! blocks). See `tui::build_chat_lines` for rendering.

use serde_json::Value;

/// A tool call block. Has a running→done lifecycle so the UI can show "⏳
/// running…" (via [`Self::done`] == false) and then the result.
#[derive(Debug, Clone)]
pub struct ToolBlock {
    pub tool: String,
    pub args_summary: String,
    pub result: String,
    pub collapsed: bool,
    /// `false` while the tool is executing (only the start was reported);
    /// `true` once the result arrived.
    pub done: bool,
}

impl ToolBlock {
    /// Start a tool call (running state, no result yet).
    pub fn start(tool: &str, args: &Value) -> Self {
        Self {
            tool: tool.into(),
            args_summary: format_args_summary(tool, args),
            result: String::new(),
            collapsed: true,
            done: false,
        }
    }

    /// Complete a tool call with its result.
    pub fn finish(result: String) -> Self {
        Self {
            tool: String::new(),
            args_summary: String::new(),
            result,
            collapsed: true,
            done: true,
        }
    }
}

/// One content block within an assistant turn (or a standalone chat line).
#[derive(Debug, Clone)]
pub enum Block {
    /// ● 回答 — the final answer text (streamed in via deltas).
    Answer { text: String },
    /// 💭 思考 — reasoning text produced before a tool call (i.e. a Delta
    /// stream that turns out to precede a tool call rather than end the turn).
    Thinking { text: String },
    /// A tool call block (running or done).
    Tool(ToolBlock),
    /// 你> — a user message.
    User { text: String },
    /// A system/info message.
    System { text: String },
    /// An error message.
    Error { text: String },
    /// A divider line (turn summary).
    Divider { text: String },
}

impl Block {
    /// Is this an answer/thinking text block (eligible to be reclassified)?
    pub fn is_streaming_text(&self) -> bool {
        matches!(self, Block::Answer { .. } | Block::Thinking { .. })
    }

    /// Is this a tool block that is still running (start reported, no result)?
    pub fn is_running_tool(&self) -> bool {
        matches!(self, Block::Tool(t) if !t.done)
    }
}

/// A single assistant turn: an ordered sequence of blocks.
#[derive(Debug, Clone)]
pub struct AssistantTurn {
    pub blocks: Vec<Block>,
    /// When this turn started (for the dialog header timestamp).
    pub created_at: chrono::DateTime<chrono::Local>,
}

impl Default for AssistantTurn {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            created_at: chrono::Local::now(),
        }
    }
}

impl AssistantTurn {
    /// Append text to the last block if it's a text block (Answer/Thinking),
    /// otherwise start a new Answer block. Returns nothing.
    pub fn append_text(&mut self, text: &str) {
        if let Some(Block::Answer { text: t }) = self.blocks.last_mut() {
            t.push_str(text);
        } else {
            self.blocks.push(Block::Answer { text: text.into() });
        }
    }

    /// Append thinking text to the last block if it's a Thinking block,
    /// otherwise start a new Thinking block.
    pub fn append_thinking(&mut self, text: &str) {
        if let Some(Block::Thinking { text: t }) = self.blocks.last_mut() {
            t.push_str(text);
        } else {
            self.blocks.push(Block::Thinking { text: text.into() });
        }
    }

    /// Reclassify the trailing Answer block (if any) as a Thinking block —
    /// used when a tool call arrives, marking the preceding text as reasoning
    /// rather than the final answer. No-op if the last block isn't an Answer.
    pub fn demote_answer_to_thinking(&mut self) {
        if let Some(Block::Answer { text }) = self.blocks.last() {
            let text = text.clone();
            *self.blocks.last_mut().unwrap() = Block::Thinking { text };
        }
    }
}

/// One line in the chat log.
#[derive(Debug, Clone)]
pub enum ChatLine {
    Assistant(AssistantTurn),
    /// A user message, with its timestamp (for the dialog header).
    User { text: String, created_at: chrono::DateTime<chrono::Local> },
    System(String),
    Error(String),
    Divider(String),
}

/// The full chat history + current streaming state.
#[derive(Debug, Clone, Default)]
pub struct ChatLog {
    pub lines: Vec<ChatLine>,
    /// Index of the current assistant turn being streamed (if any).
    pub streaming_idx: Option<usize>,
}

impl ChatLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new assistant turn (for streaming).
    pub fn start_assistant(&mut self) {
        self.lines.push(ChatLine::Assistant(AssistantTurn::default()));
        self.streaming_idx = Some(self.lines.len() - 1);
    }

    /// Append text to the current streaming assistant turn.
    /// Text goes into the trailing Answer block (or starts one).
    pub fn append_delta(&mut self, text: &str) {
        if let Some(idx) = self.streaming_idx {
            if let ChatLine::Assistant(turn) = &mut self.lines[idx] {
                turn.append_text(text);
            }
        }
    }

    /// A tool is starting (running state). First demotes any trailing Answer
    /// text to Thinking (it was reasoning before this tool call), then pushes
    /// a new running Tool block.
    pub fn start_tool(&mut self, tool: &str, args: &Value) {
        if let Some(idx) = self.streaming_idx {
            if let ChatLine::Assistant(turn) = &mut self.lines[idx] {
                turn.demote_answer_to_thinking();
                turn.blocks.push(Block::Tool(ToolBlock::start(tool, args)));
            }
        }
    }

    /// A tool finished with a result. Marks the trailing running Tool block
    /// as done and fills in its result.
    pub fn finish_tool(&mut self, tool: &str, args: &Value, result: &str) {
        if let Some(idx) = self.streaming_idx {
            if let ChatLine::Assistant(turn) = &mut self.lines[idx] {
                // Find the trailing running tool block and complete it.
                if let Some(Block::Tool(t)) = turn.blocks.last_mut() {
                    if !t.done {
                        t.tool = tool.into();
                        t.args_summary = format_args_summary(tool, args);
                        t.result = result.into();
                        t.done = true;
                        return;
                    }
                }
                // No running block to complete — push a finished one.
                let mut tb = ToolBlock::start(tool, args);
                tb.result = result.into();
                tb.done = true;
                turn.blocks.push(Block::Tool(tb));
            }
        }
    }

    /// Finish the current assistant message.
    pub fn finish_assistant(&mut self) {
        self.streaming_idx = None;
    }

    /// Add a user message (timestamped at now).
    pub fn add_user(&mut self, text: &str) {
        self.lines.push(ChatLine::User {
            text: text.into(),
            created_at: chrono::Local::now(),
        });
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

    /// Toggle the last tool block's collapsed state in the current/last assistant turn.
    pub fn toggle_last_tool(&mut self) {
        // Find the last assistant turn and toggle its last tool block.
        for line in self.lines.iter_mut().rev() {
            if let ChatLine::Assistant(turn) = line {
                // Walk blocks backwards to find the last Tool block.
                for block in turn.blocks.iter_mut().rev() {
                    if let Block::Tool(t) = block {
                        if t.done {
                            t.collapsed = !t.collapsed;
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Does the current/last assistant turn have any running (unfinished)
    /// tool block? Used by the TUI to keep a spinner alive.
    pub fn has_running_tool(&self) -> bool {
        self.lines.iter().rev().any(|line| {
            if let ChatLine::Assistant(turn) = line {
                turn.blocks.iter().any(|b| b.is_running_tool())
            } else {
                false
            }
        })
    }
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Build a short human-readable summary of a tool call's args, by tool name.
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
        "list_dir" => args.get("path")
            .and_then(|p| p.as_str())
            .map(|p| p.to_string())
            .unwrap_or_default(),
        "search" => {
            let q = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
            format!("\"{q}\"")
        }
        "spawn_pipeline" => {
            let flow = args.get("flow").and_then(|f| f.as_str()).unwrap_or("?");
            format!("flow={flow}")
        }
        _ => String::new(),
    }
}
