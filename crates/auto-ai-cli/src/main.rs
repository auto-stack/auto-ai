//! auto-ai-cli — interactive agent demo for the AutoOS AI stack.
//!
//! Usage:
//!   auto-ai-cli chat                    Interactive REPL (default role: assistant)
//!   auto-ai-cli chat --role coder       REPL with a specific built-in role
//!   auto-ai-cli run "<task>"            One-shot task (default role: assistant)
//!   auto-ai-cli run --role reviewer "<task>"
//!   auto-ai-cli pipeline "<task>"       Multi-agent pipeline (assistant→coder→reviewer)
//!   auto-ai-cli roles                   List available built-in roles
//!
//! Prerequisites: `aaid` must be running (cargo run -p auto-ai-daemon).

mod tools;

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use clap::{Parser, Subcommand};

use auto_ai_agent::{
    builtin_names, load_builtin, Agent, Client, StreamEvent,
    AgentFactory, FlowSpec, FlowStep, GateType, GateDecision,
    HandoffDocument, PipelineDriver, PipelineEvent,
};
use auto_ai_client::AiClient;

#[derive(Parser)]
#[command(name = "auto-ai-cli", version, about = "Interactive agent demo for the AutoOS AI stack")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Interactive multi-turn chat REPL.
    Chat {
        #[arg(long, default_value = "assistant")]
        role: String,
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
    match cli.cmd {
        Cmd::Roles => {
            println!("Built-in roles:");
            for name in builtin_names() {
                if let Some(r) = load_builtin(name) {
                    println!(
                        "  {name:<14} tier={:<4} max_turns={:<3} tools={}",
                        format!("{:?}", r.model_tier()).to_lowercase(),
                        r.max_turns(),
                        if r.allowed_tools().is_empty() { "all" } else { "restricted" }
                    );
                }
            }
        }
        Cmd::Run { task, role } => {
            let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
            if let Err(e) = rt.block_on(run_task(&task, &role)) {
                eprintln!("auto-ai-cli: {e}");
                std::process::exit(1);
            }
        }
        Cmd::Chat { role } => {
            let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
            if let Err(e) = rt.block_on(chat_loop(&role)) {
                eprintln!("auto-ai-cli: {e}");
                std::process::exit(1);
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
    Arc::new(AiClient::with_url(
        std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into()),
    ))
}

/// Build an agent from a built-in role name, register the demo tools.
fn build_agent(role_name: &str, client: Arc<dyn Client>) -> Result<Agent, String> {
    let role = load_builtin(role_name)
        .ok_or_else(|| {
            format!(
                "unknown role '{role_name}'. Available: {}",
                builtin_names().join(", ")
            )
        })?;
    // OwnedRole wrapper: load_builtin returns Arc<dyn Role>, but Agent::new
    // needs Role + 'static. We use a thin wrapper.
    let mut agent = Agent::new(OwnedRole(role), client);
    agent.register_tool(tools::ReadFile);
    agent.register_tool(tools::WriteFile);
    agent.register_tool(tools::EditFile);
    agent.register_tool(tools::ListDir);
    agent.register_tool(tools::Search);
    agent.register_tool(tools::RunCommand);
    Ok(agent)
}

/// Thin wrapper to satisfy Agent::new's `P: Role + 'static` bound.
/// (load_builtin returns Arc<dyn Role>; Arc<dyn Trait> doesn't impl the trait.)
struct OwnedRole(Arc<dyn auto_ai_agent::Role>);

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
    let mut agent = build_agent(role, client)?;

    let role_display = role.to_string();
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
async fn chat_loop(role: &str) -> Result<(), String> {
    let client = build_client();
    let mut agent = build_agent(role, client)?;
    let role_display = role.to_string();

    println!(
        "auto-ai-cli chat — role '{}' (exit/quit or Ctrl-D to leave)",
        role_display
    );

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

        let current_turn = turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        println!("\n{} ───", role_display);

        // Streaming on_event callback.
        let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |ev| {
            match ev {
                StreamEvent::Delta { text } => {
                    print!("{text}");
                    let _ = io::stdout().flush();
                }
                StreamEvent::Tool { tool, result, .. } => {
                    let preview: String = result.chars().take(80).collect();
                    let ellipsis = if result.len() > 80 { "…" } else { "" };
                    println!("\n  [tool] {tool} → {preview}{ellipsis}");
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
            }
        });

        match agent.run_stream(input, on_event).await {
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
        build_agent(role_id, self.client.clone())
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

    println!("auto-ai-cli pipeline:\n  {task_owned}\n");
    println!("Flow: assistant → coder → reviewer");

    driver
        .drive(&task_owned, on_event)
        .await
        .map_err(|e| format_agent_error(&e))
}
