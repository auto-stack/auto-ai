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
        }
    }
}

/// Parse `ai-client.at` content (root must be `client { … }`).
pub fn parse_client_config(content: &str) -> Result<ClientConfig, ConfigError> {
    let node = root_node(content, "client")?;

    let default_provider = opt_str(&node, "default_provider").unwrap_or_default();
    let default_model = opt_str(&node, "default_model").unwrap_or_default();
    let providers = parse_provider_blocks(&node);

    if providers.is_empty() {
        return Err(ConfigError::Parse(
            "no providers configured in client { } block".into(),
        ));
    }
    let default_provider = if default_provider.is_empty() {
        providers.keys().next().cloned().unwrap_or_default()
    } else {
        default_provider
    };

    Ok(ClientConfig {
        providers,
        default_provider,
        default_model,
    })
}

/// Parse `ai-daemon.at` content (root must be `daemon { … }`).
pub fn parse_daemon_config(content: &str) -> Result<DaemonConfig, ConfigError> {
    let node = root_node(content, "daemon")?;

    let mut cfg = DaemonConfig::default();
    if let Some(s) = opt_str(&node, "listen_addr") {
        cfg.listen_addr = s;
    }
    if let Some(n) = opt_uint(&node, "idle_timeout_min") {
        cfg.idle_timeout_min = n as u64;
    }
    if let Some(s) = opt_str(&node, "log_level") {
        cfg.log_level = s;
    }
    cfg.default_provider = opt_str(&node, "default_provider").unwrap_or_default();
    cfg.default_model = opt_str(&node, "default_model").unwrap_or_default();
    cfg.providers = parse_provider_blocks(&node);

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

/// Walk a node's children, turning each `name { … }` block into a
/// `ProviderConfig`.
fn parse_provider_blocks(node: &Node) -> HashMap<String, ProviderConfig> {
    let mut providers = HashMap::new();
    for (_key, kid) in node.kids_iter() {
        if let Kid::Node(child) = kid {
            let pc = ProviderConfig {
                kind: opt_str(child, "kind").unwrap_or_default(),
                base_url: opt_str(child, "base_url").unwrap_or_default(),
                api_key: opt_str(child, "api_key"),
                key_env: opt_str(child, "key_env"),
                models: opt_models(child, "models"),
                max_concurrency: opt_uint(child, "max_concurrency"),
            };
            if !pc.kind.is_empty() {
                providers.insert(child.name.to_string(), pc);
            }
        }
    }
    providers
}

fn opt_str(node: &Node, key: &str) -> Option<String> {
    match node.get_prop_of(key) {
        Value::Str(s) => Some(s.to_string()),
        Value::Nil => None,
        other => Some(other.to_astr().to_string()),
    }
}

/// Read the `models` field. Accepts either a quoted-string array
/// `["glm-4.6", "glm-flash"]` (preferred — model names contain dots) or a
/// legacy comma-separated bare string `glm-4.6,glm-flash` (only works when
/// names don't trip the number parser).
/// Read the `models` field as a list of [`ModelDefinition`]s (id + tier).
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
    match s.trim().to_ascii_lowercase().as_str() {
        "min" => ModelTier::Min,
        "lite" | "light" => ModelTier::Lite,
        "mid" => ModelTier::Mid,
        "pro" | "large" => ModelTier::Pro,
        "max" | "heavy" => ModelTier::Max,
        _ => ModelTier::Mid,
    }
}

/// Read a non-negative integer prop. auto-atom parses whole numbers as
/// `Value::Int` (i32) or `Value::Uint` (u32); accept both.
fn opt_uint(node: &Node, key: &str) -> Option<usize> {
    match node.get_prop_of(key) {
        Value::Uint(u) => Some(u as usize),
        Value::Int(i) if i >= 0 => Some(i as usize),
        Value::Nil => None,
        _ => None,
    }
}

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
