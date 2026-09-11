//! Built-in tools for auto-ai-cli — read + write + command set.
//! Demonstrates how to implement the Tool trait for a new app.

use async_trait::async_trait;
use auto_ai_agent::{Tool, ToolError, ToolOutput};
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
    async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"].as_str().ok_or_else(|| ToolError::Args("missing 'path'".into()))?;
        std::fs::read_to_string(path).map_err(|e| ToolError::Exec(format!("read '{path}': {e}"))).map(ToolOutput::from)
    }
}

/// Write text to a file (overwrites; creates parent dirs).
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str { "write_file" }
    fn description(&self) -> &str { "Write text content to a file, overwriting if it exists. Parent directories are created automatically." }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string","description":"file path"},"content":{"type":"string","description":"text content"}},"required":["path","content"]})
    }
    async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"].as_str().ok_or_else(|| ToolError::Args("missing 'path'".into()))?;
        let content = args["content"].as_str().ok_or_else(|| ToolError::Args("missing 'content'".into()))?;
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| ToolError::Exec(format!("mkdir: {e}")))?;
            }
        }
        std::fs::write(path, content).map_err(|e| ToolError::Exec(format!("write '{path}': {e}")))?;
        Ok(format!("wrote {} bytes to {}", content.len(), path).into())
    }
}

/// Replace a unique string in a file (precise edit, not full overwrite).
pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str { "edit_file" }
    fn description(&self) -> &str { "Replace a unique string in a file. The old_string must appear exactly once (ambiguous matches error)." }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string","description":"file to edit"},"old_string":{"type":"string","description":"exact text to find (must be unique)"},"new_string":{"type":"string","description":"replacement text"}},"required":["path","old_string","new_string"]})
    }
    async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"].as_str().ok_or_else(|| ToolError::Args("missing 'path'".into()))?;
        let old = args["old_string"].as_str().ok_or_else(|| ToolError::Args("missing 'old_string'".into()))?;
        let new = args["new_string"].as_str().ok_or_else(|| ToolError::Args("missing 'new_string'".into()))?;
        let content = std::fs::read_to_string(path).map_err(|e| ToolError::Exec(format!("read '{path}': {e}")))?;
        let count = content.matches(old).count();
        if count == 0 { return Err(ToolError::Exec(format!("old_string not found in '{path}'"))); }
        if count > 1 { return Err(ToolError::Exec(format!("old_string appears {count} times; must be unique."))); }
        let new_content = content.replacen(old, new, 1);
        std::fs::write(path, &new_content).map_err(|e| ToolError::Exec(format!("write: {e}")))?;
        Ok(format!("edited '{path}'").into())
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
    async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError> {
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
        Ok(out.into())
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
    async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError> {
        let pattern = args["pattern"].as_str().ok_or_else(|| ToolError::Args("missing 'pattern'".into()))?;
        if pattern.trim().is_empty() {
            // An empty regex matches every line of every file — almost never
            // what the model wants. Fail fast so it retries with a real
            // pattern instead of paying for a full-tree scan.
            return Err(ToolError::Args("'pattern' must not be empty".into()));
        }
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
        Ok(lines.join("\n").into())
    }
}

/// Run a shell command — ash-first (PLAN-033). When the sibling `ash`
/// (AutoShell) is available, commands execute through it with a path sandbox
/// rooted at the working directory; the system shell is only used when ash is
/// absent or could not even start the command (zero side effects). Policy
/// denials and genuine command failures never fall back — falling back there
/// would bypass the sandbox or re-run side effects.
pub struct RunCommand {
    desc: &'static str,
}

impl RunCommand {
    pub fn new() -> Self {
        Self { desc: description_for(crate::shell_exec::ash_available().is_some()) }
    }
}

impl Default for RunCommand {
    fn default() -> Self { Self::new() }
}

/// Default and maximum command timeout (design §5.2; the old tool had none).
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

/// Commands safe to run directly.
const ALLOWED_PREFIXES: &[&str] = &[
    "cargo", "npm", "npx", "rustc", "echo", "type", "cat", "ls", "dir",
    "pwd", "git status", "git diff", "git log", "git branch", "test", "true",
    "python", "python3", "go ", "make",
];

fn sys_shell_name() -> &'static str {
    if cfg!(windows) { "cmd.exe" } else { "sh" }
}

fn system_shell_command(cmd: &str) -> std::process::Command {
    if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", cmd]);
        c
    }
}

/// The model-facing description switches on the executor so the model is
/// told which shell habits apply (AC-10).
pub fn description_for(ash: bool) -> &'static str {
    if ash {
        "Run a shell command and return stdout+stderr. \
         Commands run through ash (AutoShell) with a path sandbox: file operations \
         are confined to the working directory, and denied operations return a \
         PAUSED notice instead of running. ash is cross-platform and supports \
         structured pipelines (ls | filter .size > 10.mb | sort .name); no heredocs. \
         If ash cannot even start a command, it is retried on the system shell \
         automatically. Whitelisted commands run directly; anything else needs \
         \"force\": true (runs on the system shell WITHOUT the sandbox — only \
         after user approval)."
    } else {
        "Run a shell command and return stdout+stderr. \
         IMPORTANT: This is a Windows cmd.exe environment. \
         - Use `python` not `python3` \
         - Do NOT use heredoc (<<EOF), &&, or Unix pipes with complex syntax \
         - Do NOT use `python -c` for multi-line code — write a file and run it \
         - Paths: use forward slashes or backslashes, avoid /tmp/ \
         Whitelisted commands run directly; others need \"force\": true."
    }
}

/// Render an ExecOutcome for the model: annotation line, output, exit code
/// when non-zero, plus UI-only details (never enters LLM context).
fn format_outcome(
    o: &crate::shell_exec::ExecOutcome,
    annotation: &str,
    details: Value,
) -> ToolOutput {
    let mut result = String::new();
    if !o.stdout.is_empty() { result.push_str(&o.stdout); }
    if !o.stderr.is_empty() {
        if !result.is_empty() { result.push_str("\n[stderr]\n"); }
        result.push_str(&o.stderr);
    }
    if result.is_empty() { result.push_str("(no output)"); }
    let mut content = format!("{annotation}\n{result}");
    if o.exit_code.unwrap_or(0) != 0 {
        let code = o.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
        content = format!("{content}\n[exit: {code}]");
    }
    ToolOutput { content, details: Some(details) }
}

/// Run on the system shell (fallback / forced / ash-unavailable track).
/// `why` appears in the annotation so the model can tell tracks apart.
async fn run_system_track(cmd: &str, timeout_ms: u64, why: &str) -> Result<ToolOutput, ToolError> {
    let cmd = cmd.to_string();
    let why = why.to_string();
    let cmd_for_error = cmd.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut c = system_shell_command(&cmd);
        crate::shell_exec::run_with_timeout(&mut c, timeout_ms)
    })
    .await
    .map_err(|e| ToolError::Exec(format!("join executor: {e}")))?;
    if let Some(err) = &outcome.spawn_error {
        return Err(ToolError::Exec(format!("spawn '{cmd_for_error}': {err}")));
    }
    if outcome.timed_out {
        return Ok(ToolOutput {
            content: format!(
                "[exec: {} ({why}, timed out after {timeout_ms}ms)]\n⏸ Timed out and killed; not retried (the command may have had partial effects).",
                sys_shell_name()
            ),
            details: Some(json!({"executor": sys_shell_name(), "reason": why, "timed_out": true, "timeout_ms": timeout_ms})),
        });
    }
    let annotation = format!("[exec: {} ({why})]", sys_shell_name());
    let details = json!({
        "executor": sys_shell_name(), "reason": why,
        "exit_code": outcome.exit_code, "timed_out": false, "timeout_ms": timeout_ms,
    });
    Ok(format_outcome(&outcome, &annotation, details))
}

/// Run through ash and apply the fallback matrix (design §5.3/§6).
async fn run_ash_track(
    info: &crate::shell_exec::AshInfo,
    cmd: &str,
    no_network: bool,
    timeout_ms: u64,
) -> Result<ToolOutput, ToolError> {
    use crate::shell_exec::Classification;
    let opts = crate::shell_exec::AshOptions::for_run(no_network, timeout_ms);
    let ash_path = info.path.clone();
    let cmd_owned = cmd.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        crate::shell_exec::execute_via_ash(&ash_path, &cmd_owned, &opts)
    })
    .await
    .map_err(|e| ToolError::Exec(format!("join executor: {e}")))?;

    let mut details = json!({
        "executor": "ash", "exit_code": outcome.exit_code,
        "timed_out": outcome.timed_out, "timeout_ms": timeout_ms,
    });
    match outcome.classification() {
        Classification::RanOk => Ok(format_outcome(&outcome, "[exec: ash]", details)),
        Classification::Denied => {
            // Zero side effects, but falling back would bypass the sandbox:
            // surface as PAUSED with remediation instead (AC-03).
            let reason: String = outcome.stderr.lines()
                .rev().find(|l| !l.trim().is_empty())
                .unwrap_or("policy denied").trim().chars().take(300).collect();
            details["classification"] = json!("denied");
            details["ash_error"] = json!(reason);
            let content = format!(
                "[exec: ash (denied)]\n⏸ PAUSED: ash policy refused this command (nothing ran): {reason}\n\
                 Stay inside the working directory or adjust the command; pass force:true only with \
                 user approval to run it on the system shell (no sandbox)."
            );
            Ok(ToolOutput { content, details: Some(details) })
        }
        Classification::PreExecFailure => {
            // ash could not start the command — zero side effects, so the
            // system-shell retry is safe (AC-04). The system track annotates
            // the output with the ash failure reason.
            let reason: String = outcome.stderr.lines()
                .rev().find(|l| !l.trim().is_empty())
                .unwrap_or("could not start command").trim().chars().take(200).collect();
            run_system_track(cmd, timeout_ms, &format!("fallback: ash {reason}")).await
        }
        Classification::RanFailed => {
            // Command ran and failed, timed out, or the outcome is ambiguous:
            // never re-run it elsewhere (AC-05/AC-08).
            let (classification, annotation) = if outcome.timed_out {
                ("timeout", format!("[exec: ash (timeout after {timeout_ms}ms)]"))
            } else {
                ("ran_failed", "[exec: ash]".to_string())
            };
            details["classification"] = json!(classification);
            Ok(format_outcome(&outcome, &annotation, details))
        }
    }
}

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str { "run_command" }
    fn description(&self) -> &str { self.desc }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
            "cmd":{"type":"string","description":"the shell command"},
            "force":{"type":"boolean","description":"skip whitelist check and run directly on the system shell WITHOUT the ash sandbox (after user approval)"},
            "timeout_ms":{"type":"integer","description":format!("hard timeout in milliseconds (default {DEFAULT_TIMEOUT_MS}, max {MAX_TIMEOUT_MS})")},
            "no_network":{"type":"boolean","description":"ash track only: block network-capable commands for this call (default false)"}
        },"required":["cmd"]})
    }
    async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError> {
        let cmd = args["cmd"].as_str().ok_or_else(|| ToolError::Args("missing 'cmd'".into()))?.to_string();
        let force = args["force"].as_bool().unwrap_or(false);
        let no_network = args["no_network"].as_bool().unwrap_or(false);
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1, MAX_TIMEOUT_MS);

        // Explicit human authorization: straight to the system shell,
        // sandbox off — the existing force semantics, unchanged (AC-06).
        if force {
            return run_system_track(&cmd, timeout_ms, "forced").await;
        }

        {
            let lower = cmd.trim().to_lowercase();
            // Danger patterns.
            for pat in &["rm -rf", "format ", "del /s", "curl ", "wget ", "shutdown", "| sh"] {
                if lower.contains(pat) {
                    return Ok(format!("⏸ PAUSED: dangerous pattern '{pat}'. Needs user approval.").into());
                }
            }
            // Whitelist.
            let allowed = ALLOWED_PREFIXES.iter().any(|p| lower == *p || lower.starts_with(&format!("{p} ")));
            if !allowed {
                return Ok(format!("⏸ PAUSED: '{cmd}' is not on the whitelist. Pass force:true after approval.").into());
            }
        }

        match crate::shell_exec::ash_available() {
            Some(info) => run_ash_track(info, &cmd, no_network, timeout_ms).await,
            None => run_system_track(&cmd, timeout_ms, "ash unavailable").await,
        }
    }
}

#[cfg(test)]
mod run_command_tests {
    use super::*;

    #[test]
    fn schema_exposes_new_params() {
        let params = RunCommand::new().parameters();
        for key in ["cmd", "force", "timeout_ms", "no_network"] {
            assert!(params["properties"][key].is_object(), "missing param {key}");
        }
        let required = params["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "cmd"));
    }

    #[test]
    fn description_tracks_executor() {
        assert!(description_for(true).contains("ash (AutoShell)"));
        assert!(description_for(true).contains("sandbox"));
        assert!(description_for(false).contains("cmd.exe"));
    }

    #[tokio::test]
    async fn force_goes_straight_to_system_shell() {
        let out = RunCommand::new()
            .execute(&json!({"cmd": "echo force-track-marker", "force": true}))
            .await
            .expect("execute");
        assert!(out.content.contains("(forced)"), "annotation: {}", out.content);
        assert!(out.content.contains("force-track-marker"));
        assert!(!out.content.contains("[exec: ash]"));
    }

    #[tokio::test]
    async fn whitelist_pause_unchanged() {
        let out = RunCommand::new()
            .execute(&json!({"cmd": "definitely_not_whitelisted_xyz"}))
            .await
            .expect("execute");
        assert!(out.content.contains("PAUSED") && out.content.contains("whitelist"));
    }

    /// Live ash track: whitelisted echo runs via ash (AC-01).
    #[tokio::test]
    async fn live_ash_track_runs_echo() {
        if crate::shell_exec::ash_available().is_none() {
            eprintln!("SKIP: no ash discovered in this environment");
            return;
        }
        let out = RunCommand::new()
            .execute(&json!({"cmd": "echo ash-track-marker"}))
            .await
            .expect("execute");
        assert!(out.content.contains("[exec: ash]"), "content: {}", out.content);
        assert!(out.content.contains("ash-track-marker"));
        assert_eq!(out.details.as_ref().unwrap()["executor"], "ash");
    }

    /// Live denial: `cat` a file outside the cwd sandbox → PAUSED, no
    /// fallback, no re-execution (AC-03).
    #[tokio::test]
    async fn live_sandbox_denial_does_not_fall_back() {
        if crate::shell_exec::ash_available().is_none() {
            eprintln!("SKIP: no ash discovered in this environment");
            return;
        }
        let outside = std::env::temp_dir().join("auto-ai-ac03-outside.txt");
        std::fs::write(&outside, "sentinel").expect("fixture file");
        let out = RunCommand::new()
            .execute(&json!({"cmd": format!("cat {}", outside.display())}))
            .await
            .expect("execute");
        assert!(out.content.contains("denied"), "content: {}", out.content);
        assert!(out.content.contains("PAUSED"));
        assert!(!out.content.contains("fallback"), "must not fall back: {}", out.content);
        assert!(!out.content.contains("sentinel"), "file must not have been read");
    }

    /// Live command failure: cargo with a bogus flag runs, fails, is NOT
    /// retried elsewhere (AC-05).
    #[tokio::test]
    async fn live_command_failure_is_not_retried() {
        if crate::shell_exec::ash_available().is_none() {
            eprintln!("SKIP: no ash discovered in this environment");
            return;
        }
        let out = RunCommand::new()
            .execute(&json!({"cmd": "cargo --definitely-bogus-flag"}))
            .await
            .expect("execute");
        assert!(out.content.contains("[exec: ash]"), "content: {}", out.content);
        assert!(out.content.contains("[exit:"), "exit code surfaced: {}", out.content);
        assert!(!out.content.contains("fallback"), "must not fall back: {}", out.content);
    }
}
