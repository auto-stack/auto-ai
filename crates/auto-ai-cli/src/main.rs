//! auto-ai-cli — interactive agent demo for the AutoOS AI stack.
//!
//! Usage:
//!   auto-ai-cli chat                    TUI chat (assistant auto-routes)
//!   auto-ai-cli chat --mode relay       Force a specific execution mode
//!   auto-ai-cli run "<task>"            One-shot task (default role: assistant)
//!   auto-ai-cli pipeline "<task>"       Multi-agent pipeline demo
//!   auto-ai-cli roles                   List available built-in roles
//!
//! Prerequisites: `aaid` must be running (cargo run -p auto-ai-daemon).

pub mod chat_model;
pub mod markdown;
pub mod tui;
pub mod tools;
mod spawn_pipeline;

use std::io::{self, BufRead, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use auto_ai_agent::{
    Agent, Client, StreamEvent, Tool,
    AgentFactory, FlowSpec, FlowStep, GateType, GateDecision,
    HandoffDocument, PipelineDriver, PipelineEvent, RoleRegistry,
};
use auto_ai_client::AiClient;

#[derive(Parser)]
#[command(
    name = "auto-ai-cli",
    version,
    about = "Interactive agent demo for the AutoOS AI stack",
    // No subcommand → default to `chat` (like common interactive CLIs).
    subcommand_required = false,
    arg_required_else_help = false,
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Interactive multi-turn chat REPL (assistant auto-routes to normal/superpowers/relay).
    Chat {
        /// Execution mode: normal (auto-route), superpowers, relay.
        #[arg(long, default_value = "normal")]
        mode: String,
    },
    /// Run a single task (one-shot).
    Run {
        task: String,
        #[arg(long, default_value = "assistant")]
        role: String,
    },
    /// Run a multi-agent pipeline (Plan 008 orchestration demo).
    Pipeline {
        task: String,
    },
    /// List available built-in roles.
    Roles,
}

fn main() {
    let cli = Cli::parse();
    // No subcommand → default to `chat` (normal/TUI mode).
    match cli.cmd.unwrap_or(Cmd::Chat { mode: "normal".into() }) {
        Cmd::Roles => {
            let registry = RoleRegistry::load();
            println!("Available roles:");
            for s in registry.list() {
                let kind = if s.is_builtin { "builtin" } else { "user" };
                println!(
                    "  {name:<14} tier={tier:<4} [{kind}]",
                    name = s.name,
                    tier = format!("{:?}", s.tier).to_lowercase(),
                    kind = kind,
                );
            }
        }
        Cmd::Run { task, role } => {
            let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
            if let Err(e) = rt.block_on(run_task(&task, &role)) {
                eprintln!("auto-ai-cli: {e}");
                std::process::exit(1);
            }
        }
        Cmd::Chat { mode } => {
            let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
            // Normal mode → TUI. Forced mode → legacy text REPL.
            if mode == "normal" {
                // Launch TUI.
                if let Err(e) = rt.block_on(tui::run_tui_chat("assistant")) {
                    eprintln!("auto-ai-cli: {e}");
                    std::process::exit(1);
                }
            } else {
                // Legacy text-based chat for superpowers/relay modes.
                if let Err(e) = rt.block_on(chat_loop(&mode)) {
                    eprintln!("auto-ai-cli: {e}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Pipeline { task } => {
            let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
            if let Err(e) = rt.block_on(run_pipeline(&task)) {
                eprintln!("auto-ai-cli: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Build the client. Use with_url to avoid the blocking ensure_daemon
/// (which creates a nested tokio runtime).
fn build_client() -> Arc<dyn Client> {
    Arc::new(AiClient::with_url(daemon_url()))
}

/// Resolve the aaid daemon URL (env override or default).
fn daemon_url() -> String {
    std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into())
}

// ── Startup banner (Session ID + debug info) ──────────────────────────────

/// Generate a zero-dependency session id: `session_YYYYMMDD-HHMMSS-PID`.
/// Uses `SystemTime` (Unix epoch) converted to civil time via the
/// well-known days-from-civil algorithm, plus the OS process id. This is
/// both human-readable (so logs are easy to scan) and unique enough per run.
fn generate_session_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let pid = std::process::id();

    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    let (y, mo, d) = civil_from_days(days);
    format!("session-{y:04}{mo:02}{d:02}-{hh:02}{mm:02}{ss:02}-{pid}")
}

/// Convert days since 1970-01-01 to (year, month, day) — Howard Hinnant's
/// days_from_civil algorithm in reverse. Pure arithmetic, no deps.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]: day of era
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]: month offset from March
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Build the startup banner shown by every interactive subcommand.
/// Resolves `role` (e.g. "assistant") to surface the model tier/id in use.
pub fn build_banner(role: &str) -> String {
    let dir = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let session = generate_session_id();
    let version = env!("CARGO_PKG_VERSION");
    let daemon = daemon_url();

    // Model line: resolve role → tier (+ concrete model override if set).
    let model_line = {
        let registry = RoleRegistry::load();
        let resolved = registry.resolve_role(role);
        match resolved {
            Some(r) => {
                let tier = format!("{:?}", r.model_tier()).to_lowercase();
                let model = r.model();
                let model_desc = if model.is_empty() { "auto".into() } else { model.to_string() };
                format!("{role} (tier={tier}, model={model_desc})")
            }
            None => format!("{role} (unresolved)"),
        }
    };

    format!(
        "┌─ auto-ai-cli ───────────────────────────────\n\
         │  Directory: {dir}\n\
         │  Session:   {session}\n\
         │  Model:     {model_line}\n\
         │  Daemon:    {daemon}\n\
         │  Version:   {version}\n\
         └──────────────────────────────────────────────"
    )
}

/// Build an agent from a role name, resolving user overrides when available.
/// When `with_pipeline` is true, also registers the spawn_pipeline tool
/// (enables the assistant to auto-route to superpowers/relay flows).
fn build_agent(role_name: &str, client: Arc<dyn Client>, with_pipeline: bool) -> Result<Agent, String> {
    let registry = RoleRegistry::load();
    let role = registry
        .resolve_role(role_name)
        .ok_or_else(|| format!("unknown role '{role_name}'"))?;
    // OwnedRole wrapper: Agent::new needs Role + 'static.
    let mut agent = Agent::new(OwnedRole(role), client.clone());
    agent.register_tool(tools::ReadFile);
    agent.register_tool(tools::WriteFile);
    agent.register_tool(tools::EditFile);
    agent.register_tool(tools::ListDir);
    agent.register_tool(tools::Search);
    agent.register_tool(tools::RunCommand);
    if with_pipeline {
        agent.register_tool(spawn_pipeline::SpawnPipeline::new(client));
    }
    Ok(agent)
}

/// Thin wrapper to satisfy Agent::new's `P: Role + 'static` bound.
/// (load_builtin returns Arc<dyn Role>; Arc<dyn Trait> doesn't impl the trait.)
pub struct OwnedRole(Arc<dyn auto_ai_agent::Role>);

impl auto_ai_agent::Role for OwnedRole {
    fn name(&self) -> &str { self.0.name() }
    fn system_prompt(&self) -> &str { self.0.system_prompt() }
    fn model_tier(&self) -> auto_ai_agent::ModelTier { self.0.model_tier() }
    fn model(&self) -> &str { self.0.model() }
    fn temperature(&self) -> f64 { self.0.temperature() }
    fn max_turns(&self) -> usize { self.0.max_turns() }
    fn allowed_tools(&self) -> Vec<String> { self.0.allowed_tools() }
    fn memory_limit(&self) -> Option<usize> { self.0.memory_limit() }
    fn allowed_tiers(&self) -> Vec<auto_ai_agent::ModelTier> { self.0.allowed_tiers() }
    fn token_budget(&self) -> Option<u64> { self.0.token_budget() }
    fn skills(&self) -> Vec<String> { self.0.skills() }
    fn handoff_to(&self) -> Vec<String> { self.0.handoff_to() }
    fn dispatchable_to(&self) -> Vec<String> { self.0.dispatchable_to() }
    fn approval_gates(&self) -> Vec<String> { self.0.approval_gates() }
}

/// One-shot task: run the agent and print the result.
async fn run_task(task: &str, role: &str) -> Result<(), String> {
    let client = build_client();
    let mut agent = build_agent(role, client, false)?;

    let role_display = role.to_string();
    println!("{}", build_banner(&role_display));
    println!("auto-ai-cli: running role '{role_display}' on task:\n  {task}\n");

    let result = agent.run(task).await.map_err(|e| format_agent_error(&e))?;

    println!(
        "──── result ({} turn{}, {} tool call{}) ────",
        result.turns,
        if result.turns == 1 { "" } else { "s" },
        result.tool_calls.len(),
        if result.tool_calls.len() == 1 { "" } else { "s" }
    );
    println!("{}", result.output);

    if !result.tool_calls.is_empty() {
        println!("\n──── tool calls ────");
        for (i, tc) in result.tool_calls.iter().enumerate() {
            let preview: String = tc.result.chars().take(120).collect();
            let ellipsis = if tc.result.len() > 120 { "…" } else { "" };
            println!("  {}. {} → {}{}", i + 1, tc.tool, preview, ellipsis);
        }
    }
    Ok(())
}

/// Interactive multi-turn chat REPL with streaming output.
/// mode: "normal" (assistant auto-routes), "superpowers", "relay".
async fn chat_loop(mode: &str) -> Result<(), String> {
    let client = build_client();

    // If a specific mode is requested, start the pipeline directly.
    if mode == "superpowers" || mode == "relay" {
        println!("{}", build_banner("assistant"));
        println!("auto-ai-cli — mode: {mode} (pipeline will start on first message)");
        let stdin = io::stdin();
        loop {
            print!("\ntask> ");
            io::stdout().flush().ok();
            let mut line = String::new();
            let n = stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
            if n == 0 { break; }
            let task = line.trim();
            if task.is_empty() { continue; }
            if task == "exit" || task == "quit" { break; }
            run_pipeline_flow(mode, task, &client).await?;
        }
        return Ok(());
    }

    // Normal mode: assistant with spawn_pipeline tool (auto-routes).
    let mut agent = build_agent("assistant", client.clone(), true)?;

    println!("{}", build_banner("assistant"));
    println!(
        "auto-ai-cli chat — mode: normal (assistant auto-routes)"
    );
    println!("  The assistant will decide: direct answer, superpowers, or relay.");
    println!("  (exit/quit or Ctrl-D to leave)");

    let stdin = io::stdin();
    let turn = Arc::new(std::sync::atomic::AtomicU32::new(0));

    loop {
        print!("\n你> ");
        io::stdout().flush().ok();
        let mut line = String::new();
        let n = stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        let input = line.trim();
        if input.is_empty() { continue; }
        if input == "exit" || input == "quit" { break; }

        // Slash commands.
        if input.starts_with('/') {
            match input {
                "/config" | "/settings" => {
                    open_config(&client).await;
                    continue;
                }
                "/help" => {
                    print_slash_help();
                    continue;
                }
                "/roles" => {
                    let registry = RoleRegistry::load();
                    println!("\nAvailable roles:");
                    for s in registry.list() {
                        let kind = if s.is_builtin { "builtin" } else { "user" };
                        println!("  {name:<14} tier={tier:<4} [{kind}]",
                            name = s.name,
                            tier = format!("{:?}", s.tier).to_lowercase(),
                            kind = kind,
                        );
                    }
                    continue;
                }
                _ => {
                    println!("Unknown command: {input}. Type /help for commands.");
                    continue;
                }
            }
        }

        let current_turn = turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        println!("\nassistant ───");

        // Streaming on_event callback.
        let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |ev| {
            match ev {
                StreamEvent::Delta { text } => {
                    print!("{text}");
                    let _ = io::stdout().flush();
                }
                StreamEvent::Thinking { text } => {
                    // Intermediate ReAct reasoning — print dimmed so it's
                    // visually distinct from the final answer delta stream.
                    print!("\x1b[2m{text}\x1b[0m");
                    let _ = io::stdout().flush();
                }
                StreamEvent::ToolStart { tool, args } => {
                    let hint = match tool.as_str() {
                        "run_command" => args.get("cmd").and_then(|c| c.as_str())
                            .map(|c| c.chars().take(60).collect::<String>()).unwrap_or_default(),
                        "write_file" | "edit_file" | "read_file" | "list_dir" =>
                            args.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string(),
                        _ => String::new(),
                    };
                    println!("\n  [running] {tool}  {hint}…");
                }
                StreamEvent::Tool { tool, args, result, .. } => {
                    // Show the tool name + key args (for debugging).
                    let arg_preview = if tool == "run_command" {
                        args.get("cmd").and_then(|c| c.as_str())
                            .map(|c| format!(" cmd={}", &c.chars().take(60).collect::<String>()))
                            .unwrap_or_default()
                    } else if tool == "write_file" || tool == "edit_file" {
                        args.get("path").and_then(|p| p.as_str())
                            .map(|p| format!(" path={p}"))
                            .unwrap_or_default()
                    } else if tool == "read_file" {
                        args.get("path").and_then(|p| p.as_str())
                            .map(|p| format!(" path={p}"))
                            .unwrap_or_default()
                    } else if tool == "spawn_pipeline" {
                        let flow = args.get("flow").and_then(|f| f.as_str()).unwrap_or("?");
                        format!(" flow={flow}")
                    } else {
                        String::new()
                    };
                    let result_preview: String = result.lines().next().unwrap_or("").chars().take(80).collect();
                    let ellipsis = if result.len() > 80 { "…" } else { "" };
                    println!("\n  [tool] {tool}{arg_preview}");
                    println!("  │ {result_preview}{ellipsis}");
                }
                StreamEvent::Done { result } => {
                    let n = result.tool_calls.len();
                    println!(
                        "\n──── turn {current_turn}, {n} tool call{} ────",
                        if n == 1 { "" } else { "s" }
                    );
                }
                StreamEvent::Error { message } => {
                    println!("\n  [error] {message}");
                }
                // The legacy text REPL doesn't support cancellation; the TUI is
                // the only consumer that wires up a real cancel handle.
                StreamEvent::Cancelled { .. } => {
                    println!("\n  [cancelled]");
                }
            }
        });

        let no_cancel = Arc::new(AtomicBool::new(false));
        match agent.run_stream(input, on_event, no_cancel).await {
            Ok(_) => {}
            Err(e) => {
                eprintln!("\n{}", format_agent_error(&e));
                eprintln!("  (session continues; type another message or 'exit')");
            }
        }
    }
    Ok(())
}

// ── Error formatting ──────────────────────────────────────────────────────

/// Human-friendly error rendering. Detects rate limits, auth failures, and
/// network issues, showing actionable guidance instead of raw JSON.
fn format_agent_error(e: &dyn std::fmt::Display) -> String {
    let msg = e.to_string();

    // ── Rate limit / quota exhausted ────────────────────────────────────
    if msg.contains("429") || msg.contains("rate_limit") || msg.contains("使用上限") {
        let reset_hint = extract_zhipu_reset_time(&msg);
        let mut out = String::from(
            "🚫 API quota exhausted (rate limit / 429).\n",
        );
        if !reset_hint.is_empty() {
            out.push_str(&format!("   {reset_hint}\n"));
        }
        out.push_str(
            "   Tips:\n\
             • Wait for quota to reset\n\
             • Switch model:  auto-ai-cli chat --role coder  (uses Max tier)\n\
             • Check usage:  visit https://open.bigmodel.cn"
        );
        return out;
    }

    // ── Authentication ──────────────────────────────────────────────────
    if msg.contains("401") || msg.contains("Unauthorized")
        || msg.contains("api_key") || msg.contains("invalid key")
    {
        return format!(
            "🔑 Authentication failed.\n\
             The API key in ~/.config/autoos/ai-daemon.at may be invalid.\n\
             Details: {msg}"
        );
    }

    // ── Network / connection ────────────────────────────────────────────
    if msg.contains("connection") || msg.contains("timeout")
        || msg.contains("refused") || msg.contains("dns")
    {
        return format!(
            "🌐 Network error — cannot reach the LLM API.\n\
             Check: curl http://127.0.0.1:17654/\n\
             Details: {msg}"
        );
    }

    // ── Daemon not running ──────────────────────────────────────────────
    if msg.contains("daemon") {
        return format!(
            "⚙️  Daemon (aaid) issue.\n\
             Start it:  cargo run -p auto-ai-daemon\n\
             Details: {msg}"
        );
    }

    // ── Generic fallback ────────────────────────────────────────────────
    format!("❌ {msg}")
}

/// Try to extract the quota-reset time from a Zhipu rate-limit error message.
fn extract_zhipu_reset_time(msg: &str) -> String {
    // Pattern: "您的限额将在 YYYY-MM-DD HH:MM:SS 重置"
    if let Some(start) = msg.find("您的限额将在 ") {
        let rest = &msg[start + "您的限额将在 ".len()..];
        if let Some(end) = rest.find(" 重置") {
            return format!("Quota resets at: {}", &rest[..end]);
        }
    }
    // Pattern: "quota will reset at …"
    for kw in &["reset at", "resets at"] {
        if let Some(pos) = msg.to_lowercase().find(kw) {
            let snippet: String = msg[pos..].chars().take(60).collect();
            return snippet;
        }
    }
    String::new()
}

// ── Pipeline (Plan 008 Phase 8) ───────────────────────────────────────────

/// Agent factory for the CLI — uses the same `build_agent` as chat/run.
struct CliAgentFactory {
    client: Arc<dyn Client>,
}

impl AgentFactory for CliAgentFactory {
    fn build_agent(
        &self,
        role_id: &str,
        _handoff: Option<&HandoffDocument>,
    ) -> Result<Agent, String> {
        build_agent(role_id, self.client.clone(), false)
    }
}

/// Built-in demo flow: assistant → coder → reviewer (with human gate).
fn create_demo_flow() -> FlowSpec {
    let mut flow = FlowSpec::new("demo-pipeline");
    flow.add_step(FlowStep::new("triage", "assistant"));
    flow.add_step(FlowStep::new("implementation", "coder"));
    flow.add_step(FlowStep::new("review", "reviewer").with_gate(GateType::Human));
    flow
}

/// Run a multi-agent pipeline with streaming output and interactive gate.
async fn run_pipeline(task: &str) -> Result<(), String> {
    let client = build_client();
    let factory = CliAgentFactory {
        client: client.clone(),
    };
    let flow = create_demo_flow();

    // Interactive gate handler — prompts the user for approval.
    let gate_handler: Box<dyn Fn(&str) -> GateDecision + Send + Sync> =
        Box::new(|step_id: &str| {
            println!();
            println!("⏸  Gate: step '{step_id}' requires human approval.");
            print!("   Approve? [Y/n/r(eject)]: ");
            let _ = io::stdout().flush();
            let mut line = String::new();
            if io::stdin().lock().read_line(&mut line).is_err() {
                return GateDecision::Approve; // EOF → approve
            }
            match line.trim().to_lowercase().as_str() {
                "r" | "reject" | "n" | "no" => {
                    print!("   Reason: ");
                    let _ = io::stdout().flush();
                    let mut fb = String::new();
                    let _ = io::stdin().lock().read_line(&mut fb);
                    GateDecision::Reject {
                        feedback: fb.trim().to_string(),
                    }
                }
                _ => GateDecision::Approve,
            }
        });

    let mut driver = PipelineDriver::new(flow, factory, task)
        .with_gate_handler(gate_handler);

    let task_owned = task.to_string();

    // Streaming event callback.
    let on_event: Arc<dyn Fn(PipelineEvent) + Send + Sync> = Arc::new(move |ev| match ev {
        PipelineEvent::StepStarted { step_id, role_id } => {
            println!("\n━━━ Step: {step_id} ({role_id}) ━━━");
        }
        PipelineEvent::Delta { text } => {
            print!("{text}");
            let _ = io::stdout().flush();
        }
        PipelineEvent::Tool { tool, result } => {
            let preview: String = result.chars().take(80).collect();
            let ellipsis = if result.len() > 80 { "…" } else { "" };
            println!("\n  [tool] {tool} → {preview}{ellipsis}");
        }
        PipelineEvent::StepCompleted { step_id, handoff } => {
            let summary: String = handoff.summary.chars().take(120).collect();
            let ellipsis = if handoff.summary.len() > 120 { "…" } else { "" };
            println!(
                "\n✓ Step '{step_id}' complete → {}{}",
                summary, ellipsis
            );
        }
        PipelineEvent::GateWaiting { .. } => {
            // The gate_handler already prompts; this is just a note.
        }
        PipelineEvent::Completed => {
            println!("\n══════ Pipeline completed ══════");
        }
        PipelineEvent::Failed { error } => {
            eprintln!("\n✗ Pipeline failed: {error}");
        }
        PipelineEvent::Paused { step_id, reason } => {
            println!("\n⏸  Pipeline paused at '{step_id}': {reason}");
        }
        PipelineEvent::BudgetWarning { remaining } => {
            println!("\n⚠  Budget warning: {remaining} tokens remaining");
        }
    });

    println!("{}", build_banner("assistant"));
    println!("auto-ai-cli pipeline:\n  {task_owned}\n");
    println!("Flow: assistant → coder → reviewer");

    driver
        .drive(&task_owned, on_event)
        .await
        .map_err(|e| format_agent_error(&e))
}

/// Run a named pipeline flow (superpowers / relay) directly.
/// Used by `chat --mode superpowers/relay`.
async fn run_pipeline_flow(mode: &str, task: &str, client: &Arc<dyn Client>) -> Result<(), String> {
    use auto_ai_agent::{AgentFactory, PipelineDriver, PipelineEvent};

    let flow = spawn_pipeline::flow_for(mode)
        .ok_or_else(|| format!("unknown mode '{mode}'"))?;
    let factory = spawn_pipeline::CliAgentFactory::new(client.clone());
    let mut driver = PipelineDriver::new(flow, factory, task);

    println!("\n┌─ pipeline: {mode} ─────────────────");
    println!("│ task: {task}");
    println!("│");

    let on_event: Arc<dyn Fn(PipelineEvent) + Send + Sync> = Arc::new(|ev| {
        match ev {
            PipelineEvent::StepStarted { step_id, role_id } => {
                println!("│ ▶ step '{step_id}' (role: {role_id})");
            }
            PipelineEvent::Delta { text } => {
                print!("{text}");
                let _ = io::stdout().flush();
            }
            PipelineEvent::Tool { tool, result } => {
                let preview: String = result.chars().take(60).collect();
                println!("\n│   [tool] {tool} → {preview}…");
            }
            PipelineEvent::StepCompleted { step_id, .. } => {
                println!("\n│ ✓ step '{step_id}' done");
            }
            PipelineEvent::Completed => {
                println!("│");
                println!("└─ pipeline complete ──────────────────");
            }
            PipelineEvent::Failed { error } => {
                println!("│ ✗ failed: {error}");
            }
            PipelineEvent::Paused { reason, .. } => {
                println!("│ ⏸ paused: {reason}");
            }
            PipelineEvent::GateWaiting { .. } => {
                println!("│ ⏸ gate (auto-approving…)");
            }
            PipelineEvent::BudgetWarning { remaining } => {
                println!("│ ⚠ budget: {remaining} tokens left");
            }
        }
    });

    driver.drive(task, on_event).await.map_err(|e| format_agent_error(&e))
}

/// Open the AutoOS settings UI (auto-os-config) in the browser.
/// Uses aaid's service registry to ensure ALL required services are running:
/// os-config (:17700) + musk (:8080, provides Roles/Skills/Agents config pages)
/// + aaid itself (:17654, provides AI Daemon config page).
async fn open_config(_client: &Arc<dyn Client>) {
    let daemon_url = std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into());
    let http = reqwest::Client::new();
    println!("\n  Starting AutoOS Settings…");

    // Ensure musk backend (provides Roles/Skills/Agents/Auto Musk config pages).
    match http.post(format!("{}/v1/services/musk-web/ensure", daemon_url))
        .timeout(std::time::Duration::from_secs(20)).send().await
    {
        Ok(resp) if resp.status().is_success() => {
            println!("  ✓ Musk web ready");
        }
        _ => {
            println!("  ⚠ Musk backend not reachable — Roles/Skills pages won't load.");
            println!("    Start it: cd auto-musk/backend && cargo run -p musk -- serve");
        }
    }

    // Note: musk-web (:3333) is the frontend SPA, not the backend (:8080).
    // The config pages are served by musk serve (:8080). We need to ensure
    // the backend is running. aaid's registry has musk-web (:3333) but not
    // the backend (:8080) — so we check :8080 directly.
    match http.get("http://127.0.0.1:8080/api/health")
        .timeout(std::time::Duration::from_secs(3)).send().await
    {
        Ok(r) if r.status().is_success() => {
            println!("  ✓ Musk backend (:8080) ready");
        }
        _ => {
            println!("  ⚠ Musk backend (:8080) not running — config pages won't load.");
            println!("    Start it: cd auto-musk/backend && cargo run -p musk -- serve");
        }
    }

    // Ensure os-config (:17700) — the settings UI host.
    match http.post(format!("{}/v1/services/os-config/ensure", daemon_url))
        .timeout(std::time::Duration::from_secs(20)).send().await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(val) = resp.json::<serde_json::Value>().await {
                let url = val.get("url").and_then(|u| u.as_str()).unwrap_or("http://localhost:17700");
                let status = val.get("status").and_then(|s| s.as_str()).unwrap_or("running");
                if status == "running" {
                    let full = format!("{}/", url);
                    println!("  ✓ AutoOS Settings ready at {full}");
                    let _ = open_browser(&full);
                    return;
                }
            }
            println!("  ⚠ Unexpected response from aaid. Try http://localhost:17700 manually.");
        }
        Ok(resp) => {
            println!("  ⚠ aaid responded HTTP {}. Is os-config installed?", resp.status());
        }
        Err(_) => {
            println!("  ⚠ aaid not reachable at {daemon_url}. Trying direct…");
            let url = "http://localhost:17700/";
            let _ = open_browser(url);
            println!("  Opened {url} (if os-config isn't running, start it: cd auto-os-config && npm run dev)");
        }
    }
}

/// Open a URL in the default browser (cross-platform).
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}

/// Print available slash commands.
fn print_slash_help() {
    println!("\n  Slash commands:");
    println!("    /config   Open AutoOS Settings in browser");
    println!("    /roles    List available built-in roles");
    println!("    /help     Show this help");
    println!("    exit/quit Leave the chat");
}
