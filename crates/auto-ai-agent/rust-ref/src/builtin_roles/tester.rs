//! The Tester role — writes/runs tests, verifies behavior.
//!
//! Soul ported verbatim from AutoForge's `relay/souls/tester.md`. Defaults
//! (max_turns=40, pro-tier model, moderate temperature) follow AutoForge's
//! `relay/role.rs`.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/tester.md");

/// The Tester: produces tests, runs them, and reports failures precisely.
pub struct Tester;

impl Role for Tester {
    fn name(&self) -> &str {
        "tester"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Mid
    }
    fn temperature(&self) -> f64 {
        // Tests need some creativity to find edge cases, but must stay precise.
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
    fn tester_identity() {
        let t = Tester;
        assert_eq!(t.name(), "tester");
        assert!(t.system_prompt().contains("Soul of the Tester"));
        assert_eq!(t.max_turns(), 40);
    }
}
