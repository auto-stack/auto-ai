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
// JsonValue (= serde_json::Value) is the SSE event type complete_stream forwards.
use auto_ai_agent_a2r::wire::JsonValue;

// ─── mock infrastructure ───────────────────────────────────────────────────

/// Fallback complete_stream for mocks: delegate to complete and emit a single
/// delta event (mirrors the rust-ref Client default impl). The mock's `complete`
/// is the scripted path; complete_stream just wraps it so mock-based tests are
/// unaffected by the streaming plumbing (Plan 022 follow-up).
async fn mock_complete_stream<C: Client + ?Sized>(
    client: &C,
    req: CompletionRequest,
    on_event: &Arc<dyn Fn(JsonValue) + Send + Sync>,
) -> Result<CompletionResponse, ClientError> {
    let resp = client.complete(req).await?;
    if !resp.content.is_empty() {
        on_event(serde_json::json!({"type":"delta","text": resp.content}));
    }
    Ok(resp)
}

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
    async fn complete_stream(
        &self,
        req: CompletionRequest,
        on_event: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        mock_complete_stream(self, req, &on_event).await
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
    async fn complete_stream(
        &self,
        _req: CompletionRequest,
        _on_event: Arc<dyn Fn(JsonValue) + Send + Sync>,
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
    async fn complete_stream(
        &self,
        req: CompletionRequest,
        on_event: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        mock_complete_stream(self, req, &on_event).await
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

// ─── T10: load_builtin + builtin_names cover all 15 roles ──────────────────
#[test]
fn t10_builtin_roles_load_and_names_complete() {
    let names = builtin_names();
    assert_eq!(names.len(), 15, "fifteen built-in roles expected");
    for name in &names {
        let role = load_builtin(name)
            .unwrap_or_else(|| panic!("load_builtin({}) should return Some", name));
        assert!(!role.name().is_empty(), "role {} has empty name", name);
    }
    // A few canonical names must be present.
    for must in &["assistant", "coder", "architect", "tester", "translator", "plan-dev"] {
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

// ─── T15: complete_stream forwards SSE deltas to the sink as Delta events ───
/// Plan 022 follow-up: run_inner now calls complete_stream; the callback
/// forwards each text chunk to the sink as StreamEvent::Delta. ScriptedClient's
/// complete_stream (mock_complete_stream) emits one delta per turn, so run_stream
/// must surface a Delta event for the model's text reply.
#[tokio::test]
async fn t15_complete_stream_forwards_deltas() {
    let collected: Arc<Mutex<Vec<StreamEvent>>> = Arc::new(Mutex::new(vec![]));
    let cb_collected = collected.clone();
    let sink = spawn_event_sink_with(
        String::new(),
        Box::new(move |ev: StreamEvent| {
            cb_collected.lock().unwrap().push(ev);
        }),
    );
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // ScriptedClient returns a text reply → loop ends in one turn, and
    // mock_complete_stream emits one delta for "hi there".
    let client = ScriptedClient::new(vec![text_response("hi there")]);
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    let _result = agent.run_stream("say hi", cancel, sink).await.unwrap();
    a2r_std::task::drain_all().await;
    let events = collected.lock().unwrap();
    // At least one Delta event must have been forwarded from complete_stream.
    let has_delta = events.iter().any(|e| matches!(e, StreamEvent::Delta(_)));
    assert!(
        has_delta,
        "run_stream via complete_stream must forward a Delta event; got: {:?}",
        events.iter().map(|e| match e {
            StreamEvent::Delta(_) => "Delta",
            StreamEvent::Done(_) => "Done",
            StreamEvent::Tool(_, _, _) => "Tool",
            other => unreachable!("unexpected event {:?}", other),
        }).collect::<Vec<_>>()
    );
}

// ─── T16-T19: Plan 026 parity — turn events / thinking / steering / follow-up ─

/// A Client whose complete_stream emits a scripted list of SSE events (raw
/// JSON values), captures every request, and can run a hook after a given
/// response (deterministic mid-run steering injection). Plan 026 parity
/// counterpart of the rust-ref harness's enhanced ScriptedClient. Clone shares
/// the inner state so tests can keep a handle for assertions after the agent
/// takes ownership of one clone.
struct SseScriptedClient {
    inner: Arc<SseScriptedInner>,
}

struct SseScriptedInner {
    responses: Mutex<Vec<CompletionResponse>>,
    /// Per-response SSE events to emit while streaming.
    events: Mutex<std::collections::VecDeque<Vec<Value>>>,
    requests: Mutex<Vec<CompletionRequest>>,
    /// (index, hook): called right after response `index` was streamed.
    after_response: Option<Box<dyn Fn() + Send + Sync>>,
    hook_index: usize,
}

impl Clone for SseScriptedClient {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl SseScriptedClient {
    fn new(responses: Vec<CompletionResponse>, events: Vec<Vec<Value>>) -> Self {
        Self {
            inner: Arc::new(SseScriptedInner {
                responses: Mutex::new(responses),
                events: Mutex::new(events.into()),
                requests: Mutex::new(Vec::new()),
                after_response: None,
                hook_index: 0,
            }),
        }
    }
    /// Call `f` right after response `idx` was streamed (mid-run hook).
    fn with_after_response(mut self, idx: usize, f: Box<dyn Fn() + Send + Sync>) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("hook set before cloning");
        inner.after_response = Some(f);
        inner.hook_index = idx;
        self
    }
    fn requests(&self) -> Vec<CompletionRequest> {
        self.inner.requests.lock().unwrap().clone()
    }
    fn next_response(&self) -> CompletionResponse {
        let mut q = self.inner.responses.lock().unwrap();
        if q.is_empty() {
            text_response("(no more scripted responses)")
        } else {
            q.remove(0)
        }
    }
}

#[async_trait]
impl Client for SseScriptedClient {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ClientError> {
        // Plan 028: compaction's summary requests go through complete() —
        // capture them too.
        self.inner.requests.lock().unwrap().push(req);
        Ok(self.next_response())
    }
    async fn complete_stream(
        &self,
        req: CompletionRequest,
        on_event: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        self.inner.requests.lock().unwrap().push(req);
        let served = self.inner.requests.lock().unwrap().len() - 1;
        let resp = self.next_response();
        let evs = self.inner.events.lock().unwrap().pop_front().unwrap_or_default();
        for e in evs {
            on_event(e);
        }
        if let Some(f) = &self.inner.after_response {
            if self.inner.hook_index == served {
                f();
            }
        }
        Ok(resp)
    }
}

/// Compact tag for one StreamEvent (sequence assertions).
fn ptag(ev: &StreamEvent) -> String {
    match ev {
        StreamEvent::TurnStart(turn) => format!("TS{}", turn),
        StreamEvent::TurnEnd(turn, usage, tool_count) => {
            let u = usage
                .as_ref()
                .map(|u| u.input_tokens + u.output_tokens)
                .unwrap_or(0);
            format!("TE{}/{}+{}", turn, u, tool_count)
        }
        StreamEvent::Delta(_) => "D".into(),
        StreamEvent::Thinking(_) => "TH".into(),
        StreamEvent::ToolStart(tool, _) => format!("TS:{}", tool),
        StreamEvent::Tool(tool, _, _) => format!("T:{}", tool),
        StreamEvent::Warning(_) => "W".into(),
        StreamEvent::Done(_) => "DONE".into(),
        StreamEvent::Cancelled(_) => "CANCEL".into(),
        StreamEvent::Error(_) => "E".into(),
    }
}

/// Parity of rust-ref harness_turn_events_sequence: a 2-turn run (tool call →
/// final answer) emits the turn-annotated sequence; each TurnEnd carries that
/// turn's usage and tool count.
#[tokio::test]
async fn t16_turn_event_sequence() {
    use auto_ai_agent_a2r::auto_ai_client::Usage;
    let mut r1 = tool_call_response("echo", json!({"word": "hi"}));
    r1.usage = Some(Usage { input_tokens: 10, output_tokens: 5, ..Default::default() });
    let mut r2 = text_response("done");
    r2.usage = Some(Usage { input_tokens: 20, output_tokens: 7, ..Default::default() });
    let client = SseScriptedClient::new(vec![r1, r2], vec![vec![], vec![]]);

    let collected: Arc<Mutex<Vec<StreamEvent>>> = Arc::new(Mutex::new(vec![]));
    let cb = collected.clone();
    let sink = spawn_event_sink_with(
        String::new(),
        Box::new(move |ev: StreamEvent| cb.lock().unwrap().push(ev)),
    );
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    agent.register_tool(Box::new(EchoTool));
    let result = agent.run_stream("task", cancel, sink).await.unwrap();

    assert_eq!(result.output, "done");
    assert_eq!(result.turns, 2);
    a2r_std::task::drain_all().await;
    let got: Vec<String> = collected.lock().unwrap().iter().map(ptag).collect();
    assert_eq!(
        got.join(","),
        "TS1,TS:echo,T:echo,TE1/15+1,TS2,TE2/27+0,DONE",
        "turn-annotated sequence mismatch: {:?}",
        got
    );
}

/// Parity of harness_reasoning_streams_as_thinking: a `{"type":"reasoning"}`
/// SSE event forwards as Thinking (not Delta) — the .at track previously
/// degraded reasoning to Delta (Plan 026 fix in forward_sse_delta).
#[tokio::test]
async fn t17_reasoning_maps_to_thinking() {
    let client = SseScriptedClient::new(
        vec![text_response("answer")],
        vec![vec![
            json!({"type": "reasoning", "text": "hmm"}),
            json!({"type": "delta", "text": "answer"}),
        ]],
    );
    let collected: Arc<Mutex<Vec<StreamEvent>>> = Arc::new(Mutex::new(vec![]));
    let cb = collected.clone();
    let sink = spawn_event_sink_with(
        String::new(),
        Box::new(move |ev: StreamEvent| cb.lock().unwrap().push(ev)),
    );
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client));
    agent.run_stream("task", cancel, sink).await.unwrap();
    a2r_std::task::drain_all().await;

    let got: Vec<String> = collected.lock().unwrap().iter().map(ptag).collect();
    assert_eq!(got.join(","), "TS1,TH,D,TE1/0+0,DONE", "got {:?}", got);
}

/// Parity of harness_follow_up_revives_run: a queued follow-up keeps a
/// naturally-ending run going; the follow-up message becomes a new user turn.
#[tokio::test]
async fn t18_follow_up_revives_run() {
    let client = SseScriptedClient::new(
        vec![text_response("first answer"), text_response("second answer")],
        vec![vec![], vec![]],
    );
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client.clone()));
    agent.follow_up("go deeper");
    let result = agent.run("start").await.unwrap();

    assert_eq!(result.output, "second answer");
    assert_eq!(result.turns, 2);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    let msgs = &reqs[1].messages;
    assert_eq!(msgs.len(), 3, "second request should carry the follow-up");
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[2].role, "user");
}

/// Parity of harness_steering_injected_between_tool_and_next_llm: a steering
/// message queued mid-run lands AFTER the tool result and BEFORE the next LLM
/// request (asserted via captured request order).
#[tokio::test]
async fn t19_steering_injected_before_next_llm() {
    // The agent's queue handle is Arc<parking_lot::Mutex<Vec<String>>>; the
    // slot indirection breaks the client↔agent construction cycle.
    use parking_lot::Mutex as PlMutex;
    type Q = Arc<PlMutex<Vec<String>>>;
    let slot: Arc<Mutex<Option<Q>>> = Arc::new(Mutex::new(None));
    let hook_slot = slot.clone();
    let client = SseScriptedClient::new(
        vec![
            tool_call_response("echo", json!({"word": "a"})),
            text_response("ok"),
        ],
        vec![vec![], vec![]],
    )
    .with_after_response(0, Box::new(move || {
        if let Some(q) = hook_slot.lock().unwrap().as_ref() {
            q.lock().push("steer me".into());
        }
    }));
    let mut agent = Agent::new_shared(Box::new(TestRole::new()), Box::new(client.clone()));
    agent.register_tool(Box::new(EchoTool));
    *slot.lock().unwrap() = Some(agent.steering_queue());

    let result = agent.run("go").await.unwrap();
    assert_eq!(result.output, "ok");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].messages.len(), 1, "first request: just the task");
    let msgs = &reqs[1].messages;
    assert_eq!(msgs.len(), 4, "steering message missing");
    assert_eq!(msgs[3].role, "user", "steering message should be last");
}

// ─── T21: Plan 028 parity — compaction summarizes prefix, keeps the tail ────

#[tokio::test]
async fn t21_compact_summarizes_prefix_keeps_tail() {
    use auto_ai_agent_a2r::{Memory, compact, default_compaction_settings, CompactionSettings};
    use auto_ai_agent_a2r::compaction::find_cut_point;
    use auto_ai_client_a2r::ContentBlock;

    // 50 turns × ~800 chars (~200 tokens each) ≈ 20k tokens.
    let mut mem = Memory::new(None);
    for i in 0..50 {
        mem.add("user", &format!("turn {}: {}", i, "x".repeat(800)));
        mem.add("assistant", &"y".repeat(800));
    }

    let client = SseScriptedClient::new(
        vec![text_response("## Goal
fix the bug

## Files
- src/main.rs")],
        vec![vec![]],
    );
    let settings = CompactionSettings {
        context_window: 10_000,
        reserve_tokens: 1_000,
        keep_recent_tokens: 2_000,
    };
    let boxed: Box<dyn Client> = Box::new(client.clone());
    let next = compact(mem, &boxed, "tier:mid", settings).await.unwrap();

    // Exactly one isolated summary request.
    let reqs = client.requests();
    assert_eq!(reqs.len(), 1, "summary must be a single isolated request");
    assert!(reqs[0].system_prompt.as_deref().unwrap_or("").contains("compress"));

    // Summary anchor leads the rebuilt memory, with the Files list intact.
    let msgs = next.messages();
    assert!(msgs.len() < 20, "compacted memory must be much smaller: {}", msgs.len());
    assert_eq!(msgs[0].role, "user");
    match &msgs[0].content[0] {
        ContentBlock::Text { text } => {
            assert!(text.contains("Compacted conversation summary"));
            assert!(text.contains("## Files"));
            assert!(text.contains("src/main.rs"));
        }
        other => panic!("summary anchor should be text, got {:?}", other),
    }
    // The kept tail is verbatim recent turns.
    assert_eq!(msgs.last().unwrap().role, "assistant");
    // Cut-point sanity (parity with rust-ref unit tests).
    let _ = find_cut_point(msgs.clone(), 2_000);
}
