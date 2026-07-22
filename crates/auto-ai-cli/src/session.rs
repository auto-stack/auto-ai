//! Session persistence — save/load conversation history per working directory.
//!
//! When `-c/--continue` is passed, the CLI loads the last session for the
//! current directory and replays it into the agent's memory before the first
//! turn. After each turn (Done/Cancelled), the updated history is saved back.
//!
//! Storage: `~/.config/autoos/sessions/<cwd-hash>.json` — one file per unique
//! working directory, keyed by a hash of the absolute path. JSON format
//! (the `Message` / `ContentBlock` types already derive Serialize/Deserialize).

use std::path::{Path, PathBuf};

use auto_ai_client::Message;
use serde::{Deserialize, Serialize};

/// The on-disk session file: the session id + the conversation messages.
#[derive(Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub messages: Vec<Message>,
}

/// Compute the session directory: `~/.config/autoos/sessions/`.
fn sessions_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/autoos/sessions"))
}

/// A stable filename for the given working directory: the hex hash of the
/// absolute path. Two different directories never collide (SHA-256); the same
/// directory always maps to the same file.
fn cwd_hash(cwd: &Path) -> String {
    // FNV-1a is sufficient — we just need a stable, filesystem-safe identifier.
    let abs = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf());
    let s = abs.to_string_lossy();
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

/// The session file path for the given working directory.
pub fn session_file(cwd: &Path) -> Option<PathBuf> {
    sessions_dir().map(|d| d.join(format!("{}.json", cwd_hash(cwd))))
}

/// Save a session (messages + id) for the given cwd. Called after each turn.
/// Best-effort: errors are logged but don't crash the CLI.
pub fn save(cwd: &Path, session_id: &str, messages: &[Message]) {
    let Some(path) = session_file(cwd) else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let record = SessionRecord {
        session_id: session_id.to_string(),
        messages: messages.to_vec(),
    };
    match serde_json::to_string_pretty(&record) {
        Ok(json) => {
            if std::fs::write(&path, json).is_err() {
                eprintln!("  (warning: could not save session to {})", path.display());
            }
        }
        Err(e) => {
            eprintln!("  (warning: could not serialize session: {e})");
        }
    }
}

/// Load the last session for the given cwd. Returns `None` if no session file
/// exists or it can't be parsed (caller silently starts a fresh session).
pub fn load(cwd: &Path) -> Option<SessionRecord> {
    let path = session_file(cwd)?;
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_hash_is_stable() {
        let cwd = Path::new("/tmp/my-project");
        let h1 = cwd_hash(cwd);
        let h2 = cwd_hash(cwd);
        assert_eq!(h1, h2, "same cwd must hash the same");
        assert_eq!(h1.len(), 16, "hash should be 16 hex chars");
    }

    #[test]
    fn cwd_hash_differs_for_different_dirs() {
        let h1 = cwd_hash(Path::new("/tmp/a"));
        let h2 = cwd_hash(Path::new("/tmp/b"));
        assert_ne!(h1, h2);
    }
}
