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
    pub config: DaemonConfig,
    pub registry: ProviderRegistry,
    pub pool: ConcurrencyManager,
    pub tracker: UsageTracker,
    pub current_model: std::sync::Mutex<String>,
}

impl AppState {
    pub fn new(config: DaemonConfig) -> Self {
        let registry = ProviderRegistry::from_daemon_config(&config)
            .expect("daemon config must have at least one provider");
        let pool = ConcurrencyManager::from_config(&config);
        let current_model = config.default_model.clone();
        Self {
            config,
            registry,
            pool,
            tracker: UsageTracker::new(),
            current_model: std::sync::Mutex::new(current_model),
        }
    }
}

pub fn router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/status", get(status))
        .route("/v1/models", get(models))
        .route("/v1/usage", get(usage))
        .with_state(state)
}

/// POST /v1/chat/completions — receive a canonical request, call a provider,
/// return a canonical response.
///
/// The body is a canonical [`ai_config::CompletionRequest`]. The daemon picks
/// its default provider (provider/model routing is a future enhancement),
/// acquires a concurrency permit, and delegates the (canonical↔provider)
/// translation to the provider implementation.
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
    let provider_name = &state.config.default_provider;

    // Acquire concurrency permit.
    let permit = match state.pool.acquire(provider_name).await {
        Some(p) => p,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": {"message": "concurrency pool unavailable"}})),
            )
                .into_response();
        }
    };

    // Resolve the request's model: a "tier:<tier>" token → concrete model id
    // (the agent emits a tier token when the profession didn't pin a model).
    // Falls through unchanged for concrete model ids.
    let mut req = req;
    if req.model.starts_with("tier:") {
        if let Some(resolved) = resolve_tier_model(&req.model, &state.config) {
            req.model = resolved;
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": format!("could not resolve tier '{}' — configure models with tiers in ai-daemon.at", req.model)}})),
            )
                .into_response();
        }
    }

    let provider = match state.registry.default_provider() {
        Ok(p) => p.clone(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": format!("{e}")}})),
            )
                .into_response();
        }
    };

    // Hand the canonical request to the provider, which translates it to its
    // own wire format, calls upstream, and parses back a canonical response.
    if req.stream {
        return streaming_response(state, app_name, provider, req, permit).await;
    }

    match provider.complete(&req).await {
        Ok(resp) => {
            if let Some(u) = &resp.usage {
                state
                    .tracker
                    .record(&app_name, u.input_tokens as u64, u.output_tokens as u64);
            }
            drop(permit);
            (
                StatusCode::OK,
                Json(serde_json::to_value(&resp).unwrap_or(json!({"error": "serialize"}))),
            )
                .into_response()
        }
        Err(e) => {
            drop(permit);
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": {"message": format!("upstream error: {e}")}})),
            )
                .into_response()
        }
    }
}

/// Build an SSE response that streams text deltas from the provider.
///
/// Uses an mpsc channel to bridge the provider's `on_delta` callback (which is
/// sync) to axum's async stream. Events emitted:
/// - `data: {"type":"delta","text":"..."}` for each token chunk
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

    let (tx, mut rx) = mpsc::channel::<String>(64);

    // Spawn the streaming call: invokes the provider, whose `on_delta` callback
    // pushes deltas into the channel. When done, sends a final event.
    let tx2 = tx.clone();
    let provider_task = tokio::spawn(async move {
        let on_delta: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |delta: String| {
            // best-effort push; ignore if channel closed (client disconnected)
            let _ = tx2.try_send(format!(
                "data: {}\n\n",
                json!({"type": "delta", "text": delta})
            ));
        });

        match provider.complete_stream(&req, on_delta).await {
            Ok(resp) => {
                if let Some(u) = &resp.usage {
                    state
                        .tracker
                        .record(&app_name, u.input_tokens as u64, u.output_tokens as u64);
                }
                let _ = tx.try_send(format!(
                    "data: {}\n\n",
                    json!({"type": "done", "model": resp.model, "usage": resp.usage})
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

    // Build an SSE body from the channel. When the client disconnects or the
    // provider task ends (channel closes), the stream ends.
    let stream = async_stream::stream! {
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

/// Resolve a `"tier:<tier>"` token to a concrete model id from the default
/// provider's tier-tagged models. Returns None if the tier name is unknown or
/// the provider has no models.
fn resolve_tier_model(token: &str, config: &crate::config::DaemonConfig) -> Option<String> {
    let tier_name = token.strip_prefix("tier:")?.trim().to_ascii_lowercase();
    let tier = match tier_name.as_str() {
        "min" => ai_config::ModelTier::Min,
        "lite" | "light" => ai_config::ModelTier::Lite,
        "mid" => ai_config::ModelTier::Mid,
        "pro" | "large" => ai_config::ModelTier::Pro,
        "max" | "heavy" => ai_config::ModelTier::Max,
        _ => return None,
    };
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

    let current_model = state.current_model.lock().unwrap().clone();

    Json(json!({
        "status": "running",
        "current_model": current_model,
        "pools": pools,
    }))
}

async fn models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let models: Vec<serde_json::Value> = state
        .config
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
