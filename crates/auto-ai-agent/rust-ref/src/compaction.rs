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

/// Conservative substrings that identify a context-overflow error message
/// (Plan 031, pi's `ai/src/utils/overflow.ts` OVERFLOW_PATTERNS — cut down
/// to the three provider families we speak, plus the generic error-code
/// forms). Matched case-insensitively against the error text.
const OVERFLOW_MARKERS: &[&str] = &[
    // Anthropic: "prompt is too long: 213462 tokens > 200000 maximum",
    // 413 {"type":"request_too_large",...}
    "prompt is too long",
    "request_too_large",
    // OpenAI + OpenAI-compatible proxies (LiteLLM, vLLM, ...):
    // "input exceeds the context window", "maximum context length of N tokens"
    "exceeds the context window",
    "exceeds the model's maximum context length",
    "exceeds model's maximum context length",
    "maximum context length",
    "context_length_exceeded",
    // Ollama / llama.cpp / LM Studio:
    // "prompt too long; exceeded max context length", "request exceeds the
    // available context size", "... greater than the context length"
    "exceeds the available context size",
    "exceeded max context length",
    "exceeds the context length",
    "greater than the context length",
];

/// Non-overflow look-alikes: rate limiting / throttling messages that could
/// otherwise match loosely (pi's NON_OVERFLOW_PATTERNS — e.g. Bedrock's
/// "Throttling error: Too many tokens, please wait").
const NON_OVERFLOW_MARKERS: &[&str] = &["rate limit", "too many requests", "throttling"];

/// True when an LLM error message indicates the request overflowed the
/// model's context window (Plan 031). Deliberately conservative: an unknown
/// error returns false (the run fails, same as today); a false positive only
/// costs one extra compaction attempt.
pub fn is_context_overflow(message: &str) -> bool {
    let lower = message.to_lowercase();
    if NON_OVERFLOW_MARKERS.iter().any(|m| lower.contains(m)) {
        return false;
    }
    OVERFLOW_MARKERS.iter().any(|m| lower.contains(m))
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
const SUMMARY_SYSTEM: &str = "You compress a coding-agent conversation history. Produce a terse markdown summary with exactly these sections:\n## Goal\n## Progress\n## Key Decisions\n## Next Steps\nNever invent facts; omit sections with no content. Do not list files — the file manifest is appended mechanically.";

/// Incremental-update instructions (pi's UPDATE_SUMMARIZATION_PROMPT): used
/// when a previous summary exists. The previous summary is embedded in the
/// user message; the model rewrites Goal/Progress/Decisions in place.
const UPDATE_SYSTEM: &str = "You update an existing conversation summary with new messages. PRESERVE all still-relevant information from the previous summary; ADD new progress, decisions, and context; move finished items to done; drop what is no longer relevant. Keep the same sections:\n## Goal\n## Progress\n## Key Decisions\n## Next Steps\nNever invent facts; omit sections with no content. Do not list files — the file manifest is appended mechanically.";

/// File paths touched by the conversation (pi's FileOperations — mechanically
/// extracted from the wire's tool_use blocks, never from LLM output).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileOps {
    /// Files read but not modified.
    pub read: std::collections::BTreeSet<String>,
    /// Files replaced by full writes.
    pub written: std::collections::BTreeSet<String>,
    /// Files modified by targeted edits.
    pub edited: std::collections::BTreeSet<String>,
}

/// Extract file operations from wire messages by mechanically scanning
/// assistant `ToolUse` blocks (Plan 031; pi's extractFileOpsFromMessage +
/// computeFileLists). Tool names are matched loosely (`read…`/`write…`/
/// `edit…`/`patch…`) and the path comes from the `path` / `file_path` /
/// `notebook_path` argument fields. A file both read and modified counts as
/// modified only.
pub fn extract_file_ops(messages: &[Message]) -> FileOps {
    let mut ops = FileOps::default();
    for m in messages {
        if m.role != "assistant" {
            continue;
        }
        for b in &m.content {
            let ContentBlock::ToolUse { name, input, .. } = b else {
                continue;
            };
            let path = ["path", "file_path", "notebook_path"]
                .iter()
                .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
                .unwrap_or("");
            if path.is_empty() {
                continue;
            }
            let lower = name.to_lowercase();
            if lower.contains("read") {
                ops.read.insert(path.to_string());
            } else if lower.contains("edit") || lower.contains("patch") {
                ops.edited.insert(path.to_string());
            } else if lower.contains("write") {
                ops.written.insert(path.to_string());
            }
        }
    }
    // A file that was also modified is not "read-only".
    for f in ops.edited.iter().chain(ops.written.iter()) {
        ops.read.remove(f);
    }
    ops
}

/// Render the machine-extracted file manifest (pi's formatFileOperations):
/// `<read-files>` / `<modified-files>` blocks, empty string when nothing was
/// touched. This is spliced into the summary anchor verbatim — the LLM's own
/// `## Files` output is at best a paraphrase of this ground truth.
pub fn format_file_operations(ops: &FileOps) -> String {
    let mut sections = Vec::new();
    if !ops.read.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            ops.read.iter().cloned().collect::<Vec<_>>().join("\n")
        ));
    }
    let mut modified: Vec<String> = ops.edited.iter().chain(ops.written.iter()).cloned().collect();
    modified.sort();
    modified.dedup();
    if !modified.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        sections.join("\n\n")
    }
}

/// Compact `memory`: the pre-cut turns are summarized by an independent LLM
/// request (isolated from the main conversation), and the new Memory keeps
/// `[system messages, summary, recent tail]`. With `previous_summary` the
/// request uses the incremental UPDATE template instead of a fresh summary.
/// The summary anchor always carries the machine-extracted file manifest of
/// the cut prefix. On any failure the original memory is preserved (the
/// caller falls back to the ring-buffer trim). Returns the new Memory and
/// the summary text (the caller feeds it back as the next `previous_summary`).
pub async fn compact(
    memory: &Memory,
    client: &Arc<dyn Client>,
    model: &str,
    settings: &CompactionSettings,
    previous_summary: Option<&str>,
) -> Result<(Memory, String), AgentError> {
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

    // Machine file manifest for the summarized range — spliced into the
    // anchor verbatim so it never depends on the model obeying the template.
    let file_manifest = format_file_operations(&extract_file_ops(prefix));

    let (system, user) = match previous_summary {
        Some(prev) => (
            UPDATE_SYSTEM,
            format!(
                "Update the existing summary with this new conversation history.\n\n<previous-summary>\n{prev}\n</previous-summary>\n\n<conversation>\n{transcript}\n</conversation>\n\nProduce the updated summary now."
            ),
        ),
        None => (
            SUMMARY_SYSTEM,
            format!("Summarize this conversation history for continuation:\n\n{transcript}"),
        ),
    };

    let req = CompletionRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: "user".into(),
            content: vec![ContentBlock::Text { text: user }],
        }],
        max_tokens: None,
        temperature: Some(0.0),
        system_prompt: Some(system.to_string()),
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

    // The stored summary = the model's narrative + the machine manifest (the
    // manifest is ground truth; a `## Files` section the model emitted stays
    // inside its narrative but the manifest below is what later turns trust).
    let mut summary = resp.content.trim().to_string();
    if !file_manifest.is_empty() {
        summary.push_str("\n\n");
        summary.push_str(&file_manifest);
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
            summary
        ),
    );
    for m in tail {
        next.add_message(m);
    }
    Ok((next, summary))
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

    #[test]
    fn overflow_detection_matches_three_provider_families() {
        // Anthropic
        assert!(is_context_overflow(
            "prompt is too long: 213462 tokens > 200000 maximum"
        ));
        assert!(is_context_overflow(
            "413 {\"error\":{\"type\":\"request_too_large\"}}"
        ));
        // OpenAI + compatible proxies
        assert!(is_context_overflow(
            "Your input exceeds the context window of this model"
        ));
        assert!(is_context_overflow(
            "Requested token count exceeds the model's maximum context length of 131072 tokens"
        ));
        assert!(is_context_overflow("maximum context length is 8192 tokens"));
        assert!(is_context_overflow("Error code: context_length_exceeded"));
        // Ollama / llama.cpp / LM Studio
        assert!(is_context_overflow(
            "prompt too long; exceeded max context length by 42 tokens"
        ));
        assert!(is_context_overflow("the request exceeds the available context size"));
        assert!(is_context_overflow("tokens to keep ... greater than the context length"));
    }

    #[test]
    fn overflow_detection_excludes_look_alikes_and_unknowns() {
        // Rate limiting / throttling never counts (pi's NON_OVERFLOW_PATTERNS).
        assert!(!is_context_overflow("rate limit exceeded, retry later"));
        assert!(!is_context_overflow("Error: Too many requests (429)"));
        assert!(!is_context_overflow("Throttling error: please wait"));
        // Unknown errors stay unknown — conservative.
        assert!(!is_context_overflow("connection reset by peer"));
        assert!(!is_context_overflow("invalid request: unknown parameter"));
        // A model id or doc mention of "context window" alone is not an error.
        assert!(!is_context_overflow("model has a context window of 128k"));
    }

    fn tool_use_msg(name: &str, path: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: name.into(),
                input: serde_json::json!({ "path": path }),
            }],
        }
    }

    #[test]
    fn file_ops_extract_classifies_by_tool_name() {
        let msgs = vec![
            tool_use_msg("read_file", "src/a.rs"),
            tool_use_msg("write_file", "src/b.rs"),
            tool_use_msg("edit_file", "src/c.rs"),
            tool_use_msg("apply_patch", "src/d.rs"),
            // read of a file later modified → counts as modified only
            tool_use_msg("read_file", "src/c.rs"),
            // file_path arg variant (Jupyter-style tools)
            Message {
                role: "assistant".into(),
                content: vec![ContentBlock::ToolUse {
                    id: "c2".into(),
                    name: "read_notebook".into(),
                    input: serde_json::json!({ "file_path": "nb.ipynb" }),
                }],
            },
            // no path argument → ignored
            tool_use_msg("list_dir", "."),
        ];
        let ops = extract_file_ops(&msgs);
        assert_eq!(ops.read.iter().cloned().collect::<Vec<_>>(), vec!["nb.ipynb", "src/a.rs"]);
        assert!(ops.edited.contains("src/c.rs"));
        assert!(ops.edited.contains("src/d.rs"));
        assert!(ops.written.contains("src/b.rs"));
        assert!(!ops.read.contains("src/c.rs"), "modified file must leave the read set");
    }

    #[test]
    fn file_ops_format_renders_sorted_manifest() {
        let mut ops = FileOps::default();
        ops.read.insert("src/a.rs".into());
        ops.edited.insert("src/c.rs".into());
        ops.written.insert("src/b.rs".into());
        let text = format_file_operations(&ops);
        assert!(text.contains("<read-files>\nsrc/a.rs\n</read-files>"));
        assert!(text.contains("<modified-files>\nsrc/b.rs\nsrc/c.rs\n</modified-files>"));
        assert!(format_file_operations(&FileOps::default()).is_empty());
    }
}
