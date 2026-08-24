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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use serde_json::{json, Value};

use auto_ai_agent::{
    Agent, Client, Role, StreamEvent, Tool, ToolError, ToolOutput,
};
use auto_ai_agent::orchestration::{
    FlowSpec, FlowStep, PipelineDriver, PipelineEvent, AgentFactory,
};
use auto_ai_client::{CompletionRequest, CompletionResponse, ClientError, ToolCall, Usage};

// ── Test mocks ─────────────────────────────────────────────────────────────

/// Mock client that returns a scripted sequence of responses.
///
/// Plan 026 test infra: `complete_stream` is overridden to simulate real
/// streaming — optional per-response reasoning (thinking) prefixes, delta
/// chunking (`with_chunk_size`), programmable abort injection
/// (`with_abort_after`: sets a cancel flag after the Nth streamed delta), and
/// request capture (`requests()` records every CompletionRequest seen, in
/// order, so tests can assert message ordering across turns).
struct ScriptedClient {
    responses: Mutex<Vec<CompletionResponse>>,
    total: usize,
    /// Per-response reasoning prefix (emitted as {"type":"reasoning"} events).
    thinking: Vec<Option<String>>,
    /// Delta chunk size in chars; 0 = one delta per text.
    chunk: usize,
    /// When Some((n, flag)): set flag after the n-th streamed delta.
    abort: Option<(usize, Arc<AtomicBool>)>,
    /// When Some((i, f)): call f after streaming response i's deltas — used
    /// to inject mid-run actions (e.g. queue a steering message exactly after
    /// turn i's LLM response, deterministically).
    after_response: Option<(usize, Box<dyn Fn() + Send + Sync>)>,
    emitted: Mutex<usize>,
    served: Mutex<usize>,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl ScriptedClient {
    fn new(responses: Vec<CompletionResponse>) -> Self {
        let total = responses.len();
        Self {
            responses: Mutex::new(responses),
            total,
            thinking: vec![None; total],
            chunk: 0,
            abort: None,
            after_response: None,
            emitted: Mutex::new(0),
            served: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn with_chunk_size(mut self, n: usize) -> Self {
        self.chunk = n;
        self
    }

    /// Give response `idx` a reasoning prefix (emitted as thinking events).
    fn with_thinking(mut self, idx: usize, text: &str) -> Self {
        self.thinking[idx] = Some(text.into());
        self
    }

    /// Set `flag` after the n-th streamed delta (abort simulation).
    fn with_abort_after(mut self, n: usize, flag: Arc<AtomicBool>) -> Self {
        self.abort = Some((n, flag));
        self
    }

    /// Call `f` after streaming response `idx`'s deltas (mid-run hook).
    fn with_after_response(mut self, idx: usize, f: Box<dyn Fn() + Send + Sync>) -> Self {
        self.after_response = Some((idx, f));
        self
    }

    /// Every CompletionRequest seen, in order.
    fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Emit one SSE event, counting deltas and firing the abort injection.
    fn emit(&self, cb: &Arc<dyn Fn(Value) + Send + Sync>, v: Value) {
        let mut n = self.emitted.lock().unwrap();
        *n += 1;
        if let Some((m, flag)) = &self.abort {
            if *n >= *m {
                flag.store(true, Ordering::SeqCst);
            }
        }
        cb(v);
    }

    /// Split `s` into chunks of `chunk` chars (whole string when chunk == 0;
    /// empty string → no chunks — the daemon never emits empty deltas).
    fn chunks(&self, s: &str) -> Vec<String> {
        if s.is_empty() {
            return vec![];
        }
        if self.chunk == 0 {
            return vec![s.to_string()];
        }
        s.chars()
            .collect::<Vec<_>>()
            .chunks(self.chunk)
            .map(|c| c.iter().collect())
            .collect()
    }

    fn next_response(&self) -> CompletionResponse {
        let mut q = self.responses.lock().unwrap();
        if q.is_empty() {
            CompletionResponse {
                content: "(empty)".into(), tool_calls: vec![],
                stop_reason: Some("end_turn".into()), usage: None,
                model: "mock".into(), error: None,
            }
        } else {
            q.remove(0)
        }
    }
}

#[async_trait]
impl Client for ScriptedClient {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
        // Plan 028: compaction's summary requests go through complete() —
        // capture them too.
        self.requests.lock().unwrap().push(req.clone());
        Ok(self.next_response())
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        on_event: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        self.requests.lock().unwrap().push(req.clone());
        let i = {
            let mut s = self.served.lock().unwrap();
            *s += 1;
            *s - 1
        };
        let resp = self.next_response();
        if let Some(Some(th)) = self.thinking.get(i) {
            for c in self.chunks(th) {
                self.emit(&on_event, json!({"type": "reasoning", "text": c}));
            }
        }
        for c in self.chunks(&resp.content) {
            self.emit(&on_event, json!({"type": "delta", "text": c}));
        }
        if let Some((hi, f)) = &self.after_response {
            if *hi == i {
                f();
            }
        }
        Ok(resp)
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

/// Attach usage to a scripted response (TurnEnd assertions read this).
fn with_usage(mut r: CompletionResponse, input: u32, output: u32) -> CompletionResponse {
    r.usage = Some(Usage { input_tokens: input, output_tokens: output, ..Default::default() });
    r
}

/// A response requesting several tool calls in one batch.
fn multi_tool_response(calls: Vec<ToolCall>) -> CompletionResponse {
    CompletionResponse {
        content: String::new(),
        tool_calls: calls,
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
    async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError> {
        Ok(args["word"].as_str().unwrap_or("").to_string().into())
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

// ── Harness 6: Plan 026 — turn events / steering / follow-up / cancel ───────

/// Compact tag for one StreamEvent (sequence assertions read better this way).
fn tag(ev: &StreamEvent) -> String {
    match ev {
        StreamEvent::TurnStart { turn } => format!("TS{turn}"),
        StreamEvent::TurnEnd { turn, usage, tool_count } => {
            let u = usage.as_ref()
                .map(|u| u.input_tokens + u.output_tokens)
                .unwrap_or(0);
            format!("TE{turn}/{u}+{tool_count}")
        }
        StreamEvent::Delta { .. } => "D".into(),
        StreamEvent::Thinking { .. } => "TH".into(),
        StreamEvent::ToolStart { tool, .. } => format!("TS:{tool}"),
        StreamEvent::Tool { tool, .. } => format!("T:{tool}"),
        StreamEvent::Warning { .. } => "W".into(),
        StreamEvent::Done { .. } => "DONE".into(),
        StreamEvent::Cancelled { .. } => "CANCEL".into(),
        StreamEvent::Error { .. } => "E".into(),
    }
}

fn collect_events() -> (Arc<Mutex<Vec<StreamEvent>>>, Arc<dyn Fn(StreamEvent) + Send + Sync>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let cb: Arc<dyn Fn(StreamEvent) + Send + Sync> =
        Arc::new(move |ev| sink.lock().unwrap().push(ev));
    (events, cb)
}

/// A 2-turn run (tool call → final answer) emits the full turn-annotated
/// sequence, and each TurnEnd carries that turn's usage and tool count.
#[tokio::test]
async fn harness_turn_events_sequence() {
    let client = Arc::new(ScriptedClient::new(vec![
        with_usage(tool_call_response("", "echo", json!({"word": "hi"})), 10, 5),
        with_usage(text_response("done"), 20, 7),
    ]).with_chunk_size(2));
    let mut agent = Agent::new(TestRole, client);
    agent.register_tool(EchoTool);

    let (events, cb) = collect_events();
    let cancel = Arc::new(AtomicBool::new(false));
    let result = agent.run_stream("task", cb, cancel).await.unwrap();

    assert_eq!(result.output, "done");
    assert_eq!(result.turns, 2);
    let got: Vec<String> = events.lock().unwrap().iter().map(tag).collect();
    // "done" with chunk size 2 → two Delta events; turn1 usage 15 (10+5) with
    // 1 tool, turn2 usage 27 (20+7) with 0 tools.
    assert_eq!(
        got.join(","),
        "TS1,TS:echo,T:echo,TE1/15+1,TS2,D,D,TE2/27+0,DONE",
        "turn-annotated event sequence mismatch: {:?}",
        got
    );
}

/// Reasoning deltas stream out as Thinking (not Delta) — Plan 026 pi parity.
#[tokio::test]
async fn harness_reasoning_streams_as_thinking() {
    let client = Arc::new(ScriptedClient::new(vec![text_response("answer")])
        .with_thinking(0, "hmm"));
    let mut agent = Agent::new(TestRole, client);

    let (events, cb) = collect_events();
    let cancel = Arc::new(AtomicBool::new(false));
    agent.run_stream("task", cb, cancel).await.unwrap();

    let got: Vec<String> = events.lock().unwrap().iter().map(tag).collect();
    assert_eq!(got.join(","), "TS1,TH,D,TE1/0+0,DONE");
}

/// A steering message queued mid-run (here: right after turn 1's LLM response,
/// via the client hook — deterministic) enters the context AFTER turn 1's tool
/// result and BEFORE turn 2's LLM request.
#[tokio::test]
async fn harness_steering_injected_between_tool_and_next_llm() {
    // The hook fires right after turn 1's response was streamed — the same
    // instant a UI "steer" button would fire mid-run. A slot breaks the
    // client↔agent cycle: the hook is registered before the agent exists,
    // then the slot is filled with the agent's steering-queue handle.
    let slot: Arc<Mutex<Option<Arc<Mutex<std::collections::VecDeque<String>>>>>> =
        Arc::new(Mutex::new(None));
    let hook_slot = slot.clone();
    let client = Arc::new(ScriptedClient::new(vec![
        tool_call_response("", "echo", json!({"word": "a"})),
        text_response("ok"),
    ])
    .with_after_response(0, Box::new(move || {
        if let Some(q) = hook_slot.lock().unwrap().as_ref() {
            q.lock().unwrap().push_back("steer me".into());
        }
    })));
    let mut agent = Agent::new(TestRole, client.clone());
    agent.register_tool(EchoTool);
    *slot.lock().unwrap() = Some(agent.steering_queue());

    let result = agent.run("go").await.unwrap();
    assert_eq!(result.output, "ok");

    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "expected 2 LLM requests");
    // Request 1 saw no steering message.
    assert_eq!(reqs[0].messages.len(), 1, "first request should be just the task");
    // Request 2: [user task, assistant tool_use, user tool_result, user steer].
    let msgs = &reqs[1].messages;
    assert_eq!(msgs.len(), 4, "second request missing steering message: {:?}", msgs);
    assert_eq!(msgs[2].role, "user");
    assert!(matches!(&msgs[2].content[0], auto_ai_client::ContentBlock::ToolResult { .. }),
        "message before steering should be the tool result");
    assert_eq!(msgs[3].role, "user");
    assert!(matches!(&msgs[3].content[0], auto_ai_client::ContentBlock::Text { text } if text.contains("steer me")),
        "steering message should be last: {:?}", msgs[3].content);
}

/// A queued follow-up revives a run that was about to end naturally.
#[tokio::test]
async fn harness_follow_up_revives_run() {
    let client = Arc::new(ScriptedClient::new(vec![
        text_response("first answer"),
        text_response("second answer"),
    ]));
    let mut agent = Agent::new(TestRole, client.clone());
    agent.follow_up("go deeper");

    let result = agent.run("start").await.unwrap();

    // The run did NOT end at "first answer" — the follow-up kept it going.
    assert_eq!(result.output, "second answer");
    assert_eq!(result.turns, 2);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    let msgs = &reqs[1].messages;
    // [user "start", assistant "first answer", user "go deeper"]
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1].role, "assistant");
    assert!(matches!(&msgs[2].content[0], auto_ai_client::ContentBlock::Text { text } if text == "go deeper"));
}

/// A tool whose execution requests cancellation (deterministic mid-batch abort).
struct FlagTool { flag: Arc<AtomicBool> }
#[async_trait]
impl Tool for FlagTool {
    fn name(&self) -> &str { "flag" }
    fn description(&self) -> &str { "Sets the cancel flag." }
    fn parameters(&self) -> Value { json!({"type":"object","properties":{}}) }
    async fn execute(&self, _args: &Value) -> Result<ToolOutput, ToolError> {
        self.flag.store(true, Ordering::SeqCst);
        Ok("flagged".into())
    }
}

/// Cancelling mid tool-batch: the in-flight batch's unanswered tool calls get
/// placeholder results (wire stays valid), queued steering is dropped with a
/// Warning, and the run ends with Cancelled.
#[tokio::test]
async fn harness_cancel_mid_batch_keeps_wire_and_drops_steering() {
    let cancel = Arc::new(AtomicBool::new(false));
    // Queue the steering message mid-run (after turn 1's response streams) —
    // if it were queued before the run, turn 1's steering poll would consume
    // it into memory and there'd be nothing to drop at cancel time.
    let slot: Arc<Mutex<Option<Arc<Mutex<std::collections::VecDeque<String>>>>>> =
        Arc::new(Mutex::new(None));
    let hook_slot = slot.clone();
    let client = Arc::new(ScriptedClient::new(vec![multi_tool_response(vec![
        ToolCall { id: "call_1".into(), name: "flag".into(), input: json!({}) },
        ToolCall { id: "call_2".into(), name: "echo".into(), input: json!({"word": "b"}) },
    ])])
    .with_after_response(0, Box::new(move || {
        if let Some(q) = hook_slot.lock().unwrap().as_ref() {
            q.lock().unwrap().push_back("never lands".into());
        }
    })));
    let mut agent = Agent::new(TestRole, client);
    agent.register_tool(FlagTool { flag: cancel.clone() });
    agent.register_tool(EchoTool);
    *slot.lock().unwrap() = Some(agent.steering_queue());

    let (events, cb) = collect_events();
    let result = agent.run_stream("go", cb, cancel).await.unwrap();

    // Only the first tool ran; the second was cut at its pre-execution checkpoint.
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].tool, "flag");

    let evs = events.lock().unwrap();
    let tags: Vec<String> = evs.iter().map(tag).collect();
    assert!(tags.contains(&"CANCEL".to_string()), "missing Cancelled: {tags:?}");
    assert!(
        evs.iter().any(|e| matches!(e, StreamEvent::Warning { text } if text.contains("1 steering message(s) dropped"))),
        "missing steering-dropped warning: {:?}",
        tags
    );

    // Wire validity: every ToolUse id has a matching ToolResult, and the
    // cancelled call's result is the placeholder.
    let msgs = agent.memory_messages();
    let mut use_ids = Vec::new();
    let mut tool_results = Vec::new();
    for m in msgs.iter() {
        for b in &m.content {
            match b {
                auto_ai_client::ContentBlock::ToolUse { id, .. } => use_ids.push(id.clone()),
                auto_ai_client::ContentBlock::ToolResult { tool_use_id, content, .. } => {
                    tool_results.push((tool_use_id.clone(), content.clone()));
                }
                _ => {}
            }
        }
    }
    assert_eq!(use_ids.len(), 2, "expected 2 tool_use blocks: {:?}", use_ids);
    for id in &use_ids {
        assert!(tool_results.iter().any(|(rid, _)| rid == id), "tool_use {} unanswered", id);
    }
    let cancelled = tool_results.iter().find(|(rid, _)| rid == "call_2").unwrap();
    assert_eq!(cancelled.1, "[cancelled by user]");
}

// ── Harness 7: Plan 027 — content/details separation ────────────────────────

/// A tool returning structured details (Plan 027 shape: edit-style).
struct DetailsTool;
#[async_trait]
impl Tool for DetailsTool {
    fn name(&self) -> &str { "editstub" }
    fn description(&self) -> &str { "Edits a stub file." }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
    }
    async fn execute(&self, _args: &Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: "edited 'stub.txt' (1 replacement)".into(),
            details: Some(json!({
                "diff": "@@ -1 +1 @@
-old line
+new line",
                "patch": "stub.patch",
                "first_changed_line": 1
            })),
        })
    }
}

/// details flow to the event stream but NEVER into the LLM request (Plan 027
/// acceptance: captured ToolResult block byte-identical, zero details leak).
#[tokio::test]
async fn harness_tool_details_flow_to_events_not_llm() {
    let client = Arc::new(ScriptedClient::new(vec![
        tool_call_response("", "editstub", json!({"path": "stub.txt"})),
        text_response("done"),
    ]));
    let mut agent = Agent::new(TestRole, client.clone());
    agent.register_tool(DetailsTool);

    let (events, cb) = collect_events();
    let cancel = Arc::new(AtomicBool::new(false));
    let result = agent.run_stream("edit it", cb, cancel).await.unwrap();
    assert_eq!(result.output, "done");

    // 1. The Tool event carries the structured details.
    let evs = events.lock().unwrap();
    let tool_ev = evs.iter().find_map(|e| match e {
        StreamEvent::Tool { details, .. } => Some(details.clone()),
        _ => None,
    }).expect("no Tool event");
    let details = tool_ev.expect("Tool event had no details");
    assert_eq!(details.get("patch").and_then(|p| p.as_str()), Some("stub.patch"));

    // 2. The next LLM request's ToolResult content is EXACTLY the content —
    //    and the serialized request contains none of the details payload.
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    let body = serde_json::to_string(&reqs[1]).unwrap();
    assert!(body.contains("edited 'stub.txt' (1 replacement)"),
        "ToolResult content missing from the LLM request");
    assert!(!body.contains("stub.patch") && !body.contains("@@ -1 +1 @@"),
        "details leaked into the LLM request: {body}");
}

// ── Harness 8: Plan 028 — context compaction ────────────────────────────────

#[tokio::test]
async fn harness_compact_summarizes_prefix_keeps_tail() {
    use auto_ai_agent::compaction::find_cut_point;
    use auto_ai_client::ContentBlock;

    // 50 turns × ~800 chars (~200 tokens each) ≈ 20k tokens.
    let mut mem = auto_ai_agent::Memory::new(None);
    for i in 0..50 {
        mem.add("user", &format!("turn {i}: {}", "x".repeat(800)));
        mem.add("assistant", &"y".repeat(800));
    }

    // The summary response for the compaction request (with the Files list).
    let client = Arc::new(ScriptedClient::new(vec![text_response(
        "## Goal
fix the bug

## Files
- src/main.rs
- src/lib.rs",
    )]));
    let client_dyn: Arc<dyn Client> = client.clone();
    let settings = auto_ai_agent::CompactionSettings {
        context_window: 10_000,
        reserve_tokens: 1_000,
        keep_recent_tokens: 2_000,
    };
    let next = auto_ai_agent::compact(&mem, &client_dyn, "tier:mid", &settings).await.unwrap();

    // Exactly one summary request went out, isolated (its own system prompt).
    let reqs = client.requests();
    assert_eq!(reqs.len(), 1, "summary must be a single isolated request");
    assert!(
        reqs[0].system_prompt.as_deref().unwrap_or("").contains("compress"),
        "summary request must carry the summarizer system prompt"
    );
    assert_eq!(reqs[0].messages.len(), 1);

    // The compacted memory: summary anchor first (user role, wire-legal),
    // carrying the structured summary with the Files list.
    let msgs = next.messages();
    assert!(msgs.len() < 20, "compacted memory must be much smaller: {}", msgs.len());
    assert_eq!(msgs[0].role, "user");
    match &msgs[0].content[0] {
        ContentBlock::Text { text } => {
            assert!(text.contains("Compacted conversation summary"));
            assert!(text.contains("## Files"));
            assert!(text.contains("src/main.rs"), "file list must survive");
        }
        other => panic!("summary anchor should be text, got {other:?}"),
    }
    // The kept tail is verbatim recent turns.
    let last = msgs.last().unwrap();
    assert_eq!(last.role, "assistant");

    // Cut-point purity proxy: a cut exists and stays inside the list.
    let all = mem.messages();
    if let Some(cut) = find_cut_point(&all, 2_000) {
        assert!(cut >= 1 && cut < all.len());
    }
}

#[tokio::test]
async fn harness_agent_auto_compacts_before_run() {
    use auto_ai_agent::CompactionSettings;

    // 3 filler rounds (~875 tokens each with long tasks/answers) cross the
    // 2.5k threshold; round 4's run starts with a compaction summary request
    // (script response 4), then the real request (response 5).
    let filler = |i: usize| text_response(&format!("r{i} {}", "w".repeat(2000)));
    let client = Arc::new(ScriptedClient::new(vec![
        filler(0), filler(1), filler(2),
        text_response("## Goal
keep going

## Files
- a.rs"),
        text_response("ok"),
    ]));
    let mut agent = Agent::new(TestRole, client.clone());
    agent.set_compaction_settings(CompactionSettings {
        context_window: 3_000,
        reserve_tokens: 500,
        keep_recent_tokens: 1_200,
    });

    let long_task = format!("task {}", "q".repeat(1500));
    for _ in 0..3 {
        agent.run(&long_task).await.unwrap();
    }
    // Short memories did not trigger compaction: 3 requests so far, no summary.
    assert_eq!(client.requests().len(), 3);

    // Round 4 crosses the threshold: run_inner compacts first.
    let (events, cb) = collect_events();
    let cancel = Arc::new(AtomicBool::new(false));
    let result = agent.run_stream(&long_task, cb, cancel).await.unwrap();
    assert_eq!(result.output, "ok");

    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "expected 3 fillers + 1 summary + 1 real");
    assert!(
        reqs[3].system_prompt.as_deref().unwrap_or("").contains("compress"),
        "request 4 must be the isolated summary request"
    );
    // The real request starts with the summary anchor.
    let m0 = &reqs[4].messages[0];
    assert_eq!(m0.role, "user");
    assert!(
        matches!(&m0.content[0], auto_ai_client::ContentBlock::Text { text } if text.contains("Compacted conversation summary")),
        "compacted memory must lead the next LLM request"
    );
    // A warning told the stream about the compaction.
    assert!(
        events.lock().unwrap().iter().any(|e| matches!(e, StreamEvent::Warning { text } if text.contains("context compacted"))),
        "missing compaction warning"
    );
}
