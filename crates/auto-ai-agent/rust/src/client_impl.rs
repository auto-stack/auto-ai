//! Hand-written glue: implement the (Auto-transpiled) `Client` trait for the
//! transpiled `AiClient` (auto-ai-client-a2r), plus a `StreamingAiClient` that
//! forwards SSE deltas through a channel for live token display.
//!
//! Plan 024: the agent now consumes the TRANSPILED client (auto-ai-client-a2r),
//! whose AiClient does its own HTTP via a2r-std (ureq). This file bridges the
//! agent's `Client` spec/trait to that transpiled AiClient — the trait and the
//! struct have matching method names but slightly different signatures (owned
//! req, Arc<dyn Fn> callback), adapted here. This is the only non-a2r glue.

use crate::agent::Client;
use crate::auto_ai_client::{AiClient, ClientError, CompletionRequest, CompletionResponse};
use crate::wire::JsonValue;
use async_trait::async_trait;
use std::sync::Arc;

/// Adapter: the transpiled AiClient already has complete/complete_stream; this
/// impl just satisfies the agent's `Client` trait (signature adaptation only).
#[async_trait]
impl Client for AiClient {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        // Transpiled AiClient::complete takes req by value.
        AiClient::complete(self, req).await
    }

    async fn complete_stream(
        &self,
        req: CompletionRequest,
        on_event: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        // Transpiled AiClient::complete_stream takes (req by value, Arc<dyn Fn>).
        AiClient::complete_stream(self, req, on_event).await
    }
}

/// Streaming adapter: wraps `AiClient` and forwards each SSE delta event
/// through a channel in addition to the formal `on_event` callback. The agent
/// loop still calls `complete()`/`complete_stream()` and gets the full
/// `CompletionResponse` — but tokens also flow out via the channel for the
/// REPL's live printer (main.rs).
///
/// Uses `std::sync::mpsc` (not tokio) to avoid `blocking_send` deadlocks inside
/// the async runtime. `mpsc::Sender` is `Send + 'static`.
pub struct StreamingAiClient {
    inner: AiClient,
    tx: std::sync::mpsc::Sender<serde_json::Value>,
}

impl StreamingAiClient {
    pub fn new(url: &str, tx: std::sync::mpsc::Sender<serde_json::Value>) -> Self {
        Self {
            inner: AiClient::with_url(url),
            tx,
        }
    }
}

#[async_trait]
impl Client for StreamingAiClient {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        let tx = self.tx.clone();
        // Drive complete_stream instead of complete: the daemon sends SSE deltas,
        // each forwarded through the channel. complete_stream returns the fully
        // assembled CompletionResponse (concatenated text + tool_calls/usage).
        AiClient::complete_stream(&self.inner, req, Arc::new(move |ev: JsonValue| {
            let _ = tx.send(ev);
        }))
        .await
    }

    /// True SSE streaming (Plan 022 follow-up): forwards each daemon SSE event
    /// BOTH to the `on_event` callback (the agent's formal stream) AND to the
    /// side-channel mpsc (the REPL's live printer).
    async fn complete_stream(
        &self,
        req: CompletionRequest,
        on_event: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        let tx = self.tx.clone();
        AiClient::complete_stream(&self.inner, req, Arc::new(move |ev: JsonValue| {
            let _ = tx.send(ev.clone());
            on_event(ev);
        }))
        .await
    }
}
