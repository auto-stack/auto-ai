//! UI-agnostic resident agent task shared by the interactive frontends
//! (Plan 029; extracted from tui.rs).
//!
//! A background tokio task owns the `Agent` so its memory survives across
//! turns. UIs talk to it over one command channel:
//!   • `Run`   — run one `run_stream` turn-set for a user message.
//!   • `Steer` — queue a steering message (Plan 026): injected after the
//!               current tool batch, before the next LLM call.
//!   • `Reset` — rebuild the agent with fresh memory (`/clear`).
//! Streaming events flow back over the unbounded `stream_tx` channel owned
//! by the caller. After each run the conversation is persisted per-cwd.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::mpsc;

use auto_ai_agent::{Agent, Client, StreamEvent};

/// A command from the UI to the resident agent task.
pub enum AgentCommand {
    /// Run the agent on a user message (one `run_stream` invocation).
    Run {
        text: String,
        cancel: Arc<AtomicBool>,
    },
    /// Queue a steering message for the in-flight run (Plan 026).
    Steer(String),
    /// Queue a follow-up message (Plan 026): when the in-flight run is about
    /// to end naturally, it revives the loop with this message.
    FollowUp(String),
    /// Drop the current agent (and its memory) and rebuild a fresh one.
    /// Also clears the persisted session (the `/clear` semantics).
    Reset,
    /// Like [`AgentCommand::Reset`] but switching to a different role. The
    /// old memory is saved to the session file first so `-c` history isn't
    /// lost across a role switch.
    SetRole(String),
}

/// Spawn the resident agent task. Returns the command sender; dropping it
/// makes the task exit after finishing any in-flight command.
pub fn spawn(
    mut agent: Agent,
    role: String,
    client: Arc<dyn Client>,
    cwd: PathBuf,
    stream_tx: mpsc::UnboundedSender<StreamEvent>,
) -> mpsc::UnboundedSender<AgentCommand> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentCommand>();
    tokio::spawn(async move {
        let mut role = role;
        while let Some(cmd) = rx.recv().await {
            match cmd {
                AgentCommand::Run { text, cancel } => {
                    let tx = stream_tx.clone();
                    let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> =
                        Arc::new(move |ev| {
                            let _ = tx.send(ev);
                        });
                    // run_stream errors surface as StreamEvent::Error already;
                    // a hard failure just ends this turn (task stays alive).
                    let _ = agent.run_stream(&text, on_event, cancel).await;
                    // Persist the updated conversation after each turn.
                    crate::session::save(&cwd, "session", agent.memory_messages());
                }
                AgentCommand::Steer(text) => agent.steer(text),
                AgentCommand::FollowUp(text) => agent.follow_up(text),
                AgentCommand::Reset => {
                    // `/clear`: wipe memory *and* the persisted session so a
                    // later `-c` doesn't resurrect the cleared conversation.
                    crate::session::save(&cwd, "session", &[]);
                    if let Ok(fresh) = crate::build_agent(&role, client.clone(), true) {
                        agent = fresh;
                    }
                }
                AgentCommand::SetRole(new_role) => {
                    // Keep the conversation recoverable under the old role…
                    crate::session::save(&cwd, "session", agent.memory_messages());
                    if let Ok(fresh) = crate::build_agent(&new_role, client.clone(), true) {
                        agent = fresh;
                        role = new_role;
                    }
                }
            }
        }
    });
    tx
}
