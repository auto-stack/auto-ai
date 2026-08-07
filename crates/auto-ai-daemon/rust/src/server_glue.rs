//! Hand-written glue for server.at.
//!
//! Owns the axum framework wiring that can't be expressed in `.at` source:
//! - `build_router`: route mounting + `ServeDir` (frontend-dist static assets)
//!   + `CorsLayer` + `env!("CARGO_MANIFEST_DIR")` (compile-time macro). These
//!   are tower_http layering + a proc-macro expansion — both outside .at's
//!   expressive range. The handler *functions* live in `server.at`; this module
//!   just wires them into the Router and adds the cross-cutting layers.
//! - `CancelOnDrop`: RAII guard (`impl Drop`) that fires a CancellationToken on
//!   SSE client disconnect. a2r has no `impl Drop` syntax (Plan 025 §2.2
//!   decision: keep a ~10-line .rs helper). Used by the streaming path.
//!
//! (Plan 025 Phase 3: glue for axum/tower constructs + impl Drop, mirroring the
//! tier_router_glue.rs / provider_glue.rs pattern.)

use std::sync::Arc;

use axum::response::IntoResponse;
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::server::AppState;

// ── Map-iteration helpers ───────────────────────────────────────────────────
// .at's Map has no iteration API, so handlers that need to walk
// DaemonConfig.providers (a HashMap<String, ProviderConfig>) call these
// hand-written helpers instead. Same convention as tier_router_glue.rs — the
// .at source stays free of HashMap iteration; only the "collect keys/values"
// boundary crosses into glue.

/// The provider names in a daemon config, in HashMap iteration order.
pub fn config_provider_names(cfg: &ai_config::DaemonConfig) -> Vec<String> {
    cfg.providers.keys().cloned().collect()
}

/// The model ids a provider serves, as (provider_name, model_id) pairs (the
/// shape the /v1/models handler emits).
pub fn config_provider_models(
    cfg: &ai_config::DaemonConfig,
) -> Vec<(String, Vec<ai_config::ModelDefinition>)> {
    cfg.providers
        .iter()
        .map(|(name, p)| (name.clone(), p.models.clone()))
        .collect()
}

/// Wire all routes + cross-cutting layers onto a fresh Router.
///
/// The handler functions (`status`, `chat_completions`, etc.) are transpiled in
/// `server.at`; this module references them by path and owns only the framework
/// glue (ServeDir / CorsLayer / CARGO_MANIFEST_DIR / route_service). Handlers
/// are added here as they come online in Phase 3.3-3.5.
pub fn build_router(state: Arc<AppState>) -> axum::Router {
    // Serve federation remote assets. remoteEntry.js + chunks are in
    // frontend-dist/assets/. (Mirrors rust-ref server.rs:57-64.)
    let assets_path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../frontend-dist");
    let static_service = ServeDir::new(&assets_path);

    // CORS: allow any origin/method/header (dev-friendly; matches rust-ref).
    let cors = CorsLayer::permissive()
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_origin(Any);

    axum::Router::new()
        .route("/v1/status", get(crate::server::status))
        .route("/v1/models", get(crate::server::models))
        .route("/v1/usage", get(crate::server::usage))
        .route(
            "/v1/chat/completions",
            post(crate::server::chat_completions),
        )
        .route("/v1/config/test", post(config_test))
        .route("/v1/services", get(services_list))
        .route(
            "/v1/services/{id}/ensure",
            post(services_ensure),
        )
        // Serve remoteEntry.js explicitly; fallback_service handles chunk files
        // (./__federation_expose_*.js etc.) relative to remoteEntry.js's URL.
        .route_service("/remoteEntry.js", static_service.clone())
        .fallback_service(static_service)
        .layer(cors)
        .with_state(state)
}

/// RAII guard that fires a [`CancellationToken`] on drop. Used so that when the
/// SSE body stream is dropped (client disconnect), the upstream provider fetch
/// is aborted instead of running to completion into a dead channel.
/// (Mirrors rust-ref server.rs:349-358. a2r has no `impl Drop` syntax.)
pub(crate) struct CancelOnDrop(pub tokio_util::sync::CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

// ── streaming_response (Phase 3.6) ───────────────────────────────────────────
// The SSE bridge: spawns the provider's complete_stream task, bridges its
// on_delta callback (sync) to axum's async stream via an mpsc channel, and
// builds the SSE body. Lives in glue (not server.at) because it uses bare
// tokio::spawn + bidirectional mpsc + async_stream::stream! — all three have
// no .at transpilation precedent (only actor-abstracted spawn + one-way recv
// exist). Mirrors rust-ref server.rs:255-347.

/// Build an SSE response that streams text deltas from the provider.
#[allow(clippy::too_many_arguments)]
pub async fn streaming_response(
    state: std::sync::Arc<crate::server::AppState>,
    app_name: String,
    provider: std::sync::Arc<dyn crate::provider::AiProvider>,
    req: ai_config::CompletionRequest,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> axum::response::Response {
    use axum::body::Body;
    use axum::response::Response;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    let (tx, mut rx) = mpsc::channel::<String>(64);
    // Cancellation token: fired when the SSE consumer (client) disconnects, so
    // the provider stops pulling tokens and releases the permit promptly.
    let cancel = CancellationToken::new();

    // Spawn the streaming call: invokes the provider, whose on_delta callback
    // pushes deltas into the channel. When done, sends a final event.
    let tx2 = tx.clone();
    let cancel_for_task = cancel.clone();
    let provider_task = tokio::spawn(async move {
        let on_delta: std::sync::Arc<dyn Fn(crate::provider::StreamDelta) + Send + Sync> =
            std::sync::Arc::new(move |chunk: crate::provider::StreamDelta| {
                let payload = match chunk {
                    crate::provider::StreamDelta::Text(t) => {
                        serde_json::json!({"type": "delta", "text": t})
                    }
                    crate::provider::StreamDelta::Reasoning(t) => {
                        serde_json::json!({"type": "reasoning", "text": t})
                    }
                };
                let _ = tx2.try_send(format!("data: {}\n\n", payload));
            });

        match provider.complete_stream(&req, on_delta, cancel_for_task).await {
            Ok(resp) => {
                if let Some(u) = &resp.usage {
                    state
                        .tracker
                        .record(app_name.as_str(), u.input_tokens, u.output_tokens);
                }
                let _ = tx.try_send(format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "type": "done",
                        "model": resp.model,
                        "usage": resp.usage,
                        "tool_calls": resp.tool_calls.iter().map(|tc| serde_json::json!({
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.input,
                        })).collect::<Vec<_>>(),
                        "stop_reason": resp.stop_reason,
                    })
                ));
            }
            Err(e) => {
                let _ = tx.try_send(format!(
                    "data: {}\n\n",
                    serde_json::json!({"type": "error", "message": format!("{e}")})
                ));
            }
        }
        // Release the concurrency permit when streaming finishes.
        drop(permit);
    });

    // Build an SSE body from the channel. If the client disconnects, axum drops
    // this stream future; the CancelOnDrop guard fires the cancellation token.
    let cancel_on_drop = cancel.clone();
    let stream = async_stream::stream! {
        let _cancel_guard = CancelOnDrop(cancel_on_drop);
        while let Some(event) = rx.recv().await {
            yield Ok::<_, std::convert::Infallible>(event);
        }
        // Ensure the task completes (propagates panics / cleans up).
        let _ = provider_task.await;
    };

    Response::builder()
        .status(axum::http::StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

// ── config_test + services handlers (Phase 3.5) ─────────────────────────────
// These three handlers live in glue (not transpiled to server.at) because:
// - config_test uses reqwest::Client chain + conditional header building — no
//   .at precedent for reqwest client calls (the project uses VM-internal http).
// - services_list/services_ensure delegate to crate::services (hand-written OS
//   glue, not transpiled). Keeping them next to their dep in glue is cleaner.
// Mirrors rust-ref server.rs:364-417 (config_test), 486-499 (services_list),
// 504-529 (services_ensure).

/// `POST /v1/config/test` — test a provider connection.
async fn config_test(
    axum::Json(body): axum::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    let kind = body["kind"].as_str().unwrap_or("openai");
    let base_url = body["base_url"].as_str().unwrap_or("");
    let api_key = body["api_key"].as_str().unwrap_or("");
    let model = body["model"].as_str().unwrap_or("");

    let url = if kind == "anthropic" {
        format!("{}/v1/messages", base_url.trim_end_matches('/'))
    } else {
        format!("{}/chat/completions", base_url.trim_end_matches('/'))
    };

    let req_body = serde_json::json!({
        "model": model,
        "max_tokens": 10,
        "messages": [{"role": "user", "content": "Hi"}],
    });

    let client = reqwest::Client::new();
    let start = std::time::Instant::now();

    let mut req = client.post(&url).json(&req_body);
    if kind == "anthropic" {
        req = req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    } else {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }

    match req.timeout(std::time::Duration::from_secs(15)).send().await {
        Ok(resp) => {
            let latency = start.elapsed().as_millis();
            let status = resp.status();
            if status.is_success() {
                axum::Json(serde_json::json!({"success": true, "latency_ms": latency}))
            } else {
                let body = resp.text().await.unwrap_or_default();
                axum::Json(serde_json::json!({
                    "success": false,
                    "error": format!("HTTP {status}: {}", body.chars().take(200).collect::<String>()),
                    "latency_ms": latency
                }))
            }
        }
        Err(e) => axum::Json(serde_json::json!({
            "success": false,
            "error": e.to_string(),
            "latency_ms": start.elapsed().as_millis()
        })),
    }
}

/// `GET /v1/services` — list all registered services with live status.
async fn services_list() -> impl axum::response::IntoResponse {
    let reg = crate::services::ServiceRegistry::load();
    let mut services = Vec::new();
    for svc in reg.list() {
        let running = crate::services::probe_url_async(&svc.url, &svc.ready_path).await;
        services.push(serde_json::json!({
            "id": svc.id,
            "name": svc.name,
            "url": svc.url,
            "running": running,
        }));
    }
    axum::Json(serde_json::json!({"services": services}))
}

/// `POST /v1/services/{id}/ensure` — ensure a service is running.
async fn services_ensure(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let reg = crate::services::ServiceRegistry::load();
    let id_for_closure = id.clone();
    // Run the blocking ensure in spawn_blocking so we don't stall the runtime.
    let result = tokio::task::spawn_blocking(move || reg.ensure(&id_for_closure))
        .await
        .map_err(|e| format!("internal: {e}"));

    match result {
        Ok(Ok(url)) => axum::Json(serde_json::json!({"status": "running", "url": url, "id": id}))
            .into_response(),
        Ok(Err(e)) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"status": "error", "id": id, "error": e})),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"status": "error", "id": id, "error": e})),
        )
            .into_response(),
    }
}
