//! Daemon discovery + lazy start (ssh-agent model).
//!
//! When any app creates an `AiClient` via [`crate::AiClient::new`], this module
//! checks if `aaid` is running. If not, it finds and spawns the daemon binary,
//! then waits for it to be ready. If the daemon can't be found or started,
//! `new()` returns `Err(DaemonUnavailable)` — callers then use
//! [`crate::AiClient::with_url`] to point at a known daemon URL instead.

use std::time::{Duration, Instant};

/// Default daemon URL.
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:17654";

/// The daemon URL (overridable via `$AAID_URL`).
pub fn daemon_url() -> String {
    std::env::var("AAID_URL").unwrap_or_else(|_| DEFAULT_DAEMON_URL.to_string())
}

/// Check if the daemon is running by probing `/v1/status`.
pub fn is_running() -> bool {
    let url = format!("{}/v1/status", daemon_url());
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(&url).send() {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Ensure the daemon is running. If not, try to start it.
/// Returns `Some(url)` if daemon is available, `None` if not (caller should
/// fall back to [`crate::AiClient::with_url`] or surface an error).
pub fn ensure_daemon() -> Option<String> {
    // 1. Already running?
    if is_running() {
        return Some(daemon_url());
    }

    // 2. Find aaid binary.
    let aaid_path = find_aaid_binary()?;

    // 3. Spawn daemon in background.
    if !spawn_daemon(&aaid_path) {
        return None;
    }

    // 4. Wait for ready (up to 3 seconds).
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if is_running() {
            return Some(daemon_url());
        }
    }

    // Timeout — daemon didn't start in time.
    None
}

/// Find the `aaid` binary by searching:
/// 1. `$AAID_PATH` env var
/// 2. `PATH` lookup (`aaid` / `aaid.exe`)
/// 3. Dev relative paths (auto-ai repo target dir)
fn find_aaid_binary() -> Option<std::path::PathBuf> {
    // 1. Explicit path.
    if let Ok(path) = std::env::var("AAID_PATH") {
        let p = std::path::PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }

    // 2. PATH lookup.
    let exe_name = if cfg!(windows) { "aaid.exe" } else { "aaid" };
    if let Some(path) = which(exe_name) {
        return Some(path);
    }

    // 3. Dev relative paths (try to find auto-ai/target/debug/aaid).
    for rel in &[
        "../../auto-ai/target/debug/aaid",
        "../../auto-ai/target/debug/aaid.exe",
        "../auto-ai/target/debug/aaid",
        "../../../auto-ai/target/debug/aaid",
        "../../../auto-ai/target/debug/aaid.exe",
    ] {
        let p = std::path::PathBuf::from(rel);
        if p.exists() {
            return Some(p);
        }
    }

    None
}

/// Simple `which` implementation (search PATH).
fn which(exe: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var("PATH").ok()?;
    let separator = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(separator) {
        let candidate = std::path::PathBuf::from(dir).join(exe);
        if candidate.exists() {
            // On Windows, also check .exe extension.
            if cfg!(windows) && !candidate.to_string_lossy().ends_with(".exe") {
                let with_exe = candidate.with_extension("exe");
                if with_exe.exists() {
                    return Some(with_exe);
                }
            }
            return Some(candidate);
        }
    }
    None
}

/// Spawn the daemon binary as a detached background process.
fn spawn_daemon(aaid_path: &std::path::Path) -> bool {
    use std::process::{Command, Stdio};
    match Command::new(aaid_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => {
            // Detach: don't wait for it. The child becomes orphan (init/OS reaps).
            drop(child);
            true
        }
        Err(e) => {
            tracing::warn!("failed to spawn aaid: {}", e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_url_default() {
        // Without AAID_URL set, returns default.
        std::env::remove_var("AAID_URL");
        assert_eq!(daemon_url(), DEFAULT_DAEMON_URL);
    }

    #[test]
    fn daemon_url_override() {
        std::env::set_var("AAID_URL", "http://localhost:9999");
        assert_eq!(daemon_url(), "http://localhost:9999");
        std::env::remove_var("AAID_URL");
    }

    #[test]
    fn is_running_returns_bool() {
        // Just verify it doesn't panic (daemon probably not running in test env).
        let _ = is_running();
    }
}
