//! The Super Tester role — verifies implementation against the plan.
//!
//! Soul simplified from AutoForge's `relay/souls/super-tester.md`
//! ("route back to execute-plan" relay routing removed; verification +
//! review discipline kept). Struct is `SuperTester`; the role name keeps
//! its hyphen as `"super-tester"`.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/super-tester.md");

/// The Super Tester: the last line of defense — verifies code against plan.
pub struct SuperTester;

impl Role for SuperTester {
    fn name(&self) -> &str {
        "super-tester"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Max
    }
    fn temperature(&self) -> f64 {
        // Review must be consistent and rigorous.
        0.3
    }
    fn max_turns(&self) -> usize {
        // Thorough review of large changes.
        100
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_tester_identity() {
        let t = SuperTester;
        assert_eq!(t.name(), "super-tester");
        assert!(t.system_prompt().contains("Soul of the Super Tester"));
        assert_eq!(t.max_turns(), 100);
    }
}
