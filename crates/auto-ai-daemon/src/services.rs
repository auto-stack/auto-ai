//! AutoOS service registry — aaid as the central service discovery + launch hub.
//!
//! Like Android Intent: apps can ask aaid to discover/start/redirect to other
//! AutoOS services (auto-os-config, auto-musk web, future apps).
//!
//! Each service has a URL (for probing), a start command + working directory
//! (for lazy-start), and a ready-check path. The `ensure` method probes the URL
//! and, if unreachable, spawns the start command in the right directory and
//! waits for it to become ready.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// One registered AutoOS service.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceEntry {
    /// Unique id: "os-config", "musk-web".
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// URL to probe for liveness (e.g. "http://localhost:17700").
    pub url: String,
    /// Shell command to start the service (e.g. "npm run dev").
    pub start_cmd: String,
    /// Relative path hint to find the working directory from aaid's CWD
    /// (e.g. "auto-os-config", "auto-musk/web").
    pub cwd_hint: String,
    /// HTTP path appended to `url` for the ready check (e.g. "/").
    pub ready_path: String,
}

/// The service registry. Currently hardcoded with sensible defaults; a future
/// enhancement can load from `~/.config/autoos/services.at`.
#[derive(Clone, Debug, Default)]
pub struct ServiceRegistry {
    services: Vec<ServiceEntry>,
}

impl ServiceRegistry {
    /// Hardcoded default services (the known AutoOS components).
    pub fn load() -> Self {
        Self {
            services: vec![
                ServiceEntry {
                    id: "os-config".into(),
                    name: "AutoOS Settings".into(),
                    url: "http://localhost:17700".into(),
                    start_cmd: "npm run dev".into(),
                    cwd_hint: "auto-os-config".into(),
                    ready_path: "/".into(),
                },
                ServiceEntry {
                    id: "musk-web".into(),
                    name: "Auto Musk Web".into(),
                    url: "http://localhost:8090".into(),
                    start_cmd: "npm run dev".into(),
                    cwd_hint: "auto-musk/web".into(),
                    ready_path: "/".into(),
                },
            ],
        }
    }

    /// Look up a service by id.
    pub fn get(&self, id: &str) -> Option<&ServiceEntry> {
        self.services.iter().find(|s| s.id == id)
    }

    /// All registered services.
    pub fn list(&self) -> &[ServiceEntry] {
        &self.services
    }

    /// Ensure a service is running. Probes the URL first; if unreachable,
    /// spawns the start command in the discovered working directory and waits
    /// for readiness (up to 15 seconds).
    ///
    /// Returns `Ok(url)` if the service is (or became) reachable, or `Err`
    /// with a human-readable reason.
    pub fn ensure(&self, id: &str) -> Result<String, String> {
        let svc = self.get(id).ok_or_else(|| format!("unknown service '{id}'"))?;

        // 1. Already running?
        if probe_url(&svc.url, &svc.ready_path) {
            return Ok(svc.url.clone());
        }

        // 2. Find the working directory.
        let cwd = find_service_cwd(&svc.cwd_hint).ok_or_else(|| {
            format!(
                "cannot find service directory for '{}' (hint: {}). \
                 Make sure the repo is checked out relative to auto-ai.",
                svc.id, svc.cwd_hint
            )
        })?;

        // 3. Spawn the start command detached.
        tracing::info!("starting service '{}' in {}", svc.id, cwd.display());
        spawn_service(&svc.start_cmd, &cwd)?;

        // 4. Wait for ready (up to 15s — vite dev server takes a few seconds).
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
            if probe_url(&svc.url, &svc.ready_path) {
                tracing::info!("service '{}' is ready at {}", svc.id, svc.url);
                return Ok(svc.url.clone());
            }
        }

        Err(format!(
            "service '{}' started but didn't become ready at {} within 15s",
            svc.id, svc.url
        ))
    }
}

/// Probe `url + path` with a quick GET. Returns true if HTTP 200.
///
/// Uses a raw TCP connect check (no reqwest) to avoid nested runtime issues
/// when called from async contexts. For the ensure() polling loop (which runs
/// in spawn_blocking), this is fine. For the list handler (async), we use
/// `probe_url_async`.
pub fn probe_url(url: &str, path: &str) -> bool {
    probe_url_blocking(url, path)
}

/// Blocking probe (safe in spawn_blocking context).
fn probe_url_blocking(url: &str, path: &str) -> bool {
    let full = format!("{url}{path}");
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1000))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(&full).send() {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Async probe (safe in async handler context). Uses a raw TCP connect to
/// avoid the nested-runtime issue with reqwest::blocking.
pub async fn probe_url_async(url: &str, _path: &str) -> bool {
    // Parse host:port from the URL.
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port().unwrap_or(80);
    let addr = format!("{host}:{port}");
    match tokio::net::TcpStream::connect(&addr).await {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Find the working directory for a service by searching relative paths.
/// Mirrors `find_aaid_binary` in daemon.rs: tries `../`, `../../`, etc.
fn find_service_cwd(hint: &str) -> Option<PathBuf> {
    // Search from aaid's current working directory upward.
    // In dev mode, aaid runs from auto-ai/, and sibling repos (auto-os-config,
    // auto-musk) are at ../auto-os-config, ../auto-musk.
    for base in &["..", "../..", "../../..", "../../../.."] {
        let candidate = PathBuf::from(base).join(hint);
        if candidate.join("package.json").exists() {
            return Some(candidate);
        }
    }
    None
}

/// Spawn a service command detached (the child outlives the caller).
fn spawn_service(cmd: &str, cwd: &std::path::Path) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let result = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", cmd])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    } else {
        Command::new("sh")
            .args(["-c", cmd])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };

    match result {
        Ok(child) => {
            tracing::info!("spawned service (pid {}): {} in {}", child.id(), cmd, cwd.display());
            drop(child); // detach
            Ok(())
        }
        Err(e) => Err(format!("failed to spawn '{cmd}' in {}: {e}", cwd.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_default_services() {
        let reg = ServiceRegistry::load();
        assert!(reg.get("os-config").is_some());
        assert!(reg.get("musk-web").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn ensure_unknown_service_errors() {
        let reg = ServiceRegistry::load();
        assert!(reg.ensure("nonexistent").is_err());
    }

    #[test]
    fn probe_returns_bool_without_panic() {
        // Just verify it doesn't panic on an unreachable URL.
        let _ = probe_url("http://127.0.0.1:59999", "/");
    }
}
