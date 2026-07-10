//! The Planner role — writes dependency-aware implementation plans.
//!
//! Soul simplified from AutoForge's `relay/souls/planner.md` (orchestration
//! tool semantics removed; planning discipline + plan format kept).

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/planner.md");

/// The Planner: turns goals into dependency-aware plans.
pub struct Planner;

impl Role for Planner {
    fn name(&self) -> &str {
        "planner"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Pro
    }
    fn temperature(&self) -> f64 {
        // Planning is structured; keep creativity low.
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
    fn planner_identity() {
        let p = Planner;
        assert_eq!(p.name(), "planner");
        assert!(p.system_prompt().contains("Soul of the Planner"));
        assert_eq!(p.max_turns(), 40);
    }
}
