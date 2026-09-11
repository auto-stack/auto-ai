//! ash-first shell execution support (PLAN-033).
//!
//! Locates the sibling `ash` (AutoShell) binary, probes it once per process,
//! and (T-02) classifies ash's exit code + stderr into the fallback-decision
//! taxonomy. The CLI's `run_command`/`run_ash_script` tools route through
//! here; see docs/designs/2026-09-11-ash-first-shell-execution-design.md §5.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// A probed ash installation.
#[derive(Debug, Clone)]
pub struct AshInfo {
    pub path: PathBuf,
    pub version: String,
}

/// Process-wide probe result. `None` means ash is unavailable this session
/// (not installed, or probe failed) — negative-cached so we pay discovery
/// at most once instead of per command.
pub fn ash_available() -> Option<&'static AshInfo> {
    static CACHE: OnceLock<Option<AshInfo>> = OnceLock::new();
    CACHE.get_or_init(|| probe_ash()).as_ref()
}

fn probe_ash() -> Option<AshInfo> {
    let path = discover_from(
        &std::env::var("AUTO_AI_ASH_BIN").unwrap_or_default(),
        &std::env::var("PATH").unwrap_or_default(),
        std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())),
        std::env::current_dir().ok(),
    )?;
    // Two-step probe: the binary must run at all, and must support the
    // sandbox flag this design depends on (guards against a partial build
    // or an unrelated `ash` shadowing it on PATH).
    let version = Command::new(&path).arg("--version").output().ok()?;
    if !version.status.success() {
        return None;
    }
    let help = Command::new(&path).arg("--help").output().ok()?;
    let help_text = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    if !help_text.contains("--sandbox") {
        return None;
    }
    Some(AshInfo {
        path,
        version: String::from_utf8_lossy(&version.stdout).trim().to_string(),
    })
}

/// Discovery chain (design §5.1): `AUTO_AI_ASH_BIN` → PATH scan → sibling
/// repo heuristic. First hit wins. An explicit env override is authoritative:
/// if it points at a missing file we return `None` without falling through,
/// so the env var doubles as the test injection point for "ash absent".
fn discover_from(
    env_bin: &str,
    path_var: &str,
    exe_dir: Option<PathBuf>,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    let env_bin = env_bin.trim();
    if !env_bin.is_empty() {
        let p = PathBuf::from(env_bin);
        return if is_executable_file(&p) { Some(p) } else { None };
    }
    if let Some(p) = find_on_path(path_var) {
        return Some(p);
    }
    // Dev-layout fallback: walk up ≤3 levels from the executable's directory
    // and from the working directory looking for a sibling auto-shell
    // checkout (covers D:\autostack\{auto-ai,auto-shell} side by side).
    // release is preferred over debug.
    for root in [exe_dir.as_deref(), cwd.as_deref()].into_iter().flatten() {
        if let Some(p) = find_sibling_ash(root) {
            return Some(p);
        }
    }
    None
}

fn exe_candidates() -> &'static [&'static str] {
    if cfg!(windows) {
        &["ash.exe", "ash"]
    } else {
        &["ash"]
    }
}

fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

fn find_on_path(path_var: &str) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        for name in exe_candidates() {
            let cand = dir.join(name);
            if is_executable_file(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

fn find_sibling_ash(start: &Path) -> Option<PathBuf> {
    // ancestors() yields `start` itself first; take(4) = start + 3 levels up.
    for ancestor in start.ancestors().take(4) {
        for profile in ["release", "debug"] {
            for name in exe_candidates() {
                let cand = ancestor.join("auto-shell").join("ash").join("target")
                    .join(profile).join(name);
                if is_executable_file(&cand) {
                    return Some(cand);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Unique temp dir per test (no tempfile dep); caller cleans up.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("auto-ai-shell-exec-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).expect("parent dir");
        fs::write(p, b"").expect("dummy ash file");
    }

    #[test]
    fn env_override_found_beats_path_and_sibling() {
        let root = temp_root("env-found");
        let ash = root.join("custom-ash.exe");
        touch(&ash);
        let path_dir = root.join("onpath");
        touch(&path_dir.join("ash.exe"));
        let got = discover_from(
            ash.to_str().unwrap(),
            path_dir.to_str().unwrap(),
            Some(root.clone()),
            Some(root.clone()),
        );
        assert_eq!(got.as_deref(), Some(ash.as_path()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn env_override_missing_is_authoritative_no_fallthrough() {
        let root = temp_root("env-missing");
        let path_dir = root.join("onpath");
        touch(&path_dir.join("ash.exe"));
        let got = discover_from(
            root.join("no-such-ash.exe").to_str().unwrap(),
            path_dir.to_str().unwrap(),
            None,
            None,
        );
        assert_eq!(got, None, "explicit broken override must not fall through");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn path_scan_finds_ash() {
        let root = temp_root("path-scan");
        let dir = root.join("bin");
        touch(&dir.join("ash.exe"));
        let got = discover_from("", dir.to_str().unwrap(), None, None);
        assert_eq!(got.as_deref(), Some(dir.join("ash.exe").as_path()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sibling_heuristic_walks_up_and_prefers_release() {
        let root = temp_root("sibling");
        touch(&root.join("auto-shell/ash/target/release/ash.exe"));
        touch(&root.join("auto-shell/ash/target/debug/ash.exe"));
        let start = root.join("proj/a/b"); // 3 levels below root
        let got = discover_from("", "", Some(start.clone()), None);
        assert_eq!(
            got.as_deref(),
            Some(root.join("auto-shell/ash/target/release/ash.exe").as_path())
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sibling_heuristic_respects_depth_limit() {
        let root = temp_root("sibling-depth");
        touch(&root.join("auto-shell/ash/target/release/ash.exe"));
        let start = root.join("a/b/c/d"); // 4 levels below root: out of reach
        let got = discover_from("", "", Some(start), None);
        assert_eq!(got, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_available_returns_none() {
        let root = temp_root("none");
        let empty = root.join("empty");
        fs::create_dir_all(&empty).expect("dir");
        assert_eq!(discover_from("", empty.to_str().unwrap(), None, None), None);
        let _ = fs::remove_dir_all(&root);
    }

    /// Live probe against the real environment. Skips (with a note) when no
    /// ash is installed so CI without auto-shell stays green.
    #[test]
    fn live_probe_when_ash_present() {
        match ash_available() {
            Some(info) => {
                assert!(!info.version.is_empty(), "probe captured a version string");
                assert!(info.path.is_file());
            }
            None => eprintln!("SKIP: no ash discovered in this environment"),
        }
    }
}
