//! Plan 022 Phase 2.3 — live integration test for the TRANSPILED ReAct loop
//! against a real LLM via the daemon.
//!
//! Skipped by default (needs an API key + a running daemon). Run with:
//!
//! ```sh
//! cargo test --manifest-path crates/auto-ai-agent/rust/Cargo.toml \
//!   --test live_run -- --ignored
//! ```
//!
//! Mirrors `rust-ref/tests/live_run.rs` but uses the transpiled crate's API
//! (Box<dyn> construction, owned-value signatures). The real `AiClient` is
//! bridged into the transpiled `Client` trait by `client_impl.rs` (Plan 013
//! option A). If the daemon isn't running, the probe fails and the test
//! soft-skips.

use auto_ai_agent_a2r::agent::Agent;
use auto_ai_agent_a2r::builtin_roles::Assistant;
use auto_ai_agent_a2r::echo_tool::EchoTool;
use auto_ai_agent_a2r::{Client, Role};
// The transpiled client + wire types (re-exported through the crate's
// auto_ai_client shim — same path client_impl.rs uses; the bridge makes the
// transpiled AiClient implement the transpiled Client trait).
use auto_ai_agent_a2r::auto_ai_client::{AiClient, CompletionRequest};

/// Probe the daemon; return true if it is reachable.
async fn daemon_alive(url: &str) -> bool {
    let client = AiClient::with_url(url);
    let probe = CompletionRequest {
        model: "tier:min".into(),
        messages: vec![],
        max_tokens: None,
        temperature: None,
        tools: vec![],
        system_prompt: None,
        stream: false,
        preferred_provider: None,
    };
    // The transpiled Client impl for AiClient takes the request by value.
    Client::complete(&client, probe).await.is_ok()
}

#[tokio::test]
#[ignore = "requires live LLM access (daemon/config + API key)"]
async fn live_transpiled_react_one_turn() {
    let url =
        std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into());

    if !daemon_alive(&url).await {
        eprintln!("skipping live test — daemon unreachable at {url}");
        return;
    }

    // Build the transpiled Agent over the real AiClient (bridged via
    // client_impl.rs). Assistant is a builtin role; the echo tool lets us
    // exercise the tool-calling path end-to-end.
    let client = AiClient::with_url(&url);
    let role = Assistant {};
    let mut agent = Agent::new_shared(Box::new(role), Box::new(client));
    agent.register_tool(Box::new(EchoTool));

    let result = agent.run("Say exactly: hello world").await;
    match result {
        Ok(r) => {
            println!(
                "turns={} tokens={} output={:?} tool_calls={:?}",
                r.turns, r.total_tokens, r.output, r.tool_calls
            );
            assert!(r.turns >= 1, "at least one turn must run");
            assert!(
                !r.output.is_empty(),
                "the model must produce some output"
            );
        }
        Err(e) => panic!("live transpiled run failed: {e}"),
    }
}

#[tokio::test]
#[ignore = "requires live LLM access (daemon/config + API key)"]
async fn live_transpiled_react_tool_call() {
    let url =
        std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into());

    if !daemon_alive(&url).await {
        eprintln!("skipping live test — daemon unreachable at {url}");
        return;
    }

    // A minimal role whose prompt directs the model to use the echo tool.
    struct EchoRole;
    impl Role for EchoRole {
        fn name(&self) -> String {
            "echo-test".into()
        }
        fn system_prompt(&self) -> String {
            "You are a test assistant. Use the echo tool to echo the user's word, then reply with the echoed value only.".into()
        }
        fn max_turns(&self) -> u32 {
            4
        }
    }

    let client = AiClient::with_url(&url);
    let mut agent = Agent::new_shared(Box::new(EchoRole), Box::new(client));
    agent.register_tool(Box::new(EchoTool));

    let result = agent.run("Please echo the word: hello").await;
    match result {
        Ok(r) => {
            println!(
                "turns={} tool_calls={:?} output={:?}",
                r.turns, r.tool_calls, r.output
            );
            assert!(r.turns >= 1);
        }
        Err(e) => panic!("live transpiled tool-call run failed: {e}"),
    }
}
