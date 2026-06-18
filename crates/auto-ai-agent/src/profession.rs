//! The [`Profession`] trait (design doc §3.1).
//!
//! A Profession bundles the *personality* of an agent: its system prompt
//! (the tuned "soul") plus model/temperature/turn-budget/tool-policy defaults.
//! Built-in Professions live in [`crate::professions`]; Phase 4 adds
//! `.at`-config-driven Professions on top.

/// A Profession describes how an agent should behave.
///
/// Only [`Profession::name`] and [`Profession::system_prompt`] are required;
/// every other method has a sensible default that an implementor overrides.
pub trait Profession: Send + Sync {
    /// Role name ("coder", "reviewer", ...).
    fn name(&self) -> &str;

    /// The full system prompt (the tuned essence of the role).
    fn system_prompt(&self) -> &str;

    /// Recommended model.
    fn model(&self) -> &str {
        "glm-4.6"
    }

    /// Generation temperature (creativity vs determinism).
    fn temperature(&self) -> f64 {
        0.3
    }

    /// Max ReAct turns (guards against infinite loops).
    fn max_turns(&self) -> usize {
        10
    }

    /// Names of tools this role may use, drawn from the app's registered set.
    /// Empty = all tools allowed.
    fn allowed_tools(&self) -> Vec<String> {
        Vec::new()
    }

    /// Optional memory constraint (e.g. keep only the last N turns).
    fn memory_limit(&self) -> Option<usize> {
        Some(20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal Profession for trait-level tests; the real library lives in
    /// `crate::professions`.
    struct StubProfession {
        prompt: String,
    }

    impl Profession for StubProfession {
        fn name(&self) -> &str {
            "stub"
        }
        fn system_prompt(&self) -> &str {
            &self.prompt
        }
    }

    #[test]
    fn profession_defaults() {
        let p = StubProfession {
            prompt: "be helpful".into(),
        };
        assert_eq!(p.name(), "stub");
        assert_eq!(p.system_prompt(), "be helpful");
        assert_eq!(p.model(), "glm-4.6");
        assert!((p.temperature() - 0.3).abs() < 1e-9);
        assert_eq!(p.max_turns(), 10);
        assert!(p.allowed_tools().is_empty());
        assert_eq!(p.memory_limit(), Some(20));
    }
}
