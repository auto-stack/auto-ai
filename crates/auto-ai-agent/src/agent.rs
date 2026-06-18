//! The autonomous agent (Layer 3 core).
//!
//! An [`Agent`] binds a [`Profession`] (personality), a [`ToolRegistry`]
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
use crate::profession::Profession;
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
}

/// Adapter wrapping the real Layer-2 [`AiClient`].
#[async_trait]
impl Client for AiClient {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
        AiClient::complete(self, req).await
    }
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
    profession: Arc<dyn Profession>,
    tools: ToolRegistry,
    memory: Memory,
    client: Arc<dyn Client>,
}

impl Agent {
    /// Build a new agent from a Profession, an LLM transport, and the
    /// Profession's memory-limit preference.
    pub fn new<P: Profession + 'static>(profession: P, client: Arc<dyn Client>) -> Self {
        let limit = profession.memory_limit();
        Self {
            profession: Arc::new(profession),
            tools: ToolRegistry::new(),
            memory: Memory::new(limit),
            client,
        }
    }

    /// Register a tool the agent may call.
    pub fn register_tool<T: crate::tool::Tool + 'static>(&mut self, tool: T) {
        self.tools.register(tool);
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

    /// The Profession this agent embodies.
    pub fn profession(&self) -> &dyn Profession {
        self.profession.as_ref()
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
    /// Stops when the model replies with plain text (no tool calls), the
    /// Profession's `max_turns` is hit, or a tool-call cycle is detected.
    pub async fn run(&mut self, task: &str) -> Result<AgentResult, AgentError> {
        // Seed the conversation with the user task.
        self.memory.add("user", task);

        let max_turns = self.profession.max_turns();
        let mut result = AgentResult::default();
        // Track how many times each (tool, args) pair has recurred, for loop
        // detection (ported from AutoForge turn.rs:396-427).
        let mut seen: HashMap<String, usize> = HashMap::new();

        for turn in 0..max_turns {
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

        // Exceeded max_turns without a plain-text answer.
        Err(AgentError::MaxTurnsExceeded(max_turns))
    }

    /// Build the completion request for the current turn: system prompt from
    /// the Profession, the profession's model/temperature, the full memory,
    /// and the tools the Profession allows.
    fn build_request(&self) -> CompletionRequest {
        let allowed = self.profession.allowed_tools();
        let visible = self.tools.filter(&allowed);
        let tool_defs = visible
            .iter()
            .map(|t| tool_to_definition(t.as_ref()))
            .collect();

        CompletionRequest {
            model: self.profession.model().to_string(),
            messages: self.memory.to_messages(),
            max_tokens: None,
            temperature: Some(self.profession.temperature()),
            system_prompt: Some(self.profession.system_prompt().to_string()),
            tools: tool_defs,
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

    struct MockProfession;

    impl Profession for MockProfession {
        fn name(&self) -> &str {
            "mock"
        }
        fn system_prompt(&self) -> &str {
            "you are a test profession"
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
        let mut agent = Agent::new(MockProfession, client as Arc<dyn Client>);
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
        let mut agent = Agent::new(MockProfession, client as Arc<dyn Client>);
        let result = agent.run("hi").await.unwrap();
        assert_eq!(result.turns, 1);
        assert_eq!(result.output, "hello!");
        assert!(result.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn run_exceeds_max_turns() {
        // Profession max_turns = 5. Feed 5 tool calls then it would still want
        // a tool → should error with MaxTurnsExceeded.
        let client = mock_client(vec![
            tool_resp("add_one", "c1", json!({"n":1})),
            tool_resp("add_one", "c2", json!({"n":2})),
            tool_resp("add_one", "c3", json!({"n":3})),
            tool_resp("add_one", "c4", json!({"n":4})),
            tool_resp("add_one", "c5", json!({"n":5})),
        ]);
        let mut agent = Agent::new(MockProfession, client as Arc<dyn Client>);
        agent.register_tool(AddOne);

        let err = agent.run("keep going").await.unwrap_err();
        match err {
            AgentError::MaxTurnsExceeded(n) => assert_eq!(n, 5),
            other => panic!("expected MaxTurnsExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_detects_loop() {
        // Same tool + same args 3 times → LoopDetected.
        let client = mock_client(vec![
            tool_resp("add_one", "c1", json!({"n":1})),
            tool_resp("add_one", "c2", json!({"n":1})),
            tool_resp("add_one", "c3", json!({"n":1})),
        ]);
        let mut agent = Agent::new(MockProfession, client as Arc<dyn Client>);
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
        let mut agent = Agent::new(MockProfession, client as Arc<dyn Client>);
        agent.register_tool(AddOne);
        agent.memory.add("user", "hi");

        let req = agent.build_request();
        assert_eq!(req.system_prompt.as_deref(), Some("you are a test profession"));
        assert_eq!(req.model, "glm-4.5");
        assert!((req.temperature.unwrap() - 0.3).abs() < 1e-9);
        // MockProfession.allowed_tools() is empty → all tools visible.
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "add_one");
        // Memory carries the seeded user message.
        assert!(req.messages.iter().any(|m| m.role == "user"));
    }
}
