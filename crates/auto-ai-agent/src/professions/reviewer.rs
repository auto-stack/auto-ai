//! The Reviewer profession — reviews code/design, catches issues.
//!
//! Soul ported verbatim from AutoForge's `relay/souls/reviewer.md`. Defaults
//! (max_turns=50, max-tier model, low temperature for rigorous analysis)
//! follow AutoForge's `relay/profession.rs`.

use crate::profession::Profession;

const SOUL: &str = include_str!("../../resources/souls/reviewer.md");

/// The Reviewer: audits work, finds defects, and reports findings precisely.
pub struct Reviewer;

impl Profession for Reviewer {
    fn name(&self) -> &str {
        "reviewer"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model(&self) -> &str {
        "glm-4.6"
    }
    fn temperature(&self) -> f64 {
        // Reviewing rewards careful, deterministic analysis.
        0.2
    }
    fn max_turns(&self) -> usize {
        50
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewer_identity() {
        let r = Reviewer;
        assert_eq!(r.name(), "reviewer");
        assert!(r.system_prompt().contains("Soul of the Reviewer"));
        assert_eq!(r.max_turns(), 50);
    }
}
