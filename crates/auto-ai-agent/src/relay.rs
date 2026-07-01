//! The [`RelayTarget`] abstraction (design doc §6.2).
//!
//! A RelayTarget is something an [`Agent`] (or a [`crate::workflow::Workflow`]
//! step) can delegate a sub-task to and get a string result back. Today the
//! only implementation is [`Agent`] itself, enabling one agent to "relay" work
//! to another within a workflow.
//!
//! **Scope note (v1):** this module defines the trait and the `Agent` impl.
//! Wiring *runtime* delegation — where an agent, mid-ReAct-loop, spontaneously
//! hands off to a registered peer agent — is a v2 concern (it requires the
//! Agent to hold a registry of peers and expose them as tools). The Workflow
//! engine uses these types structurally today.

use crate::agent::Agent;
use crate::error::AgentError;

/// Something that can receive a delegated task and return a result.
pub trait RelayTarget {
    /// Receive a task and return its textual result.
    fn delegate(&mut self, task: &str) -> Result<String, AgentError>;
}

impl RelayTarget for Agent {
    fn delegate(&mut self, task: &str) -> Result<String, AgentError> {
        // Delegate by running the agent's ReAct loop synchronously. We can't
        // await inside a non-async trait method, so this blocks on a fresh
        // current-thread runtime. In practice the Workflow engine calls
        // `Agent::run` directly (see `workflow::WorkflowStep::run`); this impl
        // exists for callers that want the trait abstraction and are not in an
        // async context.
        //
        // NOTE: prefer `Agent::run` in async code; this is a bridge for
        // non-async RelayTarget consumers.
        let task = task.to_string();
        // We can't move `&mut self` across the `block_on` boundary cleanly while
        // also borrowing it, so re-export through a local that re-runs run().
        // The simplest correct approach: block on a future that calls run.
        // `&mut self` is Send because Agent: Send.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| AgentError::Config(format!("relay runtime: {e}")))?;
        // Safety of the borrow: we hold &mut self for the duration, no aliasing.
        let result = runtime.block_on(async { self.run(&task).await })?;
        Ok(result.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // A scripted client so we can test Agent::delegate without a live LLM.
    struct OnceClient {
        reply: Mutex<Option<String>>,
    }
    #[async_trait::async_trait]
    impl crate::agent::Client for OnceClient {
        async fn complete(
            &self,
            _req: &auto_ai_client::CompletionRequest,
        ) -> Result<auto_ai_client::CompletionResponse, auto_ai_client::ClientError> {
            let reply = self
                .reply
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| "default".to_string());
            Ok(auto_ai_client::CompletionResponse {
                content: reply,
                tool_calls: vec![],
                stop_reason: Some("end_turn".into()),
                usage: None,
                model: "mock".into(),
                error: None,
            })
        }
    }

    struct DummyProfession;
    impl crate::Role for DummyProfession {
        fn name(&self) -> &str {
            "dummy"
        }
        fn system_prompt(&self) -> &str {
            "you are a dummy"
        }
    }

    #[test]
    fn agent_implements_relay_target() {
        let client = Arc::new(OnceClient {
            reply: Mutex::new(Some("delegated answer".into())),
        });
        let mut agent = Agent::new(DummyProfession, client as Arc<dyn crate::agent::Client>);
        let out = agent.delegate("do something").unwrap();
        assert_eq!(out, "delegated answer");
    }
}
