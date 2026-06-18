//! The autonomous agent (Layer 3 core).
//!
//! An [`Agent`] binds a [`Profession`] (personality), a [`ToolRegistry`]
//! (capabilities), a [`Memory`] (conversation), and a [`Client`] (LLM
//! transport) into a single ReAct loop implemented in [`Agent::run`] (added in
//! Phase 2).
//!
//! The LLM transport is abstracted behind the [`Client`] trait so the loop can
//! be driven by the real [`auto_ai_client::AiClient`] in production *or* a
//! deterministic mock in tests. [`AiClientAdapter`] wraps the real client.

use std::sync::Arc;

use async_trait::async_trait;
use auto_ai_client::{AiClient, ClientError, CompletionRequest, CompletionResponse};

use crate::error::AgentError;
use crate::memory::Memory;
use crate::profession::Profession;
use crate::tool::ToolRegistry;

/// The LLM transport an Agent talks to.
///
/// Abstracts [`auto_ai_client::AiClient`] so the ReAct loop can be unit-tested
/// with a deterministic mock (see tests in Phase 2).
#[async_trait]
pub trait Client: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError>;
}

/// Adapter wrapping the real Layer-2 [`AiClient`].
#[async_trait]
impl Client for AiClient {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, ClientError> {
        AiClient::complete(self, req).await
    }
}

/// A record of one tool call made during a run (for diagnostics/results).
#[derive(Clone, Debug)]
pub struct ToolCallRecord {
    pub tool: String,
    pub args: serde_json::Value,
    pub result: String,
}

/// The outcome of [`Agent::run`].
#[derive(Clone, Debug, Default)]
pub struct AgentResult {
    /// The agent's final textual answer.
    pub output: String,
    /// How many ReAct turns the loop ran.
    pub turns: usize,
    /// Every tool call the agent made, in order.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Total tokens consumed (if the transport reports usage).
    pub total_tokens: u64,
}

/// An autonomous agent that drives an LLM toward a goal via a ReAct loop.
///
/// Phase 1 wires the pieces together and exposes `new` / `register_tool` /
/// `history`. The `run` loop arrives in Phase 2.
pub struct Agent {
    profession: Arc<dyn Profession>,
    tools: ToolRegistry,
    memory: Memory,
    client: Arc<dyn Client>,
}

impl Agent {
    /// Build a new agent from a Profession, an LLM transport, and the
    /// Profession's memory-limit preference.
    pub fn new<P: Profession + 'static>(profession: P, client: Arc<dyn Client>) -> Self {
        let limit = profession.memory_limit();
        Self {
            profession: Arc::new(profession),
            tools: ToolRegistry::new(),
            memory: Memory::new(limit),
            client,
        }
    }

    /// Register a tool the agent may call.
    pub fn register_tool<T: crate::tool::Tool + 'static>(&mut self, tool: T) {
        self.tools.register(tool);
    }

    /// Borrow the shared tool registry (used by Phase 5's Workflow to share
    /// tools across agents).
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// The Profession this agent embodies.
    pub fn profession(&self) -> &dyn Profession {
        self.profession.as_ref()
    }

    /// Current conversation memory (system prompt is injected at run time, not
    /// stored here).
    pub fn history(&self) -> &[auto_ai_client::Message] {
        self.memory.messages()
    }

    /// Underlying transport (used by Phase 5).
    pub fn client(&self) -> &Arc<dyn Client> {
        &self.client
    }
}

// Placeholder so the module compiles in Phase 1 without `run`. Replaced in
// Phase 2. Kept minimal to avoid unused-import churn.
#[allow(dead_code)]
fn _phase2_placeholder(_e: AgentError) {}
