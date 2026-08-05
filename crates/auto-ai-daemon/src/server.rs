//! HTTP server (axum) — the daemon's public API.
//!
//! Endpoints:
//! - `POST /v1/chat/completions` — receives a **canonical** `CompletionRequest`
//!   (from `auto-ai-client`), selects a provider, translates to the provider's
//!   wire format, calls the upstream LLM, and returns a **canonical**
//!   `CompletionResponse`. All provider shape knowledge lives in the daemon.
//! - `GET /v1/status` / `/v1/models` / `/v1/usage` — observability.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use serde_json::json;

use crate::config::DaemonConfig;
use crate::pool::ConcurrencyManager;
use crate::provider::ProviderRegistry;
use crate::tracker::UsageTracker;

pub struct AppState {
    pub config: std::sync::Arc<parking_lot::RwLock<DaemonConfig>>,
    pub registry: ProviderRegistry,
    pub pool: ConcurrencyManager,
    pub tracker: UsageTracker,
    pub current_model: parking_lot::Mutex<String>,
    pub tier_router: crate::tier_router::TierRouter,
}

impl AppState {
    pub fn new(config: DaemonConfig) -> Self {
        let registry = ProviderRegistry::from_daemon_config(&config)
            .expect("daemon config must have at least one provider");
        let pool = ConcurrencyManager::from_config(&config);
        let current_model = config.default_model.clone();
        let tier_router = crate::tier_router::TierRouter::from_config(&config);
        Self {
            config: std::sync::Arc::new(parking_lot::RwLock::new(config)),
            registry,
            pool,
            tracker: UsageTracker::new(),
            current_model: parking_lot::Mutex::new(current_model),
            tier_router,
        }
    }

    /// Read-locked access to the config (for GET handlers).
    pub fn cfg(&self) -> parking_lot::RwLockReadGuard<'_, DaemonConfig> {
        self.config.read()
    }
}

pub fn router(state: Arc<AppState>) -> axum::Router {
    // Serve federation remote assets. remoteEntry.js + chunks are in
    // frontend-dist/assets/. The federation runtime loads chunks with relative
    // paths (e.g. ./__federation_expose_*.js) relative to remoteEntry.js's URL
    // (/remoteEntry.js), so they resolve to /__federation_expose_*.js.
    // We serve from frontend-dist/assets/ at the root level.
    let assets_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("frontend-dist");
    let static_service = tower_http::services::ServeDir::new(&assets_path);

    // CORS: allow auto-os-config (and any localhost dev server) to load
    // federation remotes + config API cross-origin.
    let cors = tower_http::cors::CorsLayer::permissive()
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
        .allow_origin(tower_http::cors::Any);

    axum::Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/status", get(status))
        .route("/v1/models", get(models))
        .route("/v1/usage", get(usage))
        .route("/v1/config/test", post(config_test))
        // Service registry (aaid as AutoOS service discovery + launch hub).
        .route("/v1/services", get(services_list))
        .route("/v1/services/{id}/ensure", post(services_ensure))
        // Federation remote: serve remoteEntry.js explicitly, and use
        // fallback_service for chunk files (./__federation_expose_*.js etc.)
        .route_service("/remoteEntry.js", static_service.clone())
        .fallback_service(static_service)
        .layer(cors)
        .with_state(state)
}

/// POST /v1/chat/completions — receive a canonical request, call a provider,
/// return a canonical response.
///
/// The body is a canonical [`ai_config::CompletionRequest`]. The daemon
/// resolves the model/tier to a provider (via [`TierRouter`], with fallback
/// across the candidate chain for tier requests), acquires a concurrency
/// permit, and delegates the (canonical↔provider) translation to the provider.
async fn chat_completions(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ai_config::CompletionRequest>,
) -> impl IntoResponse {
    let app_name = headers
        .get("x-app-name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    // Resolve the request's model + provider.
    let mut req = req;
    let is_tier_request = req.model.starts_with("tier:");

    // Build the ordered list of (provider_name, model_id) candidates to try.
    // For tier requests this is the TierRouter's candidate chain (enabling
    // fallback across providers); for concrete model ids it's a single entry.
    let candidates: Vec<(String, String)> = if is_tier_request {
        let tier_name = req.model.strip_prefix("tier:").unwrap_or("").trim().to_ascii_lowercase();
        let tier = match ai_config::ModelTier::parse_name(&tier_name) {
            Some(t) => t,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": {"message": format!("unknown tier '{tier_name}'")}})),
                )
                    .into_response();
            }
        };
        let chain: Vec<(String, String)> = state
            .tier_router
            .candidates_preferred(tier, req.preferred_provider.as_deref())
            .iter()
            .map(|c| (c.provider.clone(), c.model.clone()))
            .collect();
        if chain.is_empty() {
            // Legacy fallback: resolve via default provider's model list.
            if let Some(resolved) = resolve_tier_model(&req.model, &state.cfg()) {
                vec![(state.cfg().default_provider.clone(), resolved)]
            } else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": {"message": format!("could not resolve tier '{}' — no candidates", req.model)}})),
                )
                    .into_response();
            }
        } else {
            chain
        }
    } else {
        // Concrete model id — find the provider that owns it (single candidate).
        let cfg = state.cfg();
        let found = cfg.providers.iter()
            .find(|(_, pc)| pc.models.iter().any(|m| m.id == req.model))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| cfg.default_provider.clone());
        vec![(found, req.model.clone())]
    };

    // Iterate candidates with fallback. For each: acquire a permit (fail-fast
    // on saturation) → invoke the provider. A retryable failure (rate limit,
    // timeout, 5xx, transport) on a non-streaming call moves to the next
    // candidate. Streaming only falls back before the stream starts (a
    // saturated first candidate's acquire failure moves to the next); once a
    // stream begins, mid-stream errors are reported as-is.
    let mut last_error: Option<String> = None;
    for (provider_name, model_id) in &candidates {
        req.model = model_id.clone();

        // Acquire a permit (bounded wait so a saturated provider fails fast).
        let permit = match state
            .pool
            .acquire_with_timeout(provider_name, std::time::Duration::from_secs(30))
            .await
        {
            Some(p) => p,
            None => {
                // Saturated — try the next candidate (fallback).
                last_error = Some(format!(
                    "provider '{}' concurrency pool unavailable",
                    provider_name
                ));
                tracing::warn!("tier fallback: '{}' saturated, trying next candidate", provider_name);
                continue;
            }
        };

        let provider = match state.registry.get(provider_name) {
            Some(p) => p.clone(),
            None => {
                last_error = Some(format!("provider '{}' not in registry", provider_name));
                continue;
            }
        };

        // Streaming: once we enter streaming_response we commit to this
        // provider (mid-stream fallback would need to replay deltas).
        if req.stream {
            return streaming_response(state, app_name, provider, req, permit).await;
        }

        // Non-streaming: try the call, fall back on retryable errors.
        match provider.complete(&req).await {
            Ok(resp) => {
                if let Some(u) = &resp.usage {
                    state.tracker.record(
                        &app_name,
                        u.input_tokens as u64,
                        u.output_tokens as u64,
                    );
                }
                drop(permit);
                return (
                    StatusCode::OK,
                    Json(serde_json::to_value(&resp).unwrap_or(json!({"error": "serialize"}))),
                )
                    .into_response();
            }
            Err(e) => {
                drop(permit);
                let retryable = e.is_retryable();
                last_error = Some(format!("{e}"));
                if retryable {
                    tracing::warn!(
                        "tier fallback: '{}' failed with retryable error ({}), trying next candidate",
                        provider_name,
                        e
                    );
                    continue;
                } else {
                    // Non-retryable (4xx param error, etc.) — don't fall back.
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({"error": {"message": format!("upstream error: {e}")}})),
                    )
                        .into_response();
                }
            }
        }
    }

    // All candidates exhausted.
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error": {"message": format!("all providers failed; last error: {}", last_error.unwrap_or_else(|| "unknown".into()))}})),
    )
        .into_response()
}

/// Build an SSE response that streams text deltas from the provider.
///
/// Uses an mpsc channel to bridge the provider's `on_delta` callback (which is
/// sync) to axum's async stream. Events emitted:
/// - `data: {"type":"delta","text":"..."}` for each visible text chunk
/// - `data: {"type":"reasoning","text":"..."}` for each reasoning/thinking chunk
/// - `data: {"type":"done","turns":1,"usage":{...}}` at the end
/// - `data: {"type":"error","message":"..."}` on failure
async fn streaming_response(
    state: Arc<AppState>,
    app_name: String,
    provider: Arc<dyn crate::provider::AiProvider>,
    req: ai_config::CompletionRequest,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> axum::response::Response {
    use axum::body::Body;
    use axum::response::Response;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    let (tx, mut rx) = mpsc::channel::<String>(64);
    // Cancellation token: fired when the SSE consumer (client) disconnects,
    // so the provider stops pulling tokens from the upstream and releases the
    // permit promptly instead of running to completion into a dead channel.
    let cancel = CancellationToken::new();

    // Spawn the streaming call: invokes the provider, whose `on_delta` callback
    // pushes deltas into the channel. When done, sends a final event.
    let tx2 = tx.clone();
    let cancel_for_task = cancel.clone();
    let provider_task = tokio::spawn(async move {
        let on_delta: Arc<dyn Fn(crate::provider::StreamDelta) + Send + Sync> =
            Arc::new(move |chunk: crate::provider::StreamDelta| {
                // best-effort push; ignore if channel closed (client disconnected)
                let payload = match chunk {
                    crate::provider::StreamDelta::Text(t) => {
                        json!({"type": "delta", "text": t})
                    }
                    crate::provider::StreamDelta::Reasoning(t) => {
                        json!({"type": "reasoning", "text": t})
                    }
                };
                let _ = tx2.try_send(format!("data: {}\n\n", payload));
            });

        match provider.complete_stream(&req, on_delta, cancel_for_task).await {
            Ok(resp) => {
                if let Some(u) = &resp.usage {
                    state
                        .tracker
                        .record(&app_name, u.input_tokens as u64, u.output_tokens as u64);
                }
                let _ = tx.try_send(format!(
                    "data: {}\n\n",
                    json!({
                        "type": "done",
                        "model": resp.model,
                        "usage": resp.usage,
                        "tool_calls": resp.tool_calls.iter().map(|tc| json!({
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
                    json!({"type": "error", "message": format!("{e}")})
                ));
            }
        }
        // Release the concurrency permit when streaming finishes.
        drop(permit);
    });

    // Build an SSE body from the channel. If the client disconnects, axum
    // drops this stream future; the `CancelOnDrop` guard then fires the
    // cancellation token so the provider stops fetching from the upstream.
    let cancel_on_drop = cancel.clone();
    let stream = async_stream::stream! {
        // When this stream is dropped (client disconnect), fire cancellation
        // so the spawned provider task stops fetching from the upstream.
        let _cancel_guard = CancelOnDrop(cancel_on_drop);
        while let Some(event) = rx.recv().await {
            yield Ok::<_, std::convert::Infallible>(event);
        }
        // Ensure the task completes (propagates panics / cleans up).
        let _ = provider_task.await;
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// RAII guard that fires a [`CancellationToken`] on drop. Used so that when
/// the SSE body stream is dropped (client disconnect), the upstream provider
/// fetch is aborted instead of running to completion into a dead channel.
struct CancelOnDrop(tokio_util::sync::CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

// ── Config page (web UI) ─────────────────────────────────────────────────────

/// `POST /v1/config/test` — test a provider connection. Body:
/// `{ "kind": "anthropic", "base_url": "...", "api_key": "...", "model": "..." }`
async fn config_test(
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let kind = body["kind"].as_str().unwrap_or("openai");
    let base_url = body["base_url"].as_str().unwrap_or("");
    let api_key = body["api_key"].as_str().unwrap_or("");
    let model = body["model"].as_str().unwrap_or("");

    let url = if kind == "anthropic" {
        format!("{}/v1/messages", base_url.trim_end_matches('/'))
    } else {
        format!("{}/chat/completions", base_url.trim_end_matches('/'))
    };

    let req_body = if kind == "anthropic" {
        json!({
            "model": model,
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "Hi"}],
        })
    } else {
        json!({
            "model": model,
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "Hi"}],
        })
    };

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
                Json(json!({"success": true, "latency_ms": latency}))
            } else {
                let body = resp.text().await.unwrap_or_default();
                Json(json!({"success": false, "error": format!("HTTP {status}: {}", body.chars().take(200).collect::<String>()), "latency_ms": latency}))
            }
        }
        Err(e) => Json(json!({"success": false, "error": e.to_string(), "latency_ms": start.elapsed().as_millis()})),
    }
}

fn resolve_tier_model(token: &str, config: &crate::config::DaemonConfig) -> Option<String> {
    let tier_name = token.strip_prefix("tier:")?.trim().to_ascii_lowercase();
    let tier = ai_config::ModelTier::parse_name(&tier_name)?;
    let provider = config.providers.get(&config.default_provider)?;
    let models: Vec<ai_config::ModelDefinition> = provider.models.clone();
    ai_config::resolve_model_id(tier, &models)
}

async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pools: Vec<serde_json::Value> = state
        .pool
        .status()
        .iter()
        .map(|(name, available, max)| {
            json!({
                "provider": name,
                "available_permits": available,
                "max_concurrency": max,
                "in_use": max - available,
            })
        })
        .collect();

    let current_model = state.current_model.lock().clone();

    Json(json!({
        "status": "running",
        "current_model": current_model,
        "pools": pools,
    }))
}

async fn models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.cfg();
    let models: Vec<serde_json::Value> = cfg
        .providers
        .iter()
        .flat_map(|(name, p)| {
            p.models
                .iter()
                .map(move |m| json!({"provider": name, "model": m}))
        })
        .collect();
    Json(json!({"models": models}))
}

async fn usage(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let apps: Vec<serde_json::Value> = state
        .tracker
        .all()
        .iter()
        .map(|(name, u)| {
            json!({
                "app": name,
                "input_tokens": u.total_input_tokens,
                "output_tokens": u.total_output_tokens,
                "total_tokens": u.total_tokens(),
                "requests": u.request_count,
            })
        })
        .collect();
    Json(json!({"usage": apps}))
}

// ── Service Registry endpoints ──────────────────────────────────────────────

/// `GET /v1/services` — list all registered services with live status.
async fn services_list() -> impl IntoResponse {
    let reg = crate::services::ServiceRegistry::load();
    let mut services = Vec::new();
    for svc in reg.list() {
        let running = crate::services::probe_url_async(&svc.url, &svc.ready_path).await;
        services.push(json!({
            "id": svc.id,
            "name": svc.name,
            "url": svc.url,
            "running": running,
        }));
    }
    Json(json!({"services": services}))
}

/// `POST /v1/services/{id}/ensure` — ensure a service is running (start if
/// needed), return its URL. Blocking: may take up to 15s if the service needs
/// to be started.
async fn services_ensure(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    let reg = crate::services::ServiceRegistry::load();
    let id_for_closure = id.clone();
    // Run the blocking ensure in a spawn_blocking so we don't stall the async
    // runtime while waiting for the service to come up.
    let result = tokio::task::spawn_blocking(move || reg.ensure(&id_for_closure))
        .await
        .map_err(|e| format!("internal: {e}"));

    match result {
        Ok(Ok(url)) => Json(json!({"status": "running", "url": url, "id": id}))
            .into_response(),
        Ok(Err(e)) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "error", "id": id, "error": e})),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "error", "id": id, "error": e})),
        )
            .into_response(),
    }
}
