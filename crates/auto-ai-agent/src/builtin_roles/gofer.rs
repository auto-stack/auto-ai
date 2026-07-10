//! The Gofer role — researches and reports facts concisely.
//!
//! Soul simplified from AutoForge's `relay/souls/gofer.md` (orchestration
//! tool semantics removed; fact-gathering discipline kept).

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/gofer.md");

/// The Gofer: gathers facts and reports them, nothing more.
pub struct Gofer;

impl Role for Gofer {
    fn name(&self) -> &str {
        "gofer"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Lite
    }
    fn temperature(&self) -> f64 {
        // Facts only — be deterministic.
        0.1
    }
    fn max_turns(&self) -> usize {
        // Short errands; stop early when the answer is found.
        20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gofer_identity() {
        let g = Gofer;
        assert_eq!(g.name(), "gofer");
        assert!(g.system_prompt().contains("Soul of the Gofer"));
        assert_eq!(g.max_turns(), 20);
    }
}
