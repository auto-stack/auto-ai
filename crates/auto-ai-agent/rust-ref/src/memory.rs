//! Conversation memory for an [`crate::Agent`].
//!
//! A thin ring buffer over [`auto_ai_client::Message`] that keeps the system
//! message(s) plus the most recent turns, trimming older history once the
//! Role's `memory_limit` is exceeded.

use auto_ai_client::Message;

/// Bounded conversation history.
///
/// "Turns" are counted as user+assistant message pairs (a tool round-trip is
/// two messages but counts as one turn). [`Memory::trim`] enforces the limit,
/// always preserving leading system messages.
#[derive(Default)]
pub struct Memory {
    messages: Vec<Message>,
    limit: Option<usize>,
}

impl Memory {
    /// Create memory with an optional turn limit (None = unbounded).
    pub fn new(limit: Option<usize>) -> Self {
        Self {
            messages: Vec::new(),
            limit,
        }
    }

    /// Append a message built from role + plain text.
    pub fn add(&mut self, role: &str, content: impl Into<String>) {
        let msg = match role {
            "assistant" => Message::assistant(content),
            "system" => Message::system(content),
            _ => Message::user(content),
        };
        self.messages.push(msg);
        self.trim();
    }

    /// Append an already-constructed message (used for tool-use / tool-result
    /// blocks that aren't plain text).
    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        self.trim();
    }

    /// Pre-load a sequence of (role, content) turns from a prior conversation.
    ///
    /// Used to rebuild an agent's context across stateless HTTP requests (e.g.
    /// musk chat sessions): each prior user/assistant turn is appended so the
    /// model sees the history. Trims once at the end. (Plan 008.)
    pub fn extend_pairs<I, S>(&mut self, pairs: I)
    where
        I: IntoIterator<Item = (S, S)>,
        S: AsRef<str>,
    {
        for (role, content) in pairs {
            self.add(role.as_ref(), content.as_ref());
        }
    }

    /// All messages currently held (in order).
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Owned snapshot, suitable for handing to the client.
    pub fn to_messages(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Current number of stored messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Number of non-system messages currently held.
    fn non_system_count(&self) -> usize {
        self.messages.iter().filter(|m| m.role != "system").count()
    }

    /// Drop the oldest non-system messages until we're within the turn limit.
    /// System messages are always kept.
    ///
    /// Trimming is **pairing-aware** (review-003 S1): an assistant message
    /// containing `ToolUse` and the subsequent user-role `ToolResult` messages
    /// are treated as an atomic "conversation unit" and removed together.
    /// Splitting them would leave an orphan ToolResult (no matching ToolUse)
    /// or an unanswered ToolUse, both of which make providers return 400.
    pub fn trim(&mut self) {
        let Some(limit) = self.limit else {
            return;
        };
        if limit == 0 {
            // Keep only system messages.
            self.messages.retain(|m| m.role == "system");
            return;
        }
        // Count non-system messages; if over 2*limit, drop oldest unit(s).
        let max_non_system = limit * 2;
        while self.non_system_count() > max_non_system {
            // Find the start of the oldest non-system conversation unit: the
            // first non-system message.
            let Some(start) = self.messages.iter().position(|m| m.role != "system") else {
                break;
            };
            // The unit extends to include that message plus any immediately
            // following user-role messages (ToolResults answering an assistant
            // ToolUse). This keeps assistant(tool_use)+user(tool_result) pairs
            // intact. An assistant turn is followed by 0+ user messages; a
            // standalone user message (the human's input) is a unit of size 1.
            let mut end = start + 1;
            if self.messages[start].role == "assistant" {
                while end < self.messages.len() && self.messages[end].role == "user" {
                    end += 1;
                }
            }
            // Remove [start, end) as one atomic unit.
            self.messages.drain(start..end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_messages() {
        let mut mem = Memory::new(None);
        mem.add("user", "hi");
        mem.add("assistant", "hello");
        assert_eq!(mem.len(), 2);
        assert_eq!(mem.messages()[0].role, "user");
        assert_eq!(mem.messages()[1].role, "assistant");
    }

    #[test]
    fn system_messages_preserved_on_trim() {
        // limit=2 → keep last 2 turns (4 non-system msgs max).
        let mut mem = Memory::new(Some(2));
        mem.add("system", "you are nice");
        mem.add("user", "a");
        mem.add("assistant", "b");
        // 2 non-system == limit, nothing trimmed yet
        assert_eq!(mem.len(), 3);
        mem.add("user", "c");
        mem.add("assistant", "d");
        // 4 non-system == limit*2, still nothing trimmed
        assert_eq!(mem.len(), 5);
        mem.add("user", "e");
        mem.add("assistant", "f");
        // 6 non-system > limit*2(4): trim oldest conversation units.
        // Units are removed atomically: user-e alone can't be the oldest unit
        // because user-e follows assistant-d (part of d's unit only if d had
        // tool_use; here it's plain text so each message is its own unit).
        let texts: Vec<_> = mem.messages().iter().map(|m| m.text()).collect();
        assert_eq!(texts[0], "you are nice"); // system always kept
        assert!(!texts.contains(&"a".to_string())); // oldest trimmed
        assert!(texts.contains(&"f".to_string())); // newest kept
    }

    #[test]
    fn unbounded_never_trims() {
        let mut mem = Memory::new(None);
        for i in 0..100 {
            mem.add("user", format!("m{i}"));
        }
        assert_eq!(mem.len(), 100);
    }

    #[test]
    fn add_message_appends_raw() {
        let mut mem = Memory::new(None);
        mem.add_message(Message::tool_result("call_1", "42"));
        assert_eq!(mem.len(), 1);
        assert_eq!(mem.messages()[0].role, "user"); // tool_result is user-role
    }

    /// S1 (review-003): trimming must never split a tool_use ↔ tool_result
    /// pair — an orphan on either side makes the provider return 400.
    #[test]
    fn trim_keeps_tool_use_result_pairs_intact() {
        use auto_ai_client::ContentBlock;
        let mut mem = Memory::new(Some(1)); // keep ≤2 non-system msgs

        // Build: assistant(tool_use X) + user(tool_result X) + assistant(final)
        mem.add_message(Message {
            role: "assistant".into(),
            content: vec![ContentBlock::ToolUse {
                id: "X".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            }],
        });
        mem.add_message(Message::tool_result("X", "data"));
        mem.add_message(Message::assistant("final answer"));
        // 3 non-system > 2 → must trim the assistant(X)+user(X) unit together,
        // NOT leave the user(tool_result X) orphaned.
        assert_eq!(mem.non_system_count(), 1); // only "final answer" remains

        // No orphan ToolResult should remain.
        for m in mem.messages() {
            for b in &m.content {
                assert!(
                    !matches!(b, ContentBlock::ToolResult { .. }),
                    "orphan tool_result left after trim: {:?}",
                    b
                );
            }
        }
    }

    /// S1: even under tighter pressure, a tool_result always travels with its
    /// tool_use assistant turn.
    #[test]
    fn trim_drops_assistant_and_all_its_tool_results_together() {
        use auto_ai_client::ContentBlock;
        let mut mem = Memory::new(Some(2)); // keep ≤4 non-system msgs

        // Unit 1: assistant with two tool_use calls + two tool_results.
        mem.add_message(Message {
            role: "assistant".into(),
            content: vec![
                ContentBlock::ToolUse { id: "A".into(), name: "r1".into(), input: serde_json::json!({}) },
                ContentBlock::ToolUse { id: "B".into(), name: "r2".into(), input: serde_json::json!({}) },
            ],
        });
        mem.add_message(Message::tool_result("A", "ra"));
        mem.add_message(Message::tool_result("B", "rb"));
        // Unit 2: another full turn.
        mem.add_message(Message {
            role: "assistant".into(),
            content: vec![ContentBlock::ToolUse { id: "C".into(), name: "r3".into(), input: serde_json::json!({}) }],
        });
        mem.add_message(Message::tool_result("C", "rc"));
        // 5 non-system > 4 → trim unit 1 (3 messages together).
        assert_eq!(mem.non_system_count(), 2); // only unit 2 remains
        let ids: Vec<_> = mem.messages().iter().flat_map(|m| m.content.iter()).filter_map(|b| {
            if let ContentBlock::ToolResult { tool_use_id, .. } = b { Some(tool_use_id.clone()) } else { None }
        }).collect();
        assert_eq!(ids, vec!["C".to_string()]); // A, B gone with their assistant
    }
}
