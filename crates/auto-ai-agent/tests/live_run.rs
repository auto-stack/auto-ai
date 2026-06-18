//! Live integration test for the ReAct loop against a real LLM.
//!
//! Skipped by default (needs an API key + network). Run with:
//!
//! ```sh
//! cargo test -p auto-ai-agent --test live_run -- --ignored
//! ```
//!
//! Relies on the standard `auto-ai-client` config (providers/keys) — if no
//! daemon/config is available, `AiClient::new()` errors and the test is
//! treated as a soft skip.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use auto_ai_agent::{Agent, Client, Profession, Tool, ToolError};

struct EchoProfession;
impl Profession for EchoProfession {
    fn name(&self) -> &str {
        "echo-test"
    }
    fn system_prompt(&self) -> &str {
        "You are a test assistant. Use the echo tool to echo the user's word, then reply with the echoed value only."
    }
    fn max_turns(&self) -> usize {
        4
    }
}

struct EchoTool;
#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo back a word"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"word":{"type":"string"}},"required":["word"]})
    }
    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        Ok(args["word"].as_str().unwrap_or("").to_string())
    }
}

#[tokio::test]
#[ignore = "requires live LLM access (daemon/config + API key)"]
async fn live_react_one_tool_call() {
    // Build the real client. If no daemon/config is present, soft-skip.
    let client = match auto_ai_client::AiClient::new() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("skipping live test — no client: {e}");
            return;
        }
    };

    let mut agent = Agent::new(EchoProfession, client as Arc<dyn Client>);
    agent.register_tool(EchoTool);

    let result = agent.run("Please echo the word: hello").await;
    match result {
        Ok(r) => {
            println!(
                "turns={} output={:?} tool_calls={:?}",
                r.turns, r.output, r.tool_calls
            );
            assert!(r.turns >= 1);
        }
        Err(e) => panic!("live run failed: {e}"),
    }
}
