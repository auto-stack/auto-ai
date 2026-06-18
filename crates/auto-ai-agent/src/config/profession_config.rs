//! Parsing + inherit/merge for `.at` Profession config files.

use std::sync::Arc;

use auto_atom::{Atom, AtomParser};
use auto_val::Value;

use crate::error::AgentError;
use crate::profession::Profession;
use crate::professions::load_builtin;

/// The parsed representation of a `profession { … }` block.
///
/// Every field is optional so an `inherit`-based config only overrides what it
/// sets. See `docs/auto-ai-agent-design.md` §4.4.
///
/// # Tool policy
/// `tools` *replaces* the inherited tool set. `tools_append` *adds to* it.
/// (The design doc sketched a `+name` prefix syntax; that isn't parseable by
/// auto-atom because `+` isn't an identifier character, so the policy is split
/// into two props instead.)
#[derive(Clone, Debug, Default)]
pub struct ProfessionConfig {
    pub name: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_turns: Option<usize>,
    pub system_prompt: Option<String>,
    pub system_prompt_append: Option<String>,
    /// When set, the profession may use *only* these tools (replaces base).
    pub tools: Option<Vec<String>>,
    /// When set, these tools are *added* to the (possibly inherited) set.
    pub tools_append: Option<Vec<String>>,
    pub inherit: Option<String>,
    pub memory_limit: Option<usize>,
}

impl ProfessionConfig {
    /// Merge `self` *over* `base`, applying the design-doc §4.4 rules:
    /// - scalar fields override when `Some`;
    /// - `system_prompt_append` is appended to the base prompt;
    /// - `tools` with any `+name` entry append to the base set; a plain list
    ///   (no `+` prefix anywhere) replaces the base set entirely.
    pub fn merge_over(mut self, mut base: ProfessionConfig) -> ProfessionConfig {
        // Scalars: override when Some.
        base.name = self.name.take().or(base.name);
        base.model = self.model.take().or(base.model);
        base.temperature = self.temperature.take().or(base.temperature);
        base.max_turns = self.max_turns.take().or(base.max_turns);
        base.system_prompt = self.system_prompt.take().or(base.system_prompt);
        base.memory_limit = self.memory_limit.take().or(base.memory_limit);
        base.inherit = self.inherit.take().or(base.inherit);

        // system_prompt_append accumulates (base append, then self append).
        if let Some(extra) = base.system_prompt_append.take() {
            self.system_prompt_append = Some(match self.system_prompt_append.take() {
                Some(mine) => format!("{extra}\n{mine}"),
                None => extra,
            });
        }
        base.system_prompt_append = self.system_prompt_append.take();

        // tools: `tools` replaces; `tools_append` extends.
        if let Some(replace) = self.tools.take() {
            base.tools = Some(replace);
        }
        if let Some(extra) = self.tools_append.take() {
            let mut combined = base.tools.unwrap_or_default();
            combined.extend(extra);
            base.tools = Some(combined);
        }

        base
    }
}

/// Parse a single `profession { … }` block from `.at` source.
pub fn parse_at_profession(content: &str) -> Result<ProfessionConfig, AgentError> {
    let atom = AtomParser::parse(content).map_err(|e| {
        AgentError::Config(format!("failed to parse profession .at: {e}"))
    })?;

    let node = match atom {
        Atom::Node(n) if n.name.as_str() == "profession" => n,
        Atom::Node(n) => {
            return Err(AgentError::Config(format!(
                "expected a 'profession' block, found '{}'",
                n.name
            )))
        }
        other => {
            return Err(AgentError::Config(format!(
                "expected a 'profession' node, found {:?}",
                other
            )))
        }
    };

    let mut cfg = ProfessionConfig::default();
    cfg.name = opt_string(&node, "name");
    cfg.model = opt_string(&node, "model");
    cfg.temperature = opt_float(&node, "temperature");
    cfg.max_turns = opt_uint(&node, "max_turns");
    cfg.memory_limit = opt_uint(&node, "memory_limit").map(|u| u as usize);
    cfg.system_prompt = opt_string(&node, "system_prompt");
    cfg.system_prompt_append = opt_string(&node, "system_prompt_append");
    cfg.inherit = opt_string(&node, "inherit");
    cfg.tools = opt_string_list(&node, "tools");
    cfg.tools_append = opt_string_list(&node, "tools_append");

    Ok(cfg)
}

// ── small Value readers (the auto-atom navigation pattern) ──────────────────
//
// Every reader treats Value::Nil (the "prop absent" sentinel from
// `Node::get_prop_of`) as None.

fn opt_string(node: &auto_val::Node, key: &str) -> Option<String> {
    match node.get_prop_of(key) {
        Value::Str(s) => Some(s.to_string()),
        Value::Nil => None,
        other => Some(other.to_astr().to_string()),
    }
}

/// Read a float prop. auto-atom parses decimals as `Value::Double`, so we must
/// match both `Float` and `Double` (the design-doc's `get_float_or` only
/// matches `Float` — a footgun noted in the Explore report).
fn opt_float(node: &auto_val::Node, key: &str) -> Option<f64> {
    match node.get_prop_of(key) {
        Value::Double(f) | Value::Float(f) => Some(f),
        Value::Int(i) => Some(i as f64),
        Value::Uint(u) => Some(u as f64),
        Value::Nil => None,
        _ => None,
    }
}

fn opt_uint(node: &auto_val::Node, key: &str) -> Option<usize> {
    match node.get_prop_of(key) {
        Value::Uint(u) => Some(u as usize),
        Value::Int(i) if i >= 0 => Some(i as usize),
        Value::Nil => None,
        _ => None,
    }
}

fn opt_string_list(node: &auto_val::Node, key: &str) -> Option<Vec<String>> {
    match node.get_prop_of(key) {
        Value::Array(arr) => {
            let items: Vec<String> = arr
                .values
                .iter()
                .map(|v| match v {
                    Value::Str(s) => s.to_string(),
                    other => other.to_astr().to_string(),
                })
                .collect();
            Some(items)
        }
        Value::Str(s) => Some(vec![s.to_string()]),
        Value::Nil => None,
        _ => None,
    }
}

// ── ConfigProfession: a Profession built from a merged config ───────────────

/// A [`Profession`] whose behavior comes from a (possibly merged)
/// [`ProfessionConfig`].
pub struct ConfigProfession {
    cfg: ProfessionConfig,
    /// The system prompt, with any `system_prompt_append` folded in. If this
    /// profession `inherit`s a builtin, this is the builtin's prompt plus the
    /// append.
    prompt: String,
}

impl ConfigProfession {
    /// Build directly from a fully-merged config. `prompt` must already have
    /// any `system_prompt_append` applied (see [`load_profession`]).
    pub(crate) fn new(cfg: ProfessionConfig, prompt: String) -> Self {
        Self { cfg, prompt }
    }
}

impl Profession for ConfigProfession {
    fn name(&self) -> &str {
        self.cfg.name.as_deref().unwrap_or("config")
    }
    fn system_prompt(&self) -> &str {
        &self.prompt
    }
    fn model(&self) -> &str {
        self.cfg.model.as_deref().unwrap_or("glm-4.5")
    }
    fn temperature(&self) -> f64 {
        self.cfg.temperature.unwrap_or(0.3)
    }
    fn max_turns(&self) -> usize {
        self.cfg.max_turns.unwrap_or(10)
    }
    fn allowed_tools(&self) -> Vec<String> {
        self.cfg.tools.clone().unwrap_or_default()
    }
    fn memory_limit(&self) -> Option<usize> {
        self.cfg.memory_limit.or(Some(20))
    }
}

/// Load a Profession from `.at` source text, resolving `inherit` against the
/// built-in library.
pub fn load_profession(content: &str) -> Result<Arc<dyn Profession>, AgentError> {
    let cfg = parse_at_profession(content)?;

    if let Some(base_name) = &cfg.inherit {
        let base_builtin =
            load_builtin(base_name).ok_or_else(|| {
                AgentError::Config(format!(
                    "inherit: builtin '{}' not found",
                    base_name
                ))
            })?;

        // Merge config-over-builtin.
        let merged = cfg;
        let mut prompt = base_builtin.system_prompt().to_string();
        if let Some(extra) = &merged.system_prompt_append {
            prompt.push('\n');
            prompt.push_str(extra);
        }
        // Tools: `tools` replaces the builtin's set; `tools_append` extends it.
        let tools = match (&merged.tools, &merged.tools_append) {
            (Some(replace), _) => Some(replace.clone()),
            (None, Some(extra)) => {
                let mut combined = base_builtin.allowed_tools();
                combined.extend(extra.iter().cloned());
                Some(combined)
            }
            (None, None) => {
                if base_builtin.allowed_tools().is_empty() {
                    None
                } else {
                    Some(base_builtin.allowed_tools())
                }
            }
        };

        // Build a merged config carrying the resolved prompt + tools, and let
        // the other scalars (model/temperature/max_turns/memory_limit) fall
        // through to ConfigProfession's defaults when None — but those defaults
        // should be the *builtin's* values, not the generic ones. Override the
        // None fields with the builtin's values.
        let resolved = ProfessionConfig {
            name: Some(merged.name.clone().unwrap_or_else(|| base_builtin.name().to_string())),
            model: Some(merged.model.clone().unwrap_or_else(|| base_builtin.model().to_string())),
            temperature: Some(
                merged.temperature.unwrap_or_else(|| base_builtin.temperature()),
            ),
            max_turns: Some(merged.max_turns.unwrap_or_else(|| base_builtin.max_turns())),
            system_prompt: None, // carried by `prompt`
            system_prompt_append: None,
            tools,
            tools_append: None,
            inherit: None,
            memory_limit: Some(
                merged.memory_limit.unwrap_or_else(|| {
                    base_builtin.memory_limit().unwrap_or(20)
                }),
            ),
        };

        Ok(Arc::new(ConfigProfession::new(resolved, prompt)))
    } else {
        // No inherit: the config must supply its own system_prompt.
        let prompt = match &cfg.system_prompt {
            Some(p) => {
                let mut p = p.clone();
                if let Some(extra) = &cfg.system_prompt_append {
                    p.push('\n');
                    p.push_str(extra);
                }
                p
            }
            None => {
                return Err(AgentError::Config(
                    "profession config without 'inherit' must set 'system_prompt'".into(),
                ))
            }
        };
        Ok(Arc::new(ConfigProfession::new(cfg, prompt)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pure_config_profession() {
        let src = r#"
            profession {
                name : "my-coder"
                model : "glm-4.5"
                temperature : 0.25
                max_turns : 12
                system_prompt : "you write code"
                tools : [read_file, write_file]
            }
        "#;
        let cfg = parse_at_profession(src).unwrap();
        assert_eq!(cfg.name.as_deref(), Some("my-coder"));
        assert_eq!(cfg.model.as_deref(), Some("glm-4.5"));
        assert!((cfg.temperature.unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(cfg.max_turns, Some(12));
        assert_eq!(cfg.system_prompt.as_deref(), Some("you write code"));
        assert_eq!(cfg.tools, Some(vec!["read_file".to_string(), "write_file".to_string()]));
        assert!(cfg.inherit.is_none());
    }

    #[test]
    fn parse_rejects_non_profession_root() {
        let src = "workflow { name : \"x\" }";
        let err = parse_at_profession(src).unwrap_err();
        assert!(matches!(err, AgentError::Config(_)));
    }

    #[test]
    fn load_pure_config_builds_profession() {
        let src = r#"
            profession {
                name : "p"
                model : "glm-4.5"
                temperature : 0.4
                max_turns : 7
                system_prompt : "be precise"
                tools : [a, b]
            }
        "#;
        let p = load_profession(src).unwrap();
        assert_eq!(p.name(), "p");
        assert_eq!(p.system_prompt(), "be precise");
        assert_eq!(p.model(), "glm-4.5");
        assert!((p.temperature() - 0.4).abs() < 1e-9);
        assert_eq!(p.max_turns(), 7);
        assert_eq!(p.allowed_tools(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn load_pure_config_without_prompt_errors() {
        let src = r#"profession { name : "p" }"#;
        let err = load_profession(src).err().unwrap();
        assert!(matches!(err, AgentError::Config(_)));
    }

    #[test]
    fn load_inherit_overrides_model_and_temperature() {
        // Coder defaults: temperature 0.3, max_turns 40.
        let src = r#"
            profession {
                name : "precise-coder"
                inherit : "coder"
                model : "glm-4.5-air"
                temperature : 0.1
                max_turns : 5
            }
        "#;
        let p = load_profession(src).unwrap();
        assert_eq!(p.name(), "precise-coder");
        assert_eq!(p.model(), "glm-4.5-air"); // overridden
        assert!((p.temperature() - 0.1).abs() < 1e-9); // overridden
        assert_eq!(p.max_turns(), 5); // overridden
        // Prompt inherited from coder (not empty).
        assert!(p.system_prompt().contains("Soul of the Coder"));
    }

    #[test]
    fn load_inherit_keeps_unset_fields_from_builtin() {
        let src = r#"
            profession {
                name : "x"
                inherit : "coder"
            }
        "#;
        let p = load_profession(src).unwrap();
        // Nothing overridden → coder's values shine through.
        assert_eq!(p.max_turns(), 40);
        assert!((p.temperature() - 0.3).abs() < 1e-9);
        assert!(p.system_prompt().contains("Soul of the Coder"));
    }

    #[test]
    fn load_inherit_system_prompt_append() {
        let src = r#"
            profession {
                name : "x"
                inherit : "coder"
                system_prompt_append : "ALWAYS add a doc comment."
            }
        "#;
        let p = load_profession(src).unwrap();
        let prompt = p.system_prompt();
        assert!(prompt.contains("Soul of the Coder"));
        assert!(prompt.contains("ALWAYS add a doc comment."));
        // The append comes after the base.
        assert!(prompt.find("Soul of the Coder") < prompt.find("ALWAYS add a doc comment."));
    }

    #[test]
    fn load_inherit_append_tool() {
        // Coder has no explicit allowed_tools (empty = all). With tools_append
        // we add to an empty base.
        let src = r#"
            profession {
                name : "x"
                inherit : "coder"
                tools_append : [custom_tool]
            }
        "#;
        let p = load_profession(src).unwrap();
        assert!(p.allowed_tools().contains(&"custom_tool".to_string()));
    }

    #[test]
    fn load_inherit_replace_tool() {
        // Plain tool list (no +) replaces.
        let src = r#"
            profession {
                name : "x"
                inherit : "coder"
                tools : [only_this]
            }
        "#;
        let p = load_profession(src).unwrap();
        assert_eq!(p.allowed_tools(), vec!["only_this".to_string()]);
    }

    #[test]
    fn load_inherit_unknown_builtin_errors() {
        let src = r#"profession { name : "x", inherit : "nope" }"#;
        let err = load_profession(src).err().unwrap();
        assert!(matches!(err, AgentError::Config(_)));
    }

    #[test]
    fn merge_over_append_tools() {
        let base = ProfessionConfig {
            tools: Some(vec!["a".into(), "b".into()]),
            ..Default::default()
        };
        let over = ProfessionConfig {
            tools_append: Some(vec!["c".into()]),
            ..Default::default()
        };
        let merged = over.merge_over(base);
        assert_eq!(
            merged.tools.unwrap(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn merge_over_replace_tools() {
        let base = ProfessionConfig {
            tools: Some(vec!["a".into()]),
            ..Default::default()
        };
        let over = ProfessionConfig {
            tools: Some(vec!["x".into(), "y".into()]),
            ..Default::default()
        };
        let merged = over.merge_over(base);
        assert_eq!(merged.tools.unwrap(), vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn merge_over_accumulates_prompt_append() {
        let base = ProfessionConfig {
            system_prompt_append: Some("base-extra".into()),
            ..Default::default()
        };
        let over = ProfessionConfig {
            system_prompt_append: Some("mine-extra".into()),
            ..Default::default()
        };
        let merged = over.merge_over(base);
        let append = merged.system_prompt_append.unwrap();
        assert!(append.contains("base-extra"));
        assert!(append.contains("mine-extra"));
        assert!(append.find("base-extra") < append.find("mine-extra"));
    }
}
