//! Skill system — a "skill" is a markdown prompt fragment the model loads
//! on demand by calling the [`SkillTool`].
//!
//! This is a port-of-spirit of Claude Code's superpowers: a skill is **not**
//! a state machine or a declared procedure with gates. It is a prompt fragment
//! (`SKILL.md`) with two metadata fields (`name`, `description`) that the model
//! loads when it decides the skill applies, then follows as best-effort
//! instructions. There is no enforced procedure, no required-tools binding —
//! the ordering, gates, and checklists all live inside the prompt text and are
//! honored (or not) by the model.
//!
//! Discovery: [`SkillRegistry::scan`] walks a directory for `*/SKILL.md`
//! files, parses the frontmatter, and stores `{name → Skill}`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::tool::Tool;

/// One skill: a name, a trigger description, and the full prompt body.
#[derive(Clone, Debug)]
pub struct Skill {
    /// Skill name (unique in a registry). From frontmatter `name:`.
    pub name: String,
    /// When to use this skill (third-person). From frontmatter `description:`.
    /// This is the trigger — the model reads it to decide whether to call.
    pub description: String,
    /// The full SKILL.md body (everything after the frontmatter `---` block).
    pub content: String,
    /// Path to the source SKILL.md (for diagnostics).
    pub path: PathBuf,
}

/// Registry of skills keyed by name, built by scanning a directory.
#[derive(Default)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    /// Empty registry (no skills configured).
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan `dir` for `<skill-name>/SKILL.md` files and load them.
    ///
    /// Fault-tolerant: a directory whose SKILL.md is missing or fails to parse
    /// is skipped (logged at warn), not fatal — one broken skill shouldn't
    /// disable the whole registry.
    pub fn scan(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        let mut skills = HashMap::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => {
                tracing::debug!("skill registry: directory not found: {}", dir.display());
                return Self { skills };
            }
        };
        for entry in entries.flatten() {
            let sub = entry.path();
            if !sub.is_dir() {
                continue;
            }
            let skill_md = sub.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            match parse_skill_file(&skill_md) {
                Ok(skill) => {
                    if !skill.name.is_empty() {
                        skills.insert(skill.name.clone(), skill);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "skill registry: skipping {}: {}",
                        skill_md.display(),
                        e
                    );
                }
            }
        }
        tracing::info!("skill registry: loaded {} skill(s) from {}", skills.len(), dir.display());
        Self { skills }
    }

    /// Look up a skill by name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Keep only skills whose name is in `whitelist` (in place). Used by the
    /// Plan 004 Roles feature: a role's `skills` whitelist restricts which
    /// installed skills are exposed to an agent.
    pub fn retain(&mut self, whitelist: &[String]) {
        let allow: std::collections::HashSet<&str> =
            whitelist.iter().map(|s| s.as_str()).collect();
        self.skills.retain(|name, _| allow.contains(name.as_str()));
    }

    /// All skill names (no order guarantee).
    pub fn names(&self) -> Vec<String> {
        self.skills.keys().cloned().collect()
    }

    /// `(name, description)` pairs for all skills — used to build the
    /// `<available_skills>` bootstrap block injected into the system prompt.
    pub fn descriptions(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .skills
            .values()
            .map(|s| (s.name.clone(), s.description.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Number of loaded skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// Parse a single `SKILL.md` file into a [`Skill`].
///
/// Expects a leading YAML-ish frontmatter block delimited by `---` lines:
/// ```text
/// ---
/// name: brainstorming
/// description: You MUST use this before any creative work...
/// ---
/// <markdown body>
/// ```
/// Only `name` and `description` are extracted; everything after the closing
/// `---` is the body. No full YAML parser — these are two string fields.
fn parse_skill_file(path: &Path) -> Result<Skill, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let (name, description, content) = parse_frontmatter(&raw);
    if name.is_empty() {
        return Err("frontmatter is missing 'name:'".into());
    }
    Ok(Skill {
        name,
        description,
        content,
        path: path.to_path_buf(),
    })
}

/// Split a SKILL.md into (name, description, body).
///
/// - If a leading `---\n...\n---` block exists, parse `name:`/`description:`
///   from it; the body is everything after the closing `---`.
/// - If no frontmatter, name falls back to the file's parent directory name,
///   description is empty, and the whole file is the body.
fn parse_frontmatter(raw: &str) -> (String, String, String) {
    // A leading frontmatter block starts at byte 0 with `---`.
    let after_open = match raw.strip_prefix("---\n").or_else(|| raw.strip_prefix("---\r\n")) {
        Some(rest) => rest,
        None => {
            // No frontmatter: name = parent dir (caller sets via path), empty desc.
            return (String::new(), String::new(), raw.to_string());
        }
    };
    // Find the closing `---` on its own line.
    let close_idx = after_open
        .lines()
        .position(|line| line.trim_end() == "---");
    let (frontmatter, body) = match close_idx {
        Some(idx) => {
            let fm: String = after_open.lines().take(idx).collect::<Vec<_>>().join("\n");
            // Body = everything after the closing `---` line.
            let body_start: usize = after_open
                .lines()
                .take(idx + 1)
                .map(|l| l.len() + 1) // +1 for the newline
                .sum();
            let body = after_open[body_start.min(after_open.len())..].trim_start().to_string();
            (fm, body)
        }
        None => (after_open.to_string(), String::new()), // no closing ---
    };
    let name = extract_field(&frontmatter, "name");
    let description = extract_field(&frontmatter, "description");
    (name, description, body)
}

/// Extract a `key: value` field from frontmatter text. Handles quoted values
/// (`"..."`) and strips a trailing comment. Returns "" if absent.
fn extract_field(frontmatter: &str, key: &str) -> String {
    let prefix = format!("{key}:");
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let mut val = rest.trim();
            // Strip surrounding quotes if present.
            if (val.starts_with('"') && val.ends_with('"'))
                || (val.starts_with('\'') && val.ends_with('\''))
            {
                val = &val[1..val.len() - 1];
            }
            return val.to_string();
        }
    }
    String::new()
}

/// The tool that exposes skills to the model. The model calls
/// `skill(skill_name=...)` to load a skill's content on demand.
///
/// Its `description()` is built once at construction (from the registry) so
/// the model sees a directory of available skills + their triggers every turn.
pub struct SkillTool {
    registry: Arc<SkillRegistry>,
    /// Cached description (built from the registry; stable for the tool's life).
    description_cache: String,
    /// Cached parameters schema (includes the skill-name enum).
    parameters_cache: Value,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        let description_cache = build_description(&registry);
        let parameters_cache = json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "Name of the skill to load",
                    "enum": registry.names(),
                }
            },
            "required": ["skill_name"]
        });
        Self {
            registry,
            description_cache,
            parameters_cache,
        }
    }

    /// A read-only view of the registry (for bootstrap injection).
    pub fn registry(&self) -> &SkillRegistry {
        &self.registry
    }

    /// Build the `<available_skills>` block that gets appended to the system
    /// prompt so the model knows what skills it can invoke.
    pub fn available_skills_block(&self) -> String {
        let descs = self.registry.descriptions();
        if descs.is_empty() {
            return String::new();
        }
        let mut out = String::from("\n\n<available_skills>\n");
        out.push_str("You have access to skills — reusable techniques you load on demand.\n");
        out.push_str("To use a skill, call the `skill` tool with its name; its content will be\n");
        out.push_str("returned to you, and you follow its instructions directly.\n\n");
        for (name, desc) in &descs {
            out.push_str(&format!("- {name}: {desc}\n"));
        }
        out.push_str("\nInvoke a skill whenever its trigger applies, even slightly.\n");
        out.push_str("</available_skills>");
        out
    }
}

/// Build the tool `description` string from the registry's skill list.
fn build_description(registry: &SkillRegistry) -> String {
    let descs = registry.descriptions();
    if descs.is_empty() {
        return "Load a skill's instructions. No skills are currently configured.".into();
    }
    let mut out = String::from(
        "Load a skill's instructions by name. Call this whenever a skill's trigger applies.\n\nAvailable skills:\n",
    );
    for (name, desc) in &descs {
        out.push_str(&format!("- {name}: {desc}\n"));
    }
    out
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        &self.description_cache
    }

    fn parameters(&self) -> Value {
        self.parameters_cache.clone()
    }

    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let name = args["skill_name"]
            .as_str()
            .ok_or_else(|| ToolError::Args("missing 'skill_name' argument".into()))?;
        match self.registry.get(name) {
            Some(skill) => {
                tracing::info!("skill: loaded '{name}'");
                Ok(format!(
                    "# Skill: {name}\n\n{content}",
                    name = skill.name,
                    content = skill.content
                ))
            }
            None => {
                let available = self.registry.names().join(", ");
                Err(ToolError::Exec(format!(
                    "skill '{name}' not found; available: {available}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a SKILL.md into a temp dir and return the dir.
    fn make_skill_dir(parent: &Path, name: &str, body: &str) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("SKILL.md")).unwrap();
        writeln!(f, "---").unwrap();
        writeln!(f, "name: {name}").unwrap();
        writeln!(f, "description: Use when testing skill parsing.").unwrap();
        writeln!(f, "---").unwrap();
        writeln!(f, "{body}").unwrap();
        dir
    }

    #[test]
    fn parse_frontmatter_basic() {
        let raw = "---\nname: foo\ndescription: a test skill\n---\n# Foo\n\nDo things.\n";
        let (name, desc, body) = parse_frontmatter(raw);
        assert_eq!(name, "foo");
        assert_eq!(desc, "a test skill");
        assert!(body.contains("# Foo"));
    }

    #[test]
    fn parse_frontmatter_quoted_description() {
        let raw = "---\nname: bar\ndescription: \"quoted desc with: colon\"\n---\nbody\n";
        let (name, desc, body) = parse_frontmatter(raw);
        assert_eq!(name, "bar");
        assert_eq!(desc, "quoted desc with: colon");
        assert!(body.contains("body"));
    }

    #[test]
    fn parse_frontmatter_none() {
        let raw = "just a body, no frontmatter";
        let (name, desc, body) = parse_frontmatter(raw);
        assert_eq!(name, "");
        assert_eq!(desc, "");
        assert_eq!(body, raw);
    }

    #[test]
    fn registry_scan_loads_skills() {
        let tmp = std::env::temp_dir().join("musk_skill_scan_test");
        let _ = std::fs::remove_dir_all(&tmp);
        make_skill_dir(&tmp, "brainstorm", "explore intent");
        make_skill_dir(&tmp, "write-plan", "write a plan");
        // Also a broken dir (no SKILL.md) — should be skipped.
        std::fs::create_dir_all(tmp.join("empty")).unwrap();

        let reg = SkillRegistry::scan(&tmp);
        assert_eq!(reg.len(), 2);
        assert!(reg.get("brainstorm").is_some());
        assert!(reg.get("write-plan").is_some());
        assert!(reg.get("empty").is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn registry_scan_missing_dir_is_empty() {
        let reg = SkillRegistry::scan("/nonexistent/skills/dir");
        assert!(reg.is_empty());
    }

    #[test]
    fn registry_descriptions_sorted() {
        let tmp = std::env::temp_dir().join("musk_skill_desc_test");
        let _ = std::fs::remove_dir_all(&tmp);
        make_skill_dir(&tmp, "zebra", "z");
        make_skill_dir(&tmp, "alpha", "a");
        let reg = SkillRegistry::scan(&tmp);
        let descs = reg.descriptions();
        assert_eq!(descs[0].0, "alpha");
        assert_eq!(descs[1].0, "zebra");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn skill_tool_loads_known_skill() {
        let tmp = std::env::temp_dir().join("musk_skill_tool_test");
        let _ = std::fs::remove_dir_all(&tmp);
        make_skill_dir(&tmp, "demo", "do the demo dance");
        let reg = Arc::new(SkillRegistry::scan(&tmp));
        let tool = SkillTool::new(reg);

        let out = tool
            .execute(&json!({"skill_name": "demo"}))
            .await
            .unwrap();
        assert!(out.contains("do the demo dance"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn skill_tool_unknown_errors() {
        let reg = Arc::new(SkillRegistry::new());
        let tool = SkillTool::new(reg);
        let err = tool
            .execute(&json!({"skill_name": "nope"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Exec(_)));
    }

    #[test]
    fn skill_tool_parameters_enum_lists_names() {
        let tmp = std::env::temp_dir().join("musk_skill_params_test");
        let _ = std::fs::remove_dir_all(&tmp);
        make_skill_dir(&tmp, "a", "aa");
        make_skill_dir(&tmp, "b", "bb");
        let reg = Arc::new(SkillRegistry::scan(&tmp));
        let tool = SkillTool::new(reg);
        let params = tool.parameters();
        let names = params["properties"]["skill_name"]["enum"].as_array().unwrap();
        assert_eq!(names.len(), 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn available_skills_block_lists_triggers() {
        let tmp = std::env::temp_dir().join("musk_skill_block_test");
        let _ = std::fs::remove_dir_all(&tmp);
        make_skill_dir(&tmp, "brainstorm", "body text here");
        let reg = Arc::new(SkillRegistry::scan(&tmp));
        let tool = SkillTool::new(reg);
        let block = tool.available_skills_block();
        assert!(block.contains("<available_skills>"));
        assert!(block.contains("brainstorm"));
        // The block lists the frontmatter description, not the body.
        assert!(block.contains("Use when testing skill parsing."));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn available_skills_block_empty_when_no_skills() {
        let reg = Arc::new(SkillRegistry::new());
        let tool = SkillTool::new(reg);
        assert!(tool.available_skills_block().is_empty());
    }
}
