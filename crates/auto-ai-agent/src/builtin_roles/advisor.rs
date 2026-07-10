//! The Advisor role — gathers requirements and writes goals.
//!
//! Soul simplified from AutoForge's `relay/souls/advisor.md` (orchestration
//! tool semantics removed; personality + goal-writing discipline kept).
//! Defaults follow the task spec.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/advisor.md");

/// The Advisor: questions requirements, writes goals.
pub struct Advisor;

impl Role for Advisor {
    fn name(&self) -> &str {
        "advisor"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Max
    }
    fn temperature(&self) -> f64 {
        // Thoughtful but precise questioning.
        0.3
    }
    fn max_turns(&self) -> usize {
        40
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisor_identity() {
        let a = Advisor;
        assert_eq!(a.name(), "advisor");
        assert!(a.system_prompt().contains("Soul of the Advisor"));
        assert_eq!(a.max_turns(), 40);
    }
}
