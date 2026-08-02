//! Built-in Role library.
//!
//! System prompts are ported verbatim from AutoForge's `relay/souls/*.md`
//! (the tuned "souls"). Each built-in Role embeds its prompt via
//! `include_str!` from [`../../resources/souls`] and overrides the
//! `model`/`temperature`/`max_turns` defaults with values informed by
//! AutoForge's `relay/role.rs`.
//!
//! Two roles with no Forge source — [`Translator`] (NL→command) and
//! [`Runner`] (execute/find) — are added for downstream Ash support.

pub mod advisor;
pub mod architect;
pub mod assistant;
pub mod coder;
pub mod documenter;
pub mod gofer;
pub mod planner;
pub mod reviewer;
pub mod runner;
pub mod super_advisor;
pub mod super_coder;
pub mod super_tester;
pub mod tester;
pub mod translator;

pub use advisor::Advisor;
pub use architect::Architect;
pub use assistant::Assistant;
pub use coder::Coder;
pub use documenter::Documenter;
pub use gofer::Gofer;
pub use planner::Planner;
pub use reviewer::Reviewer;
pub use runner::Runner;
pub use super_advisor::SuperAdvisor;
pub use super_coder::SuperCoder;
pub use super_tester::SuperTester;
pub use tester::Tester;
pub use translator::Translator;

use std::sync::Arc;

use crate::role_def::Role;

/// Look up a built-in Role by its lowercase name.
///
/// Returns `None` for unknown names. Phase 4's `.at` loader uses this to
/// resolve an `inherit:` base.
pub fn load_builtin(name: &str) -> Option<Arc<dyn Role>> {
    let p: Arc<dyn Role> = match name {
        "assistant" => Arc::new(Assistant),
        "coder" => Arc::new(Coder),
        "architect" => Arc::new(Architect),
        "tester" => Arc::new(Tester),
        "reviewer" => Arc::new(Reviewer),
        "documenter" => Arc::new(Documenter),
        "translator" => Arc::new(Translator),
        "runner" => Arc::new(Runner),
        "advisor" => Arc::new(Advisor),
        "planner" => Arc::new(Planner),
        "gofer" => Arc::new(Gofer),
        "super-advisor" => Arc::new(SuperAdvisor),
        "super-coder" => Arc::new(SuperCoder),
        "super-tester" => Arc::new(SuperTester),
        _ => return None,
    };
    Some(p)
}

/// All built-in role names, in a stable order.
pub fn builtin_names() -> &'static [&'static str] {
    &[
        "assistant",
        "coder",
        "architect",
        "tester",
        "reviewer",
        "documenter",
        "translator",
        "runner",
        "advisor",
        "planner",
        "gofer",
        "super-advisor",
        "super-coder",
        "super-tester",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_builtin_resolves_all() {
        for name in builtin_names() {
            assert!(load_builtin(name).is_some(), "missing builtin: {name}");
        }
    }

    #[test]
    fn load_builtin_unknown_returns_none() {
        assert!(load_builtin("nonexistent").is_none());
    }

    #[test]
    fn each_builtin_has_nonempty_prompt_and_known_marker() {
        // Each ported soul opens with a "# Soul of the <Role>" header.
        let markers = [
            ("assistant", "Soul of the Assistant"),
            ("coder", "Soul of the Coder"),
            ("architect", "Soul of the Architect"),
            ("tester", "Soul of the Tester"),
            ("reviewer", "Soul of the Reviewer"),
            ("documenter", "Soul of the Documenter"),
            ("advisor", "Soul of the Advisor"),
            ("planner", "Soul of the Planner"),
            ("gofer", "Soul of the Gofer"),
            ("super-advisor", "Soul of the Super Advisor"),
            ("super-coder", "Soul of the Super Coder"),
            ("super-tester", "Soul of the Super Tester"),
        ];
        for (name, marker) in markers {
            let p = load_builtin(name).unwrap();
            let prompt = p.system_prompt();
            assert!(
                !prompt.is_empty(),
                "{name} system_prompt is empty"
            );
            assert!(
                prompt.contains(marker),
                "{name} prompt missing marker '{marker}'"
            );
        }
    }

    #[test]
    fn each_builtin_has_sane_model_and_turns() {
        for name in builtin_names() {
            let p = load_builtin(name).unwrap();
            // Each builtin declares a tier (not a concrete model id, which is
            // empty by default and resolved by the daemon).
            let _ = p.model_tier();
            assert!(
                p.max_turns() >= 1,
                "{name} max_turns too low: {}",
                p.max_turns()
            );
            let t = p.temperature();
            assert!((0.0..=2.0).contains(&t), "{name} temperature out of range: {t}");
        }
    }
}
