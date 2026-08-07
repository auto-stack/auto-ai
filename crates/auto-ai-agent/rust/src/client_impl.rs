//! Hand-written glue: implement the (Auto-transpiled) `Client` trait for the
//! transpiled `AiClient` (auto-ai-client-a2r), plus a `StreamingAiClient` that
//! forwards SSE deltas through a channel for live token display.
//!
//! Plan 024: the agent now consumes the TRANSPILED client (auto-ai-client-a2r),
//! whose AiClient does its own HTTP via a2r-std (ureq). This file bridges the
//! agent's `Client` spec/trait to that transpiled AiClient — the trait and the
//! struct have matching method names but slightly different signatures (owned
//! req, Arc<dyn Fn> callback), adapted here. This is the only non-a2r glue.
//!
//! SYNC-IN-ASYNC MITIGATION (Plan 024): the transpiled AiClient's async methods
//! internally call a2r-std http, which uses synchronous ureq on background
//! threads with blocking `.join()`/`recv()`. Calling these directly inside an
//! `async fn` would block the tokio executor. We wrap each call in
//! `tokio::task::spawn_blocking`, which moves the blocking work onto tokio's
//! dedicated blocking thread pool and yields the executor until it completes.
//! The transpiled AiClient methods are themselves `async fn`, so inside the
//! spawn_blocking closure we drive them to completion with a fresh
//! current-thread runtime (`Runtime::new().block_on`) — this is the documented
//! pattern for running an async-that's-really-sync from a blocking context,
//! and avoids touching the parent runtime.

use crate::agent::Client;
use crate::auto_ai_client::{AiClient, ClientError, CompletionRequest, CompletionResponse};
use crate::wire::JsonValue;
use async_trait::async_trait;
use std::sync::Arc;

/// Run a transpiled AiClient async method on the blocking pool, isolating its
/// synchronous ureq I/O from the tokio executor. The closure receives an owned
/// AiClient (a cheap clone — it holds only a String URL) and returns a future
/// that owns it (via `async move`), so the future is `'static` and can be
/// driven to completion inside a fresh current-thread runtime on the blocking
/// thread. This is the documented pattern for running an async-that's-really-
/// sync from a blocking context, without touching the parent runtime.
async fn run_blocking<F, Fut, T>(client: AiClient, f: F) -> T
where
    F: FnOnce(AiClient) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build blocking runtime");
        rt.block_on(f(client))
    })
    .await
    .expect("blocking client task panicked")
}

/// Adapter: the transpiled AiClient already has complete/complete_stream; this
/// impl just satisfies the agent's `Client` trait (signature adaptation only),
/// and wraps the synchronous-via-ureq calls in spawn_blocking.
#[async_trait]
impl Client for AiClient {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        run_blocking(self.clone(), move |c| async move {
            AiClient::complete(&c, req).await
        })
        .await
    }

    async fn complete_stream(
        &self,
        req: CompletionRequest,
        on_event: Arc<dyn Fn(JsonValue) + Send + Sync>,
    ) -> Result<CompletionResponse, ClientError> {
        run_blocking(self.clone(), move |c| async move {
            AiClient::complete_stream(&c, req, on_event).await
        })
        .await
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
        let client = self.inner.clone();
        run_blocking(client, move |c| async move {
            AiClient::complete_stream(&c, req, Arc::new(move |ev: JsonValue| {
                let _ = tx.send(ev);
            }))
            .await
        })
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
        let client = self.inner.clone();
        run_blocking(client, move |c| async move {
            AiClient::complete_stream(&c, req, Arc::new(move |ev: JsonValue| {
                let _ = tx.send(ev.clone());
                on_event(ev);
            }))
            .await
        })
        .await
    }
}
