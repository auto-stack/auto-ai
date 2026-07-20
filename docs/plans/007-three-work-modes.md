# 007 — 三种工作模式实现计划

> **状态**：实施计划，待执行。
> **日期**：2026-07-10
> **影响**：auto-ai-agent（Part A）+ auto-musk（Part B）

## 0. 背景

auto-musk 设计了三种工作模式：

1. **Chat**：Assistant 直接处理简单任务，可 bring_in 引入特定 agent
2. **Superpowers**：brainstorm→plan→execute→review→spec，中等复杂度
3. **Relay**：brainstorm→design→plan→execute→test→review→spec→report，复杂任务

当前问题：Superpowers 和 Relay 需要的 agent 角色（advisor/planner/gofer/super-advisor/super-coder/super-tester）在 auto-ai-agent 中没有 Role 实现，导致 relay flow 无法构建 agent。

## Part A: auto-ai-agent 新增 6 个 Role

在 `crates/auto-ai-agent/` 新增 6 个 builtin role + soul 文件，从 auto-forge 的 `relay/souls/*.md` 移植（去掉编排工具语义，保留核心人格 + 工作能力）。

### 新增 Role 清单

| Role name | 人格名 | max_turns | model_tier | temperature | 核心职责 |
|---|---|---|---|---|---|
| advisor | Isaac | 40 | Max | 0.3 | 需求澄清 + 写 Goals |
| planner | Felix | 40 | Pro | 0.3 | 设计→带依赖的计划表格 |
| gofer | Gus | 20 | Lite | 0.1 | 差事 agent，只收集事实 |
| super-advisor | Atlas | 120 | Max | 0.3 | 战略架构，brainstorm+设计文档+计划 |
| super-coder | Titan | 120 | Max | 0.3 | 严格按计划执行 |
| super-tester | Argus | 100 | Max | 0.3 | 验证+review+路由回 coder |

### 每个 Role 需要

1. **Soul 文件** `resources/souls/{name}.md`：从 `auto-forge/backend/src/relay/souls/{name}.md` 移植，去掉编排工具相关描述（bring_in/dispatch/spawn_relay/handoff 语义），保留人格 + 工作能力 + 纪律规则。
2. **Role 实现** `src/builtin_roles/{name}.rs`：参照 `coder.rs` 结构，`impl Role`。
3. **注册** `src/builtin_roles/mod.rs`：在 `load_builtin` + `builtin_names` 注册。

### Task A1-A6: 逐个创建 6 个 Role

每个 task：读 auto-forge soul → 创建精简版 soul → 创建 .rs 实现 → 注册到 mod.rs → cargo test 验证。

### Task A7: 全量测试

`cargo test -p auto-ai-agent`，确认所有 builtin role 可加载。

## Part B: auto-musk 新增 flows + 工具 + 命令

### Task B1: superpower flow

`relay/flow.rs` 新增：

```rust
fn superpower_flow() -> FlowSpec {
    let mut flow = FlowSpec::new("superpower");
    flow.add_step(FlowStep::new("brainstorm", "super-advisor"));
    flow.add_step(FlowStep::new("plan", "super-advisor"));
    flow.add_step(FlowStep::new("execute", "super-coder"));
    flow.add_step(FlowStep::new("review", "super-tester"));
    flow
}
```

### Task B2: relay flow

```rust
fn relay_flow() -> FlowSpec {
    let mut flow = FlowSpec::new("relay");
    flow.add_step(FlowStep::new("brainstorm", "advisor"));
    flow.add_step(FlowStep::new("design", "architect"));
    flow.add_step(FlowStep::new("plan", "planner"));
    flow.add_step(FlowStep::new("execute", "coder"));
    flow.add_step(FlowStep::new("testing", "tester"));
    flow.add_step(FlowStep::new("review", "reviewer"));
    flow.add_step(FlowStep::new("report", "documenter"));
    flow
}
```

保留现有 `default` flow（兼容），新增 `superpower` 和 `relay` 两个 flow。

### Task B3: bring_in 工具

`orch_tools.rs` 新增 BringIn 工具——chat 模式下 Nicole 引入特定 agent（如 coder）处理子任务：

```rust
pub struct BringIn { ctx: ToolContext }
// execute: 创建 kind=Errand 子对话，用指定 profession 跑一个完整 turn
```

在 `build_agent_with_context` 注册 bring_in（与 spawn_relay/dispatch 并列）。

### Task B4: 前端斜杠命令

ChatsView.vue：
- `/superpower <task>` — 启动 superpower flow（startRun + advanceRun）
- `/relay <task>` — 更新现有命令，改用 `relay` flow

### Task B5: superpowers.at 工具白名单

加入 `bring_in`。

### Task B6: 验证

编译 + 测试 + 重启 + curl 测试 `/superpower` 和 `/relay` 能创建 run 并启动 driver。

## 实施顺序

Part A（A1-A7）→ Part B（B1-B6）。Part A 必须先完成。
