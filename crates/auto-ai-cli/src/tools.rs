//! Built-in tools for auto-ai-cli — a minimal read-only + command set.
//! Demonstrates how to implement the Tool trait for a new app.

use async_trait::async_trait;
use auto_ai_agent::{Tool, ToolError};
use serde_json::{json, Value};

/// Read a file's UTF-8 text.
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str { "Read the full UTF-8 text of a file." }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string","description":"file path"}},"required":["path"]})
    }
    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path = args["path"].as_str().ok_or_else(|| ToolError::Args("missing 'path'".into()))?;
        std::fs::read_to_string(path).map_err(|e| ToolError::Exec(format!("read '{path}': {e}")))
    }
}

/// List a directory's contents.
pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str { "list_dir" }
    fn description(&self) -> &str { "List directory contents. Returns 'name <dir|file size>' per line." }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string","description":"directory path (default: .)"}}})
    }
    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path = args["path"].as_str().unwrap_or(".");
        let entries = std::fs::read_dir(path).map_err(|e| ToolError::Exec(format!("list '{path}': {e}")))?;
        let mut items: Vec<(String, bool, u64)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| { let m = e.metadata().ok()?; Some((e.file_name().to_string_lossy().into_owned(), m.is_dir(), m.len())) })
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut out = String::new();
        for (name, is_dir, size) in items {
            if is_dir { out.push_str(&format!("{name} <dir>\n")); }
            else { out.push_str(&format!("{name} <file {size}B>\n")); }
        }
        if out.is_empty() { out.push_str("(empty directory)"); }
        Ok(out)
    }
}

/// Search file contents with a regex pattern.
pub struct Search;

#[async_trait]
impl Tool for Search {
    fn name(&self) -> &str { "search" }
    fn description(&self) -> &str { "Search file contents for a pattern (regex). Returns matching lines with file:line prefixes." }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"pattern":{"type":"string","description":"regex pattern"},"path":{"type":"string","description":"directory to search (default: .)"}},"required":["pattern"]})
    }
    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let pattern = args["pattern"].as_str().ok_or_else(|| ToolError::Args("missing 'pattern'".into()))?;
        let path = args["path"].as_str().unwrap_or(".");
        let output = if cfg!(windows) {
            std::process::Command::new("cmd").args(["/C", &format!("findstr /S /N /R \"{pattern}\" {path}\\*")]).output()
        } else {
            std::process::Command::new("grep").args(["-rn", "--include=*", pattern, path]).output()
        };
        let output = output.map_err(|e| ToolError::Exec(format!("search: {e}")))?;
        let result = String::from_utf8_lossy(&output.stdout);
        if result.trim().is_empty() { return Ok("(no matches)".into()); }
        // Cap output to avoid flooding context.
        let lines: Vec<&str> = result.lines().take(50).collect();
        Ok(lines.join("\n"))
    }
}

/// Run a shell command (with basic whitelist safety, Design 004 style).
pub struct RunCommand;

/// Commands safe to run directly.
const ALLOWED_PREFIXES: &[&str] = &[
    "cargo", "npm", "npx", "rustc", "echo", "type", "cat", "ls", "dir",
    "pwd", "git status", "git diff", "git log", "git branch", "test", "true",
    "python", "python3", "go ", "make",
];

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str { "run_command" }
    fn description(&self) -> &str {
        "Run a shell command and return stdout+stderr. Whitelisted commands (cargo/npm/git status/echo/…) run directly; others are PAUSED for approval. Pass \"force\": true to override."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"cmd":{"type":"string","description":"the shell command"},"force":{"type":"boolean","description":"skip whitelist check (after user approval)"}},"required":["cmd"]})
    }
    async fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let cmd = args["cmd"].as_str().ok_or_else(|| ToolError::Args("missing 'cmd'".into()))?;
        let force = args["force"].as_bool().unwrap_or(false);

        if !force {
            let lower = cmd.trim().to_lowercase();
            // Danger patterns.
            for pat in &["rm -rf", "format ", "del /s", "curl ", "wget ", "shutdown", "| sh"] {
                if lower.contains(pat) {
                    return Ok(format!("⏸ PAUSED: dangerous pattern '{pat}'. Needs user approval."));
                }
            }
            // Whitelist.
            let allowed = ALLOWED_PREFIXES.iter().any(|p| lower == *p || lower.starts_with(&format!("{p} ")));
            if !allowed {
                return Ok(format!("⏸ PAUSED: '{cmd}' is not on the whitelist. Pass force:true after approval."));
            }
        }

        let output = if cfg!(windows) {
            std::process::Command::new("cmd").args(["/C", cmd]).output()
        } else {
            std::process::Command::new("sh").args(["-c", cmd]).output()
        };
        let output = output.map_err(|e| ToolError::Exec(format!("spawn '{cmd}': {e}")))?;
        let mut result = String::new();
        if !output.stdout.is_empty() { result.push_str(&String::from_utf8_lossy(&output.stdout)); }
        if !output.stderr.is_empty() {
            if !result.is_empty() { result.push_str("\n[stderr]\n"); }
            result.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if result.is_empty() { result.push_str("(no output)"); }
        Ok(result)
    }
}
