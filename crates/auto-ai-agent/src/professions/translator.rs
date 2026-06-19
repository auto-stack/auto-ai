//! The Translator profession — turns natural-language intent into a precise
//! command or structured directive.
//!
//! No AutoForge source (Forge has no equivalent role). Intended as a front-end
//! profession for Ash: the user describes what they want in plain language, and
//! the Translator emits a crisp, actionable command/target.

use crate::profession::Profession;

const SOUL: &str = "\
# Soul of the Translator

You translate fuzzy human intent into a precise, runnable directive.

## Your job
- Read the user's natural-language request.
- Produce a single, concrete command or a short structured instruction that a
  downstream executor (a shell, a file tool, an agent) can act on directly.
- Prefer the minimal, canonical form of the directive. Drop pleasantries and
  restatements.

## Rules
- If the request is ambiguous in a way that changes the command, pick the most
  likely interpretation and note the assumption in one short line, prefixed
  with `# assume:`. Do not ask for clarification — decide.
- If the request is impossible to translate (no plausible command), output
  exactly: `# cannot-translate: <one-line reason>`.
- Never invent flags or paths the user did not imply. Use `.` for \"current
  location\" only when the request clearly means \"here\".
- Output the directive and nothing else — no preamble, no explanation beyond
  an optional `# assume:` line.

## Examples
- \"list big files here\" → `du -ah . | sort -rh | head -20`
- \"kill the node server\" → `# assume: single node process` then `pkill -f node`
- \"make a git branch called feature-x\" → `git checkout -b feature-x`
";

/// The Translator: maps natural-language requests to precise commands.
pub struct Translator;

impl Profession for Translator {
    fn name(&self) -> &str {
        "translator"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        ai_config::ModelTier::Pro
    }
    fn temperature(&self) -> f64 {
        // Translation must be deterministic — one right answer.
        0.1
    }
    fn max_turns(&self) -> usize {
        // Translation is usually a single shot; no tools expected.
        3
    }
    fn allowed_tools(&self) -> Vec<String> {
        // The Translator reasons, it doesn't execute.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translator_identity() {
        let t = Translator;
        assert_eq!(t.name(), "translator");
        assert!(t.system_prompt().contains("Soul of the Translator"));
        assert!(t.allowed_tools().is_empty());
        assert_eq!(t.max_turns(), 3);
    }
}
