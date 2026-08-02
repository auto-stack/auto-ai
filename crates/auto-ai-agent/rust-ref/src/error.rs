//! Error types for the agent layer.
//!
//! [`AgentError`] is the top-level error returned by [`crate::Agent`] and
//! [`crate::workflow`]. It wraps the Layer-2 client error, tool-execution
//! errors, and configuration/parse errors from later phases.

use thiserror::Error;

/// Errors raised while executing a tool.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The tool's arguments were missing/invalid.
    #[error("invalid tool arguments: {0}")]
    Args(String),
    /// The tool ran but failed (e.g. IO error, non-zero exit).
    #[error("tool execution failed: {0}")]
    Exec(String),
}

/// The unified error type for the agent layer.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Propagated from the Layer-2 LLM client.
    #[error("client error: {0}")]
    Client(#[from] auto_ai_client::ClientError),
    /// Propagated from a tool invocation.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),
    /// A requested tool was not registered.
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    /// The ReAct loop exceeded its turn budget without finishing.
    #[error("max turns ({0}) exceeded without completion")]
    MaxTurnsExceeded(usize),
    /// The loop detected a tool being called identically in a tight cycle.
    #[error("loop detected: tool '{0}' called with identical args repeatedly")]
    LoopDetected(String),
    /// A Role or Workflow configuration was malformed (later phases).
    #[error("config error: {0}")]
    Config(String),
}

// Manual `From<String>` for the config-error convenience used in later phases.
impl From<String> for AgentError {
    fn from(s: String) -> Self {
        AgentError::Config(s)
    }
}
