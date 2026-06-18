//! The [`Tool`] trait and [`ToolRegistry`].
//!
//! Apps register concrete tools (file IO, shell, search, ...) and the
//! [`crate::Agent`] exposes them to the model as callable functions. A
//! [`Profession`][crate::Profession] can restrict which tools are visible via
//! `allowed_tools()`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use auto_ai_client::ToolDefinition;
use serde_json::Value as JsonValue;

use crate::error::ToolError;

/// A callable tool the agent can hand to the LLM.
///
/// Mirrors the design doc (§3.2). Implementors supply the metadata the model
/// sees (`name`/`description`/`parameters`) and an async `execute`.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (must be unique within a registry).
    fn name(&self) -> &str;

    /// Human/LLM-facing description — the model uses this to decide whether to
    /// call the tool.
    fn description(&self) -> &str;

    /// JSON-Schema fragment describing the tool's `input` object.
    fn parameters(&self) -> JsonValue {
        serde_json::json!({"type": "object", "properties": {}})
    }

    /// Run the tool with the model-supplied arguments, returning a string
    /// result that is fed back to the model.
    async fn execute(&self, args: &JsonValue) -> Result<String, ToolError>;
}

/// Convert a [`Tool`] into the Layer-2 [`ToolDefinition`] the client sends.
pub fn tool_to_definition(tool: &dyn Tool) -> ToolDefinition {
    ToolDefinition::new(tool.name(), tool.description(), tool.parameters())
}

/// Registry of tools keyed by name.
///
/// Stores tools as `Arc<dyn Tool>` so the same tool set can be shared across
/// agents / workflow steps.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Overwrites an existing tool with the same name.
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name().to_string(), Arc::new(tool));
    }

    /// Register a tool from an `Arc` (handy for sharing one tool across
    /// registries/agents).
    pub fn register_shared(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// All registered tool names.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// All tools whose name is in `filter`. If `filter` is empty, returns all
    /// tools (matches the Profession `allowed_tools()` "empty = all" rule).
    pub fn filter(&self, filter: &[String]) -> Vec<Arc<dyn Tool>> {
        if filter.is_empty() {
            return self.tools.values().cloned().collect();
        }
        filter
            .iter()
            .filter_map(|name| self.tools.get(name).cloned())
            .collect()
    }

    /// Run a tool by name, returning its string output.
    pub async fn execute(&self, name: &str, args: &JsonValue) -> Result<String, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::Exec(format!("tool not found: {}", name)))?
            .clone();
        tool.execute(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo back the input"
        }
        async fn execute(&self, args: &JsonValue) -> Result<String, ToolError> {
            Ok(args.to_string())
        }
    }

    struct Reverse;
    #[async_trait]
    impl Tool for Reverse {
        fn name(&self) -> &str {
            "reverse"
        }
        fn description(&self) -> &str {
            "reverse a string"
        }
        async fn execute(&self, args: &JsonValue) -> Result<String, ToolError> {
            let s = args["s"].as_str().unwrap_or("");
            Ok(s.chars().rev().collect())
        }
    }

    #[test]
    fn register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(Echo);
        assert_eq!(reg.len(), 1);
        assert!(reg.get("echo").is_some());
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn names_lists_all() {
        let mut reg = ToolRegistry::new();
        reg.register(Echo);
        reg.register(Reverse);
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["echo".to_string(), "reverse".to_string()]);
    }

    #[test]
    fn filter_empty_returns_all() {
        let mut reg = ToolRegistry::new();
        reg.register(Echo);
        reg.register(Reverse);
        assert_eq!(reg.filter(&[]).len(), 2);
    }

    #[test]
    fn filter_by_name() {
        let mut reg = ToolRegistry::new();
        reg.register(Echo);
        reg.register(Reverse);
        let filtered = reg.filter(&["echo".to_string()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name(), "echo");
    }

    #[tokio::test]
    async fn execute_runs_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Reverse);
        let out = reg
            .execute("reverse", &serde_json::json!({"s": "abc"}))
            .await
            .unwrap();
        assert_eq!(out, "cba");
    }

    #[tokio::test]
    async fn execute_missing_tool_errors() {
        let reg = ToolRegistry::new();
        let err = reg.execute("nope", &serde_json::json!({})).await;
        assert!(err.is_err());
    }
}
