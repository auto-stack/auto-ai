//! Auto-ported ReAct agent REPL (plan 013 — G2+G3+G5: tools + interactive + streaming).
//!
//! Constructs an Agent over Assistant role + AiClient, registers the EchoTool,
//! then loops reading stdin questions and printing answers (+ tool-call info).
//! G5: tokens are streamed live (each SSE delta printed as it arrives).
//! /exit quits; empty lines are skipped.

use std::io::{self, BufRead, Write};
use std::sync::Arc;
use auto_ai_agent_a2r::agent::{spawn_event_sink_with, Agent};
use auto_ai_agent_a2r::builtin_roles::Assistant;
use auto_ai_agent_a2r::client_impl::StreamingAiClient;
use auto_ai_agent_a2r::echo_tool::EchoTool;
use auto_ai_agent_a2r::StreamEvent;

fn daemon_url() -> String {
    std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into())
}

#[tokio::main]
async fn main() {
    let url = daemon_url();
    eprintln!("[react] daemon at {url}");

    // G5: channel for live token streaming.
    // std::sync::mpsc (not tokio) to avoid blocking_send deadlocks.
    let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();

    // Printer thread: reads SSE delta events from the channel and prints text
    // tokens as they arrive (live streaming display).
    let printer = std::thread::spawn(move || {
        let mut stdout = io::stdout();
        while let Ok(ev) = rx.recv() {
            // Delta events look like: { "type": "delta", "text": "..." }
            if let Some(text) = ev.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    let _ = write!(stdout, "{}", text);
                    let _ = stdout.flush();
                }
            }
        }
        // Channel closed (agent turn finished) — print a trailing newline.
        let _ = writeln!(stdout);
    });

    let client = StreamingAiClient::new(&url, tx);
    let role = Assistant {};
    let mut agent = Agent::new_shared(Box::new(role), Box::new(client));

    // Plan 031 debt (closes 026 task 8): surface turn-boundary + thinking
    // markers on stderr so live e2e can assert the event sequence. Thinking
    // chunks fold into one marker per turn (they can be numerous); text
    // deltas stay on stdout via the streaming printer above.
    let in_thinking = Arc::new(std::sync::Mutex::new(false));
    let itc = in_thinking.clone();
    let sink = spawn_event_sink_with(
        String::new(),
        Box::new(move |ev: StreamEvent| {
            match &ev {
                StreamEvent::TurnStart(turn) => {
                    *itc.lock().unwrap() = false;
                    eprintln!("[event] turn {turn} start");
                }
                StreamEvent::TurnEnd(turn, _, _) => {
                    *itc.lock().unwrap() = false;
                    eprintln!("[event] turn {turn} end");
                }
                StreamEvent::Thinking(_) => {
                    let mut folding = itc.lock().unwrap();
                    if !*folding {
                        *folding = true;
                        eprintln!("[event] thinking");
                    }
                }
                _ => {}
            }
        }),
    );
    // Cancellation flag: never set in the REPL (Ctrl+C kills the process);
    // run_stream requires one.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Register the echo tool so the model can invoke it.
    // Single-wrap (Plan 390 §15.11): ToolRegistry now stores Arc<dyn Tool>, so
    // register_shared takes Arc<dyn Tool> — Arc::new(EchoTool) coerces directly
    // (no inner Box needed; the old Arc::new(Box::new(...)) was the double-wrap).
    agent.register_shared(Arc::new(EchoTool {}));

    eprintln!("[react] ready (streaming).  Type a question (or /exit).");
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        // prompt
        let _ = write!(stdout, "> ");
        let _ = stdout.flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,                   // EOF
            Err(e) => {
                eprintln!("[react] read error: {e}");
                break;
            }
            _ => {}
        }

        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }
        if input == "/exit" {
            eprintln!("[react] bye.");
            break;
        }

        match agent.run_stream(&input, cancel.clone(), sink.clone()).await {
            Ok(result) => {
                // The streaming printer already output the text tokens live.

                if !result.tool_calls.is_empty() {
                    eprintln!("[react] tool calls this turn:");
                    for tc in &result.tool_calls {
                        eprintln!("  • {} : {}", tc.tool, tc.result);
                    }
                }
                eprintln!(
                    "  ({} turn{}, {} tokens)",
                    result.turns,
                    if result.turns == 1 { "" } else { "s" },
                    result.total_tokens
                );
            }
            Err(e) => {
                eprintln!("[react] error: {}", e.message());
            }
        }
    }

    // Drop agent → drops client → drops tx → printer thread exits.
    drop(agent);
    let _ = printer.join();
}
