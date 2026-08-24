//! Context compaction (Plan 028, pi parity): when a conversation nears the
//! context window, replace the oldest turns with a structured LLM summary
//! instead of dropping them (the ring-buffer trim loses them forever).
//!
//! Ported from pi's `agent/src/harness/compaction/compaction.ts`
//! (shouldCompact / findCutPoint / estimateContextTokens), adapted to our
//! Memory model: cut points must land on human-turn boundaries so
//! ToolUse/ToolResult pairs are never split (same rule as `Memory::trim`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use auto_ai_client::{ClientError, CompletionRequest, ContentBlock, Message, Usage};

use crate::agent::Client;

use crate::error::AgentError;
use crate::memory::Memory;

/// Compaction thresholds. Defaults follow pi's DEFAULT_COMPACTION_SETTINGS.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactionSettings {
    /// The model's context window in tokens (from model metadata or assumed).
    pub context_window: usize,
    /// Keep this many tokens of headroom for the answer (pi: 16384).
    pub reserve_tokens: usize,
    /// Never compact away the most recent tokens (pi: 20000).
    pub keep_recent_tokens: usize,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            context_window: 128_000,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

/// True when the estimated context has crossed the compaction threshold.
pub fn should_compact(tokens: usize, s: &CompactionSettings) -> bool {
    tokens > s.context_window.saturating_sub(s.reserve_tokens)
}

/// Rough token count of the conversation: the most recent assistant usage
/// (real accounting, when available) plus a chars/4 estimate of the message
/// list (pi's estimateContextTokens heuristic).
pub fn estimate_tokens(messages: &[Message], last_usage: Option<&Usage>) -> usize {
    let base = last_usage.map(|u| u.total_tokens() as usize).unwrap_or(0);
    base + messages.iter().map(|m| message_cost(m) / 4).sum::<usize>()
}

fn message_cost(m: &Message) -> usize {
    let mut chars = m.role.len();
    for b in &m.content {
        chars += match b {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
            ContentBlock::ToolResult { content, .. } => content.len(),
        };
    }
    chars
}

/// A human turn boundary: the start of a user message whose first block is
/// plain text (not a ToolResult — those belong to the previous assistant
/// turn's unit). Index 0 is never a boundary (there must be something to cut).
fn is_turn_boundary(m: &Message) -> bool {
    m.role == "user"
        && matches!(m.content.first(), Some(ContentBlock::Text { .. }))
}

/// The index at which to cut so the kept tail is ≤ keep_recent_tokens,
/// adjusted FORWARD to the nearest human-turn boundary (never splits a
/// ToolUse/ToolResult pair). None when the memory is too short to compact.
pub fn find_cut_point(messages: &[Message], keep_recent_tokens: usize) -> Option<usize> {
    if messages.len() < 2 {
        return None;
    }
    // Smallest tail that still fits the keep-recent budget.
    let mut acc = 0usize;
    let mut start = messages.len();
    while start > 0 {
        start -= 1;
        acc += message_cost(&messages[start]) / 4;
        if acc > keep_recent_tokens {
            start += 1; // this one overflowed — tail starts after it
            break;
        }
    }
    if start == 0 {
        return None; // even the whole conversation fits — nothing to cut
    }
    // Walk forward to the nearest turn boundary (keeps the tail ≤ budget).
    let mut cut = start;
    while cut < messages.len() && !is_turn_boundary(&messages[cut]) {
        cut += 1;
    }
    if cut >= messages.len() {
        // No boundary ahead (mid-turn tail start): fall back to the previous
        // boundary — a slightly-over-budget tail is safer than refusing to
        // compact and letting the ring-buffer trim delete turns outright.
        cut = start.min(messages.len() - 1);
        while cut > 1 && !is_turn_boundary(&messages[cut]) {
            cut -= 1;
        }
    }
    if cut <= 1 || cut >= messages.len() {
        return None;
    }
    Some(cut)
}

/// System prompt for the summarizer (pi's structured template).
const SUMMARY_SYSTEM: &str = "You compress a coding-agent conversation history. Produce a terse markdown summary with exactly these sections:\n## Goal\n## Progress\n## Key Decisions\n## Next Steps\nThen a final section `## Files` listing every file path that was read, written, or edited (one per line, machine-extracted expectations). Never invent facts; omit sections with no content.";

/// Compact `memory`: the pre-cut turns are summarized by an independent LLM
/// request (isolated from the main conversation), and the new Memory keeps
/// `[system messages, summary, recent tail]`. On any failure the original
/// memory is preserved (the caller falls back to the ring-buffer trim).
pub async fn compact(
    memory: &Memory,
    client: &Arc<dyn Client>,
    model: &str,
    settings: &CompactionSettings,
) -> Result<Memory, AgentError> {
    let msgs = memory.messages();
    let cut = find_cut_point(&msgs, settings.keep_recent_tokens)
        .ok_or(AgentError::Config("memory too short to compact".into()))?;
    let prefix = &msgs[..cut];
    let tail = msgs[cut..].to_vec();

    // Render the prefix as plain text for the summarizer.
    let mut transcript = String::new();
    for m in prefix {
        transcript.push_str(&format!("[{}] ", m.role));
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => {
                    transcript.push_str(text);
                    transcript.push('\n');
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    transcript.push_str(&format!("(tool_use {name} {input})\n"));
                }
                ContentBlock::ToolResult { content, .. } => {
                    transcript.push_str(&format!("(tool_result {content})\n"));
                }
            }
        }
    }

    let req = CompletionRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: "user".into(),
            content: vec![ContentBlock::Text {
                text: format!("Summarize this conversation history for continuation:\n\n{transcript}"),
            }],
        }],
        max_tokens: None,
        temperature: Some(0.0),
        system_prompt: Some(SUMMARY_SYSTEM.to_string()),
        tools: vec![],
        stream: false,
        preferred_provider: None,
    };
    let resp = client.complete(&req).await.map_err(|e: ClientError| {
        AgentError::Config(format!("compaction summary request failed: {e}"))
    })?;
    if let Some(err) = &resp.error {
        return Err(AgentError::Config(format!("compaction summary error: {err}")));
    }

    // Rebuild: system messages from the prefix, the summary as a user anchor,
    // then the kept tail verbatim.
    let mut next = Memory::new(memory.limit());
    for m in prefix {
        if m.role == "system" {
            next.add_message(m.clone());
        }
    }
    next.add(
        "user",
        &format!(
            "[Compacted conversation summary — older turns were summarized to save context]\n\n{}",
            resp.content
        ),
    );
    for m in tail {
        next.add_message(m);
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message {
            role: "user".into(),
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
    fn assistant(text: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    #[test]
    fn should_compact_threshold() {
        let s = CompactionSettings {
            context_window: 1000,
            reserve_tokens: 200,
            keep_recent_tokens: 100,
        };
        assert!(!should_compact(800, &s));
        assert!(should_compact(801, &s));
    }

    #[test]
    fn estimate_uses_usage_then_chars() {
        let msgs = vec![user(&"x".repeat(400))];
        let u = Usage { input_tokens: 100, output_tokens: 50, ..Default::default() };
        // 150 real + (role+400)/4 heuristic.
        assert_eq!(estimate_tokens(&msgs, Some(&u)), 251);
        assert_eq!(estimate_tokens(&msgs, None), 101);
    }

    #[test]
    fn cut_point_lands_on_turn_boundary() {
        // 10 alternating turns of ~800 chars (~200 tokens each).
        let mut msgs = Vec::new();
        for i in 0..10 {
            msgs.push(user(&format!("turn {i}: {}", "x".repeat(800))));
            msgs.push(assistant(&"y".repeat(800)));
        }
        // keep_recent = 300 tokens → tail must start at a user message and be
        // at most ~1.5 turns; also >= 1 message must remain in the prefix.
        let cut = find_cut_point(&msgs, 300).expect("should find a cut");
        assert!(cut >= 1 && cut < msgs.len());
        assert!(is_turn_boundary(&msgs[cut]), "cut must be a human-turn start");
        // Tail fits the budget (with one message of slack).
        let tail_cost: usize = msgs[cut..].iter().map(|m| message_cost(m) / 4).sum();
        assert!(tail_cost <= 300 + 200, "tail too large: {tail_cost}");
    }

    #[test]
    fn cut_point_never_splits_tool_pair() {
        // assistant(ToolUse) + user(ToolResult) must stay together: with a
        // keep budget that overflows mid-pair, the cut moves to the NEXT
        // human turn (or None), never between the pair.
        let mut msgs = vec![user("start")];
        for i in 0..5 {
            msgs.push(Message {
                role: "assistant".into(),
                content: vec![ContentBlock::ToolUse {
                    id: format!("c{i}"),
                    name: "echo".into(),
                    input: serde_json::json!({"i": i}),
                }],
            });
            msgs.push(Message {
                role: "user".into(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: format!("c{i}"),
                    content: "ok".into(),
                    is_error: false,
                }],
            });
            msgs.push(user(&format!("next {i} {}", "x".repeat(400))));
        }
        for cut in [0, 1, 2, 3] {
            if let Some(idx) = find_cut_point(&msgs, cut * 100) {
                // Every ToolResult must have its ToolUse on the same side.
                let uses: Vec<&str> = msgs[..idx]
                    .iter()
                    .flat_map(|m| {
                        m.content.iter().filter_map(|b| match b {
                            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                            _ => None,
                        })
                    })
                    .collect();
                let results: Vec<&str> = msgs[..idx]
                    .iter()
                    .flat_map(|m| {
                        m.content.iter().filter_map(|b| match b {
                            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                            _ => None,
                        })
                    })
                    .collect();
                for r in results {
                    assert!(uses.contains(&r), "cut split a tool pair at {idx}");
                }
            }
        }
    }

    #[test]
    fn too_short_returns_none() {
        assert_eq!(find_cut_point(&[], 100), None);
        assert_eq!(find_cut_point(&[user("only")], 100), None);
    }
}
