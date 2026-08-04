//! `.at` configuration loading for client and daemon, via the shared
//! [`auto_atom`] parser.
//!
//! Two file shapes, both **single-root** (auto-atom parses exactly one root
//! value, so the legacy flat format is wrapped in a root node):
//!
//! `ai-client.at`:
//! ```text
//! client {
//!     default_provider : zhipu
//!     default_model : "glm-4.6"
//!     zhipu {
//!         kind : openai
//!         base_url : "https://open.bigmodel.cn/api/paas/v4"
//!         key_env : ZHIPU_API_KEY
//!         models : [
//!             { id : "glm-5.2", tier : max },
//!             { id : "glm-4.6", tier : mid }
//!         ]
//!     }
//! }
//! ```
//!
//! `ai-daemon.at` — same, rooted in `daemon { … }`, plus daemon-only fields
//! (`listen_addr`, `idle_timeout_min`, `log_level`, and `max_concurrency`
//! inside each provider block).
//!
//! Note: model names are quoted (`default_model` and inside the `models`
//! array) because they often contain dots/dashes (e.g. `glm-4.6`) that
//! auto-atom would otherwise try to parse as a number literal.

use std::collections::HashMap;

use auto_atom::{Atom, AtomParser};
use auto_val::{Kid, Node, Value};
use serde::Deserialize;

use crate::provider::ProviderConfig;
use crate::tier::{ModelDefinition, ModelTier};

/// Configuration error.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config parse error: {0}")]
    Parse(String),
}

/// Client-side view: provider registry + defaults.
#[derive(Clone, Debug, Default)]
pub struct ClientConfig {
    pub providers: HashMap<String, ProviderConfig>,
    pub default_provider: String,
    pub default_model: String,
}

/// Daemon-side view: client config + daemon-only operational fields.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub listen_addr: String,
    pub idle_timeout_min: u64,
    pub log_level: String,
    pub providers: HashMap<String, ProviderConfig>,
    pub default_provider: String,
    pub default_model: String,
    /// Explicit tier routing table. When non-empty, overrides auto-derivation.
    pub tier_routing: TierRouting,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:17654".into(),
            idle_timeout_min: 10,
            log_level: "info".into(),
            providers: HashMap::new(),
            default_provider: String::new(),
            default_model: String::new(),
            tier_routing: TierRouting::default(),
        }
    }
}

// ── serde deserialization views (Plan 381) ──────────────────────────────────
// These read the scalar props of a config node in one call, replacing the
// hand-written opt_str/opt_uint/opt_bool helpers. Complex structures (providers
// keyed by child-node name, tier_routing nested block, models' 3 shapes) stay
// hand-written below — only the simple scalar fields migrate.

#[derive(Debug, Deserialize)]
struct ClientScalars {
    #[serde(default)] default_provider: String,
    #[serde(default)] default_model: String,
}

#[derive(Debug, Default, Deserialize)]
struct DaemonScalars {
    #[serde(default)] listen_addr: Option<String>,
    #[serde(default)] idle_timeout_min: Option<u64>,
    #[serde(default)] log_level: Option<String>,
    #[serde(default)] default_provider: Option<String>,
    #[serde(default)] default_model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProviderScalars {
    #[serde(default)] kind: Option<String>,
    #[serde(default)] base_url: Option<String>,
    #[serde(default)] api_key: Option<String>,
    #[serde(default)] key_env: Option<String>,
    #[serde(default)] max_concurrency: Option<usize>,
    /// auth_required accepts bool / 0,1 / "yes","on",… (loader.rs opt_bool).
    #[serde(default, deserialize_with = "auto_val::lenient_bool_opt")]
    auth_required: Option<bool>,
}

/// Parse `ai-client.at` content (root must be `client { … }`).
pub fn parse_client_config(content: &str) -> Result<ClientConfig, ConfigError> {
    let node = root_node(content, "client")?;

    // Top-level scalar fields via serde (Plan 381). Providers stay hand-written
    // (they're child nodes keyed by node name — dynamic, not a fixed struct).
    let s: ClientScalars = node
        .deserialize()
        .map_err(|e| ConfigError::Parse(format!("client.at: {e}")))?;
    let providers = parse_provider_blocks(&node);

    if providers.is_empty() {
        return Err(ConfigError::Parse(
            "no providers configured in client { } block".into(),
        ));
    }
    let default_provider = if s.default_provider.is_empty() {
        providers.keys().next().cloned().unwrap_or_default()
    } else {
        s.default_provider
    };

    Ok(ClientConfig {
        providers,
        default_provider,
        default_model: s.default_model,
    })
}

/// A single tier routing candidate: provider + model.
#[derive(Clone, Debug)]
pub struct TierRouteCandidate {
    pub provider: String,
    pub model: String,
}

/// Tier routing table: tier name → ordered candidate list (primary first).
/// When present in the config, overrides auto-derivation.
#[derive(Clone, Debug, Default)]
pub struct TierRouting {
    pub routes: HashMap<String, Vec<TierRouteCandidate>>,
}

impl TierRouting {
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn candidates(&self, tier: &str) -> Option<&[TierRouteCandidate]> {
        self.routes.get(tier).map(|v| v.as_slice())
    }
}

/// Parse `daemon { … }` content (root must be `daemon { … }`).
pub fn parse_daemon_config(content: &str) -> Result<DaemonConfig, ConfigError> {
    let node = root_node(content, "daemon")?;

    // Top-level scalar fields via serde (Plan 381). Providers and tier_routing
    // stay hand-written (child-node-keyed / nested-block structures).
    let s: DaemonScalars = node
        .deserialize()
        .map_err(|e| ConfigError::Parse(format!("daemon.at: {e}")))?;
    let mut cfg = DaemonConfig {
        listen_addr: s.listen_addr.unwrap_or_default(),
        idle_timeout_min: s.idle_timeout_min.unwrap_or(0),
        log_level: s.log_level.unwrap_or_default(),
        default_provider: s.default_provider.unwrap_or_default(),
        default_model: s.default_model.unwrap_or_default(),
        ..DaemonConfig::default()
    };
    cfg.providers = parse_provider_blocks(&node);
    cfg.tier_routing = parse_tier_routing(&node);

    if cfg.default_provider.is_empty() && !cfg.providers.is_empty() {
        cfg.default_provider = cfg.providers.keys().next().cloned().unwrap_or_default();
    }
    if cfg.default_model.is_empty() {
        cfg.default_model = cfg
            .providers
            .get(&cfg.default_provider)
            .and_then(|p| p.models.first().map(|m| m.id.clone()))
            .unwrap_or_default();
    }

    Ok(cfg)
}

/// Parse the single root node and assert its name is `expected`.
fn root_node(content: &str, expected: &str) -> Result<Node, ConfigError> {
    let atom = AtomParser::parse(content)
        .map_err(|e| ConfigError::Parse(format!("{expected}.at: {e}")))?;
    match atom {
        Atom::Node(n) if n.name.as_str() == expected => Ok(n),
        Atom::Node(n) => Err(ConfigError::Parse(format!(
            "expected a '{expected}' root block, found '{}'",
            n.name
        ))),
        other => Err(ConfigError::Parse(format!(
            "expected a '{expected}' root node, found {other:?}"
        ))),
    }
}

/// Parse the `tier_routing { … }` child block from a daemon config.
///
/// Format:
/// ```text
/// tier_routing {
///     max : [{ provider : "zhipu", model : "glm-5.2" }, { provider : "deepseek", model : "v4-pro" }]
///     mid : [{ provider : "zhipu", model : "glm-5-turbo" }]
/// }
/// ```
fn parse_tier_routing(node: &Node) -> TierRouting {
    let mut routing = TierRouting::default();

    // Find the tier_routing child node.
    for (_key, kid) in node.kids_iter() {
        if let Kid::Node(tr_node) = kid {
            if tr_node.name.as_str() != "tier_routing" {
                continue;
            }
            // Each prop is a tier name → array of candidates.
            for (tier_key, tier_val) in tr_node.props_iter() {
                let tier_name = tier_key.to_string();
                if let Value::Array(arr) = tier_val {
                    let candidates: Vec<TierRouteCandidate> = arr.values.iter()
                        .filter_map(|v| {
                            if let Value::Obj(o) = v {
                                let provider = o.get("provider")
                                    .and_then(|val| match val { Value::Str(s) => Some(s.to_string()), _ => None })?;
                                let model = o.get("model")
                                    .and_then(|val| match val { Value::Str(s) => Some(s.to_string()), _ => None })?;
                                Some(TierRouteCandidate { provider, model })
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !candidates.is_empty() {
                        routing.routes.insert(tier_name, candidates);
                    }
                }
            }
        }
    }

    routing
}

/// Walk a node's children, turning each `name { … }` block into a
/// `ProviderConfig`. The per-provider scalar fields are deserialized via serde
/// (Plan 381); `models` stays hand-written (`opt_models` handles 3 input shapes).
fn parse_provider_blocks(node: &Node) -> HashMap<String, ProviderConfig> {
    let mut providers = HashMap::new();
    for (_key, kid) in node.kids_iter() {
        if let Kid::Node(child) = kid {
            let s: ProviderScalars = match child.deserialize() {
                Ok(s) => s,
                Err(_) => continue, // a non-provider child node; skip
            };
            let pc = ProviderConfig {
                kind: s.kind.unwrap_or_default(),
                base_url: s.base_url.unwrap_or_default(),
                api_key: s.api_key,
                key_env: s.key_env,
                models: opt_models(child, "models"),
                max_concurrency: s.max_concurrency,
                auth_required: s.auth_required.unwrap_or(true),
            };
            if !pc.kind.is_empty() {
                providers.insert(child.name.to_string(), pc);
            }
        }
    }
    providers
}

/// Read the `models` field as a list of [`ModelDefinition`]s (id + tier).
/// (Stays hand-written: accepts 3 input shapes — Obj array, bare-Str array,
/// and legacy comma-separated string — plus lenient tier parsing. Too
/// specialized for a generic deserialize_with helper.)
///
/// Accepted shapes (each element of the `models` array):
/// - `Obj { id: "glm-5.2", name: "...", tier: max }` — full, preferred.
/// - `Str "glm-5.2"` — bare model id, defaults to `ModelTier::Mid` (callers
///   who don't care about tiers get a sane default).
fn opt_models(node: &Node, key: &str) -> Vec<ModelDefinition> {
    use crate::tier::ModelTier;
    match node.get_prop_of(key) {
        Value::Array(arr) => arr
            .values
            .iter()
            .filter_map(|v| match v {
                // object: { id: "...", name: "...", tier: <tier> }
                Value::Obj(o) => {
                    let id = match o.get("id") {
                        Some(Value::Str(s)) => s.to_string(),
                        Some(other) => other.to_astr().to_string(),
                        None => return None,
                    };
                    let name = match o.get("name") {
                        Some(Value::Str(s)) => s.to_string(),
                        Some(other) => other.to_astr().to_string(),
                        None => String::new(),
                    };
                    let tier = match o.get("tier") {
                        Some(Value::Str(s)) => parse_tier(s.as_str()),
                        _ => ModelTier::Mid,
                    };
                    Some(ModelDefinition { id, name, tier })
                }
                // bare string: "glm-5.2" → Mid default
                Value::Str(s) => Some(ModelDefinition::new(s.to_string(), ModelTier::Mid)),
                _ => None,
            })
            .collect(),
        // legacy: comma-separated string
        Value::Str(s) => s
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .map(|m| ModelDefinition::new(m, ModelTier::Mid))
            .collect(),
        Value::Nil => Vec::new(),
        _ => Vec::new(),
    }
}

/// Parse a tier name → ModelTier. Accepts snake_case ("max", "mid"), display
/// ("Max", "Mid"), and auto-forge aliases ("large"=Pro, "heavy"=Max).
/// Unknown → Mid (sane default).
fn parse_tier(s: &str) -> ModelTier {
    use crate::tier::ModelTier;
    // Lenient parse: unknown names default to Mid (config-file compatibility).
    ModelTier::parse_name(s).unwrap_or_default()
}

/// Read the `models` field as a list of [`ModelDefinition`]s (id + tier).
/// (Stays hand-written: accepts 3 input shapes — Obj array, bare-Str array,
/// and legacy comma-separated string — plus lenient tier parsing. Too
/// specialized for a generic deserialize_with helper.)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_config_example() {
        let src = r#"
            client {
                default_provider : zhipu
                default_model : "glm-4.6"

                zhipu {
                    kind : openai
                    base_url : "https://open.bigmodel.cn/api/paas/v4"
                    key_env : ZHIPU_API_KEY
                    models : [
                        { id : "glm-5.2", tier : max },
                        { id : "glm-4.6", tier : mid }
                    ]
                }
            }
        "#;
        let cfg = parse_client_config(src).unwrap();
        assert_eq!(cfg.default_provider, "zhipu");
        assert_eq!(cfg.default_model, "glm-4.6");
        let zhipu = cfg.providers.get("zhipu").unwrap();
        assert_eq!(zhipu.kind, "openai");
        assert_eq!(zhipu.models.len(), 2);
        assert_eq!(zhipu.models[0].id, "glm-5.2");
        assert_eq!(zhipu.models[0].tier, ModelTier::Max);
        assert_eq!(zhipu.models[1].id, "glm-4.6");
        assert_eq!(zhipu.models[1].tier, ModelTier::Mid);
        assert_eq!(zhipu.key_env.as_deref(), Some("ZHIPU_API_KEY"));
        assert_eq!(zhipu.max_concurrency, None); // client view: unset
    }

    #[test]
    fn parse_daemon_config_example() {
        let src = r#"
            daemon {
                listen_addr : "127.0.0.1:9999"
                idle_timeout_min : 30
                log_level : debug
                default_provider : zhipu
                default_model : "glm-4.6"

                zhipu {
                    kind : openai
                    base_url : "https://open.bigmodel.cn/api/paas/v4"
                    api_key : "test-key"
                    models : ["glm-4.6", "glm-flash"]
                    max_concurrency : 4
                }
            }
        "#;
        let cfg = parse_daemon_config(src).unwrap();
        assert_eq!(cfg.listen_addr, "127.0.0.1:9999");
        assert_eq!(cfg.idle_timeout_min, 30);
        assert_eq!(cfg.log_level, "debug");
        let zhipu = cfg.providers.get("zhipu").unwrap();
        assert_eq!(zhipu.max_concurrency, Some(4));
        assert_eq!(zhipu.api_key.as_deref(), Some("test-key"));
    }

    #[test]
    fn parse_client_rejects_wrong_root() {
        let src = "daemon { }";
        let err = parse_client_config(src).unwrap_err();
        assert!(err.to_string().contains("client"));
    }

    #[test]
    fn parse_daemon_rejects_wrong_root() {
        let src = "client { }";
        let err = parse_daemon_config(src).unwrap_err();
        assert!(err.to_string().contains("daemon"));
    }

    #[test]
    fn parse_client_defaults_provider_when_unset() {
        let src = r#"
            client {
                anthropic { kind : anthropic, models : ["claude-3-5-sonnet"] }
            }
        "#;
        let cfg = parse_client_config(src).unwrap();
        // default_provider falls back to the first (only) provider.
        assert_eq!(cfg.default_provider, "anthropic");
    }

    #[test]
    fn parse_daemon_defaults_model_from_provider() {
        let src = r#"
            daemon {
                zhipu { kind : openai, models : ["glm-4.6", "glm-flash"] }
            }
        "#;
        let cfg = parse_daemon_config(src).unwrap();
        // default_model falls back to the provider's first model.
        assert_eq!(cfg.default_model, "glm-4.6");
    }

    #[test]
    fn parse_client_errors_when_no_providers() {
        let src = "client { default_provider : none }";
        let err = parse_client_config(src).unwrap_err();
        assert!(err.to_string().contains("no providers"));
    }

    #[test]
    fn parse_multiple_providers() {
        let src = r#"
            client {
                default_provider : anthropic
                anthropic {
                    kind : anthropic
                    base_url : "https://api.anthropic.com"
                    key_env : ANTHROPIC_API_KEY
                    models : ["claude-3-5-sonnet"]
                }
                zhipu {
                    kind : openai
                    base_url : "https://open.bigmodel.cn/api/paas/v4"
                    key_env : ZHIPU_API_KEY
                    models : ["glm-4.6"]
                }
            }
        "#;
        let cfg = parse_client_config(src).unwrap();
        assert_eq!(cfg.providers.len(), 2);
        assert!(cfg.providers.contains_key("anthropic"));
        assert!(cfg.providers.contains_key("zhipu"));
    }
}
