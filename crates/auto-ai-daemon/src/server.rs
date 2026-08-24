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
            // Plan 031: the metadata travels with the *actual* serving model
            // (after this candidate won), in the SSE done tail frame.
            let model_meta = model_meta_for(&state.cfg(), provider_name, model_id);
            return streaming_response(state, app_name, provider, req, permit, model_meta)
                .await;
        }

        // Non-streaming: try the call, fall back on retryable errors.
        match provider.complete(&req).await {
            Ok(mut resp) => {
                // Plan 031: embed the serving model's metadata so the client
                // can adapt its context-window math (tier fallback may have
                // swapped in a different-window model than requested).
                resp.model_meta = model_meta_for(&state.cfg(), provider_name, model_id);
                if let Some(u) = &resp.usage {
                    state.tracker.record_full(
                        &app_name,
                        u.input_tokens as u64,
                        u.output_tokens as u64,
                        u.cache_read_tokens as u64,
                        u.cache_write_tokens as u64,
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
                // Plan 028: quota/billing exhaustion is account-level — falling
                // back to the next candidate only burns the retry window.
                if e.is_quota_exhausted() {
                    tracing::error!(
                        "tier fallback: '{}' failed with quota/billing error ({}) — aborting candidate chain",
                        provider_name,
                        e
                    );
                    return (
                        StatusCode::PAYMENT_REQUIRED,
                        Json(json!({"error": {"message": format!("quota/billing exhausted: {e}"), "type": "quota_exhausted"}})),
                    )
                        .into_response();
                }
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
    model_meta: Option<ai_config::ModelMeta>,
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
                        // Plan 031: serving-model metadata in the tail frame
                        // (consumers adapt context-window math to it).
                        "model_meta": serde_json::to_value(&model_meta).unwrap_or(serde_json::Value::Null),
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

/// Plan 031: metadata of the model that actually served a request, looked up
/// from the daemon config by (provider, model id). `None` when the config
/// doesn't declare a context window for the model — we never guess a window,
/// so consumers keep their own default rather than trusting a fabricated one.
fn model_meta_for(
    config: &DaemonConfig,
    provider: &str,
    model_id: &str,
) -> Option<ai_config::ModelMeta> {
    let m = config
        .providers
        .get(provider)?
        .models
        .iter()
        .find(|m| m.id == model_id)?;
    Some(ai_config::ModelMeta {
        id: model_id.to_string(),
        context_window: m.context_window?,
        max_output_tokens: m.max_output_tokens,
    })
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

/// Plan 028: model metadata projection for /v1/models (omits unknown fields).
fn model_metadata(m: &ai_config::ModelDefinition) -> serde_json::Value {
    let mut v = serde_json::Map::new();
    if let Some(w) = m.context_window {
        v.insert("context_window".into(), json!(w));
    }
    if let Some(o) = m.max_output_tokens {
        v.insert("max_output_tokens".into(), json!(o));
    }
    if let Some(c) = &m.cost_per_mtok {
        v.insert(
            "cost_per_mtok".into(),
            json!({"input_usd_per_mtok_usd": c.input as f64 / 1e6,
                   "output_usd_per_mtok": c.output as f64 / 1e6,
                   "cache_read_usd_per_mtok": c.cache_read as f64 / 1e6}),
        );
    }
    if let Some(c) = &m.capabilities {
        v.insert("capabilities".into(), json!({"vision": c.vision, "thinking": c.thinking}));
    }
    serde_json::Value::Object(v)
}

async fn models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.cfg();
    let models: Vec<serde_json::Value> = cfg
        .providers
        .iter()
        .flat_map(|(name, p)| {
            p.models
                .iter()
                .map(move |m| json!({"provider": name, "model": m.id, "metadata": model_metadata(m)}))
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
                // Plan 028: cache dimensions.
                "cache_read_tokens": u.total_cache_read_tokens,
                "cache_write_tokens": u.total_cache_write_tokens,
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

// ── Tests (Plan 031: metadata embedding + tier-fallback gating) ─────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LlmError;
    use ai_config::tier::ModelDefinition;
    use ai_config::ProviderConfig;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mock provider whose responses are scripted per call. The call
    /// counter is a shared handle so tests can read it after the mock moved
    /// into the registry.
    struct MockProvider {
        name: String,
        model: String,
        responses: std::sync::Mutex<Vec<Result<ai_config::CompletionResponse, LlmError>>>,
        calls: Arc<AtomicUsize>,
    }

    impl MockProvider {
        fn new(name: &str, model: &str, responses: Vec<Result<ai_config::CompletionResponse, LlmError>>) -> Self {
            Self {
                name: name.into(),
                model: model.into(),
                responses: std::sync::Mutex::new(responses),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::provider::AiProvider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn models(&self) -> Vec<String> {
            vec![self.model.clone()]
        }
        async fn complete(&self, _req: &ai_config::CompletionRequest) -> Result<ai_config::CompletionResponse, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Ok(ai_config::CompletionResponse {
                    content: "(script exhausted)".into(),
                    tool_calls: vec![],
                    stop_reason: Some("end_turn".into()),
                    usage: None,
                    model: self.model.clone(),
                    error: None,
                    model_meta: None,
                });
            }
            let idx = n.min(q.len() - 1);
            q.remove(idx)
        }
        async fn complete_stream(
            &self,
            req: &ai_config::CompletionRequest,
            _on_delta: Arc<dyn Fn(crate::provider::StreamDelta) + Send + Sync>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<ai_config::CompletionResponse, LlmError> {
            self.complete(req).await
        }
    }

    fn ok_response() -> ai_config::CompletionResponse {
        ai_config::CompletionResponse {
            content: "hi".into(),
            tool_calls: vec![],
            stop_reason: Some("end_turn".into()),
            usage: Some(ai_config::Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }),
            model: String::new(),
            error: None,
            model_meta: None,
        }
    }

    fn model_def(id: &str, tier: ai_config::ModelTier, window: Option<u32>) -> ModelDefinition {
        ModelDefinition {
            id: id.into(),
            name: String::new(),
            tier,
            context_window: window,
            max_output_tokens: Some(4_096),
            cost_per_mtok: None,
            capabilities: None,
        }
    }

    fn test_config() -> DaemonConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "mocka".to_string(),
            ProviderConfig {
                kind: "openai".into(),
                base_url: "http://localhost:1".into(),
                api_key: Some("k".into()),
                key_env: None,
                models: vec![model_def("model-a", ai_config::ModelTier::Mid, Some(32_000))],
                max_concurrency: Some(4),
                auth_required: true,
            },
        );
        providers.insert(
            "mockb".to_string(),
            ProviderConfig {
                kind: "openai".into(),
                base_url: "http://localhost:2".into(),
                api_key: Some("k".into()),
                key_env: None,
                models: vec![model_def("model-b", ai_config::ModelTier::Mid, Some(200_000))],
                max_concurrency: Some(4),
                auth_required: true,
            },
        );
        DaemonConfig {
            listen_addr: "127.0.0.1:0".into(),
            idle_timeout_min: 10,
            log_level: "info".into(),
            providers,
            default_provider: "mocka".into(),
            default_model: "model-a".into(),
            tier_routing: ai_config::loader::TierRouting::default(),
        }
    }

    #[allow(clippy::type_complexity)]
    fn state_with(
        config: DaemonConfig,
        mocks: Vec<(&str, &str, Vec<Result<ai_config::CompletionResponse, LlmError>>)>,
    ) -> (Arc<AppState>, HashMap<String, Arc<AtomicUsize>>) {
        let mut state = AppState::new(config);
        let mut counters = HashMap::new();
        for (name, model, responses) in mocks {
            let mock = MockProvider::new(name, model, responses);
            counters.insert(name.to_string(), mock.calls.clone());
            state.registry.insert(name, Arc::new(mock));
        }
        (Arc::new(state), counters)
    }

    #[test]
    fn model_meta_for_lookup() {
        let cfg = test_config();
        // Known model with a window → full metadata.
        let meta = model_meta_for(&cfg, "mocka", "model-a").expect("meta must resolve");
        assert_eq!(meta.id, "model-a");
        assert_eq!(meta.context_window, 32_000);
        assert_eq!(meta.max_output_tokens, Some(4_096));
        // Unknown provider / model → None (never fabricate a window).
        assert!(model_meta_for(&cfg, "nope", "model-a").is_none());
        assert!(model_meta_for(&cfg, "mocka", "nope").is_none());
        // Model without a declared window → None.
        let mut cfg2 = test_config();
        cfg2.providers.get_mut("mockb").unwrap().models[0].context_window = None;
        assert!(model_meta_for(&cfg2, "mockb", "model-b").is_none());
    }

    #[tokio::test]
    async fn usage_endpoint_projects_cache_dimensions() {
        let (state, _) = state_with(test_config(), vec![]);
        state.tracker.record_full("app-x", 1000, 200, 700, 250);
        let resp = usage(State(state)).await.into_response();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let app = v["usage"]
            .as_array()
            .and_then(|a| a.iter().find(|o| o["app"] == "app-x"))
            .expect("app-x missing");
        assert_eq!(app["input_tokens"], 1000);
        assert_eq!(app["output_tokens"], 200);
        assert_eq!(app["total_tokens"], 1200);
        assert_eq!(app["requests"], 1);
        assert_eq!(app["cache_read_tokens"], 700);
        assert_eq!(app["cache_write_tokens"], 250);
    }

    async fn call_chat(state: Arc<AppState>, model: &str) -> (axum::http::StatusCode, serde_json::Value) {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-app-name", "test".parse().unwrap());
        let req = ai_config::CompletionRequest::single(model, "hi");
        let resp = chat_completions(State(state), headers, Json(req)).await.into_response();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn response_embeds_serving_model_meta() {
        // A tier request whose first candidate (mocka, 32k window) serves it.
        let (state, _) = state_with(
            test_config(),
            vec![("mocka", "model-a", vec![Ok(ok_response())])],
        );
        let (status, v) = call_chat(state.clone(), "tier:mid").await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let meta = v["model_meta"].as_object().expect("model_meta missing");
        assert_eq!(meta["id"], "model-a");
        assert_eq!(meta["context_window"], 32_000);
        // usage recorded for the app
        assert_eq!(state.tracker.get("test").total_input_tokens, 10);
    }

    #[tokio::test]
    async fn quota_error_aborts_candidate_chain() {
        // Plan 028 debt: quota/billing exhaustion must NOT consume the chain —
        // mocka fails with a quota error and mockb must never be called.
        let quota_err = Err(LlmError::Upstream {
            status: 429,
            message: "You exceeded your current quota".into(),
            retryable: true,
        });
        let (state, counters) = state_with(
            test_config(),
            vec![
                ("mocka", "model-a", vec![quota_err]),
                ("mockb", "model-b", vec![Ok(ok_response())]),
            ],
        );
        let (status, v) = call_chat(state, "tier:mid").await;
        assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{v}");
        assert_eq!(v["error"]["type"], "quota_exhausted");
        assert_eq!(counters["mockb"].load(Ordering::SeqCst), 0, "quota error must not consume the candidate chain");
    }

    #[tokio::test]
    async fn retryable_error_falls_back_to_next_candidate() {
        // Control: a retryable 5xx DOES move to the next candidate, and the
        // response's model_meta reflects the model that actually served it.
        let server_err = Err(LlmError::Upstream {
            status: 503,
            message: "unavailable".into(),
            retryable: true,
        });
        let (state, counters) = state_with(
            test_config(),
            vec![
                ("mocka", "model-a", vec![server_err]),
                ("mockb", "model-b", vec![Ok(ok_response())]),
            ],
        );
        let (status, v) = call_chat(state, "tier:mid").await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["model_meta"]["id"], "model-b");
        assert_eq!(v["model_meta"]["context_window"], 200_000, "meta must follow the fallback model");
        assert_eq!(counters["mocka"].load(Ordering::SeqCst), 1);
    }
}
