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
    CACHE.get_or_init(probe_ash).as_ref()
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

// ═══════════════════════════════════════════════════════════════════════════
// Failure classification (design §5.3) — drives the fallback matrix
// ═══════════════════════════════════════════════════════════════════════════

/// The four-way taxonomy of an ash invocation's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Exit 0 — the command succeeded.
    RanOk,
    /// ash's policy layer refused (sandbox path, read-only, no-network, deny).
    /// Never falls back: a fallback would bypass the sandbox.
    Denied,
    /// ash could not even start the command (unknown command, undefined
    /// symbol, parse error) — zero side effects, safe to fall back.
    PreExecFailure,
    /// The command ran and failed, or the outcome is ambiguous (unknown
    /// stderr shape, ash crashed). Conservative: no fallback.
    RanFailed,
}

/// stderr markers for policy denials, as emitted by ash v0.1.0 (verified
/// 2026-09-11). Note two distinct prefixes: `Error: security:` (capability
/// switches / deny lists — sometimes doubled) and `Error: sandbox:` (path
/// confinement).
const DENIED_MARKERS: &[&str] = &["Error: security:", "Error: sandbox:"];

/// stderr markers proving ash failed *before* executing anything.
const PRE_EXEC_MARKERS: &[&str] = &[
    "is not recognized",   // Windows: external command missing (PowerShell text)
    "command not found",   // Unix: external command missing
    "Undefined function:", // AutoLang: unknown function
    "Undefined variable:", // AutoLang: unknown variable in -c evaluation
    "Undefined command:",  // defensive: registry lookup failure
    "Parse error",         // ash parser failure
];

/// Classify an ash outcome from its exit code and full stderr. Unknown
/// non-zero shapes land in [`Classification::RanFailed`] on purpose: falling
/// back is a compatibility optimization, never worth double side effects.
pub fn classify(exit_code: Option<i32>, stderr: &str) -> Classification {
    match exit_code {
        Some(0) => Classification::RanOk,
        None => Classification::RanFailed,
        Some(_) => {
            if DENIED_MARKERS.iter().any(|m| stderr.contains(m)) {
                Classification::Denied
            } else if PRE_EXEC_MARKERS.iter().any(|m| stderr.contains(m)) {
                Classification::PreExecFailure
            } else {
                Classification::RanFailed
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Execution layer — subprocess with a hard timeout, and ash invocation
// assembly (design §5.2)
// ═══════════════════════════════════════════════════════════════════════════

/// What one subprocess run produced. `exit_code: None` + `timed_out` marks a
/// timeout kill; `spawn_error` is set when the process never started.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// Failure to spawn at all (binary missing, permission, ...).
    pub spawn_error: Option<String>,
}

impl ExecOutcome {
    pub fn classification(&self) -> Classification {
        // A timed-out command RAN — partial stderr may coincidentally contain
        // pre-exec markers (e.g. "command not found" inside the command's own
        // log output), and re-running it elsewhere would double side effects.
        // The timeout guard therefore wins over every stderr shape (R1 F-02).
        if self.timed_out {
            return Classification::RanFailed;
        }
        if self.spawn_error.is_some() {
            // ash itself unreachable is a pre-exec condition, but we treat it
            // conservatively at the tool layer; callers decide on fallback.
            return Classification::PreExecFailure;
        }
        classify(self.exit_code, &self.stderr)
    }
}

/// Run a prepared [`Command`] to completion with piped stdio and a hard
/// timeout. On timeout the (direct) child is killed; grandchildren may
/// survive on Windows — a known limitation, recorded in the design §5.2.
pub fn run_with_timeout(cmd: &mut Command, timeout_ms: u64) -> ExecOutcome {
    use std::io::Read;
    use std::thread;
    use std::time::{Duration, Instant};

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ExecOutcome {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                spawn_error: Some(format!("{e}")),
            }
        }
    };
    // Drain pipes on threads so a chatty child can't deadlock the poll loop.
    let out_handle = {
        let mut pipe = child.stdout.take().expect("piped stdout");
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let err_handle = {
        let mut pipe = child.stderr.take().expect("piped stderr");
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };

    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    let mut timed_out = false;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok().and_then(|s| s.code());
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break None,
        }
    };
    let stdout = String::from_utf8_lossy(&out_handle.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_handle.join().unwrap_or_default()).into_owned();
    ExecOutcome { exit_code, stdout, stderr, timed_out, spawn_error: None }
}

/// Policy flags + limits for one ash invocation (design §5.2).
#[derive(Debug, Clone, Default)]
pub struct AshOptions {
    /// Sandbox root (usually the canonicalized cwd); `None` = no confinement.
    pub sandbox_root: Option<PathBuf>,
    /// Pass `--no-network`.
    pub no_network: bool,
    /// `--audit <file>` destination.
    pub audit_file: Option<PathBuf>,
    /// Hard timeout in milliseconds.
    pub timeout_ms: u64,
}

impl AshOptions {
    /// Options for a normal run_command / run_ash_script invocation:
    /// sandbox = cwd, audit from `AUTO_AI_ASH_AUDIT` (unset = off).
    pub fn for_run(no_network: bool, timeout_ms: u64) -> AshOptions {
        AshOptions {
            sandbox_root: std::env::current_dir().ok().map(|mut p| {
                let _ = p.canonicalize().map(|c| p = c);
                p
            }),
            no_network,
            audit_file: std::env::var("AUTO_AI_ASH_AUDIT")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            timeout_ms,
        }
    }
}

/// What to feed ash: either a one-liner (`-c`) or a script file plus args.
#[derive(Debug, Clone)]
pub enum AshArgs {
    Cmd(String),
    Script { path: PathBuf, args: Vec<String> },
}

/// Build the ash command line. Pure — arg assembly is unit-tested per AC-01.
pub fn ash_invocation(ash: &Path, args: &AshArgs, opts: &AshOptions) -> Command {
    let mut c = Command::new(ash);
    if let Some(root) = &opts.sandbox_root {
        c.arg("--sandbox").arg(root);
    }
    if opts.no_network {
        c.arg("--no-network");
    }
    if let Some(audit) = &opts.audit_file {
        c.arg("--audit").arg(audit);
    }
    match args {
        AshArgs::Cmd(s) => {
            c.arg("-c").arg(s);
        }
        AshArgs::Script { path, args } => {
            c.arg(path);
            for a in args {
                c.arg(a);
            }
        }
    }
    c
}

/// Convenience: run one command line through ash with the given options.
pub fn execute_via_ash(ash: &Path, cmd: &str, opts: &AshOptions) -> ExecOutcome {
    let mut invocation = ash_invocation(ash, &AshArgs::Cmd(cmd.to_string()), opts);
    let mut out = run_with_timeout(&mut invocation, opts.timeout_ms);
    if out.timed_out {
        // Keep the classification conservative: a timeout is never a
        // pre-exec failure (the command may have had partial effects).
        out.stderr = format!("{}[ash timed out after {}ms]", out.stderr, opts.timeout_ms);
    }
    out
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

    // ── FailureClassifier fixtures ────────────────────────────────────────
    // All samples below are REAL ash v0.1.0 stderr output, captured
    // 2026-09-11 (design §3.2). They pin the classifier's stderr contract;
    // if ash changes its wording these tests are the tripwire.

    #[test]
    fn classify_ran_ok_on_zero_exit() {
        assert_eq!(classify(Some(0), ""), Classification::RanOk);
        assert_eq!(classify(Some(0), "some warning on stderr"), Classification::RanOk);
    }

    #[test]
    fn classify_denied_policy_rejections() {
        // --read-only blocking a write command (exact capture)
        assert_eq!(
            classify(Some(1), "Error: security: write command 'rm' blocked by --read-only"),
            Classification::Denied
        );
        // --sandbox path confinement (exact capture, \\?\-style paths)
        assert_eq!(
            classify(
                Some(1),
                "Error: sandbox: \\\\?\\D:\\d\\autostack\\auto-ai\\README.md is outside sandbox \\\\?\\C:\\Users\\zhaop"
            ),
            Classification::Denied
        );
        // --no-network (note ash doubles the prefix: "security: security:")
        assert_eq!(
            classify(Some(1), "Error: security: security: network command 'curl' blocked by --no-network"),
            Classification::Denied
        );
        // --deny name list
        assert_eq!(
            classify(Some(1), "Error: security: 'cargo' is denied by --deny"),
            Classification::Denied
        );
    }

    #[test]
    fn classify_pre_exec_failures() {
        // Windows: external command missing — full PowerShell text (truncated)
        assert_eq!(
            classify(
                Some(1),
                "definitely_not_a_cmd_xyz : The term 'definitely_not_a_cmd_xyz' is not recognized \
                 as the name of a cmdlet, function, script file, or operable program."
            ),
            Classification::PreExecFailure
        );
        // Unix shape
        assert_eq!(
            classify(Some(1), "sh: definitely_not_a_cmd_xyz: command not found"),
            Classification::PreExecFailure
        );
        // AutoLang symbol lookup failures (exact captures)
        assert_eq!(
            classify(Some(1), "Error: Undefined function: println"),
            Classification::PreExecFailure
        );
        assert_eq!(
            classify(Some(1), "Error: Undefined variable: undefined_thing"),
            Classification::PreExecFailure
        );
    }

    #[test]
    fn classify_ran_failed_for_unknown_or_real_failures() {
        // A command that genuinely ran and failed (cargo-style error text)
        assert_eq!(
            classify(Some(101), "error: could not compile `auto-ai-cli` due to 2 previous errors"),
            Classification::RanFailed
        );
        // Unknown stderr shape → conservative
        assert_eq!(classify(Some(1), "weird new ash error format"), Classification::RanFailed);
        // ash itself crashed before reporting an exit code → conservative
        assert_eq!(classify(None, ""), Classification::RanFailed);
    }

    #[test]
    fn classify_denied_wins_over_pre_exec_when_both_present() {
        // Denial check runs first: a stderr mixing both shapes (e.g. a denied
        // command whose name also looks unknown) must never fall back.
        assert_eq!(
            classify(
                Some(1),
                "Error: security: 'x' is denied by --deny\nThe term 'x' is not recognized"
            ),
            Classification::Denied
        );
    }

    // ── Execution layer ───────────────────────────────────────────────────

    fn invocation_args(c: &Command) -> Vec<String> {
        // std::process::Command has no arg getter; use its Debug form
        // `"prog" "arg1" "arg2"` — after split('"') the args are the quoted
        // fragments at indices 3, 5, 7, … (1 is the program, even are spaces).
        format!("{c:?}")
            .split('"')
            .skip(3)
            .step_by(2)
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn ash_invocation_assembles_flags_in_order() {
        let opts = AshOptions {
            sandbox_root: Some(PathBuf::from("/tmp/proj")),
            no_network: true,
            audit_file: Some(PathBuf::from("/tmp/audit.jsonl")),
            timeout_ms: 1000,
        };
        let c = ash_invocation(
            Path::new("/bin/ash"),
            &AshArgs::Cmd("ls -la".into()),
            &opts,
        );
        let args = invocation_args(&c);
        assert_eq!(
            args,
            vec![
                "--sandbox", "/tmp/proj",
                "--no-network",
                "--audit", "/tmp/audit.jsonl",
                "-c", "ls -la",
            ]
        );
    }

    #[test]
    fn ash_invocation_minimal_and_script_form() {
        let opts = AshOptions { timeout_ms: 50, ..Default::default() };
        let c = ash_invocation(Path::new("ash"), &AshArgs::Cmd("true".into()), &opts);
        assert_eq!(invocation_args(&c), vec!["-c", "true"]);

        let c = ash_invocation(
            Path::new("ash"),
            &AshArgs::Script {
                path: PathBuf::from("s.ash"),
                args: vec!["a1".into(), "a2".into()],
            },
            &opts,
        );
        assert_eq!(invocation_args(&c), vec!["s.ash", "a1", "a2"]);
    }

    #[test]
    fn spawn_error_is_pre_exec_failure() {
        let mut c = Command::new("definitely-no-such-binary-xyz");
        let out = run_with_timeout(&mut c, 100);
        assert!(out.spawn_error.is_some());
        assert_eq!(out.classification(), Classification::PreExecFailure);
    }

    /// R1 F-02 regression: a timed-out command RAN, so even if its partial
    /// stderr coincidentally contains a pre-exec marker (e.g. the command's
    /// own log output saying "command not found"), it must classify as
    /// RanFailed — falling back would re-run a command with side effects.
    #[test]
    fn timed_out_wins_over_pre_exec_markers() {
        let out = ExecOutcome {
            exit_code: Some(1),
            stdout: "partial build log".into(),
            stderr: "make: tool-not-there: command not found".into(),
            timed_out: true,
            spawn_error: None,
        };
        assert_eq!(out.classification(), Classification::RanFailed);
        // Same stderr without the timeout is (correctly) still a fallback
        // candidate at the pure-classifier level.
        assert_eq!(classify(Some(1), "make: tool-not-there: command not found"),
                   Classification::PreExecFailure);
    }

    /// Live: real ash honors the timeout (kill + timed_out flag, no fallback).
    #[test]
    fn live_ash_timeout_kills_process() {
        let Some(info) = ash_available() else {
            eprintln!("SKIP: no ash discovered in this environment");
            return;
        };
        // ping exists on Windows and Unix-ish shells alike; ash delegates it
        // to the OS. 10 pings ≈ 9s, deadline 300ms.
        let out = execute_via_ash(&info.path, "ping -n 10 127.0.0.1", &AshOptions {
            timeout_ms: 300,
            ..Default::default()
        });
        assert!(out.timed_out, "expected a timeout kill");
        assert_eq!(out.classification(), Classification::RanFailed);
    }
}
