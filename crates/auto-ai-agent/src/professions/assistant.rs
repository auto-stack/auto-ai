//! The Assistant profession — the user's first point of contact.
//!
//! Handles direct conversation, answers questions, and performs lightweight
//! tasks (read/search/check). Complex orchestration (handoff/relay) is the app
//! layer's job. Soul adapted from AutoForge's `relay/souls/assistant.md`,
//! stripped of orchestration semantics.

use crate::profession::Profession;

const SOUL: &str = include_str!("../../resources/souls/assistant.md");

/// The Assistant: the conversational entry point. Answers questions directly
/// and performs light tasks; defers complex work to the app's orchestration.
pub struct Assistant;

impl Profession for Assistant {
    fn name(&self) -> &str {
        "assistant"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        // Mid-tier is enough for triage + direct answers; the app can upgrade
        // via a mode's tier override if needed.
        ai_config::ModelTier::Mid
    }
    fn temperature(&self) -> f64 {
        0.3
    }
    fn max_turns(&self) -> usize {
        12
    }
    fn allowed_tools(&self) -> Vec<String> {
        // Lightweight tool set — the assistant reads/searches/checks but
        // doesn't write by default. The app's mode config can expand this.
        vec![
            "read_file".into(),
            "search".into(),
            "list_dir".into(),
            "run_command".into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_identity() {
        let a = Assistant;
        assert_eq!(a.name(), "assistant");
        assert!(a.system_prompt().contains("Soul of the Assistant"));
        assert_eq!(a.max_turns(), 12);
    }
}
