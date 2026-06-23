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
    pub config: std::sync::Arc<std::sync::RwLock<DaemonConfig>>,
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
            config: std::sync::Arc::new(std::sync::RwLock::new(config)),
            registry,
            pool,
            tracker: UsageTracker::new(),
            current_model: std::sync::Mutex::new(current_model),
        }
    }

    /// Read-locked access to the config (for GET handlers).
    pub fn cfg(&self) -> std::sync::RwLockReadGuard<'_, DaemonConfig> {
        self.config.read().unwrap()
    }
}

pub fn router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/status", get(status))
        .route("/v1/models", get(models))
        .route("/v1/usage", get(usage))
        .route("/v1/config", get(config_page))
        .route("/v1/config/data", get(config_data).put(config_update))
        .route("/v1/config/test", post(config_test))
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
    let provider_name = state.cfg().default_provider.clone();

    // Acquire concurrency permit.
    let permit = match state.pool.acquire(&provider_name).await {
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
        if let Some(resolved) = resolve_tier_model(&req.model, &state.cfg()) {
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

// ── Config page (web UI) ─────────────────────────────────────────────────────

const CONFIG_HTML: &str = include_str!("config.html");

/// `GET /v1/config` — serve the embedded config web page.
async fn config_page() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Content-Type", "text/html; charset=utf-8")],
        CONFIG_HTML,
    )
}

/// `GET /v1/config/data` — return current daemon config as JSON (API keys
/// masked for security).
async fn config_data(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let c = state.cfg();
    let providers: Vec<serde_json::Value> = c
        .providers
        .iter()
        .map(|(name, p)| {
            json!({
                "name": name,
                "kind": p.kind,
                "base_url": p.base_url,
                "api_key_masked": mask_key(p.api_key.as_deref()),
                "key_env": p.key_env,
                "models": p.models.iter().map(|m| json!({
                    "id": m.id,
                    "name": if m.name.is_empty() { m.id.clone() } else { m.name.clone() },
                    "tier": format!("{:?}", m.tier).to_lowercase(),
                })).collect::<Vec<_>>(),
                "max_concurrency": p.max_concurrency,
            })
        })
        .collect();

    Json(json!({
        "listen_addr": c.listen_addr,
        "idle_timeout_min": c.idle_timeout_min,
        "log_level": c.log_level,
        "default_provider": c.default_provider,
        "default_model": c.default_model,
        "providers": providers,
    }))
}

/// `PUT /v1/config/data` — update providers (persist to ai-daemon.at).
async fn config_update(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Build a new .at string from the request and save it.
    let at_content = {
        let cfg_guard = state.cfg();
        match build_daemon_at(&body, &cfg_guard) {
            Ok(s) => s,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("invalid config: {e}")})),
                )
            }
        }
    };
    match save_daemon_at(&at_content) {
        Ok(path) => {
            tracing::info!("daemon config saved to {}", path.display());
            // Hot-reload: re-parse the file and update the in-memory config so
            // GET /v1/config/data reflects the new values immediately (no restart
            // needed for display; a restart is still needed to re-build the
            // provider registry for actual LLM calls).
            match crate::config::parse_daemon_config(&at_content) {
                Ok(new_config) => {
                    *state.config.write().unwrap() = new_config;
                    tracing::info!("daemon config hot-reloaded (display only; restart to apply to provider registry)");
                    (
                        StatusCode::OK,
                        Json(json!({
                            "status": "saved",
                            "note": "config updated. Restart aaid to apply to provider registry."
                        })),
                    )
                }
                Err(e) => {
                    tracing::warn!("daemon config saved but failed to hot-reload: {e}");
                    (
                        StatusCode::OK,
                        Json(json!({
                            "status": "saved",
                            "note": "saved to file but hot-reload failed; restart aaid."
                        })),
                    )
                }
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("save failed: {e}")})),
        ),
    }
}

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

/// Mask an API key for display: show first 6 + last 4 chars, middle masked.
fn mask_key(key: Option<&str>) -> String {
    match key {
        None | Some("") => String::new(),
        Some(k) if k.len() <= 10 => "****".to_string(),
        Some(k) => format!("{}****{}", &k[..6], &k[k.len() - 4..]),
    }
}

/// Build a `.at` config string from a JSON config update request.
///
/// `current`: the current config, used to **preserve existing API keys** when
/// the UI sends an empty/masked key (the UI masks keys for display, so the
/// user never sees the real key — we must not lose it on save).
fn build_daemon_at(body: &serde_json::Value, current: &DaemonConfig) -> Result<String, String> {
    let listen_addr = body["listen_addr"].as_str().unwrap_or("127.0.0.1:17654");
    let idle_timeout = body["idle_timeout_min"].as_u64().unwrap_or(10);
    let log_level = body["log_level"].as_str().unwrap_or("info");
    let default_provider = body["default_provider"].as_str().unwrap_or("");
    let default_model = body["default_model"].as_str().unwrap_or("");

    let mut out = String::from("daemon {\n");
    out.push_str(&format!("    listen_addr : \"{listen_addr}\"\n"));
    out.push_str(&format!("    idle_timeout_min : {idle_timeout}\n"));
    out.push_str(&format!("    log_level : {log_level}\n"));
    out.push_str(&format!("    default_provider : {default_provider}\n"));
    if !default_model.is_empty() {
        out.push_str(&format!("    default_model : \"{default_model}\"\n"));
    }
    out.push('\n');

    if let Some(providers) = body["providers"].as_array() {
        for p in providers {
            let name = p["name"].as_str().unwrap_or("provider");
            let kind = p["kind"].as_str().unwrap_or("openai");
            let base_url = p["base_url"].as_str().unwrap_or("");
            // api_key: use from request if non-empty/non-masked; else preserve
            // the existing key from current config (so we don't lose it).
            let api_key = p["api_key"].as_str().unwrap_or("");
            let existing_key = current
                .providers
                .get(name)
                .and_then(|cp| cp.api_key.as_deref())
                .unwrap_or("");
            let effective_key = if !api_key.is_empty() && !api_key.contains("****") {
                api_key
            } else {
                existing_key
            };
            let key_env = p["key_env"].as_str().unwrap_or("");
            let max_concurrency = p["max_concurrency"].as_u64();

            out.push_str(&format!("    {name} {{\n"));
            out.push_str(&format!("        kind : {kind}\n"));
            out.push_str(&format!("        base_url : \"{base_url}\"\n"));
            if !effective_key.is_empty() && !effective_key.contains("****") {
                out.push_str(&format!("        api_key : \"{effective_key}\"\n"));
            }
            if !key_env.is_empty() {
                out.push_str(&format!("        key_env : {key_env}\n"));
            }
            if let Some(models) = p["models"].as_array() {
                let model_strs: Vec<String> = models
                    .iter()
                    .map(|m| {
                        let id = m["id"].as_str().unwrap_or("");
                        let tier = m["tier"].as_str().unwrap_or("mid");
                        format!("{{ id : \"{id}\", tier : {tier} }}")
                    })
                    .collect();
                out.push_str(&format!("        models : [{}]\n", model_strs.join(", ")));
            }
            if let Some(mc) = max_concurrency {
                out.push_str(&format!("        max_concurrency : {mc}\n"));
            }
            out.push_str("    }\n\n");
        }
    }

    out.push_str("}\n");
    Ok(out)
}

/// Save the config string to `~/.config/autoos/ai-daemon.at`. Returns the path.
fn save_daemon_at(content: &str) -> std::io::Result<std::path::PathBuf> {
    let path = dirs::home_dir()
        .map(|h| h.join(".config/autoos/ai-daemon.at"))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home dir"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Back up the old config.
    let backup = path.with_extension("at.bak");
    if path.exists() {
        let _ = std::fs::copy(&path, &backup);
    }
    std::fs::write(&path, content)?;
    Ok(path)
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
