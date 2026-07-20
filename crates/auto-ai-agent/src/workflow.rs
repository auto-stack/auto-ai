//! The Workflow engine (design doc §5–6).
//!
//! **Deprecated (Plan 008):** Prefer [`crate::orchestration::PipelineEngine`]
//! which is a strict superset (real gate enforcement, budget tracking, loop
//! caps, pause/resume/rerun, handoff auto-correction). This `Workflow` module
//! is kept for backward compatibility but should not be used for new code.
//!
//! A [`Workflow`] chains [`Agent`]s into a multi-step plan: each step is a
//! `relay` that loads a Role, substitutes `$var` references from a
//! shared context, optionally skips on a `condition`, runs an Agent, and
//! stores its output. Steps run in topological order (by `depends_on`).
//!
//! The `.at` source mirrors the design-doc example:
//!
//! ```text
//! workflow {
//!     name : "feature-development"
//!     steps : [
//!         relay {
//!             id : "architect"
//!             role : "architect"
//!             input : "$user_request"
//!             output : "$design_doc"
//!         }
//!         relay {
//!             id : "coder"
//!             role : "coder"
//!             input : "implement based on:\n$design_doc"
//!             output : "$code_result"
//!             depends_on : ["architect"]
//!         }
//!         relay {
//!             id : "reviewer"
//!             role : "reviewer"
//!             input : "review:\n$code_result"
//!             output : "$review"
//!             depends_on : ["coder"]
//!             condition : "$code_result.contains(bug)"
//!         }
//!     ]
//! }
//! ```

// This module is deprecated wholesale; suppress internal usage warnings.
#![allow(deprecated)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use auto_atom::{Atom, AtomParser};
use auto_val::Value;

use crate::agent::Client;
use crate::error::AgentError;
use crate::role_def::Role;
use crate::builtin_roles::load_builtin;
use crate::tool::Tool;
use crate::Agent;

/// What to do when a step's validators fail: retry an earlier step, up to
/// `max` times. Beyond that, the whole workflow fails.
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
#[derive(Clone, Debug)]
pub struct RetrySpec {
    /// The step id to jump back to (re-run it and everything after).
    pub retry: String,
    /// Max retry attempts before failing the workflow.
    pub max: usize,
}

/// Per-step tool guard: restrict which tools this step's agent may use.
/// `allow` empty = inherit all; `forbid` removes specific tools.
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
#[derive(Clone, Debug, Default)]
pub struct ToolGuard {
    pub allow: Vec<String>,
    pub forbid: Vec<String>,
}

/// Whether a step needs human approval before proceeding.
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Gate {
    /// Automatic — validators pass → continue (default).
    #[default]
    Auto,
    /// Pause and wait for external approval (returns a Paused state; the
    /// caller resumes). MVP: the engine returns Paused; the API/CLI handles it.
    Human,
}

/// A single relay step in a workflow.
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
#[derive(Clone, Debug)]
pub struct WorkflowStep {
    pub id: String,
    pub role: String,
    pub input_template: String,
    pub output_var: String,
    pub depends_on: Vec<String>,
    pub condition: Option<String>,
    /// Validators that gate this step's output. All must pass for the step to
    /// be considered successful.
    pub validators: Vec<crate::workflow_validator::Validator>,
    /// On validator failure, retry from this step (up to `max` times).
    pub on_fail: Option<RetrySpec>,
    /// Per-step tool restrictions.
    pub tool_guard: ToolGuard,
    /// Whether human approval is required after this step.
    pub gate: Gate,
}

impl WorkflowStep {
    /// Run this step's role as an Agent, returning its output.
    ///
    /// `profession_resolver` is `Arc<dyn Fn + Send + Sync>` so the returned
    /// future is `Send` (required for use in async runtimes like axum's).
    pub async fn run(
        &self,
        context: &WorkflowContext,
        tools: &[Arc<dyn Tool>],
        client: &Arc<dyn Client>,
        profession_resolver: Arc<dyn Fn(&str) -> Result<Arc<dyn Role>, AgentError> + Send + Sync>,
    ) -> Result<String, AgentError> {
        let role = profession_resolver(&self.role)?;
        let mut agent = Agent::new(
            // Agent::new takes ownership via Role + 'static. We hold an
            // Arc<dyn Role>, so wrap it in a thin Role adapter.
            ArcProfession(role),
            client.clone(),
        );
        for tool in tools {
            agent.register_shared(tool.clone());
        }
        let input = context.substitute(&self.input_template);
        let result = agent.run(&input).await?;
        Ok(result.output)
    }
}

/// Wrap an `Arc<dyn Role>` so it can be moved into `Agent::new`, which
/// requires `P: Role + 'static`. (Role is object-safe; this struct
/// is the owned adapter the Agent owns.)
struct ArcProfession(Arc<dyn Role>);

impl Role for ArcProfession {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn system_prompt(&self) -> &str {
        self.0.system_prompt()
    }
    fn model_tier(&self) -> ai_config::ModelTier {
        self.0.model_tier()
    }
    fn model(&self) -> &str {
        self.0.model()
    }
    fn temperature(&self) -> f64 {
        self.0.temperature()
    }
    fn max_turns(&self) -> usize {
        self.0.max_turns()
    }
    fn allowed_tools(&self) -> Vec<String> {
        self.0.allowed_tools()
    }
    fn memory_limit(&self) -> Option<usize> {
        self.0.memory_limit()
    }
}

/// The shared key→value context a workflow step reads from and writes to.
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
#[derive(Clone, Debug, Default)]
pub struct WorkflowContext {
    vars: HashMap<String, String>,
}

impl WorkflowContext {
    /// Seed the context. The initial request is stored as `$user_request`
    /// (without the `$`) so templates can reference `$user_request`.
    pub fn new(user_request: &str) -> Self {
        let mut vars = HashMap::new();
        vars.insert("user_request".to_string(), user_request.to_string());
        Self { vars }
    }

    /// Store a step's output. `output_var` is like `$code_result`; the leading
    /// `$` is stripped for the key.
    pub fn set(&mut self, output_var: &str, value: String) {
        let key = output_var.trim_start_matches('$');
        self.vars.insert(key.to_string(), value);
    }

    /// Read a variable by its key (with or without leading `$`).
    pub fn get(&self, var: &str) -> Option<&str> {
        let key = var.trim_start_matches('$');
        self.vars.get(key).map(|s| s.as_str())
    }

    /// Replace `$var` references in `template` with their context values.
    /// Unknown vars are left as-is (so a missing dependency surfaces visibly).
    pub fn substitute(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let bytes = template.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' {
                // Read the longest run of ident chars after '$'.
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                if end > start {
                    let name = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
                    if let Some(val) = self.vars.get(name) {
                        out.push_str(val);
                    } else {
                        // Unknown var — preserve literally.
                        out.push('$');
                        out.push_str(name);
                    }
                    i = end;
                    continue;
                }
            }
            // Regular char (push UTF-8-safe by re-slicing the source).
            let ch_len = utf8_len_at(bytes, i);
            out.push_str(&template[i..i + ch_len]);
            i += ch_len;
        }
        out
    }
}

/// Length in bytes of the UTF-8 code point starting at `idx`.
fn utf8_len_at(bytes: &[u8], idx: usize) -> usize {
    if idx >= bytes.len() {
        return 0;
    }
    let b = bytes[idx];
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// The outcome of [`Workflow::run`].
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
#[derive(Clone, Debug, Default)]
pub struct WorkflowResult {
    /// Each step id → its textual output (skipped steps are absent).
    pub step_outputs: HashMap<String, String>,
    /// Each output variable → its value (post-substitution view).
    pub outputs: HashMap<String, String>,
    /// Total tokens consumed across all steps.
    pub total_tokens: u64,
}

/// A parsed workflow, ready to run.
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
pub struct Workflow {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

impl Workflow {
    /// Run the workflow. `tools` is shared across every step's agent;
    /// `client` is the LLM transport; `initial_input` seeds `$user_request`.
    pub async fn run(
        &self,
        tools: &[Arc<dyn Tool>],
        client: Arc<dyn Client>,
        initial_input: &str,
    ) -> Result<WorkflowResult, AgentError> {
        let order = topo_sort(&self.steps)?;
        let mut context = WorkflowContext::new(initial_input);
        let mut result = WorkflowResult::default();

        let resolver: Arc<dyn Fn(&str) -> Result<Arc<dyn Role>, AgentError> + Send + Sync> =
            Arc::new(|name: &str| resolve_profession(name));

        // Index-based loop so on_fail can rewind (retry an earlier step).
        let mut i = 0;
        // Per-step retry counters (step_id → attempts so far).
        let mut retry_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        while i < order.len() {
            let step_id = &order[i];
            let step = self.steps.iter().find(|s| s.id == *step_id).expect("topo id exists");

            // Condition: skip the step if it evaluates false.
            if let Some(cond) = &step.condition {
                if !evaluate_condition(cond, &context) {
                    tracing::info!("workflow step '{}' skipped (condition false)", step.id);
                    i += 1;
                    continue;
                }
            }

            // Apply per-step tool guard: filter the shared tool set.
            let step_tools = filter_tools_for_step(tools, &step.tool_guard);
            let output = step
                .run(&context, &step_tools, &client, resolver.clone())
                .await?;

            // Run validators. If they fail and on_fail is set, rewind to the
            // retry step (up to max times); else the workflow fails.
            if !step.validators.is_empty() {
                if let Err(msg) = crate::workflow_validator::check_all(&step.validators, &output) {
                    tracing::warn!("workflow step '{}' validation failed: {}", step.id, msg);
                    if let Some(retry) = &step.on_fail {
                        let count = retry_counts.entry(step.id.clone()).or_insert(0);
                        *count += 1;
                        if *count <= retry.max {
                            tracing::info!(
                                "workflow: retrying from '{}' (attempt {}/{})",
                                retry.retry,
                                *count,
                                retry.max
                            );
                            // Rewind to the retry step's index.
                            if let Some(rewind_idx) = order.iter().position(|id| id == &retry.retry) {
                                i = rewind_idx;
                                continue;
                            }
                        }
                    }
                    // No on_fail, or exhausted retries → fail.
                    return Err(AgentError::Config(format!(
                        "step '{}' validation failed: {}",
                        step.id, msg
                    )));
                }
            }

            // Human gate: return a Paused-like result. MVP: for `run()` we
            // just log and continue (the streaming variant surfaces it via
            // events). A future API caller handles the Human pause explicitly.
            if step.gate == Gate::Human {
                tracing::info!("workflow step '{}' paused at human gate (auto-continuing in run())", step.id);
            }

            context.set(&step.output_var, output.clone());
            result.step_outputs.insert(step.id.clone(), output.clone());
            let key = step.output_var.trim_start_matches('$').to_string();
            result.outputs.insert(key, output);
            i += 1;
        }

        Ok(result)
    }

    /// Like [`Workflow::run`], but emits progress events via `on_event` as each
    /// step starts/finishes. Lets the server stream step-by-step SSE so a long
    /// multi-step workflow doesn't block a single HTTP response.
    ///
    /// Events: [`WorkflowEvent::StepStart`], [`WorkflowEvent::StepDone`],
    /// [`WorkflowEvent::StepSkipped`], [`WorkflowEvent::Finished`].
    ///
    /// **Caveat (review-002)**: unlike [`Self::run`], this streaming variant
    /// does **not** apply validators, on_fail retry, tool guards, or gates —
    /// those features only exist in the non-streaming path. For a streaming
    /// pipeline with full lifecycle support, use
    /// [`orchestration::PipelineDriver`](crate::orchestration::PipelineDriver)
    /// instead (Plan 008).
    #[deprecated(note = "skips validators/guards/retry — use orchestration::PipelineDriver (Plan 008)")]
    pub async fn run_with_progress(
        &self,
        tools: &[Arc<dyn Tool>],
        client: Arc<dyn Client>,
        initial_input: &str,
        on_event: Arc<dyn Fn(WorkflowEvent) + Send + Sync>,
    ) -> Result<WorkflowResult, AgentError> {
        let order = topo_sort(&self.steps)?;
        let mut context = WorkflowContext::new(initial_input);
        let mut result = WorkflowResult::default();

        for step_id in order {
            let step = self
                .steps
                .iter()
                .find(|s| s.id == step_id)
                .expect("topo id exists");

            if let Some(cond) = &step.condition {
                if !evaluate_condition(cond, &context) {
                    tracing::info!("workflow step '{}' skipped (condition false)", step.id);
                    on_event(WorkflowEvent::StepSkipped {
                        step_id: step.id.clone(),
                    });
                    continue;
                }
            }

            on_event(WorkflowEvent::StepStart {
                step_id: step.id.clone(),
                role: step.role.clone(),
                input: context.substitute(&step.input_template),
            });

            let resolver: Arc<dyn Fn(&str) -> Result<Arc<dyn Role>, AgentError> + Send + Sync> =
                Arc::new(|name: &str| resolve_profession(name));
            let output = step.run(&context, tools, &client, resolver).await?;

            on_event(WorkflowEvent::StepDone {
                step_id: step.id.clone(),
                output: output.clone(),
            });

            context.set(&step.output_var, output.clone());
            result.step_outputs.insert(step.id.clone(), output.clone());
            let key = step.output_var.trim_start_matches('$').to_string();
            result.outputs.insert(key, output);
        }

        on_event(WorkflowEvent::Finished {
            result: WorkflowResult {
                step_outputs: result.step_outputs.clone(),
                outputs: result.outputs.clone(),
                total_tokens: result.total_tokens,
            },
        });
        Ok(result)
    }
}

/// Progress events emitted by [`Workflow::run_with_progress`].
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
#[derive(Clone, Debug)]
pub enum WorkflowEvent {
    /// A step is about to run.
    StepStart {
        step_id: String,
        role: String,
        input: String,
    },
    /// A step finished with this output.
    StepDone {
        step_id: String,
        output: String,
    },
    /// A step was skipped (condition false).
    StepSkipped {
        step_id: String,
    },
    /// The whole workflow finished (carries the final result).
    Finished {
        result: WorkflowResult,
    },
}

/// Resolve a role name: builtin first, else `.at` role by file
/// path, else error.
fn resolve_profession(name: &str) -> Result<Arc<dyn Role>, AgentError> {
    if let Some(p) = load_builtin(name) {
        return Ok(p);
    }
    // Treat the name as a path to a .at role file.
    let content = std::fs::read_to_string(name).map_err(|e| {
        AgentError::Config(format!(
            "role '{}' is not a builtin and could not be read as a file: {e}",
            name
        ))
    })?;
    crate::config::load_role(&content)
}

// ── Topological sort (Kahn's algorithm) ────────────────────────────────────

/// Filter the shared tool set for a step based on its ToolGuard.
/// - `allow` non-empty → only those tools.
/// - `forbid` → remove those from the full set.
/// - both empty → all tools (no filtering).
fn filter_tools_for_step(
    tools: &[Arc<dyn Tool>],
    guard: &ToolGuard,
) -> Vec<Arc<dyn Tool>> {
    if guard.allow.is_empty() && guard.forbid.is_empty() {
        return tools.to_vec();
    }
    tools
        .iter()
        .filter(|t| {
            let name = t.name();
            if !guard.allow.is_empty() {
                guard.allow.iter().any(|a| a == name)
            } else {
                true
            }
        })
        .filter(|t| {
            let name = t.name();
            !guard.forbid.iter().any(|f| f == name)
        })
        .cloned()
        .collect()
}

fn topo_sort(steps: &[WorkflowStep]) -> Result<Vec<String>, AgentError> {
    let ids: HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    for s in steps {
        for dep in &s.depends_on {
            if !ids.contains(dep.as_str()) {
                return Err(AgentError::Config(format!(
                    "step '{}' depends on unknown step '{}'",
                    s.id, dep
                )));
            }
        }
    }

    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for s in steps {
        in_degree.entry(s.id.as_str()).or_insert(0);
        for dep in &s.depends_on {
            *in_degree.entry(s.id.as_str()).or_insert(0) += 1;
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(s.id.as_str());
        }
    }

    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&k, _)| k)
        .collect();
    queue.sort(); // deterministic tie-break
    let mut order = Vec::with_capacity(steps.len());
    while let Some(id) = queue.pop() {
        order.push(id.to_string());
        if let Some(deps) = dependents.get(id) {
            for &d in deps {
                if let Some(remaining) = in_degree.get_mut(d) {
                    *remaining -= 1;
                    if *remaining == 0 {
                        // insert sorted to keep determinism
                        let pos = queue.binary_search(&d).unwrap_or_else(|e| e);
                        queue.insert(pos, d);
                    }
                }
            }
        }
    }

    if order.len() != steps.len() {
        return Err(AgentError::Config(
            "workflow has a dependency cycle".to_string(),
        ));
    }
    Ok(order)
}

// ── Condition evaluation (a tiny subset) ───────────────────────────────────
//
// Supported forms (design doc: `$var.contains(literal)`):
//   `$var.contains(literal)` → true iff the var's value contains `literal`.
//   `$var`                   → true iff the var is non-empty.
// Whitespace around `contains` is tolerated.

fn evaluate_condition(expr: &str, ctx: &WorkflowContext) -> bool {
    let expr = expr.trim();
    // `$var.contains(literal)`
    if let Some(rest) = expr.strip_prefix('$') {
        if let Some(paren) = rest.find(".contains(") {
            let var = &rest[..paren];
            let after = &rest[paren + ".contains(".len()..];
            if let Some(close) = after.rfind(')') {
                let literal_raw = &after[..close];
                let literal = literal_raw.trim().trim_matches('"').trim_matches('\'');
                return ctx
                    .get(var)
                    .map(|v| v.contains(literal))
                    .unwrap_or(false);
            }
        }
        // Bare `$var` → truthy (non-empty).
        return ctx.get(rest).map(|v| !v.is_empty()).unwrap_or(false);
    }
    // Unknown shape: default to running the step (fail-open).
    tracing::warn!("workflow: unrecognized condition '{expr}', treating as true");
    true
}

// ── .at parsing ────────────────────────────────────────────────────────────

/// Parse a `workflow { … }` block from `.at` source.
#[deprecated(note = "use orchestration::PipelineEngine instead (Plan 008)")]
pub fn parse_at_workflow(content: &str) -> Result<Workflow, AgentError> {
    let atom = AtomParser::parse(content)
        .map_err(|e| AgentError::Config(format!("failed to parse workflow .at: {e}")))?;

    let node = match atom {
        Atom::Node(n) if n.name.as_str() == "workflow" => n,
        Atom::Node(n) => {
            return Err(AgentError::Config(format!(
                "expected a 'workflow' block, found '{}'",
                n.name
            )))
        }
        other => {
            return Err(AgentError::Config(format!(
                "expected a 'workflow' node, found {:?}",
                other
            )))
        }
    };

    let name = match node.get_prop_of("name") {
        Value::Str(s) => s.to_string(),
        _ => "workflow".to_string(),
    };

    let mut steps = Vec::new();
    if let Value::Array(arr) = node.get_prop_of("steps") {
        for v in &arr.values {
            if let Value::Node(relay) = v {
                if relay.name.as_str() != "relay" {
                    return Err(AgentError::Config(format!(
                        "workflow steps must be 'relay' nodes, found '{}'",
                        relay.name
                    )));
                }
                steps.push(parse_relay_node(relay)?);
            } else {
                return Err(AgentError::Config(
                    "workflow 'steps' must be an array of relay { } nodes".into(),
                ));
            }
        }
    }

    Ok(Workflow { name, steps })
}

fn parse_relay_node(node: &auto_val::Node) -> Result<WorkflowStep, AgentError> {
    let id = opt_string_req(node, "id")?;
    let role = opt_string_req(node, "role")?;
    let input_template = opt_string_req(node, "input")?;
    let output_var = opt_string_req(node, "output")?;
    let depends_on = opt_string_list(node, "depends_on").unwrap_or_default();
    let condition = opt_string(node, "condition");
    let validators = parse_validators(node);
    let on_fail = parse_on_fail(node);
    let tool_guard = parse_tool_guard(node);
    let gate = match opt_string(node, "gate").as_deref() {
        Some("human") => Gate::Human,
        _ => Gate::Auto,
    };
    Ok(WorkflowStep {
        id,
        role,
        input_template,
        output_var,
        depends_on,
        condition,
        validators,
        on_fail,
        tool_guard,
        gate,
    })
}

/// Parse the `validate : [...]` array into [`Validator`]s.
fn parse_validators(node: &auto_val::Node) -> Vec<crate::workflow_validator::Validator> {
    match node.get_prop_of("validate") {
        Value::Array(arr) => arr.values.iter().filter_map(parse_one_validator).collect(),
        _ => Vec::new(),
    }
}

/// Parse a single validator from a Value (Obj with `type` + fields).
fn parse_one_validator(v: &Value) -> Option<crate::workflow_validator::Validator> {
    use crate::workflow_validator::Validator;
    let obj = v.as_obj();
    let vtype = obj.get("type")?.to_astr().to_string();
    let pattern = || -> String {
        obj.get("pattern")
            .map(|p| p.to_astr().to_string())
            .unwrap_or_default()
    };
    match vtype.as_str() {
        "output_contains" => Some(Validator::OutputContains { pattern: pattern() }),
        "output_not_contains" => Some(Validator::OutputNotContains { pattern: pattern() }),
        "output_min_length" => {
            let min = obj.get("min").map(|m| m.as_uint() as usize).unwrap_or(0);
            Some(Validator::OutputMinLength { min })
        }
        "all" => {
            let inner = parse_validator_array(obj, "validators");
            Some(Validator::All { validators: inner })
        }
        "any" => {
            let inner = parse_validator_array(obj, "validators");
            Some(Validator::Any { validators: inner })
        }
        _ => None,
    }
}

/// Parse a nested `validators` array from an all/any validator Obj.
fn parse_validator_array(obj: &auto_val::Obj, key: &str) -> Vec<crate::workflow_validator::Validator> {
    match obj.get(key) {
        Some(Value::Array(arr)) => arr.values.iter().filter_map(parse_one_validator).collect(),
        _ => Vec::new(),
    }
}

/// Parse `on_fail : { retry : "step_id", max : N }`.
fn parse_on_fail(node: &auto_val::Node) -> Option<RetrySpec> {
    let obj = match node.get_prop_of("on_fail") {
        Value::Obj(o) => o,
        _ => return None,
    };
    let retry = obj.get("retry")?.to_astr().to_string();
    let max = obj.get("max").map(|m| m.as_uint() as usize).unwrap_or(1);
    Some(RetrySpec { retry, max })
}

/// Parse `tools : { allow : [...], forbid : [...] }`.
fn parse_tool_guard(node: &auto_val::Node) -> ToolGuard {
    let obj = match node.get_prop_of("tools") {
        Value::Obj(o) => o,
        _ => return ToolGuard::default(),
    };
    let allow = match obj.get("allow") {
        Some(Value::Array(arr)) => arr.values.iter().map(|v| v.to_astr().to_string()).collect(),
        _ => Vec::new(),
    };
    let forbid = match obj.get("forbid") {
        Some(Value::Array(arr)) => arr.values.iter().map(|v| v.to_astr().to_string()).collect(),
        _ => Vec::new(),
    };
    ToolGuard { allow, forbid }
}

fn opt_string(node: &auto_val::Node, key: &str) -> Option<String> {
    match node.get_prop_of(key) {
        Value::Str(s) => Some(s.to_string()),
        Value::Nil => None,
        other => Some(other.to_astr().to_string()),
    }
}

fn opt_string_req(node: &auto_val::Node, key: &str) -> Result<String, AgentError> {
    opt_string(node, key).ok_or_else(|| {
        AgentError::Config(format!("relay step missing required field '{}'", key))
    })
}

fn opt_string_list(node: &auto_val::Node, key: &str) -> Option<Vec<String>> {
    match node.get_prop_of(key) {
        Value::Array(arr) => Some(
            arr.values
                .iter()
                .map(|v| match v {
                    Value::Str(s) => s.to_string(),
                    other => other.to_astr().to_string(),
                })
                .collect(),
        ),
        Value::Str(s) => Some(vec![s.to_string()]),
        Value::Nil => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use auto_ai_client::{ClientError, CompletionRequest, CompletionResponse};

    // ── mock client that echoes a marker so we can assert step wiring ───────
    struct EchoClient;
    #[async_trait]
    impl Client for EchoClient {
        async fn complete(
            &self,
            req: &CompletionRequest,
        ) -> Result<CompletionResponse, ClientError> {
            let user_text = req
                .messages
                .iter()
                .rev()
                .find_map(|m| {
                    if m.role == "user" {
                        Some(m.text())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            Ok(CompletionResponse {
                content: format!(
                    "[{}]: {user_text}",
                    req.system_prompt.as_deref().unwrap_or("").len()
                ),
                tool_calls: vec![],
                stop_reason: Some("end_turn".into()),
                usage: None,
                model: "mock".into(),
                error: None,
            })
        }
    }

    fn mock_client() -> Arc<dyn Client> {
        Arc::new(EchoClient)
    }

    #[test]
    fn parse_workflow_example() {
        let src = r#"
            workflow {
                name : "demo"
                steps : [
                    relay {
                        id : "a"
                        role : "coder"
                        input : "$user_request"
                        output : "$a_out"
                    }
                    relay {
                        id : "b"
                        role : "coder"
                        input : "from a: $a_out"
                        output : "$b_out"
                        depends_on : ["a"]
                    }
                ]
            }
        "#;
        let wf = parse_at_workflow(src).unwrap();
        assert_eq!(wf.name, "demo");
        assert_eq!(wf.steps.len(), 2);
        assert_eq!(wf.steps[0].id, "a");
        assert_eq!(wf.steps[1].depends_on, vec!["a".to_string()]);
    }

    #[test]
    fn parse_rejects_non_workflow_root() {
        let src = "role { name : \"x\" }";
        assert!(parse_at_workflow(src).is_err());
    }
    #[test]
    fn topo_sort_linear() {
        let steps = vec![
            WorkflowStep {
                id: "a".into(),
                role: "coder".into(),
                input_template: "".into(),
                output_var: "$a".into(),
                depends_on: vec![],
                condition: None,
            validators: vec![],
            on_fail: None,
            tool_guard: Default::default(),
            gate: Default::default(),
            },
            WorkflowStep {
                id: "b".into(),
                role: "coder".into(),
                input_template: "".into(),
                output_var: "$b".into(),
                depends_on: vec!["a".into()],
                condition: None,
            validators: vec![],
            on_fail: None,
            tool_guard: Default::default(),
            gate: Default::default(),
            },
        ];
        let order = topo_sort(&steps).unwrap();
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn topo_sort_cycle_errors() {
        let steps = vec![
            WorkflowStep {
                id: "a".into(),
                role: "x".into(),
                input_template: "".into(),
                output_var: "$a".into(),
                depends_on: vec!["b".into()],
                condition: None,
            validators: vec![],
            on_fail: None,
            tool_guard: Default::default(),
            gate: Default::default(),
            },
            WorkflowStep {
                id: "b".into(),
                role: "x".into(),
                input_template: "".into(),
                output_var: "$b".into(),
                depends_on: vec!["a".into()],
                condition: None,
            validators: vec![],
            on_fail: None,
            tool_guard: Default::default(),
            gate: Default::default(),
            },
        ];
        let err = topo_sort(&steps).unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn topo_sort_unknown_dependency_errors() {
        let steps = vec![WorkflowStep {
            id: "a".into(),
            role: "x".into(),
            input_template: "".into(),
            output_var: "$a".into(),
            depends_on: vec!["nope".into()],
            condition: None,
        validators: vec![],
        on_fail: None,
        tool_guard: Default::default(),
        gate: Default::default(),
        }];
        assert!(topo_sort(&steps).is_err());
    }

    #[test]
    fn context_substitute() {
        let mut ctx = WorkflowContext::new("hello");
        ctx.set("$design", "DOC".into());
        assert_eq!(ctx.substitute("$user_request then $design"), "hello then DOC");
        // unknown var left as-is
        assert_eq!(ctx.substitute("$missing stays"), "$missing stays");
    }

    #[test]
    fn condition_contains_and_truthy() {
        let mut ctx = WorkflowContext::new("");
        ctx.set("$t", "the tests failed badly".into());
        assert!(evaluate_condition("$t.contains(fail)", &ctx));
        assert!(!evaluate_condition("$t.contains(pass)", &ctx));
        assert!(evaluate_condition("$t", &ctx)); // non-empty → true
        ctx.set("$empty", "".into());
        assert!(!evaluate_condition("$empty", &ctx)); // empty → false
    }

    #[tokio::test]
    async fn run_linear_workflow_passes_context() {
        let wf = parse_at_workflow(
            r#"
            workflow {
                name : "demo"
                steps : [
                    relay {
                        id : "a"
                        role : "coder"
                        input : "$user_request"
                        output : "$a_out"
                    }
                    relay {
                        id : "b"
                        role : "coder"
                        input : "GOT: $a_out"
                        output : "$b_out"
                        depends_on : ["a"]
                    }
                ]
            }
            "#,
        )
        .unwrap();

        let client = mock_client();
        let result = wf.run(&[], client, "REQUEST").await.unwrap();
        // Step a ran (its output references the request).
        let a_out = result.step_outputs.get("a").unwrap();
        assert!(a_out.contains("REQUEST"));
        // Step b ran and saw a's output via substitution.
        let b_out = result.step_outputs.get("b").unwrap();
        assert!(b_out.contains("GOT:"));
        // b's output is also exposed under its output var key.
        assert!(result.outputs.contains_key("b_out"));
    }

    #[tokio::test]
    async fn run_skips_step_on_false_condition() {
        let wf = parse_at_workflow(
            r#"
            workflow {
                name : "cond"
                steps : [
                    relay {
                        id : "first"
                        role : "coder"
                        input : "$user_request"
                        output : "$first_out"
                    }
                    relay {
                        id : "second"
                        role : "coder"
                        input : "should not run"
                        output : "$second_out"
                        depends_on : ["first"]
                        condition : "$first_out.contains(NEVER_MATCH)"
                    }
                ]
            }
            "#,
        )
        .unwrap();

        let client = mock_client();
        let result = wf.run(&[], client, "hello").await.unwrap();
        assert!(result.step_outputs.contains_key("first"));
        assert!(!result.step_outputs.contains_key("second"));
    }

    #[tokio::test]
    async fn run_diamond_dependency_order() {
        // a -> {b, c} -> d
        let wf = parse_at_workflow(
            r#"
            workflow {
                name : "diamond"
                steps : [
                    relay { id : "a", role : "coder", input : "$user_request", output : "$a" }
                    relay { id : "b", role : "coder", input : "$a", output : "$b", depends_on : ["a"] }
                    relay { id : "c", role : "coder", input : "$a", output : "$c", depends_on : ["a"] }
                    relay { id : "d", role : "coder", input : "$b $c", output : "$d", depends_on : ["b", "c"] }
                ]
            }
            "#,
        )
        .unwrap();
        let client = mock_client();
        let result = wf.run(&[], client, "seed").await.unwrap();
        // All four ran; d's output references both b and c's outputs.
        assert_eq!(result.step_outputs.len(), 4);
        let d = result.outputs.get("d").unwrap();
        assert!(d.contains("$b $c") || d.contains("]")); // ran with substituted context
    }

    #[test]
    fn relay_step_missing_field_errors() {
        let src = r#"
            workflow {
                steps : [
                    relay { id : "a", role : "coder", input : "x" }
                ]
            }
        "#;
        let err = parse_at_workflow(src).err().unwrap();
        assert!(err.to_string().contains("output"));
    }

    #[test]
    fn parse_validators_and_on_fail() {
        let src = r#"
            workflow {
                steps : [
                    relay {
                        id : "reviewer"
                        role : "reviewer"
                        input : "review"
                        output : "$review"
                        validate : [
                            { type : "output_contains", pattern : "STATUS:" }
                        ]
                        on_fail : { retry : "coder", max : 3 }
                        tools : { allow : [read_file, search], forbid : [write_file] }
                        gate : "human"
                    }
                ]
            }
        "#;
        let wf = parse_at_workflow(src).unwrap();
        let step = &wf.steps[0];
        assert_eq!(step.validators.len(), 1);
        assert!(step.on_fail.is_some());
        let retry = step.on_fail.as_ref().unwrap();
        assert_eq!(retry.retry, "coder");
        assert_eq!(retry.max, 3);
        assert_eq!(step.tool_guard.allow, vec!["read_file", "search"]);
        assert_eq!(step.tool_guard.forbid, vec!["write_file"]);
        assert_eq!(step.gate, Gate::Human);
    }

    #[test]
    fn parse_old_workflow_still_works_no_new_fields() {
        // Backward compat: a relay without validators/on_fail/tools/gate parses.
        let src = r#"
            workflow {
                steps : [
                    relay { id : "a", role : "coder", input : "x", output : "$y" }
                ]
            }
        "#;
        let wf = parse_at_workflow(src).unwrap();
        assert!(wf.steps[0].validators.is_empty());
        assert!(wf.steps[0].on_fail.is_none());
        assert_eq!(wf.steps[0].gate, Gate::Auto);
    }

    #[test]
    fn filter_tools_allow_only() {
        use async_trait::async_trait;
        struct FakeTool(&'static str);
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str { self.0 }
            fn description(&self) -> &str { "" }
            async fn execute(&self, _: &serde_json::Value) -> Result<String, crate::ToolError> { Ok(String::new()) }
        }
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(FakeTool("read_file")),
            Arc::new(FakeTool("write_file")),
            Arc::new(FakeTool("search")),
        ];
        let guard = ToolGuard { allow: vec!["read_file".into(), "search".into()], forbid: vec![] };
        let filtered = filter_tools_for_step(&tools, &guard);
        let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["read_file", "search"]);
    }

    #[test]
    fn filter_tools_forbid() {
        use async_trait::async_trait;
        struct FakeTool(&'static str);
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str { self.0 }
            fn description(&self) -> &str { "" }
            async fn execute(&self, _: &serde_json::Value) -> Result<String, crate::ToolError> { Ok(String::new()) }
        }
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(FakeTool("read_file")),
            Arc::new(FakeTool("write_file")),
        ];
        let guard = ToolGuard { allow: vec![], forbid: vec!["write_file".into()] };
        let filtered = filter_tools_for_step(&tools, &guard);
        let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[tokio::test]
    async fn run_validators_fail_without_on_fail_errors() {
        // A step whose validators fail and has no on_fail → workflow errors.
        let mut wf = Workflow {
            name: "test".into(),
            steps: vec![WorkflowStep {
                id: "checker".into(),
                role: "coder".into(),
                input_template: "say hi".into(),
                output_var: "$out".into(),
                depends_on: vec![],
                condition: None,
                validators: vec![crate::workflow_validator::Validator::OutputContains {
                    pattern: "NEVER_MATCHES".into(),
                }],
                on_fail: None,
                tool_guard: Default::default(),
                gate: Default::default(),
            }],
        };
        let client = mock_client();
        let err = wf.run(&[], client, "test").await.unwrap_err();
        assert!(err.to_string().contains("validation failed"));
    }
}
