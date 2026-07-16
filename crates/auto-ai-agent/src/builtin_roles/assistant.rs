//! The Assistant role — the user's first point of contact.
//!
//! Handles direct conversation, answers questions, and performs lightweight
//! tasks (read/search/check). Complex orchestration (handoff/relay) is the app
//! layer's job. Soul adapted from AutoForge's `relay/souls/assistant.md`,
//! stripped of orchestration semantics.

use crate::role_def::Role;

const SOUL: &str = include_str!("../../resources/souls/assistant.md");

/// The Assistant: the conversational entry point. Answers questions directly
/// and performs light tasks; defers complex work to the app's orchestration.
pub struct Assistant;

impl Role for Assistant {
    fn name(&self) -> &str {
        "assistant"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model(&self) -> &str {
        // If the env var ASSISTANT_MODEL is set, use it as a concrete model id.
        // This lets users override the assistant's model without code changes:
        //   set ASSISTANT_MODEL=ornith    (use Ollama's ornith)
        //   set ASSISTANT_MODEL=glm-5.2   (use Zhipu)
        // Empty (default) = use tier resolution via the daemon.
        match std::env::var("ASSISTANT_MODEL") {
            Ok(m) if !m.is_empty() => {
                // Leak to get 'static — this is called infrequently and the
                // string is small. Acceptable for a CLI tool.
                // (A proper solution would use OnceLock, but this is simpler.)
                Box::leak(m.into_boxed_str())
            }
            _ => "",
        }
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
        // 20 turns: enough for exploring files + answering. The assistant is a
        // general-purpose chat role — it needs more room than a narrowly-focused
        // specialist. Previous value (12) was too tight for "read files then answer"
        // workflows, causing premature max-turns exhaustion.
        20
    }
    fn allowed_tools(&self) -> Vec<String> {
        // As the default chat entry point, the assistant gets the full tool set
        // (empty = no filtering). The mode's tool whitelist already constrains
        // which tools are registered; the role shouldn't further restrict them.
        Vec::new()
    }
    /// Assistant is the triage entry point — hands off to coder, architect,
    /// or reviewer depending on the task.
    fn handoff_to(&self) -> Vec<String> {
        vec!["coder".into(), "architect".into(), "reviewer".into()]
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
        assert_eq!(a.max_turns(), 20);
    }
}
