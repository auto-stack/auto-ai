//! The Super Coder role — executes an approved plan strictly.
//!
//! Soul simplified from AutoForge's `relay/souls/super-coder.md` ("read the
//! approved plan file" relay flow references removed; "follow the plan
//! strictly" discipline kept). Struct is `SuperCoder`; the role name keeps
//! its hyphen as `"super-coder"`.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/super-coder.md");

/// The Super Coder: executes an approved plan without redesign.
pub struct SuperCoder;

impl Role for SuperCoder {
    fn name(&self) -> &str {
        "super-coder"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Max
    }
    fn temperature(&self) -> f64 {
        // Executing a plan must be reliable, not creative.
        0.3
    }
    fn max_turns(&self) -> usize {
        // Large plans need a big budget.
        120
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_coder_identity() {
        let c = SuperCoder;
        assert_eq!(c.name(), "super-coder");
        assert!(c.system_prompt().contains("Soul of the Super Coder"));
        assert_eq!(c.max_turns(), 120);
    }
}
