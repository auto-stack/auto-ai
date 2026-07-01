//! The Coder role — writes/modifies implementation code.
//!
//! Soul ported verbatim from AutoForge's `relay/souls/coder.md`. Defaults
//! (max_turns=40, mid-tier model, moderate temperature) follow
//! AutoForge's `relay/role.rs`.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/coder.md");

/// The Coder: produces and edits implementation code.
pub struct Coder;

impl Role for Coder {
    fn name(&self) -> &str {
        "coder"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Max
    }
    fn temperature(&self) -> f64 {
        // Some creativity for code generation, but not chaotic.
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
    fn coder_identity() {
        let c = Coder;
        assert_eq!(c.name(), "coder");
        assert!(c.system_prompt().contains("Soul of the Coder"));
        assert_eq!(c.max_turns(), 40);
    }
}
