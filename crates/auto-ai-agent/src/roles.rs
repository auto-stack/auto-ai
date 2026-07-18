//! Agent Role registry — user-configurable, persistable roles that generalize
//! the compiled-in `Role` library.
//!
//! A Role is the same idea as a built-in Role (soul prompt + tier +
//! tools + temperature + …) but **editable at runtime**: roles live as
//! `role { … }` blocks in `~/.config/autoos/roles/<name>.at`, with an optional
//! sidecar Soul markdown file (`<name>.soul.md`). Built-in professions act as
//! read-only defaults and as `inherit:` bases for user roles.
//!
//! (Plan 004 — Agent Roles.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ai_config::ModelTier;

use crate::config::{parse_at_role, serialize_at_role, RoleConfig};
use crate::error::AgentError;
use crate::role_def::Role;
use crate::builtin_roles::{builtin_names, load_builtin};

/// Directory holding user roles: `~/.config/autoos/roles/`.
fn roles_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/autoos/roles"))
}

/// A role's `.at` file path: `<roles_dir>/<name>.at`.
fn role_at_path(name: &str) -> Option<PathBuf> {
    roles_dir().map(|d| d.join(format!("{name}.at")))
}

/// A role's sidecar Soul markdown path: `<roles_dir>/<name>.soul.md`.
fn role_soul_path(name: &str) -> Option<PathBuf> {
    roles_dir().map(|d| d.join(format!("{name}.soul.md")))
}

// ── summary / detail: what the API/UI consumes ───────────────────────────────

/// A flat summary of a role, for listing. Built-in roles are flagged so the UI
/// can render them read-only.
#[derive(Clone, Debug)]
pub struct RoleSummary {
    pub name: String,
    pub description: String,
    pub tier: ModelTier,
    pub allowed_tiers: Vec<ModelTier>,
    pub skills: Vec<String>,
    pub token_budget: Option<u64>,
    pub is_builtin: bool,
}

/// Full detail of a single role, including the Soul markdown body (loaded from
/// the sidecar file when present, else the inline `system_prompt`).
#[derive(Clone, Debug)]
pub struct RoleDetail {
    pub summary: RoleSummary,
    /// The Soul / system-prompt markdown in full.
    pub soul: String,
    /// Whether the Soul came from a sidecar `.soul.md` (vs inline).
    pub soul_from_file: bool,
    /// The raw parsed config (for the editor to bind to form fields).
    pub config: RoleConfig,
}

// ── registry ─────────────────────────────────────────────────────────────────

/// Registry of roles: built-in professions (read-only) + user `.at` roles
/// (which may override a same-named built-in). Mirrors `ModeRegistry`'s shape.
#[derive(Default)]
pub struct RoleRegistry {
    /// name -> detail. Built-ins are marked `is_builtin = true`.
    roles: HashMap<String, RoleDetail>,
}

impl RoleRegistry {
    /// Load all roles: built-in professions first, then user `.at` files
    /// (which override same-named built-ins). Single-role parse failures are
    /// warned + skipped — never fatal (mirrors ModeRegistry).
    pub fn load() -> Self {
        let mut roles = HashMap::new();

        // 1. Built-in professions (compiled-in defaults, read-only).
        for name in builtin_names() {
            if let Some(prof) = load_builtin(&name) {
                let cfg = profession_to_config(prof.as_ref());
                let summary = RoleSummary {
                    name: prof.name().to_string(),
                    description: String::new(),
                    tier: prof.model_tier(),
                    allowed_tiers: prof.allowed_tiers(),
                    skills: prof.skills(),
                    token_budget: prof.token_budget(),
                    is_builtin: true,
                };
                let detail = RoleDetail {
                    summary,
                    soul: prof.system_prompt().to_string(),
                    soul_from_file: false,
                    config: cfg,
                };
                roles.insert(name.to_string(), detail);
            }
        }

        // 2. User roles from ~/.config/autoos/roles/*.at (override built-ins).
        if let Some(dir) = roles_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("at") {
                        continue;
                    }
                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("role: failed to read {}: {e}", path.display());
                            continue;
                        }
                    };
                    match parse_at_role(&content) {
                        Ok(cfg) => {
                            let name = cfg
                                .name
                                .clone()
                                .unwrap_or_else(|| {
                                    path.file_stem()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("unnamed")
                                        .to_string()
                                });
                            // Resolve the Soul: sidecar file if present, else
                            // inline system_prompt (else inherit/builtin keeps
                            // whatever the merge resolved).
                            let (soul, soul_from_file) = resolve_soul(&cfg, &path);
                            let summary = RoleSummary {
                                name: name.clone(),
                                description: cfg.description.clone().unwrap_or_default(),
                                tier: cfg.model_tier.unwrap_or(ModelTier::Mid),
                                allowed_tiers: cfg.allowed_tiers.clone().unwrap_or_default(),
                                skills: cfg.skills.clone().unwrap_or_default(),
                                token_budget: cfg.token_budget,
                                is_builtin: false,
                            };
                            tracing::info!("role: loaded '{name}' from {}", path.display());
                            roles.insert(
                                name,
                                RoleDetail {
                                    summary,
                                    soul,
                                    soul_from_file,
                                    config: cfg,
                                },
                            );
                        }
                        Err(e) => {
                            tracing::warn!("role: failed to parse {}: {e}", path.display());
                        }
                    }
                }
            }
        }

        tracing::info!("role registry: {} role(s) loaded", roles.len());
        Self { roles }
    }

    /// Resolve a role by name into a live `Role` (for agent building).
    /// User-defined roles (`.at` files) take precedence over compiled-in
    /// built-ins with the same name, so `~/.config/autoos/roles/assistant.at`
    /// can override the built-in assistant role's `model_tier` and other fields.
    pub fn resolve_role(&self, name: &str) -> Option<std::sync::Arc<dyn Role>> {
        // 1. User override (`.at` file) takes precedence — even when it shares
        //    the same name as a built-in. `load_role` resolves the `inherit:`
        //    chain internally so the override gets the built-in's prompt/etc.
        if let Some(detail) = self.roles.get(name) {
            if !detail.summary.is_builtin {
                let src = serialize_at_role(&detail.config);
                if let Ok(role) = crate::config::load_role(&src) {
                    return Some(role);
                }
                // Parse failure: warn and fall through to built-in.
                tracing::warn!("role: failed to load user role '{name}', falling back to built-in");
            }
        }
        // 2. No user override → compiled built-in.
        load_builtin(name)
    }

    /// List all role summaries (built-in + user), sorted: user roles first
    /// (alphabetical), then built-ins (alphabetical).
    pub fn list(&self) -> Vec<RoleSummary> {
        let mut summaries: Vec<RoleSummary> = self.roles.values().map(|d| d.summary.clone()).collect();
        summaries.sort_by(|a, b| {
            // user roles first
            b.is_builtin
                .cmp(&a.is_builtin)
                .then_with(|| a.name.cmp(&b.name))
        });
        summaries
    }

    /// Full detail for one role.
    pub fn get(&self, name: &str) -> Option<&RoleDetail> {
        self.roles.get(name)
    }

    /// Save (create or overwrite) a user role. Writes the `.at` and, when
    /// `soul_md` is `Some`, the sidecar `.soul.md`. Built-in roles cannot be
    /// overwritten — returns an error.
    pub fn save(
        &self,
        name: &str,
        mut cfg: RoleConfig,
        soul_md: Option<&str>,
    ) -> Result<(), AgentError> {
        if load_builtin(name).is_some() {
            return Err(AgentError::Config(format!(
                "cannot overwrite built-in role '{name}'; choose a different name or use inherit"
            )));
        }
        // Ensure the name field is set so the file is self-describing.
        cfg.name = Some(name.to_string());

        let dir = roles_dir().ok_or_else(|| {
            AgentError::Config("could not determine home directory for roles".into())
        })?;
        std::fs::create_dir_all(&dir).map_err(|e| {
            AgentError::Config(format!("failed to create roles dir {}: {e}", dir.display()))
        })?;

        // Write the sidecar Soul when provided (and link it from the .at).
        if let Some(md) = soul_md {
            let soul_path = dir.join(format!("{name}.soul.md"));
            std::fs::write(&soul_path, md).map_err(|e| {
                AgentError::Config(format!("failed to write {}: {e}", soul_path.display()))
            })?;
            cfg.soul_file = Some(format!("{name}.soul.md"));
        }

        // Write the .at.
        let at_path = dir.join(format!("{name}.at"));
        let src = serialize_at_role(&cfg);
        std::fs::write(&at_path, src).map_err(|e| {
            AgentError::Config(format!("failed to write {}: {e}", at_path.display()))
        })?;

        tracing::info!("role: saved '{name}' to {}", at_path.display());
        Ok(())
    }

    /// Delete a user role (.at + sidecar .soul.md). Built-in roles cannot be
    /// deleted — returns an error.
    pub fn delete(&self, name: &str) -> Result<(), AgentError> {
        if load_builtin(name).is_some() {
            return Err(AgentError::Config(format!(
                "cannot delete built-in role '{name}'"
            )));
        }
        let at_path = role_at_path(name).ok_or_else(|| {
            AgentError::Config("could not determine home directory".into())
        })?;
        let existed = at_path.exists();
        if existed {
            std::fs::remove_file(&at_path).map_err(|e| {
                AgentError::Config(format!("failed to delete {}: {e}", at_path.display()))
            })?;
        }
        // Best-effort soul removal.
        if let Some(p) = role_soul_path(name) {
            let _ = std::fs::remove_file(p);
        }
        if !existed {
            return Err(AgentError::Config(format!("role '{name}' not found")));
        }
        tracing::info!("role: deleted '{name}'");
        Ok(())
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Read a built-in `Role`'s trait methods back into a config struct, so
/// the UI/API can present built-ins uniformly alongside user roles.
fn profession_to_config(prof: &dyn Role) -> RoleConfig {
    RoleConfig {
        name: Some(prof.name().to_string()),
        description: None,
        model: Some(prof.model().to_string()),
        model_tier: Some(prof.model_tier()),
        temperature: Some(prof.temperature()),
        max_turns: Some(prof.max_turns()),
        system_prompt: Some(prof.system_prompt().to_string()),
        system_prompt_append: None,
        tools: {
            let t = prof.allowed_tools();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        },
        tools_append: None,
        inherit: None,
        memory_limit: prof.memory_limit(),
        allowed_tiers: {
            let t = prof.allowed_tiers();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        },
        skills: {
            let s = prof.skills();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        },
        token_budget: prof.token_budget(),
        soul_file: None,
    }
}

/// Resolve a role's Soul markdown: sidecar file first (if `soul_file` set and
/// exists), else the inline `system_prompt`, else empty.
/// Returns (markdown, came_from_file).
fn resolve_soul(cfg: &RoleConfig, at_path: &Path) -> (String, bool) {
    if let Some(rel) = &cfg.soul_file {
        // sidecar path is relative to the .at's directory
        let sidecar = at_path
            .parent()
            .map(|d| d.join(rel))
            .unwrap_or_else(|| PathBuf::from(rel));
        if let Ok(md) = std::fs::read_to_string(&sidecar) {
            return (md, true);
        } else {
            tracing::warn!(
                "role: soul_file '{}' not found at {}, falling back to inline prompt",
                rel,
                sidecar.display()
            );
        }
    }
    (
        cfg.system_prompt.clone().unwrap_or_default(),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_loads_builtins() {
        // The registry should always surface the compiled-in professions as
        // read-only roles (their exact count is 7 today).
        let reg = RoleRegistry::load();
        let summaries = reg.list();
        let builtins: Vec<_> = summaries.iter().filter(|s| s.is_builtin).collect();
        assert!(!builtins.is_empty(), "built-in roles should be present");
        // coder is one of the canonical built-ins.
        assert!(builtins.iter().any(|s| s.name == "coder"));
        // Built-ins must be flagged read-only.
        assert!(builtins.iter().all(|s| s.is_builtin));
    }

    #[test]
    fn resolve_builtin_profession() {
        let reg = RoleRegistry::load();
        let prof = reg.resolve_role("coder");
        assert!(prof.is_some(), "coder should resolve to a built-in Role");
        assert_eq!(prof.unwrap().name(), "coder");
    }

    #[test]
    fn save_rejects_builtin_name() {
        let reg = RoleRegistry::load();
        let cfg = RoleConfig {
            name: Some("coder".into()),
            ..Default::default()
        };
        let err = reg.save("coder", cfg, None);
        assert!(err.is_err(), "overwriting a built-in must be rejected");
    }

    #[test]
    fn delete_rejects_builtin_name() {
        let reg = RoleRegistry::load();
        let err = reg.delete("coder");
        assert!(err.is_err(), "deleting a built-in must be rejected");
    }
}
