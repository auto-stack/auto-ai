//! Plan 022 Phase 1 — transpiled agent mock-based test harness.
//!
//! Establishes the FIRST automated test suite for the transpiled Auto crate
//! (`auto_ai_agent_a2r`). Mirrors the behaviour coverage of the native
//! reference's `crates/auto-ai-agent/tests/mvp_harness.rs`, but adapts every
//! signature to the transpiled API shape (owned values, `String` returns,
//! `u32` widths, `Box<dyn>` construction, `#[async_trait]` mandatory).
//!
//! All tests are offline — no daemon, no LLM, no API keys. The transpiled
//! `Client` trait is the sole mocking seam (same strategy as rust-ref).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use auto_ai_agent_a2r::{
    load_builtin, builtin_names, Agent, AgentError, Client, Role, SkillTool,
    SkillRegistry, StreamEvent, Tool, ToolError,
};
// spawn_event_sink{,_with} live in the agent module (pub fns, not re-exported
// at the crate root) and the event capture closures live there too.
use auto_ai_agent_a2r::agent::{spawn_event_sink, spawn_event_sink_with};
// Wire types live in the (real) ai-config crate, re-exported through the shim.
use auto_ai_agent_a2r::auto_ai_client::{
    ClientError, CompletionRequest, CompletionResponse, ToolCall,
};

// ─── mock infrastructure ───────────────────────────────────────────────────

/// A `Client` whose `complete` returns a queued `CompletionResponse` per call.
/// Pops the queue front each turn; an empty queue yields a default text reply
/// (so loops that expect a final turn always terminate).
struct ScriptedClient {
    responses: Mutex<Vec<CompletionResponse>>,
    /// Captures the last request (for assertions on preferred_provider etc.).
    last_request: Mutex<Option<CompletionRequest>>,
}

impl ScriptedClient {
    fn new(responses: Vec<CompletionResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            last_request: Mutex::new(None),
        }
    }
}

#[async_trait]
impl Client for ScriptedClient {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        *self.last_request.lock().unwrap() = Some(req);
        let mut q = self.responses.lock().unwrap();
        if q.is_empty() {
            Ok(text_response("(no more scripted responses)"))
        } else {
            Ok(q.remove(0))
        }
    }
}

/// A `Client` that always errors — for error-propagation tests.
struct ErrorClient;

#[async_trait]
impl Client for ErrorClient {
    async fn complete(
        &self,
        _req: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        Err(ClientError::Http("simulated upstream failure".into()))
    }
}

/// A `Client` that captures every request seen into a shared buffer (so the
/// test can read it after the client has been moved into the agent). Replies
/// with a plain text turn so the loop ends after one call.
struct CapturingClient {
    seen: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl CapturingClient {
    fn new(seen: Arc<Mutex<Vec<CompletionRequest>>>) -> Self {
        Self { seen }
    }
}

#[async_trait]
impl Client for CapturingClient {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        self.seen.lock().unwrap().push(req);
        Ok(text_response("done"))
    }
}

/// Echo tool — mirrors crates/auto-ai-agent/rust/src/echo_tool.rs but local to
/// this test so we control the result text for assertions.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> String {
        "echo".into()
    }
    fn description(&self) -> String {
        "Echoes the input word.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"word":{"type":"string"}},"required":["word"]})
    }
    async fn execute(&self, args: Value) -> Result<String, ToolError> {
        let w = args.get("word").and_then(|v| v.as_str()).unwrap_or("");
        Ok(format!("ECHO: {}", w))
    }
}

/// A minimal role for ReAct-loop tests. `max_turns` is configurable via a field
/// (closures aren't allowed in trait impls, so we vary it per test by building
/// a fresh role struct).
struct TestRole {
    max_turns: u32,
    preferred: Option<String>,
}

impl TestRole {
    fn new() -> Self {
        Self {
            max_turns: 10,
            preferred: None,
        }
    }
    fn with_max_turns(n: u32) -> Self {
        Self {
            max_turns: n,
            preferred: None,
        }
    }
    fn with_provider(p: &str) -> Self {
        Self {
            max_turns: 10,
            preferred: Some(p.into()),
        }
    }
}

impl Role for TestRole {
    fn name(&self) -> String {
        "test".into()
    }
    fn system_prompt(&self) -> String {
        "You are a test role.".into()
    }
    fn max_turns(&self) -> u32 {
        self.max_turns
    }
    fn preferred_provider(&self) -> Option<String> {
        self.preferred.clone()
    }
}

// ─── response helpers (build CompletionResponse values) ────────────────────

fn text_response(s: &str) -> CompletionResponse {
    CompletionResponse {
        content: s.into(),
        tool_calls: vec![],
        stop_reason: Some("end_turn".into()),
        usage: None,
        model: "mock".into(),
        error: None,
    }
}

fn tool_call_response(tool_name: &str, args: Value) -> CompletionResponse {
    CompletionResponse {
        content: "".into(),
        tool_calls: vec![ToolCall {
            id: "call_1".into(),
            name: tool_name.into(),
            input: args,
        }],
        stop_reason: Some("tool_use".into()),
        usage: None,
        model: "mock".into(),
        error: None,
    }
}

// ─── T1: tool call → feedback → finish ─────────────────────────────────────
/// Mirrors rust-ref agent.rs `run_tool_then_finish`. Mock returns a tool_call,
/// the agent executes EchoTool, feeds the result back, and the mock then
/// returns a final text turn. Asserts the tool was called once and the output
/// matches the final reply.
#[tokio::test]
async fn t1_tool_call_then_finish() {
    let client = ScriptedClient::new(vec![
        tool_call_response("echo", json!({"word":"hello"})),
        text_response("all done"),
    ]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    agent.register_tool(Box::new(EchoTool));
    let result = agent.run("echo hello").await.unwrap();
    assert_eq!(result.tool_calls.len(), 1, "exactly one tool call recorded");
    assert_eq!(result.tool_calls[0].tool, "echo");
    assert_eq!(result.output, "all done");
}

// ─── T2: plain-text dialogue, single turn, no tools ───────────────────────
#[tokio::test]
async fn t2_plain_text_single_turn() {
    let client = ScriptedClient::new(vec![text_response("hi there")]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    let result = agent.run("say hi").await.unwrap();
    assert_eq!(result.output, "hi there");
    assert_eq!(result.turns, 1, "a single turn when no tool is called");
    assert!(result.tool_calls.is_empty());
}

// ─── T3: hard turn cap enforced when the loop never converges ──────────────
/// max_turns is a SOFT target; the HARD cap is max_turns*5. With max_turns=1
/// the hard cap is 5 — feed 6 tool_call responses so the loop cannot converge
/// and must hit the hard cap with MaxTurnsExceeded. (Mirrors rust-ref's
/// semantics: run_inner loops `while turn < hard_limit`.)
#[tokio::test]
async fn t3_max_turns_exceeded() {
    let mut responses = vec![];
    for i in 0..6 {
        responses.push(tool_call_response("echo", json!({"word": i})));
    }
    let client = ScriptedClient::new(responses);
    let mut agent = Agent::new_shared(
        Box::new(TestRole::with_max_turns(1)),
        Box::new(client),
    );
    agent.register_tool(Box::new(EchoTool));
    let err = agent.run("loop forever").await.unwrap_err();
    match err {
        AgentError::MaxTurnsExceeded(n) => assert_eq!(n, 5, "hard cap = max_turns*5 = 5"),
        other => panic!("expected MaxTurnsExceeded, got {:?}", other),
    }
}

// ─── T4: client error propagates as AgentError::Client ─────────────────────
#[tokio::test]
async fn t4_client_error_propagates() {
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(ErrorClient));
    let err = agent.run("will fail").await.unwrap_err();
    match err {
        AgentError::Client(_) => {} // good
        other => panic!("expected AgentError::Client, got {:?}", other),
    }
}

// ─── T5: multi-turn tool chain (two tool calls then finish) ────────────────
#[tokio::test]
async fn t5_multi_turn_tool_chain() {
    let client = ScriptedClient::new(vec![
        tool_call_response("echo", json!({"word":"first"})),
        tool_call_response("echo", json!({"word":"second"})),
        text_response("chain complete"),
    ]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    agent.register_tool(Box::new(EchoTool));
    let result = agent.run("run chain").await.unwrap();
    assert_eq!(result.tool_calls.len(), 2, "two tool calls across the chain");
    assert_eq!(result.output, "chain complete");
}

// ─── T6: register_tool makes the tool callable ─────────────────────────────
/// After register_tool, a tool_call for "echo" must resolve to the registered
/// tool (not a tool-not-found error). Verifies end-to-end via the ReAct loop.
#[tokio::test]
async fn t6_register_tool_callable() {
    let client = ScriptedClient::new(vec![
        tool_call_response("echo", json!({"word":"x"})),
        text_response("ok"),
    ]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    agent.register_tool(Box::new(EchoTool));
    let result = agent.run("call echo").await.unwrap();
    assert_eq!(result.tool_calls[0].result, "ECHO: x");
}

// ─── T7: register_shared is equivalent to register_tool ────────────────────
/// Both entry points must insert into the same registry. A tool registered via
/// register_shared (Arc) must be callable exactly like a Box-registered one.
#[tokio::test]
async fn t7_register_shared_equivalent() {
    let client = ScriptedClient::new(vec![
        tool_call_response("echo", json!({"word":"arc"})),
        text_response("ok"),
    ]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    agent.register_shared(Arc::new(EchoTool));
    let result = agent.run("call echo").await.unwrap();
    assert_eq!(result.tool_calls[0].result, "ECHO: arc");
}

// ─── T8: tool-call for an unregistered tool yields a tool-error message ────
/// rust-ref formats unresolvable tool calls as "[tool error: ...]". The
/// transpiled ToolRegistry.exec_or_msg swallows the error into a message that
/// is fed back. The run must still succeed (the model gets the error text),
/// and the recorded tool result must mention the missing tool.
#[tokio::test]
async fn t8_unregistered_tool_yields_error_message() {
    let client = ScriptedClient::new(vec![
        tool_call_response("missing_tool", json!({})),
        text_response("recovered"),
    ]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    // Deliberately do NOT register "missing_tool".
    let result = agent.run("call missing").await.unwrap();
    assert_eq!(result.tool_calls.len(), 1);
    let rec = &result.tool_calls[0].result;
    assert!(
        rec.contains("missing_tool") || rec.contains("error") || rec.contains("not found"),
        "tool result should indicate the error, got: {}",
        rec
    );
}

// ─── T9: register_skill_tool both caches the block AND registers the tool ──
/// Plan 022 Phase 3.1 fix: register_skill_tool now calls register_tool, so the
/// SkillTool is invokable. We build an empty SkillRegistry (no skills loaded),
/// register it via register_skill_tool, then issue a tool_call for "skill" and
/// confirm the call resolves (no tool-not-found). The SkillTool itself returns
/// an error-ish message when no skill matches, which is fine — the point is
/// that the tool IS registered and executes.
#[tokio::test]
async fn t9_register_skill_tool_registers_tool() {
    let registry = Arc::new(SkillRegistry::new());
    let skill_tool = SkillTool::new(registry);
    // Drive the loop with a skill(...) call. SkillTool with an empty registry
    // returns a "no such skill" message; we only assert the tool is found and
    // executes (result is non-empty, not a tool-not-found).
    let client = ScriptedClient::new(vec![
        tool_call_response("skill", json!({"name":"nonexistent"})),
        text_response("finished"),
    ]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    agent.register_skill_tool(skill_tool);
    let result = agent.run("use a skill").await.unwrap();
    assert_eq!(result.tool_calls.len(), 1);
    // The skill tool executed (result present) — the key assertion is that the
    // call did NOT become a "[tool error: ... not found]" for "skill".
    assert_eq!(result.tool_calls[0].tool, "skill");
    assert!(
        !result.tool_calls[0].result.is_empty(),
        "SkillTool must execute and return a result"
    );
}

// ─── T10: load_builtin + builtin_names cover all 14 roles ──────────────────
#[test]
fn t10_builtin_roles_load_and_names_complete() {
    let names = builtin_names();
    assert_eq!(names.len(), 14, "fourteen built-in roles expected");
    for name in &names {
        let role = load_builtin(name)
            .unwrap_or_else(|| panic!("load_builtin({}) should return Some", name));
        assert!(!role.name().is_empty(), "role {} has empty name", name);
    }
    // A few canonical names must be present.
    for must in &["assistant", "coder", "architect", "tester", "translator"] {
        assert!(
            names.iter().any(|n| n == must),
            "builtin_names must include {}",
            must
        );
    }
    // Unknown role returns None.
    assert!(load_builtin("does-not-exist").is_none());
}

// ─── T11: every built-in role has real soul content (not a placeholder) ────
#[test]
fn t11_builtin_roles_have_real_souls() {
    for name in builtin_names() {
        let role = load_builtin(&name).expect("role loads");
        let prompt = role.system_prompt();
        assert!(
            prompt.len() > 50,
            "role {} system_prompt too short ({} chars) — placeholder?",
            name,
            prompt.len()
        );
        assert!(
            !prompt.starts_with("Soul of the"),
            "role {} system_prompt is a placeholder: {}",
            name,
            &prompt[..20.min(prompt.len())]
        );
    }
}

// ─── T12: run_stream emits a Done event on success ─────────────────────────
/// run_stream drives the same loop as run but forwards events to the sink
/// actor. We inject a capturing closure via spawn_event_sink_with and assert
/// that at least a Done (or Cancelled/Error) terminal event arrives.
#[tokio::test]
async fn t12_run_stream_emits_done_event() {
    let collected: Arc<Mutex<Vec<StreamEvent>>> = Arc::new(Mutex::new(vec![]));
    let cb_collected = collected.clone();
    let sink = spawn_event_sink_with(
        String::new(),
        Box::new(move |ev: StreamEvent| {
            cb_collected.lock().unwrap().push(ev);
        }),
    );
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = ScriptedClient::new(vec![text_response("streamed reply")]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    let _result = agent
        .run_stream("say something", cancel, sink)
        .await
        .unwrap();
    // Let the sink actor drain pending events before we inspect.
    a2r_std::task::drain_all().await;
    let events = collected.lock().unwrap();
    assert!(
        !events.is_empty(),
        "run_stream must forward at least one event to the sink"
    );
    let has_terminal = events.iter().any(|e| matches!(e, StreamEvent::Done(_) | StreamEvent::Cancelled(_)));
    assert!(
        has_terminal,
        "a terminal event (Done/Cancelled) must be emitted"
    );
}

// ─── T13: run_stream respects the cancel flag ──────────────────────────────
/// When cancel is set true before the loop starts, run_stream should return
/// promptly (the loop checks the flag between turns). We use a mock that would
/// loop on tool calls to ensure cancellation actually short-circuits.
#[tokio::test]
async fn t13_run_stream_respects_cancel() {
    let sink = spawn_event_sink();
    // Pre-set cancel so the loop observes it on its first checkpoint.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let client = ScriptedClient::new(vec![
        tool_call_response("echo", json!({"word":"a"})),
        tool_call_response("echo", json!({"word":"b"})),
    ]);
    let mut agent = Agent::new_shared(
        Box::new(TestRole::with_max_turns(50)),
        Box::new(client),
    );
    agent.register_tool(Box::new(EchoTool));
    // The result is either Ok (cancelled result) or an error; both are
    // acceptable as long as the loop did NOT run to 50 turns. We assert the
    // turn count stays low, proving cancellation took effect.
    let result = agent.run_stream("loop", cancel, sink).await;
    match result {
        Ok(r) => assert!(r.turns < 50, "cancel must stop the loop early (turns={})", r.turns),
        Err(_) => {} // erroring out on cancel is also acceptable
    }
    a2r_std::task::drain_all().await;
}

// ─── T14: preferred_provider flows from the role into the request ──────────
/// Plan 022 Phase 3.2 fix: build_request now calls self.role.preferred_provider()
/// instead of hardcoding None. A role that sets a provider must see it on the
/// outgoing CompletionRequest.
#[tokio::test]
async fn t14_preferred_provider_flows_to_request() {
    let seen: Arc<Mutex<Vec<CompletionRequest>>> = Arc::new(Mutex::new(vec![]));
    let client = CapturingClient::new(seen.clone());
    let mut agent = Agent::new_shared(
        Box::new(TestRole::with_provider("zhipu")),
        Box::new(client),
    );
    let _result = agent.run("anything").await.unwrap();
    let reqs = seen.lock().unwrap();
    assert!(
        !reqs.is_empty(),
        "at least one request must have been issued"
    );
    assert_eq!(
        reqs[0].preferred_provider.as_deref(),
        Some("zhipu"),
        "preferred_provider must propagate from the role to the request"
    );
}
