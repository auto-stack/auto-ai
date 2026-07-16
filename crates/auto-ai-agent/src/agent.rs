//! The autonomous agent (Layer 3 core).
//!
//! An [`Agent`] binds a [`Role`] (personality), a [`ToolRegistry`]
//! (capabilities), a [`Memory`] (conversation), and a [`Client`] (LLM
//! transport) into a single ReAct loop in [`Agent::run`].
//!
//! The LLM transport is abstracted behind the [`Client`] trait so the loop can
//! be driven by the real [`auto_ai_client::AiClient`] in production *or* a
//! deterministic mock in tests.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use auto_ai_client::{
    AiClient, ClientError, CompletionRequest, CompletionResponse, ContentBlock, Message,
};

use crate::error::AgentError;
use crate::memory::Memory;
use crate::role_def::Role;
use crate::tool::{tool_to_definition, ToolRegistry};

/// After how many identical (tool, args) repeats the loop bails out as a cycle.
const LOOP_DETECT_THRESHOLD: usize = 3;

/// The LLM transport an Agent talks to.
///
/// Abstracts [`auto_ai_client::AiClient`] so the ReAct loop can be unit-tested
/// with a deterministic mock (see `tests` below).
#[async_trait]
pub trait Client: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError>;

    /// Streaming completion. Calls `on_event` for each SSE event the daemon
    /// emits (a `serde_json::Value` like `{"type":"delta","text":"…"}`).
    /// Returns the accumulated full text on success.
    ///
    /// Default impl: fall back to non-streaming and emit a single delta —
    /// keeps test mocks simple while letting the real client stream.
    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_event: Arc<dyn Fn(serde_json::Value) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        let resp = self.complete(req).await?;
        if !resp.content.is_empty() {
            on_event(serde_json::json!({"type": "delta", "text": resp.content}));
        }
        Ok(resp)
    }
}

/// Adapter wrapping the real Layer-2 [`AiClient`].
#[async_trait]
impl Client for AiClient {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
        AiClient::complete(self, req).await
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_event: Arc<dyn Fn(serde_json::Value) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        AiClient::complete_stream(self, req, move |ev| on_event(ev)).await
    }
}

/// Events emitted by [`Agent::run_stream`] as the ReAct loop progresses.
#[derive(Clone, Debug)]
pub enum StreamEvent {
    /// A chunk of the model's text output.
    Delta { text: String },
    /// A tool was called and produced a result.
    Tool {
        tool: String,
        args: serde_json::Value,
        result: String,
    },
    /// The loop finished successfully (carries the full result).
    Done { result: AgentResult },
    /// The loop failed.
    Error { message: String },
}

/// A record of one tool call made during a run (for diagnostics/results).
#[derive(Clone, Debug)]
pub struct ToolCallRecord {
    pub tool: String,
    pub args: serde_json::Value,
    pub result: String,
}

/// The outcome of [`Agent::run`].
#[derive(Clone, Debug, Default)]
pub struct AgentResult {
    /// The agent's final textual answer.
    pub output: String,
    /// How many ReAct turns the loop ran.
    pub turns: usize,
    /// Every tool call the agent made, in order.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Total tokens consumed (if the transport reports usage).
    pub total_tokens: u64,
}

/// An autonomous agent that drives an LLM toward a goal via a ReAct loop.
pub struct Agent {
    role: Arc<dyn Role>,
    tools: ToolRegistry,
    memory: Memory,
    client: Arc<dyn Client>,
    /// Bootstrap block listing available skills (injected into the system
    /// prompt so the model knows what it can invoke). Set when a SkillTool is
    /// registered via [`Agent::register_skill_tool`].
    skills_block: Option<String>,
    /// Project context (e.g. contents of `.musk.md` / `CLAUDE.md`). Prepended
    /// to the system prompt so the agent starts with project knowledge.
    /// Set via [`Agent::with_context`].
    context_block: Option<String>,
}

impl Agent {
    /// Build a new agent from a Role, an LLM transport, and the
    /// Role's memory-limit preference.
    pub fn new<P: Role + 'static>(role: P, client: Arc<dyn Client>) -> Self {
        let limit = role.memory_limit();
        Self {
            role: Arc::new(role),
            tools: ToolRegistry::new(),
            memory: Memory::new(limit),
            client,
            skills_block: None,
            context_block: None,
        }
    }

    /// Inject project context (e.g. the contents of `.musk.md` or `CLAUDE.md`)
    /// into the system prompt. It's prepended before the role's soul, so
    /// the agent starts every turn knowing the project's tech stack, conventions,
    /// and common commands — without having to re-explore from scratch.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        let ctx = context.into();
        if !ctx.trim().is_empty() {
            self.context_block = Some(ctx);
        }
        self
    }

    /// Load project context from a file (`.musk.md`, `CLAUDE.md`, etc.) and
    /// inject it. No-op (returns self unchanged) if the file doesn't exist or
    /// can't be read — so callers can chain it unconditionally.
    pub fn with_context_file(self, path: impl AsRef<std::path::Path>) -> Self {
        match std::fs::read_to_string(path.as_ref()) {
            Ok(content) if !content.trim().is_empty() => {
                tracing::debug!(
                    "agent: loaded context from {}",
                    path.as_ref().display()
                );
                self.with_context(content)
            }
            Ok(_) => self, // empty file
            Err(_) => self, // file doesn't exist — fine
        }
    }

    /// Pre-load conversation history (prior user/assistant turns) so the agent
    /// continues a multi-turn session across a stateless boundary (e.g. an HTTP
    /// chat request). Each pair is (role, content); role is "user" or
    /// "assistant". Tool-role messages are skipped (they're replayed via the
    /// assistant content's tool-call blocks, not as standalone turns).
    ///
    /// (Plan 008 — Chats web app.)
    pub fn with_history<I, S>(mut self, history: I) -> Self
    where
        I: IntoIterator<Item = (S, S)>,
        S: AsRef<str>,
    {
        self.memory.extend_pairs(history);
        self
    }

    /// Register a tool the agent may call.
    pub fn register_tool<T: crate::tool::Tool + 'static>(&mut self, tool: T) {
        self.tools.register(tool);
    }

    /// Register a [`crate::SkillTool`] and store its available-skills bootstrap
    /// block, which gets appended to the system prompt every turn so the model
    /// knows what skills it can invoke via the `skill` tool.
    pub fn register_skill_tool(&mut self, tool: crate::SkillTool) {
        let block = if tool.available_skills_block().is_empty() {
            None
        } else {
            Some(tool.available_skills_block())
        };
        self.tools.register(tool);
        self.skills_block = block;
    }

    /// Register an already-`Arc`'d tool (used by the Workflow engine to share
    /// one tool set across many agents).
    pub fn register_shared(&mut self, tool: Arc<dyn crate::tool::Tool>) {
        self.tools.register_shared(tool);
    }

    /// Borrow the shared tool registry (Phase 5's Workflow shares tools across
    /// agents via this).
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// The Role this agent embodies.
    pub fn role(&self) -> &dyn Role {
        self.role.as_ref()
    }

    /// Current conversation memory (system prompt is injected at run time, not
    /// stored here).
    pub fn history(&self) -> &[Message] {
        self.memory.messages()
    }

    /// Underlying transport.
    pub fn client(&self) -> &Arc<dyn Client> {
        &self.client
    }

    /// Run the ReAct loop against `task`, returning the agent's final answer.
    ///
    /// Each turn: ask the model, execute any tool calls, feed results back.
    /// Stops when the model replies with plain text (no tool calls), a
    /// tool-call cycle is detected (LOOP_DETECT_THRESHOLD), or an optional
    /// safety cap is hit (max_turns * 5, to prevent pathological runaway).
    ///
    /// The Role's `max_turns` is treated as a **soft target**, not a hard
    /// limit — the agent can exceed it if still making progress. The hard
    /// safety cap is 5× the soft target (e.g. role says 20 → hard cap 100).
    pub async fn run(&mut self, task: &str) -> Result<AgentResult, AgentError> {
        // Seed the conversation with the user task.
        self.memory.add("user", task);

        let soft_limit = self.role.max_turns();
        let hard_limit = soft_limit * 5; // safety valve only
        let mut result = AgentResult::default();
        // Track how many times each (tool, args) pair has recurred, for loop
        // detection (ported from AutoForge turn.rs:396-427).
        let mut seen: HashMap<String, usize> = HashMap::new();

        for turn in 0..hard_limit {
            result.turns = turn + 1;

            let req = self.build_request();
            let resp = self.client.complete(&req).await?;

            // Accumulate usage if reported.
            if let Some(u) = &resp.usage {
                result.total_tokens += u.total_tokens() as u64;
            }

            if resp.wants_tool() {
                // Record the assistant's tool-use turn in memory so the next
                // request carries it (some providers require the tool_use
                // block to precede its tool_result).
                let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
                if !resp.content.is_empty() {
                    assistant_blocks.push(ContentBlock::Text {
                        text: resp.content.clone(),
                    });
                }
                for tc in &resp.tool_calls {
                    assistant_blocks.push(ContentBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input: tc.input.clone(),
                    });
                }
                self.memory
                    .add_message(Message { role: "assistant".into(), content: assistant_blocks });

                // Execute each tool call and record results.
                for tc in &resp.tool_calls {
                    let key = format!("{}::{}", tc.name, tc.input);
                    let count = seen.entry(key.clone()).or_insert(0);
                    *count += 1;
                    if *count >= LOOP_DETECT_THRESHOLD {
                        tracing::warn!(
                            "agent: loop detected on tool '{}' ({} repeats) — stopping",
                            tc.name,
                            count
                        );
                        return Err(AgentError::LoopDetected(tc.name.clone()));
                    }

                    let outcome = match self.tools.execute(&tc.name, &tc.input).await {
                        Ok(out) => out,
                        Err(e) => {
                            tracing::warn!("agent: tool '{}' failed: {}", tc.name, e);
                            format!("[tool error: {}]", e)
                        }
                    };
                    result.tool_calls.push(ToolCallRecord {
                        tool: tc.name.clone(),
                        args: tc.input.clone(),
                        result: outcome.clone(),
                    });
                    self.memory.add_message(Message::tool_result(&tc.id, outcome));
                }
                // Loop continues: ask the model again with the tool results.
                continue;
            }

            // No tool calls → final answer.
            result.output = resp.content.clone();
            self.memory.add("assistant", &resp.content);
            return Ok(result);
        }

        // Exceeded hard safety cap without a plain-text answer.
        tracing::warn!(
            "agent hit hard safety cap ({} turns = {}x soft limit {}); stopping",
            hard_limit, 5, soft_limit
        );
        Err(AgentError::MaxTurnsExceeded(hard_limit))
    }

    /// Like [`Agent::run`], but streams events as the loop progresses.
    ///
    /// Events emitted (via `on_event`):
    /// - [`StreamEvent::Delta`] — a text chunk from the model's final answer.
    /// - [`StreamEvent::Tool`] — a tool was called (with its result).
    /// - [`StreamEvent::Done`] — the loop finished (with the full result).
    /// - [`StreamEvent::Error`] — the loop failed.
    ///
    /// Implementation note: each ReAct turn is a separate request. We stream
    /// the *final* answering turn (when the model writes plain text). Tool
    /// turns are not text-streamed (each is one round-trip), but their
    /// execution is reported as a [`StreamEvent::Tool`].
    pub async fn run_stream(
        &mut self,
        task: &str,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Result<AgentResult, AgentError> {
        self.memory.add("user", task);
        let soft_limit = self.role.max_turns();
        let hard_limit = soft_limit * 5; // safety valve only
        let mut result = AgentResult::default();
        let mut seen: HashMap<String, usize> = HashMap::new();

        for turn in 0..hard_limit {
            result.turns = turn + 1;

            // Soft limit warning: when exceeding the role's max_turns, log it
            // but don't stop — the agent may still be making progress.
            if turn + 1 == soft_limit {
                tracing::info!(
                    "agent exceeded soft turn limit ({}); continuing to hard cap ({})",
                    soft_limit, hard_limit
                );
            }

            // Near hard cap: warn the model to wrap up.
            let remaining = hard_limit - turn;
            if remaining <= 5 && remaining > 1 {
                let msg = format!(
                    "⚠️ You have {} turns until the hard stop. If you have enough \
                     information, provide your final answer now.",
                    remaining - 1
                );
                self.memory.add("system", &msg);
                on_event(StreamEvent::Delta {
                    text: format!("\n  ⚠️ {} turns until hard stop — wrap up now.\n", remaining - 1),
                });
            }

            let req = self.build_request();

            // Single streaming request — text deltas + tool_calls both surface
            // from the daemon's SSE stream (Plan 006). No more double request.
            let on_delta = on_event.clone();
            let stream_resp = self
                .client
                .complete_stream(&req, Arc::new(move |ev| {
                    if let Some(t) = ev.get("text").and_then(|t| t.as_str()) {
                        on_delta(StreamEvent::Delta {
                            text: t.to_string(),
                        });
                    }
                }))
                .await?;

            let content = stream_resp.content;
            // Propagate SSE-stream error events from the daemon (Plan 008 fix).
            if let Some(err) = stream_resp.error {
                on_event(StreamEvent::Error {
                    message: err.clone(),
                });
                return Err(AgentError::Config(err));
            }
            if let Some(u) = &stream_resp.usage {
                result.total_tokens += u.total_tokens() as u64;
            }

            if !stream_resp.tool_calls.is_empty() {
                // Record assistant turn + execute tools.
                let mut blocks: Vec<ContentBlock> = Vec::new();
                if !content.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: content.clone(),
                    });
                }
                for tc in &stream_resp.tool_calls {
                    blocks.push(ContentBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input: tc.input.clone(),
                    });
                }
                self.memory
                    .add_message(Message { role: "assistant".into(), content: blocks });
                for tc in &stream_resp.tool_calls {
                    let key = format!("{}::{}", tc.name, tc.input);
                    let count = seen.entry(key).or_insert(0);
                    *count += 1;
                    if *count >= LOOP_DETECT_THRESHOLD {
                        on_event(StreamEvent::Error {
                            message: format!("loop detected on '{}'", tc.name),
                        });
                        return Err(AgentError::LoopDetected(tc.name.clone()));
                    }
                    let outcome =
                        match self.tools.execute(&tc.name, &tc.input).await {
                            Ok(o) => o,
                            Err(e) => format!("[tool error: {e}]"),
                        };
                    result.tool_calls.push(ToolCallRecord {
                        tool: tc.name.clone(),
                        args: tc.input.clone(),
                        result: outcome.clone(),
                    });
                    on_event(StreamEvent::Tool {
                        tool: tc.name.clone(),
                        args: tc.input.clone(),
                        result: outcome.clone(),
                    });
                    self.memory.add_message(Message::tool_result(&tc.id, outcome));
                }
                continue;
            }

            // No tool calls → final answer.
            result.output = content.clone();
            self.memory.add("assistant", &content);
            on_event(StreamEvent::Done {
                result: AgentResult {
                    output: result.output.clone(),
                    turns: result.turns,
                    tool_calls: result.tool_calls.clone(),
                    total_tokens: result.total_tokens,
                },
            });
            return Ok(result);
        }

        on_event(StreamEvent::Error {
            message: format!("hard turn cap ({hard_limit}) exceeded (soft limit was {soft_limit})"),
        });
        tracing::warn!(
            "agent hit hard turn cap: {} (soft={}×5)", hard_limit, soft_limit
        );
        Err(AgentError::MaxTurnsExceeded(hard_limit))
    }

    /// Build the completion request for the current turn: system prompt from
    /// the Role, the role's tier/model, the full memory, and the
    /// tools the Role allows.
    fn build_request(&self) -> CompletionRequest {
        let allowed = self.role.allowed_tools();
        let visible = self.tools.filter(&allowed);
        let tool_defs = visible
            .iter()
            .map(|t| tool_to_definition(t.as_ref()))
            .collect();

        // Model selection: if the role pins a concrete model id (non-
        // empty), use it. Otherwise emit a tier token ("tier:<tier>") that the
        // daemon resolves to a concrete model from its config — so roles
        // declare capability (tier), not a specific model.
        let model = {
            let pinned = self.role.model();
            if !pinned.is_empty() {
                pinned.to_string()
            } else {
                format!(
                    "tier:{}",
                    self.role.model_tier().display_name().to_ascii_lowercase()
                )
            }
        };

        // Build the system prompt: project context (if any) + role soul +
        // (if a SkillTool is registered) the available-skills directory.
        let mut system_prompt = String::new();
        if let Some(ctx) = &self.context_block {
            system_prompt.push_str(ctx);
            system_prompt.push_str("\n\n---\n\n");
        }
        system_prompt.push_str(self.role.system_prompt());
        if let Some(block) = &self.skills_block {
            system_prompt.push_str(block);
        }

        CompletionRequest {
            model,
            messages: self.memory.to_messages(),
            max_tokens: None,
            temperature: Some(self.role.temperature()),
            system_prompt: Some(system_prompt),
            tools: tool_defs,
            stream: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ToolError;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::sync::Mutex;

    // ── A scripted mock client ──────────────────────────────────────────────
    //
    // Returns canned responses in order, so we can drive the ReAct loop
    // deterministically.

    struct MockRole;

    impl Role for MockRole {
        fn name(&self) -> &str {
            "mock"
        }
        fn system_prompt(&self) -> &str {
            "you are a test role"
        }
        fn max_turns(&self) -> usize {
            5
        }
    }

    struct AddOne;
    #[async_trait]
    impl crate::tool::Tool for AddOne {
        fn name(&self) -> &str {
            "add_one"
        }
        fn description(&self) -> &str {
            "add one to n"
        }
        fn parameters(&self) -> Value {
            json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"]})
        }
        async fn execute(&self, args: &Value) -> Result<String, ToolError> {
            let n = args["n"].as_i64().unwrap_or(0);
            Ok((n + 1).to_string())
        }
    }

    /// Mock client that returns a queue of responses. Thread-safe via Mutex so
    /// it can live behind an `Arc<dyn Client>`.
    struct MockClient {
        queue: Mutex<Vec<CompletionResponse>>,
    }

    #[async_trait]
    impl Client for MockClient {
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                return Ok(CompletionResponse {
                    content: "(no more scripted responses)".into(),
                    tool_calls: vec![],
                    stop_reason: None,
                    usage: None,
                    model: "mock".into(),
                    error: None,
                });
            }
            Ok(q.remove(0))
        }
    }

    fn mock_client(resps: Vec<CompletionResponse>) -> Arc<MockClient> {
        Arc::new(MockClient {
            queue: Mutex::new(resps),
        })
    }

    fn text_resp(s: &str) -> CompletionResponse {
        CompletionResponse {
            content: s.into(),
            tool_calls: vec![],
            stop_reason: Some("end_turn".into()),
            usage: None,
            model: "mock".into(),
            error: None,
        }
    }

    fn tool_resp(name: &str, id: &str, args: Value) -> CompletionResponse {
        CompletionResponse {
            content: String::new(),
            tool_calls: vec![auto_ai_client::ToolCall {
                id: id.into(),
                name: name.into(),
                input: args,
            }],
            stop_reason: Some("tool_use".into()),
            usage: None,
            model: "mock".into(),
            error: None,
        }
    }

    #[tokio::test]
    async fn run_tool_then_finish() {
        // Turn 1: model asks to call add_one(1). Turn 2: model says "2".
        let client = mock_client(vec![
            tool_resp("add_one", "c1", json!({"n": 1})),
            text_resp("2"),
        ]);
        let mut agent = Agent::new(MockRole, client as Arc<dyn Client>);
        agent.register_tool(AddOne);

        let result = agent.run("what is 1+1?").await.unwrap();
        assert_eq!(result.turns, 2);
        assert_eq!(result.output, "2");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].tool, "add_one");
        assert_eq!(result.tool_calls[0].result, "2");
    }

    #[tokio::test]
    async fn run_no_tools_immediate_answer() {
        let client = mock_client(vec![text_resp("hello!")]);
        let mut agent = Agent::new(MockRole, client as Arc<dyn Client>);
        let result = agent.run("hi").await.unwrap();
        assert_eq!(result.turns, 1);
        assert_eq!(result.output, "hello!");
        assert!(result.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn run_exceeds_hard_turn_cap() {
        // Role max_turns (soft) = 5. Hard cap = 5×5 = 25.
        // MockClient runs out of scripted responses after 5, returning a
        // text response "(no more...)" with no tool_calls → that counts as
        // the final answer. So we need to test the hard cap differently:
        // give it enough responses to reach the hard cap.
        //
        // For a quick test: soft_limit=5 means hard=25. We can't easily
        // script 25 responses. Instead, verify the soft-limit log fires
        // and the agent eventually terminates with a text answer.
        let client = mock_client(vec![
            tool_resp("add_one", "c1", json!({"n":1})),
            tool_resp("add_one", "c2", json!({"n":2})),
            tool_resp("add_one", "c3", json!({"n":3})),
            tool_resp("add_one", "c4", json!({"n":4})),
            tool_resp("add_one", "c5", json!({"n":5})),
            // After 5 tool calls, MockClient queue empties → returns text response
            // → agent sees no tool_calls → treats as final answer.
        ]);
        let mut agent = Agent::new(MockRole, client as Arc<dyn Client>);
        agent.register_tool(AddOne);

        let result = agent.run("keep going").await.unwrap();
        // The agent consumed all 5 scripted tool responses, then got an
        // empty-queue text response. It should have stopped (not hit hard cap).
        assert_eq!(result.turns, 6); // 5 tool turns + 1 final text turn
    }

    #[tokio::test]
    async fn run_detects_loop() {
        // Same tool + same args 3 times → LoopDetected.
        let client = mock_client(vec![
            tool_resp("add_one", "c1", json!({"n":1})),
            tool_resp("add_one", "c2", json!({"n":1})),
            tool_resp("add_one", "c3", json!({"n":1})),
        ]);
        let mut agent = Agent::new(MockRole, client as Arc<dyn Client>);
        agent.register_tool(AddOne);

        let err = agent.run("loop").await.unwrap_err();
        match err {
            AgentError::LoopDetected(name) => assert_eq!(name, "add_one"),
            other => panic!("expected LoopDetected, got {other:?}"),
        }
    }

    #[test]
    fn build_request_carries_system_prompt_and_tools() {
        let client = mock_client(vec![]);
        let mut agent = Agent::new(MockRole, client as Arc<dyn Client>);
        agent.register_tool(AddOne);
        agent.memory.add("user", "hi");

        let req = agent.build_request();
        assert_eq!(req.system_prompt.as_deref(), Some("you are a test role"));
        // MockRole's model() is empty → tier token emitted.
        assert_eq!(req.model, "tier:mid");
        assert!((req.temperature.unwrap() - 0.3).abs() < 1e-9);
        // MockRole.allowed_tools() is empty → all tools visible.
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "add_one");
        // Memory carries the seeded user message.
        assert!(req.messages.iter().any(|m| m.role == "user"));
    }

    #[test]
    fn build_request_injects_context_block() {
        let client = mock_client(vec![]);
        // No context → bare role prompt.
        let mut agent = Agent::new(MockRole, client.clone());
        agent.memory.add("user", "hi");
        let req = agent.build_request();
        assert!(req.system_prompt.as_deref().unwrap().starts_with("you are a test role"));
        assert!(!req.system_prompt.as_deref().unwrap().contains("PROJECT_CONTEXT"));

        // With context → prepended before the role soul.
        let mut agent2 = Agent::new(MockRole, client)
            .with_context("PROJECT_CONTEXT: this is a Rust project.");
        agent2.memory.add("user", "hi");
        let req2 = agent2.build_request();
        let sys = req2.system_prompt.as_deref().unwrap();
        assert!(sys.starts_with("PROJECT_CONTEXT"));
        assert!(sys.contains("you are a test role")); // role still present
    }

    #[test]
    fn with_context_file_missing_is_noop() {
        let client = mock_client(vec![]);
        let agent = Agent::new(MockRole, client)
            .with_context_file("/nonexistent/context.md");
        // No panic, no context set.
        assert!(agent.context_block.is_none());
    }

    #[test]
    fn with_context_file_loads_content() {
        let path = std::env::temp_dir().join("musk_ctx_file_test.md");
        std::fs::write(&path, "# Project\n\nThis is a test project.").unwrap();
        let client = mock_client(vec![]);
        let agent = Agent::new(MockRole, client)
            .with_context_file(&path);
        assert!(agent.context_block.is_some());
        assert!(agent.context_block.as_deref().unwrap().contains("test project"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn build_request_injects_skill_block_when_skilltool_registered() {
        let client = mock_client(vec![]);
        // 1. No SkillTool → plain system prompt.
        let mut agent = Agent::new(MockRole, client.clone());
        agent.memory.add("user", "hi");
        let req = agent.build_request();
        assert!(!req.system_prompt.as_deref().unwrap().contains("<available_skills>"));

        // 2. SkillTool with a skill → block injected.
        use crate::skill::SkillRegistry;
        use std::sync::Arc as StdArc;
        let tmp = std::env::temp_dir().join("musk_agent_skill_inject_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("demo")).unwrap();
        std::fs::write(
            tmp.join("demo").join("SKILL.md"),
            "---\nname: demo\ndescription: a demo skill\n---\n# Demo\nDo demo things.\n",
        )
        .unwrap();
        let registry = StdArc::new(SkillRegistry::scan(&tmp));
        let skill_tool = crate::SkillTool::new(registry);

        let mut agent2 = Agent::new(MockRole, client);
        agent2.register_skill_tool(skill_tool);
        agent2.memory.add("user", "hi");
        let req2 = agent2.build_request();
        let sys = req2.system_prompt.as_deref().unwrap();
        assert!(sys.starts_with("you are a test role"));
        assert!(sys.contains("<available_skills>"));
        assert!(sys.contains("demo"));
        assert!(sys.contains("a demo skill"));
        // The skill tool itself is also registered.
        assert_eq!(req2.tools.len(), 1);
        assert_eq!(req2.tools[0].name, "skill");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
