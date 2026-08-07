//! `aaid-a2r` — AutoOS AI Daemon binary (hand-written bin entry, references
//! the Auto-transpiled lib `auto_ai_daemon_a2r`).
//!
//! Plan 025 Phase 3 decision: the bin target stays hand-written Rust (same
//! pattern as auto-ai-agent/rust/src/main.rs, which also keeps a hand-written
//! bin over a transpiled lib). Reasons:
//! - a2r renders `println`/`eprintln` as function calls, but they're macros in
//!   Rust (`println!`) — a codegen gap that'd need fragile sed rewrites.
//! - a2r routes `env.args()` to `a2r_std::env::args`, which doesn't exist
//!   (a2r_std has no process/args API).
//! - `#[tokio::main]` (dotted .at attr) double-emits (Phase 0 spike #3).
//! The axum validation milestone lives in server.at (the lib); the bin is pure
//! glue (arg parse + config load + bind + serve) and gains nothing from
//! transpilation. Mirrors rust-ref src/main.rs:1-98.

use std::sync::Arc;

use auto_ai_daemon_a2r::config;
use auto_ai_daemon_a2r::server::{router, AppState};

#[tokio::main]
async fn main() {
    // Parse CLI args (minimal, no clap dependency — same as rust-ref).
    let mut listen_override: Option<String> = None;
    let mut config_path: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen_override = args.next(),
            "--config" => config_path = args.next(),
            "--log-level" => {
                let _ = args.next(); // accepted but unused (println logging)
            }
            "--help" | "-h" => {
                println!("aaid — AutoOS AI Daemon");
                println!();
                println!("USAGE:");
                println!("  aaid [OPTIONS]");
                println!();
                println!("OPTIONS:");
                println!("  --listen <addr>    Override listen address (default: 127.0.0.1:17654)");
                println!("  --config <path>    Config file path (default: ~/.config/autoos/ai-daemon.at)");
                println!("  --log-level <lvl>  Log level: trace/debug/info/warn/error");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    // Load config (explicit --config path wins; else the default file/env chain).
    let mut cfg = if let Some(path) = &config_path {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|_| panic!("cannot read config: {path}"));
        ai_config::parse_daemon_config(&content)
            .unwrap_or_else(|e| panic!("failed to parse config {path}: {e}"))
    } else {
        config::load()
    };

    if let Some(addr) = &listen_override {
        cfg.listen_addr = addr.clone();
    }

    if cfg.providers.is_empty() {
        eprintln!("aaid: no providers configured.");
        eprintln!("  Set env vars (ZHIPU_API_KEY / ANTHROPIC_API_KEY / OPENAI_API_KEY)");
        eprintln!("  or create ~/.config/autoos/ai-daemon.at");
        std::process::exit(1);
    }

    let listen_addr = cfg.listen_addr.clone();
    let state = Arc::new(AppState::new(cfg));

    // Startup banner (rust-ref uses tracing::info!; the transpiled build uses
    // println — a2r can't emit macros, Phase 0 spike #4).
    println!("aaid listening on http://{listen_addr}");

    // Build router (takes ownership of the Arc) and serve.
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .expect(&format!("failed to bind {listen_addr}"));
    axum::serve(listener, app)
        .await
        .expect("server error");
}
