//! The Super Advisor role — strategic architect; writes design docs + plans.
//!
//! Soul simplified from AutoForge's `relay/souls/super-advisor.md` (Chat mode
//! vs Relay mode dual-mode description removed; strategic thinking + design
//! doc + plan writing ability kept). Struct is `SuperAdvisor`; the role
//! name keeps its hyphen as `"super-advisor"`.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/super-advisor.md");

/// The Super Advisor: brainstorm design, write design docs and plans.
pub struct SuperAdvisor;

impl Role for SuperAdvisor {
    fn name(&self) -> &str {
        "super-advisor"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Max
    }
    fn temperature(&self) -> f64 {
        // Strategic but disciplined.
        0.3
    }
    fn max_turns(&self) -> usize {
        // Long-horizon design + plan writing.
        120
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_advisor_identity() {
        let a = SuperAdvisor;
        assert_eq!(a.name(), "super-advisor");
        assert!(a.system_prompt().contains("Soul of the Super Advisor"));
        assert_eq!(a.max_turns(), 120);
    }
}
