//! The Plan-Driven Developer role (PLAN-030).
//!
//! One agent carries a feature through plan → execute → review → document,
//! with the plan file as the single handoff artifact. Registered under the
//! musk profession `plan-dev`; phase behavior is injected per relay step via
//! the musk-side phase task templates (`relay/plan_flow.rs`).

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/plan-dev.md");

/// The Plan-Driven Developer: full lifecycle on top of the plan file.
pub struct PlanDev;

impl Role for PlanDev {
    fn name(&self) -> &str {
        "plan-dev"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Max
    }
    fn temperature(&self) -> f64 {
        // Four phases of reliable execution, not creativity.
        0.3
    }
    fn max_turns(&self) -> usize {
        // Full-lifecycle runs need a big budget.
        120
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_dev_identity() {
        let c = PlanDev;
        assert_eq!(c.name(), "plan-dev");
        assert!(c.system_prompt().contains("Soul of the Plan-Driven Developer"));
        assert_eq!(c.max_turns(), 120);
    }
}
