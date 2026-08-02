//! A simple echo tool for testing the ReAct loop's tool-calling path.
//! Returns "ECHO: <input>" — lets the model practice calling a tool and
//! seeing its result fed back. Hand-written (no .at source).

use crate::tool::Tool;
use crate::wire::JsonValue;

pub struct EchoTool;

// The a2r-generated `trait Tool` carries #[async_trait::async_trait], so the
// impl must too — otherwise the macro-expanded lifetime signature mismatches
// (E0195).
#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> String {
        "echo".into()
    }

    fn description(&self) -> String {
        "Echoes back the input message. Use this to test tool calling.".into()
    }

    fn parameters(&self) -> JsonValue {
        a2r_std::json::parse("{\"type\":\"object\",\"properties\":{\"message\":{\"type\":\"string\",\"description\":\"The message to echo back\"}},\"required\":[\"message\"]}")
    }

    async fn execute(&self, args: JsonValue) -> Result<String, crate::error::ToolError> {
        let msg = args.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(format!("ECHO: {}", msg))
    }
}
