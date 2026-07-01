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

    /// Drop the oldest non-system messages until we're within the turn limit.
    /// System messages are always kept.
    pub fn trim(&mut self) {
        let Some(limit) = self.limit else {
            return;
        };
        if limit == 0 {
            // Keep only system messages.
            self.messages.retain(|m| m.role == "system");
            return;
        }
        // Count non-system messages; if over 2*limit, drop the oldest non-system.
        let max_non_system = limit * 2;
        let mut non_system_count = self
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .count();
        while non_system_count > max_non_system {
            if let Some(pos) = self
                .messages
                .iter()
                .position(|m| m.role != "system")
            {
                self.messages.remove(pos);
                non_system_count -= 1;
            } else {
                break;
            }
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
        let mut mem = Memory::new(Some(1)); // keep last 1 turn = 2 non-system msgs
        mem.add("system", "you are nice");
        mem.add("user", "a");
        mem.add("assistant", "b");
        // 2 non-system == limit*2, nothing trimmed yet
        assert_eq!(mem.len(), 3);
        mem.add("user", "c"); // now 3 non-system > 2, trim oldest non-system
        mem.add("assistant", "d");
        let texts: Vec<_> = mem.messages().iter().map(|m| m.text()).collect();
        // system kept; oldest non-system pair ('a') dropped; 'c' and 'd' kept.
        assert_eq!(texts[0], "you are nice");
        assert!(!texts.contains(&"a".to_string()));
        assert!(texts.contains(&"c".to_string()));
        assert!(texts.contains(&"d".to_string()));
        assert_eq!(mem.len(), 3); // system + user-c + assistant-d
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
}
