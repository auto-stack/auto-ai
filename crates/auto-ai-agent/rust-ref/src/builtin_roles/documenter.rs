//! The Documenter role — writes docs/READMEs/explanations.
//!
//! Soul ported verbatim from AutoForge's `relay/souls/documenter.md`. Defaults
//! (max_turns=40, max-tier model, moderate temperature) follow AutoForge's
//! `relay/role.rs`.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/documenter.md");

/// The Documenter: produces documentation, READMEs, and explanations.
pub struct Documenter;

impl Role for Documenter {
    fn name(&self) -> &str {
        "documenter"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Pro
    }
    fn temperature(&self) -> f64 {
        // Documentation benefits from clear, natural prose — slight warmth.
        0.4
    }
    fn max_turns(&self) -> usize {
        40
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documenter_identity() {
        let d = Documenter;
        assert_eq!(d.name(), "documenter");
        assert!(d.system_prompt().contains("Soul of the Documenter"));
        assert_eq!(d.max_turns(), 40);
    }
}
