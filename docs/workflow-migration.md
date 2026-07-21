# Migration Guide: Workflow → PipelineEngine

> **Plan 008** — The `Workflow` engine (`.at`-based DAG) is deprecated in favor of
> `orchestration::PipelineEngine`, which is a strict superset. This guide explains
> how to migrate existing workflow code.

---

## Why migrate?

| Feature | Workflow (deprecated) | PipelineEngine |
|---|---|---|
| Step ordering | DAG topo-sort (Kahn) | Linear + Loop routing |
| Human gate | `Gate::Human` — logged only | `GateType::Human` — **enforced** (pauses engine) |
| Token budget | Not enforced | `BudgetTracker` with HardStop |
| Handoff between steps | Raw chat history (context grows) | `HandoffDocument` — structured, bounded context |
| Loop / re-run | `RetrySpec` (rewind on validator fail) | `ExitRouting::Loop` with max iterations |
| Pause / Resume | Not supported | `pause()` / `resume()` / `rerun()` |
| Streaming events | `WorkflowEvent` (4 variants) | `PipelineEvent` (10 variants incl. budget, gate, tool) |
| Validators | `workflow_validator::Validator` (regex checks) | Step-level validators in `FlowStep` (extensible) |
| Format | `.at` text (`workflow { … }`) | Rust code (`FlowSpec::new()` + `.add_step()`) |

---

## Migration by example

### Old: `.at` Workflow

```text
workflow {
    name : "feature-development"
    steps : [
        relay {
            id : "architect"
            role : "architect"
            input : "$user_request"
            output : "$design_doc"
        }
        relay {
            id : "coder"
            role : "coder"
            input : "implement based on:\n$design_doc"
            output : "$code_result"
            depends_on : ["architect"]
        }
        relay {
            id : "reviewer"
            role : "reviewer"
            input : "review:\n$code_result"
            output : "$review"
            depends_on : ["coder"]
            gate : "human"
        }
    ]
}
```

### New: Rust `FlowSpec` + `PipelineDriver`

```rust
use auto_ai_agent::{
    FlowSpec, FlowStep, GateType, PipelineDriver,
    PipelineEvent, AgentFactory,
};

// 1. Define the flow
fn create_flow() -> FlowSpec {
    let mut flow = FlowSpec::new("feature-development");
    flow.add_step(FlowStep::new("architect", "architect"));
    flow.add_step(FlowStep::new("coder", "coder"));
    flow.add_step(
        FlowStep::new("reviewer", "reviewer")
            .with_gate(GateType::Human),
    );
    flow
}

// 2. Implement AgentFactory for your app
struct MyFactory { /* your agent building logic */ }

impl AgentFactory for MyFactory {
    fn build_agent(
        &self,
        role_id: &str,
        handoff: Option<&HandoffDocument>,
    ) -> Result<Agent, String> {
        // Build your agent with the role + handoff context
        todo!()
    }
}

// 3. Run the pipeline
async fn run_pipeline(task: &str) -> Result<(), String> {
    let flow = create_flow();
    let factory = MyFactory { /* … */ };
    let mut driver = PipelineDriver::new(flow, factory, task);

    let on_event = Arc::new(|ev: PipelineEvent| {
        match ev {
            PipelineEvent::StepStarted { step_id, role_id } => {
                println!("Running: {step_id} ({role_id})");
            }
            PipelineEvent::Delta { text } => print!("{text}"),
            PipelineEvent::StepCompleted { handoff, .. } => {
                println!("Done: {}", handoff.summary);
            }
            PipelineEvent::Completed => println!("Pipeline done!"),
            _ => {}
        }
    });

    driver.drive(task, on_event).await
        .map_err(|e| format!("{e}"))
}
```

---

## Key concept changes

### 1. Context substitution (`$var`) → `HandoffDocument`

The old workflow used `$design_doc`, `$code_result` as string-substitution
variables. The new pipeline passes a **structured `HandoffDocument`** between
steps, which the next agent reads via `handoff.render()` → markdown.

### 2. `depends_on` (DAG) → Linear + `ExitRouting::Loop`

The old engine used topological sort on `depends_on` edges. The new engine is
**linear by default**. If you need iteration (e.g., coder → tester → coder),
use `ExitRouting::Loop`:

```rust
FlowStep::new("tester", "tester")
    .with_exit(ExitRouting::Loop {
        target_step_id: "coder".into(),
        max_iterations: 3,
    });
```

### 3. Validators

Old validators (`output_contains`, `output_min_length`) live in
`workflow_validator`. The new pipeline supports **step-level validators** via
`FlowStep` + `ToolGuard`. If you need richer validation, implement it in your
`AgentFactory` or as a tool.

### 4. `on_fail` / `RetrySpec` → `ExitRouting::Loop`

Old: `on_fail : { retry : "coder", max : 3 }` rewinds on validator failure.
New: Use `ExitRouting::Loop` with `max_iterations` — the pipeline engine
pauses when the cap is reached.

---

## Rollback

If you need to keep using the old `Workflow` engine, it remains available:

```rust
#[allow(deprecated)]
use auto_ai_agent::workflow::Workflow;
```

However, new code should prefer `PipelineEngine`. The old types will be
removed in a future major version.
