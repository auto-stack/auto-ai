# 编排能力下沉设计：从 auto-musk/auto-forge 到 auto-ai-agent

> **状态**：设计文档，待评审
> **日期**：2026-07-14
> **影响**：auto-ai-agent（核心新增）、auto-musk（relay 改为消费方）、auto-forge（参考）
> **前置**：Plan 004（Roles）、Plan 005（Assistant）、Plan 006（streaming tool_calls）、Plan 007（auto-ai-cli）

---

## 1. 问题陈述

auto-ai-agent 提供通用的 Role / Skill / Tool / Agent ReAct 循环，但**不包含多 agent 编排能力**（handoff / dispatch / pipeline / budget 强制）。这些能力目前在 auto-musk 的 `relay/` 模块中独立实现，与 auto-forge 的 `relay/` 平行存在。

**后果**：
- 每个 app 都要自己实现一遍编排逻辑（musk 做了，未来其他 app 还得再做）
- auto-ai-cli 这种"参考 demo"无法展示多 agent 协作
- Role 的 `token_budget` 字段只存不用（"not yet enforced"）
- Workflow 引擎的 Gate::Human 只打日志不强制

**目标**：把通用的编排原语下沉到 auto-ai-agent，使所有 app 共享。

---

## 2. 三仓库现状对比

### 2.1 auto-forge（最完整的参考实现）

auto-forge 的 `relay/` 有 21 个模块，架构最成熟：

| 层 | 模块 | 通用? |
|---|---|---|
| 身份层 | `soul.rs` / `profession.rs` / `skills.rs` / `agent.rs` / `config.rs` | 结构通用，内容 app 专属 |
| 编排层 | `flow.rs` / `pipeline.rs` / `budget.rs` / `handoff.rs` / `turn.rs` | **通用** |
| 绑定层 | `driver.rs` / `store.rs` / `api.rs` / `checkpoint.rs` | app 专属 |

**prompt 组装顺序**（auto-forge `agent.rs`）：
```
identity → soul → profession scope → owned/readable sections → tools
→ skill fragments → role mandates → relay-mode → tool tips → constraints
```
其中 steps 4/5/8（spec sections、role mandates）是 forge 专属，其余通用。

### 2.2 auto-musk（从 forge 移植，简化版）

musk 的 `relay/` 有 8 个文件，是 forge 的精简版：
- **完全独立于** auto-ai-agent 的 Workflow 引擎（不复用）
- 唯一复用的 auto-ai-agent 原语：`Agent::run_stream`（单步 ReAct 循环）
- `PipelineEngine`（纯 Rust FSM，真实 gate 强制 + budget hardstop + handoff auto-correction）
- `HandoffDocument`（结构化 agent 间消息）
- `BudgetTracker`（token 预算强制）
- `FlowSpec`（线性 + 循环路由）
- `ProfessionRegistry`（编排元数据：handoff_to / dispatchable_to / approval_gates）
- `driver.rs`（编排循环：FSM 推进 → 建 agent → run_stream → handoff → 循环）

### 2.3 auto-ai-agent（当前）

- `Role` trait：纯静态人格（无 handoff/dispatch）
- `Workflow` 引擎：DAG 拓扑排序，Gate::Human **只打日志不强制**
- `RelayTarget` trait：1 方法，标记为 "v2 concern"
- `Role::token_budget()`：**"stored only, not yet enforced"**
- 无 `HandoffDocument` / `BudgetTracker` / `PipelineEngine`

---

## 3. 下沉决策

### 3.1 应该下沉的（通用，零 app 依赖）

| # | 能力 | 来源 | 大小 | 依赖 |
|---|---|---|---|---|
| 1 | **HandoffDocument** | musk `handoff.rs` | ~8KB | 纯 serde 类型 + markdown render |
| 2 | **BudgetTracker** | musk `budget.rs` | ~5KB | 纯 Rust |
| 3 | **FlowSpec + ExitRouting** | musk `flow.rs` 类型 | ~3KB | 纯 serde 类型 |
| 4 | **PipelineEngine** | musk `pipeline.rs` | ~23KB | 纯 Rust FSM（依赖 1+2+3） |
| 5 | **StepValidator / ToolGuard** | musk `flow.rs` | ~3KB | 纯 Rust |
| 6 | **PipelineDriver**（参数化） | musk `driver.rs` 提炼 | ~5KB | 需参数化 agent 工厂 |

### 3.2 应该留在 app 层的

| 能力 | 为什么留 |
|---|---|
| HTTP/SSE API（`store.rs` / `api.rs`） | app 专属的传输层 |
| ConversationStore 双写 | app 专属的持久化 |
| `SpawnRelay` / `Dispatch` / `BringIn` 工具 | 依赖 app 的 conversation/workspace |
| `ForgePhase` / `SectionType` ACL | forge/musk 专属的业务概念 |
| 具体内置 Flow 定义（4 个 flow） | app 产品决策 |
| `validate_step` 的 step_id 硬编码 | app 业务逻辑 |

### 3.3 两套引擎的处理

auto-ai-agent 已有 `Workflow`（DAG）。下沉 `PipelineEngine` 后有两套。

**决策：统一到 PipelineEngine，废弃 Workflow。**

理由：
- `PipelineEngine` 严格更强（真实 gate / budget / loop / pause-resume / auto-validation / handoff auto-correction）
- `Workflow` 的 DAG 拓扑排序可以通过 `ExitRouting::Condition` 递归表达
- `Workflow` 的 `$var` 替代可以保留为 `PipelineEngine` 的一个 context 功能
- 保持两套引擎会让用户困惑"该用哪个"

**迁移策略**：PipelineEngine 入驻后，Workflow 标记 `#[deprecated]`，现有调用方逐步迁移。

---

## 4. 目标架构

```
auto-ai-agent（通用层）
├── role_def.rs          ← Role trait（人格：不变）
├── builtin_roles/       ← 内置 Role（不变）
├── agent.rs             ← 单 agent ReAct 循环（不变）
├── tool.rs / skill.rs   ← 工具 + 技能（不变）
├── roles.rs             ← RoleRegistry（不变）
├── orchestration/       ← 新模块（从 musk relay/ 下沉）
│   ├── mod.rs
│   ├── handoff.rs       ← HandoffDocument（通用版，去 spec_updates）
│   ├── budget.rs        ← BudgetTracker（原样移植）
│   ├── flow.rs          ← FlowSpec / FlowStep / ExitRouting / GateType
│   ├── pipeline.rs      ← PipelineEngine（去 validate_step 硬编码）
│   ├── validator.rs     ← StepValidator / ToolGuard（原样）
│   └── driver.rs        ← PipelineDriver<AgentFactory>（参数化）
└── relay.rs             ← 扩展 RelayTarget（加 handoff 能力）

auto-musk（app 层）
├── relay/               ← 改为消费 auto-ai-agent::orchestration
│   ├── mod.rs           ← re-export + app 专属配置
│   ├── profession.rs    ← app 专属元数据（handoff_to 等）
│   ├── store.rs         ← app 专属持久化（不变）
│   └── api.rs           ← app 专属 HTTP/SSE（不变）
├── orch_tools.rs        ← app 专属工具（不变）
└── specs.rs / chats.rs  ← app 专属业务（不变）
```

---

## 5. HandoffDocument 通用版设计

去掉 forge/musk 专属字段，保留通用核心：

```rust
/// 结构化的 agent 间交接文档——替代原始 chat history 的 token 高效方案。
pub struct HandoffDocument {
    pub from: String,              // 来源 role 名
    pub to: String,                // 目标 role 名
    pub summary: String,           // 本次工作的一句话摘要
    pub decisions: Vec<Decision>,  // 做了哪些决定
    pub open_questions: Vec<Question>,  // 遗留问题
    pub work_product: Vec<WorkProduct>, // 产出物（文件列表）
    pub context_for_next: ContextPointers, // 给下一步的上下文
    pub token_usage: TokenUsage,   // token 消耗
}

pub struct Decision { pub title: String, pub rationale: String, pub status: DecisionStatus }
pub struct Question { pub text: String, pub assigned_to: Option<String> }
pub struct WorkProduct { pub path: String, pub description: String }
pub struct ContextPointers { pub files_to_read: Vec<String>, pub warnings: Vec<String> }
pub struct TokenUsage { pub step_tokens: u64, pub cumulative: u64 }
```

**去掉的字段**（app 专属）：
- `spec_updates` → forge/musk 的 spec ledger 专属
- `generated_report` → forge 的 wiki 专属
- `arch_change_flag` → forge 的 architect 专属
- `checkpoint_id` → forge 的 checkpoint 专属
- `run_id` → 编排引擎内部管理，不在 handoff 里

app 层如需扩展字段，可以通过 `HandoffDocument` 的 `serde_json::Value` extension 或 wrapper struct。

---

## 6. PipelineDriver 参数化设计

musk 的 `driver.rs` 硬编码了 `build_agent_from_mode` + `ToolContext`。下沉后需要参数化：

```rust
/// Agent 工厂 trait：app 实现，告诉 driver 如何为某步构建 agent。
pub trait AgentFactory: Send + Sync {
    /// 为 pipeline 的某一步构建一个 agent。
    /// role_name = 该步的 role（如 "coder"、"reviewer"）
    /// handoff = 上一步的交接文档（可能为 None，第一步）
    fn build_agent(
        &self,
        role_name: &str,
        handoff: Option<&HandoffDocument>,
    ) -> Result<Agent, String>;
}

/// 通用的编排驱动器：推进 pipeline + 为每步建 agent + run_stream + handoff。
pub struct PipelineDriver<F: AgentFactory> {
    engine: PipelineEngine,
    factory: F,
}

impl<F: AgentFactory> PipelineDriver<F> {
    pub async fn drive(&mut self, task: &str, on_event: impl Fn(PipelineEvent)) -> Result<(), String>;
}
```

app 实现 `AgentFactory`，注入自己的 agent 构建逻辑（tools、context、safety 等）。

---

## 7. Role trait 扩展（编排字段）

在 Role trait 上**新增**编排相关的默认方法：

```rust
pub trait Role: Send + Sync {
    // ... 现有方法不变 ...

    /// 这个 role 可以交接给哪些 role（编排拓扑）。空 = 不限。
    fn handoff_to(&self) -> Vec<String> { Vec::new() }

    /// 这个 role 可以派发子任务给哪些 role。空 = 不允许。
    fn dispatchable_to(&self) -> Vec<String> { Vec::new() }

    /// 交接前需要人工审批的目标 role。空 = 无审批。
    fn approval_gates(&self) -> Vec<String> { Vec::new() }
}
```

默认全空 = 现有行为不变（向后兼容）。内置 role 按需覆盖（如 coder 可以 handoff_to tester）。

**不加入 Role 的**：`ForgePhase` / `SectionType` / `owned_sections` —— 这些是 forge/musk 业务概念，不属于通用层。

---

## 8. BudgetTracker 接入 Role::token_budget

```rust
// pipeline.rs 中，每步执行后：
if let Some(budget) = role.token_budget() {
    budget_tracker.set_step_budget(&step_id, TokenBudget::hard_stop(budget));
}
// 记录 token 消耗
budget_tracker.record(&step_id, usage.input, usage.output);
// 检查
match budget_tracker.check(&step_id) {
    BudgetAction::HardStop => return Err("token budget exceeded".into()),
    BudgetAction::Warning(remaining) => on_event(PipelineEvent::BudgetWarning { remaining }),
    _ => {}
}
```

这样 `Role::token_budget()` 从"只存不用"变成**真实强制**。

---

## 9. 实施路线

| 阶段 | 内容 | 风险 | 验证 |
|---|---|---|---|
| **1** | `orchestration/handoff.rs` + `budget.rs`（最低风险，纯类型） | 极低 | 单测 |
| **2** | `orchestration/flow.rs` + `validator.rs`（纯类型） | 极低 | 单测 |
| **3** | `orchestration/pipeline.rs`（FSM，去硬编码 validate_step） | 低 | 单测：advance/handoff/gate/pause |
| **4** | Role trait 加 `handoff_to`/`dispatchable_to`/`approval_gates` 默认方法 | 低 | 93 测试不回归 |
| **5** | `orchestration/driver.rs`（AgentFactory trait + PipelineDriver） | 中 | auto-ai-cli 集成测试 |
| **6** | auto-musk relay 改为消费 `auto_ai_agent::orchestration` | 中 | musk relay 端到端不回归 |
| **7** | Workflow 标记 deprecated + 文档迁移指南 | 低 | — |
| **8** | auto-ai-cli 加 `--pipeline` 模式（展示多 agent 编排） | 中 | 手动验证多步流水线 |

---

## 10. 开放问题

| 问题 | 倾向 | 待定 |
|---|---|---|
| Workflow 废弃 vs 保留？ | 废弃（PipelineEngine 更强） | 确认 |
| `HandoffDocument` 的 `spec_updates` 怎么办？ | 去掉；app 用 wrapper 扩展 | 确认 |
| `PipelineDriver` 的 event 类型？ | 新建 `PipelineEvent`（比 musk 的 `RunEvent` 精简） | 设计 |
| 内置 role 的 `handoff_to` 值怎么定？ | 参考 musk profession.rs 的默认值 | 设计 |
| auto-forge 是否也迁移？ | 不迁移（它是参考/遗产）；musk 是主线 | 确认 |
