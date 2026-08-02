//! The [`RelayTarget`] abstraction (design doc §6.2).
//!
//! A RelayTarget is something an agent can delegate a sub-task to and get a
//! string result back.
//!
//! **Status note (review-003 M1):** the previous `impl RelayTarget for Agent`
//! used `tokio::runtime::Builder::new_current_thread().block_on(...)` to run
//! the agent's async loop from a sync trait method. That's a nested-runtime
//! hazard (panics if the caller is already on a tokio worker) and had no
//! production callers (workflows call `Agent::run` directly). The impl and its
//! test were removed; the trait is kept so a future, properly-async delegation
//! mechanism can implement it.

use crate::error::AgentError;

/// Something that can receive a delegated task and return a result.
///
/// Currently has no implementations (see module docs). A future async variant
/// of this trait is expected to replace the sync method.
pub trait RelayTarget {
    /// Receive a task and return its textual result.
    fn delegate(&mut self, task: &str) -> Result<String, AgentError>;
}
