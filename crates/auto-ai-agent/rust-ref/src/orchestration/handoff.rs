//! Handoff Document — the token-efficiency mechanism for multi-agent pipelines.
//!
//! When an agent finishes its step, its work is compressed into a structured
//! `HandoffDocument` — NOT raw chat history — which the next agent consumes.
//! This keeps context bounded as the pipeline grows.
//!
//! (Plan 008 Phase 1 — moved from musk relay/handoff.rs, generic version:
//! dropped forge-specific fields: spec_updates, generated_report,
//! arch_change_flag, checkpoint_id, run_id.)

use serde::{Deserialize, Serialize};

/// A structured handoff between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffDocument {
    /// Source role name (e.g. "architect").
    pub from: String,
    /// Target role name (e.g. "coder").
    pub to: String,
    /// One-sentence summary of the work done.
    pub summary: String,
    /// Key decisions made during this step.
    pub decisions: Vec<Decision>,
    /// Questions left open for the next agent or user.
    pub open_questions: Vec<Question>,
    /// Files or artifacts produced.
    pub work_product: Vec<WorkProduct>,
    /// Pointers to help the next agent get started.
    pub context_for_next: ContextPointers,
    /// Token usage for this step + cumulative.
    pub token_usage: TokenUsage,
}

/// A decision made during a pipeline step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub title: String,
    pub status: String, // "made" | "deferred" | "rejected"
    pub rationale: String,
}

/// An open question for the next agent or user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub text: String,
    pub assigned_to: Option<String>,
}

/// A file or artifact produced by a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkProduct {
    pub path: String,
    pub description: String,
    pub lines: Option<u32>,
}

/// Pointers to help the next agent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextPointers {
    pub files_to_read: Vec<String>,
    /// Files the previous agent wrote/edited that should be tested.
    #[serde(default)]
    pub files_to_test: Vec<String>,
    pub warnings: Vec<String>,
}

/// Token usage for a step.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub step_tokens: u64,
    pub cumulative: u64,
    pub budget_remaining: u64,
}

impl HandoffDocument {
    /// Create a new handoff document between two roles.
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            summary: String::new(),
            decisions: Vec::new(),
            open_questions: Vec::new(),
            work_product: Vec::new(),
            context_for_next: ContextPointers::default(),
            token_usage: TokenUsage::default(),
        }
    }

    /// Render as markdown for the next agent's consumption.
    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("# Handoff: {} → {}", self.from, self.to));
        lines.push(String::new());

        lines.push("## Summary".into());
        lines.push(self.summary.clone());
        lines.push(String::new());

        if !self.decisions.is_empty() {
            lines.push("## Decisions Made".into());
            for d in &self.decisions {
                lines.push(format!("- **{}**: {}", d.status, d.title));
                if !d.rationale.is_empty() {
                    lines.push(format!("  - Rationale: {}", d.rationale));
                }
            }
            lines.push(String::new());
        }

        if !self.open_questions.is_empty() {
            lines.push("## Open Questions".into());
            for q in &self.open_questions {
                lines.push(format!("- {}", q.text));
            }
            lines.push(String::new());
        }

        if !self.work_product.is_empty() {
            lines.push("## Work Product".into());
            for wp in &self.work_product {
                let size = wp.lines.map(|l| format!(" ({} lines)", l)).unwrap_or_default();
                lines.push(format!("- `{}`{}{}", wp.path, size, wp.description));
            }
            lines.push(String::new());
        }

        let ctx = &self.context_for_next;
        if !ctx.files_to_read.is_empty() || !ctx.files_to_test.is_empty() || !ctx.warnings.is_empty() {
            lines.push("## Context for Next Agent".into());
            if !ctx.files_to_read.is_empty() {
                lines.push("\n### Files to Read".into());
                for f in &ctx.files_to_read {
                    lines.push(format!("- {}", f));
                }
            }
            if !ctx.files_to_test.is_empty() {
                lines.push("\n### Files to Test".into());
                for f in &ctx.files_to_test {
                    lines.push(format!("- `{}`", f));
                }
            }
            if !ctx.warnings.is_empty() {
                lines.push("\n### Warnings".into());
                for w in &ctx.warnings {
                    lines.push(format!("- ⚠️ {}", w));
                }
            }
            lines.push(String::new());
        }

        lines.push(format!(
            "## Token Spend\n- This step: {} tokens\n- Cumulative: {} tokens\n",
            self.token_usage.step_tokens, self.token_usage.cumulative
        ));

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_summary_and_sections() {
        let mut doc = HandoffDocument::new("advisor", "architect");
        doc.summary = "Define the module layout.".into();
        doc.decisions.push(Decision {
            title: "Use SQLite".into(),
            status: "made".into(),
            rationale: "zero-config persistence".into(),
        });
        doc.work_product.push(WorkProduct {
            path: "src/db.rs".into(),
            description: " schema".into(),
            lines: Some(42),
        });
        let md = doc.render();
        assert!(md.contains("advisor → architect"));
        assert!(md.contains("Define the module layout."));
        assert!(md.contains("Use SQLite"));
        assert!(md.contains("`src/db.rs` (42 lines)"));
    }

    #[test]
    fn render_omits_empty_sections() {
        let doc = HandoffDocument::new("coder", "reviewer");
        let md = doc.render();
        assert!(md.contains("## Summary"));
        // No decisions/questions/work_product → those headers absent.
        assert!(!md.contains("## Decisions"));
        assert!(!md.contains("## Open Questions"));
        assert!(!md.contains("## Work Product"));
        assert!(!md.contains("## Context for Next Agent"));
    }

    #[test]
    fn context_pointers_render() {
        let mut doc = HandoffDocument::new("coder", "tester");
        doc.context_for_next.files_to_test.push("src/main.rs".into());
        doc.context_for_next.warnings.push("watch for panic on line 42".into());
        let md = doc.render();
        assert!(md.contains("Files to Test"));
        assert!(md.contains("`src/main.rs`"));
        assert!(md.contains("⚠️ watch for panic"));
    }

    #[test]
    fn serde_roundtrip() {
        let mut doc = HandoffDocument::new("architect", "coder");
        doc.summary = "Build it".into();
        doc.decisions.push(Decision {
            title: "Use Rust".into(),
            status: "made".into(),
            rationale: "safety".into(),
        });
        let json = serde_json::to_string(&doc).unwrap();
        let back: HandoffDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back.from, "architect");
        assert_eq!(back.to, "coder");
        assert_eq!(back.summary, "Build it");
        assert_eq!(back.decisions.len(), 1);
    }
}
