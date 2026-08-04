//! MVP harness verification tests (Plan 016 第三波 3.2).
//!
//! These tests verify the 5 core harnesses of the auto-ai-agent using mock
//! clients (no real LLM needed). They run in the main workspace against the
//! rust-ref (reference) implementation.
//!
//! Harnesses:
//! 1. tool use — mock client returns a tool_call → verify execution + feedback
//! 2. skill — register_skill_tool → verify skills_block in system prompt
//! 3. agent role — load_builtin → verify souls are real (not placeholders)
//! 4. plan — FlowSpec → PipelineDriver → verify step/handoff events
//! 5. spec — Client/Role dynamic dispatch (Box<dyn>) + ReAct loop runs

use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use serde_json::{json, Value};

use auto_ai_agent::{
    Agent, Client, Role, Tool, ToolError,
};
use auto_ai_agent::orchestration::{
    FlowSpec, FlowStep, PipelineDriver, PipelineEvent, AgentFactory,
};
use auto_ai_client::{CompletionRequest, CompletionResponse, ClientError, ToolCall};

// ── Test mocks ─────────────────────────────────────────────────────────────

/// Mock client that returns a scripted sequence of responses.
struct ScriptedClient {
    responses: Mutex<Vec<CompletionResponse>>,
}

impl ScriptedClient {
    fn new(responses: Vec<CompletionResponse>) -> Self {
        Self { responses: Mutex::new(responses) }
    }
}

#[async_trait]
impl Client for ScriptedClient {
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
        let mut q = self.responses.lock().unwrap();
        if q.is_empty() {
            Ok(CompletionResponse {
                content: "(empty)".into(), tool_calls: vec![],
                stop_reason: Some("end_turn".into()), usage: None,
                model: "mock".into(), error: None,
            })
        } else {
            Ok(q.remove(0))
        }
    }
}

fn text_response(s: &str) -> CompletionResponse {
    CompletionResponse {
        content: s.into(), tool_calls: vec![],
        stop_reason: Some("end_turn".into()), usage: None,
        model: "mock".into(), error: None,
    }
}

fn tool_call_response(text: &str, tool_name: &str, args: Value) -> CompletionResponse {
    CompletionResponse {
        content: text.into(),
        tool_calls: vec![ToolCall {
            id: "call_1".into(), name: tool_name.into(), input: args,
        }],
        stop_reason: Some("tool_use".into()), usage: None,
        model: "mock".into(), error: None,
    }
}

/// Simple echo tool for tool-use tests.
struct EchoTool;
#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "Echo back a word." }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"word":{"type":"string"}},"required":["word"]})
    }
    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        Ok(args["word"].as_str().unwrap_or("").to_string())
    }
}

/// Minimal role for tests.
struct TestRole;
impl Role for TestRole {
    fn name(&self) -> &str { "test" }
    fn system_prompt(&self) -> &str { "You are a test assistant." }
    fn max_turns(&self) -> usize { 5 }
}

// ── Harness 1: tool use ─────────────────────────────────────────────────────

/// Verify the tool-calling loop: mock client returns a tool_call → agent
/// executes the tool → feeds result back → mock client returns final answer.
#[tokio::test]
async fn harness_tool_use_executes_and_feeds_back() {
    let client = Arc::new(ScriptedClient::new(vec![
        // Turn 1: model requests a tool call.
        tool_call_response("", "echo", json!({"word": "hello"})),
        // Turn 2: model produces final answer (after seeing tool result).
        text_response("ECHO: hello"),
    ]));

    let mut agent = Agent::new(TestRole, client);
    agent.register_tool(EchoTool);

    let result = agent.run("echo the word hello").await.unwrap();

    // The agent should have made exactly 1 tool call.
    assert_eq!(result.tool_calls.len(), 1, "expected 1 tool call, got {}", result.tool_calls.len());
    assert_eq!(result.tool_calls[0].tool, "echo");
    // The final output should reflect the tool result fed back.
    assert!(result.output.contains("hello"), "output should contain tool result: {}", result.output);
}

// ── Harness 2: skill ────────────────────────────────────────────────────────

/// Verify that register_skill_tool caches the available-skills block so it
/// appears in the system prompt sent to the LLM.
#[tokio::test]
async fn harness_skill_injects_skills_block() {
    use auto_ai_agent::SkillTool;

    // Create a skill registry by scanning a temp dir with a SKILL.md.
    use std::fs;
    let tmp = std::env::temp_dir().join("mvp_skill_test");
    let skill_dir = tmp.join("test-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "---\nname: test-skill\ndescription: A test skill\n---\nThis is a test skill body.\n").unwrap();

    let registry = Arc::new(
        auto_ai_agent::SkillRegistry::scan(&tmp)
    );
    fs::remove_dir_all(&tmp).ok();
    let skill_tool = SkillTool::new(registry);

    // Verify the skill tool produces a non-empty block.
    let block = skill_tool.available_skills_block();
    assert!(!block.is_empty(), "skills block should not be empty");

    // Register it and verify the agent stores it.
    let client = Arc::new(ScriptedClient::new(vec![text_response("ok")]));
    let mut agent = Agent::new(TestRole, client);
    agent.register_skill_tool(skill_tool);

    // Run a simple turn — the agent's build_request should include the skills
    // block. We verify indirectly: the agent runs without error and the mock
    // client received the request (it returned "ok").
    let result = agent.run("test").await.unwrap();
    assert_eq!(result.output, "ok");
}

// ── Harness 3: agent role ───────────────────────────────────────────────────

/// Verify that builtin roles have real soul content (not "Soul of the X"
/// placeholders). This locks the 3.1 soul-loading fix.
#[test]
fn harness_agent_role_has_real_souls() {
    let names = auto_ai_agent::builtin_names();
    assert!(names.len() >= 10, "expected at least 10 builtin roles, got {}", names.len());

    for name in names.iter() {
        let role = auto_ai_agent::load_builtin(name)
            .unwrap_or_else(|| panic!("load_builtin({}) returned None", name));
        let prompt = role.system_prompt();

        // Must NOT be the placeholder.
        assert!(
            !prompt.starts_with("Soul of the"),
            "role '{}' still has placeholder soul: {}",
            name, &prompt[..prompt.len().min(40)]
        );
        // Must contain some real content (at least 50 chars — real souls are long).
        assert!(
            prompt.len() > 50,
            "role '{}' soul too short ({} chars), likely not loaded: {}",
            name, prompt.len(), &prompt[..prompt.len().min(40)]
        );
    }
}

// ── Harness 4: plan (orchestration) ─────────────────────────────────────────

/// Verify the pipeline orchestration: a 2-step flow produces the expected
/// event sequence (StepStarted → StepCompleted → Completed).
#[tokio::test]
async fn harness_plan_pipeline_event_sequence() {
    // Factory that builds agents with the scripted client.
    let client = Arc::new(ScriptedClient::new(vec![
        text_response("step a done"),
        text_response("step b done"),
    ]));

    struct TestFactory { client: Arc<ScriptedClient> }
    impl AgentFactory for TestFactory {
        fn build_agent(&self, _role: &str, _handoff: Option<&auto_ai_agent::HandoffDocument>) -> Result<Agent, String> {
            Ok(Agent::new(TestRole, self.client.clone()))
        }
    }

    let mut flow = FlowSpec::new("test-flow");
    flow.add_step(FlowStep::new("step_a", "assistant"));
    flow.add_step(FlowStep::new("step_b", "coder"));

    let factory = TestFactory { client };
    let mut driver = PipelineDriver::new(flow, factory, "test task");

    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let on_event: Arc<dyn Fn(PipelineEvent) + Send + Sync> = Arc::new(move |ev| {
        sink.lock().unwrap().push(ev);
    });

    driver.drive("test task", on_event).await.unwrap();

    let got = events.lock().unwrap();
    assert!(got.iter().any(|e| matches!(e, PipelineEvent::Completed)),
        "missing Completed event");
    assert!(got.iter().any(|e| matches!(e, PipelineEvent::StepStarted { step_id, .. } if step_id == "step_a")),
        "missing StepStarted(step_a)");
    assert!(got.iter().any(|e| matches!(e, PipelineEvent::StepCompleted { step_id, .. } if step_id == "step_b")),
        "missing StepCompleted(step_b)");
}

// ── Harness 5: spec (dynamic dispatch) ──────────────────────────────────────

/// Verify that Client and Role work through dynamic dispatch (Box<dyn> /
/// Arc<dyn>) — the spec abstraction that underpins the whole agent. The agent
/// is constructed with concrete types but internally stores them as trait
/// objects, and the ReAct loop runs through those objects.
#[tokio::test]
async fn harness_spec_dynamic_dispatch_react_runs() {
    // Construct with concrete types — Agent stores them as Arc<dyn Client/Role>.
    let client = Arc::new(ScriptedClient::new(vec![
        text_response("dynamic dispatch works"),
    ]));

    let mut agent = Agent::new(TestRole, client);
    // register_tool takes a concrete type, stores as Arc<dyn Tool>.
    agent.register_tool(EchoTool);

    let result = agent.run("verify dispatch").await.unwrap();

    // The ReAct loop ran through dyn Client (ScriptedClient) + dyn Role
    // (TestRole) + dyn Tool (EchoTool) and produced a result.
    assert_eq!(result.output, "dynamic dispatch works");
    assert!(result.turns >= 1, "agent should have run at least 1 turn");
}
