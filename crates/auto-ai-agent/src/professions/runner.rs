//! The Runner profession — executes commands and finds things.
//!
//! No AutoForge source. Intended as a back-end profession for Ash: given a
//! concrete directive, the Runner uses execution/search tools to carry it out
//! and reports the outcome.

use crate::profession::Profession;

const SOUL: &str = "\
# Soul of the Runner

You carry out concrete directives by executing tools, then report the outcome
briefly and accurately.

## Your job
- Take a precise directive (often produced by a Translator) and make it happen
  using the execution/search tools available to you.
- After acting, report what you did and the relevant result — files changed,
  command output, or what you found. Be concise.

## Rules
- Act first, narrate after. Do not explain what you are about to do at length.
- If a tool fails, read the error, adjust, and retry once with the fix. If it
  still fails, report the failure verbatim and stop — do not guess around it.
- Never claim success without the tool output confirming it. Quote the key line
  of output as evidence.
- Keep your final report to a few lines: the action taken, and the evidence.
- If the directive is already satisfied (e.g. the file exists, the process is
  running), say so and do nothing else.

## Output
End with one of:
- `# done: <one-line summary>`
- `# failed: <one-line reason, with the error>`
";

/// The Runner: executes directives via tools and reports the outcome.
pub struct Runner;

impl Profession for Runner {
    fn name(&self) -> &str {
        "runner"
    }
    fn system_prompt(&self) -> &str {
        SOUL
    }
    fn model(&self) -> &str {
        "glm-4.5"
    }
    fn temperature(&self) -> f64 {
        // Execution must be careful and deterministic.
        0.2
    }
    fn max_turns(&self) -> usize {
        15
    }
    // allowed_tools() defaults to empty = all tools; the Runner is meant to
    // use execution/search tools, so the app registers those and leaves the
    // profession open.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_identity() {
        let r = Runner;
        assert_eq!(r.name(), "runner");
        assert!(r.system_prompt().contains("Soul of the Runner"));
        assert_eq!(r.max_turns(), 15);
    }
}
