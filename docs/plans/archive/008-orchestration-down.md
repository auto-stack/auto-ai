# Plan 008: 编排能力下沉到 auto-ai-agent

> **状态**：实施计划，待执行
> **日期**：2026-07-14
> **仓库**：`auto-ai`（auto-ai-agent 新增 orchestration 模块）、`auto-musk`（relay 改为消费方）
> **前置**：Plan 004–007
> **设计文档**：[docs/orchestration-down-design.md](../docs/orchestration-down-design.md)

---

## 0. 目标

把 auto-musk relay/ 中的通用编排原语（HandoffDocument、BudgetTracker、PipelineEngine、FlowSpec、PipelineDriver）下沉到 auto-ai-agent，使所有 app（musk、auto-ai-cli、未来 app）共享编排能力。

---

## 1. Phase 1 — 基础类型（handoff + budget）

**交付**：auto-ai-agent 新增 `orchestration/` 模块，含 HandoffDocument + BudgetTracker。

### 1.1 新建 `src/orchestration/mod.rs`
```rust
pub mod handoff;
pub mod budget;
```

### 1.2 `src/orchestration/handoff.rs`
- 从 musk `relay/handoff.rs` 移植 `HandoffDocument` + 子结构
- **去掉** forge 专属字段：`spec_updates`、`generated_report`、`arch_change_flag`、`checkpoint_id`、`run_id`
- 保留：`from`、`to`、`summary`、`decisions`、`open_questions`、`work_product`、`context_for_next`、`token_usage`
- 保留 `render() -> String`（markdown 渲染）
- 单测：构造 + render + round-trip

### 1.3 `src/orchestration/budget.rs`
- 从 musk `relay/budget.rs` 原样移植
- `TokenBudget` / `BudgetStrategy` / `BudgetAction` / `BudgetTracker` / `CostReport`
- 单测：record + check + HardStop

### 1.4 lib.rs 注册 + 导出
```rust
pub mod orchestration;
pub use orchestration::{HandoffDocument, BudgetTracker, ...};
```

**验证**：`cargo test -p auto-ai-agent --lib orchestration`

---

## 2. Phase 2 — 流程定义（flow + validator）

**交付**：FlowSpec / FlowStep / ExitRouting / GateType / StepValidator / ToolGuard。

### 2.1 `src/orchestration/flow.rs`
- 从 musk `relay/flow.rs` 移植**类型定义**
- **不移植** musk 的 4 个内置 flow（`builtin_flows()`）——那是 app 产品决策
- 单测：FlowSpec 构造 + ExitRouting 匹配

### 2.2 `src/orchestration/validator.rs` — ⏸ 未实施（2026-07-20 移除）
> **复核结论（review-002 D1）**：此 Phase 未实施，且决定**不再下沉**。generic pipeline
> 无法定义跨 role 的通用内容验证规则（每个 role 的产出格式不同），内容验证更适合留给
> app 层（如 musk 的 `StepValidator`）。musk 的 `relay/flow.rs` 保留自己的实现，不强行
> 抽到通用层。原计划内容保留如下仅作历史记录：
- ~~从 musk `relay/flow.rs` 移植 `StepValidator` + `ToolGuard`~~
- ~~单测：validator check + ToolGuard guard~~

**验证**：`cargo test -p auto-ai-agent --lib orchestration`

---

## 3. Phase 3 — 状态机（pipeline）

**交付**：PipelineEngine（去硬编码 validate_step）。

### 3.1 `src/orchestration/pipeline.rs`
- 从 musk `relay/pipeline.rs` 移植 `PipelineEngine`
- **改造 `submit_handoff`**：去掉 hardcoded `validate_step` 的 step_id match
  - 改为：如果 FlowStep 有 `validators`，用它们；否则只检查 `handoff.summary` 非空
  - 去掉 code-step escalation（那是 musk 业务逻辑）
- 保留：gate 强制、loop cap、budget hardstop、handoff auto-correction、pause/resume/rerun
- 单测：advance → ExecuteStep → submit_handoff → Completed
- 单测：gate Human → WaitForHuman → resolve_gate(Approve) → 继续
- 单测：budget HardStop → Failed
- 单测：loop max → Paused

**验证**：`cargo test -p auto-ai-agent --lib orchestration::pipeline`

---

## 4. Phase 4 — Role trait 扩展

**交付**：Role trait 新增编排方法（默认空，向后兼容）。

### 4.1 `src/role_def.rs`
```rust
pub trait Role: Send + Sync {
    // ... 现有方法不变 ...

    /// 这个 role 可以交接给哪些 role。空 = 不限。
    fn handoff_to(&self) -> Vec<String> { Vec::new() }

    /// 可以派发子任务给哪些 role。空 = 不允许。
    fn dispatchable_to(&self) -> Vec<String> { Vec::new() }

    /// 交接前需要人工审批的目标。空 = 无审批。
    fn approval_gates(&self) -> Vec<String> { Vec::new() }
}
```

### 4.2 内置 role 覆盖（可选，Phase 4b）
- 参考 musk profession.rs 的默认值，给关键 role 加 `handoff_to`
  - coder → [tester, reviewer]
  - architect → [planner, coder]
  - assistant → [coder, architect, reviewer]（triage 转发）

**验证**：`cargo test -p auto-ai-agent --lib`（93 测试不回归）

---

## 5. Phase 5 — PipelineDriver

**交付**：AgentFactory trait + PipelineDriver（参数化编排循环）。

### 5.1 `src/orchestration/driver.rs`
```rust
pub trait AgentFactory: Send + Sync {
    fn build_agent(&self, role_name: &str, handoff: Option<&HandoffDocument>) -> Result<Agent, String>;
}

pub enum PipelineEvent {
    StepStarted { step_id: String, role: String },
    Delta { text: String },
    ToolCall { tool: String, args: Value, result: String },
    StepCompleted { step_id: String, handoff: HandoffDocument },
    GateWaiting { step_id: String },
    Completed,
    Failed { error: String },
    BudgetWarning { remaining: u64 },
}

pub struct PipelineDriver<F: AgentFactory> {
    engine: PipelineEngine,
    factory: F,
    task: String,
}

impl<F: AgentFactory> PipelineDriver<F> {
    pub fn new(flow: FlowSpec, factory: F, task: &str) -> Self;
    pub async fn drive(&mut self, on_event: impl Fn(PipelineEvent) + Send + Sync) -> Result<(), String>;
}
```

drive() 逻辑（从 musk driver.rs 提炼）：
1. `engine.advance()` → `AdvanceResult`
2. `ExecuteStep` → `factory.build_agent(role, handoff)` → `agent.run_stream(task_with_handoff)` → 收集输出 → 构造 HandoffDocument → `engine.submit_handoff()`
3. `WaitForHuman` → 发 GateWaiting event → 等待外部 `resolve_gate()` → 继续
4. `Completed` → 发 Completed event → 返回
5. `Failed` → 发 Failed event → 返回 Err

**验证**：单测用 MockAgentFactory（返回 canned 回复）

---

## 6. Phase 6 — auto-musk 迁移

**交付**：musk relay 改为消费 `auto_ai_agent::orchestration`。

### 6.1 musk `relay/mod.rs`
- 去掉对本地 handoff/budget/flow/pipeline 的定义
- 改为 `pub use auto_ai_agent::orchestration::{HandoffDocument, BudgetTracker, ...}`
- 保留 app 专属的 store.rs / api.rs / profession.rs

### 6.2 musk `relay/driver.rs`
- 改为实现 `auto_ai_agent::orchestration::AgentFactory`
- 保留 musk 的 build_agent_with_context + ToolContext 注入

### 6.3 musk `relay/pipeline.rs`
- 删除（用 auto-ai-agent 的）
- 如果有 musk 专属的 `validate_step` 逻辑，移到 musk 的 AgentFactory 实现里

**验证**：musk relay 端到端不回归（HTTP API + SSE 流）

---

## 7. Phase 7 — Workflow deprecated

- `workflow.rs` 加 `#[deprecated(note = "use orchestration::PipelineEngine instead")]`
- 文档加迁移指南
- 不删代码（向后兼容）

---

## 8. Phase 8 — auto-ai-cli 编排 demo

**交付**：auto-ai-cli 加 `pipeline` 子命令。

```sh
auto-ai-cli pipeline "实现一个 TODO app"
```

展示：
- assistant 判断 → 转给 architect → coder → tester → reviewer
- 流式输出每步
- gate 审批（CLI 交互式确认）
- HandoffDocument 展示

内置一个简单 FlowSpec（3 步：assistant → coder → reviewer）。

---

## 9. 验证计划

| 阶段 | 验证 |
|---|---|
| 1-3 | `cargo test -p auto-ai-agent --lib orchestration`（纯单测） |
| 4 | `cargo test -p auto-ai-agent --lib`（93+ 测试不回归） |
| 5 | 单测（MockAgentFactory） |
| 6 | musk relay 端到端 + 现有 API 不回归 |
| 7 | 编译通过（deprecated warning） |
| 8 | 手动跑 `auto-ai-cli pipeline "task"` |

---

## 10. 范围边界

- ✅ 下沉 6 项通用原语
- ✅ PipelineDriver 参数化
- ✅ Role trait 加编排方法
- ✅ BudgetTracker 接入 token_budget 强制
- ✅ Workflow deprecated
- ⏸ 不改 auto-forge（它是参考/遗产）
- ⏸ 不迁移 musk 的 HTTP/SSE/conversation 层
- ⏸ 不加 ForgePhase / SectionType 到通用层

---

## 实施状态复核（2026-07-20，见 docs/reviews/002）

大部分 Phase 已实现，但有 **4 个遗留问题**需处理（F2/F4/D1/测试）：

- **Phase 1/2.1/3/4/4b/6/7/8**：已实现。handoff、budget、flow、pipeline、Role trait 扩展、musk 迁移、workflow deprecated、cli demo 全部落地。
- **🟡 D1 — Phase 2.2（StepValidator + ToolGuard 下沉）未实施**：`orchestration/` 下没有 `validator.rs`，`FlowStep` 没有 `validators`/`tool_guard` 字段。它们仍留在 deprecated 的 `workflow.rs`。**处理方式：从本计划移除 Phase 2.2，说明"generic pipeline 不做内容验证，留给 app 层"**（见修复）。
- **🔴 F2 — Budget 从 hardstop 降级为 advisory（行为与文档矛盾）**：`pipeline.rs:276-282` 注释 "advisory, not a hard stop"，测试 `:466-473` 断言超限不失败。但 `budget.rs:50` 的 `BudgetStrategy::HardStop` 枚举仍在，§3.1 要求 hardstop。上方"✅ BudgetTracker 接入 token_budget 强制"与代码不符。**处理方式：更新本计划 §3.1 + 枚举注释为 advisory**（见修复）。
- **🔴 F4 — driver `build_handoff` 的 path 提取 bug**：`driver.rs:256-258` `tc.args.to_string()` 把整个 JSON args 当 path，应解析 `tc.args["path"]`。
- **测试缺口**：`driver.rs` **0 测试**（§5 明确要求 MockAgentFactory 单测）。musk `relay/driver.rs` 测试模块为空。

