//! The Architect profession — designs structure/specs.
//!
//! Soul ported verbatim from AutoForge's `relay/souls/architect.md`. Defaults
//! (max_turns=40, max-tier model, low temperature for determinism) follow
//! AutoForge's `relay/profession.rs`.

use crate::profession::Profession;

const SOUL: &str = include_str!("../../resources/souls/architect.md");

/// The Architect: designs architecture and writes specs/section-structure.
pub struct Architect;

impl Profession for Architect {
    fn name(&self) -> &str {
        "architect"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Max
    }
    fn temperature(&self) -> f64 {
        // Structural decisions reward determinism over creativity.
        0.2
    }
    fn max_turns(&self) -> usize {
        40
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architect_identity() {
        let a = Architect;
        assert_eq!(a.name(), "architect");
        assert!(a.system_prompt().contains("Soul of the Architect"));
        assert_eq!(a.max_turns(), 40);
    }
}
