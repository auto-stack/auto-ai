# 代码审查 002：历史计划（001-009）实施状态复核

- 日期：2026-07-20
- 审查员：代码审查（AI 辅助，人工复核关键结论）
- 范围：`docs/plans/001-009`（9 个历史计划）对照当前代码
- 方法：两个并行子审查（001-005 / 006-009）+ 人工复核 2 个最严重结论
- 目的：找出未实现 / 部分实现 / workaround 项，为后续改善排优先级

## 审查方法与结论可信度

两个子审查分别覆盖计划 001-005 和 006-009，逐任务对照代码验证。完成后**人工复核了 2 个最严重的"功能缺陷"结论**：

| 结论 | 复核结果 |
|---|---|
| Plan 004 Tier 钳制只 warn 不生效（`auto-musk`） | ✅ 属实，`auto-musk/backend/crates/musk/src/lib.rs:126-143` 算了 `clamped` 但没赋回 role |
| Plan 008 budget 从 hardstop 降级为 advisory | ✅ 属实，`orchestration/pipeline.rs:276-282` 注释承认降级，测试 `:466-473` 断言不失败 |

---

## 按严重程度分类

### 🔴 功能缺陷（实际行为与计划承诺不符）

#### F1. Plan 004 — Tier 钳制无效（只 warn，不应用）
- **计划承诺**：§5 明确标 ✅ "Tier 校验生效（越界 warn + 钳制到范围最高）"
- **代码现状**：`auto-musk/backend/crates/musk/src/lib.rs:118-140` 计算了 `clamped` 并 warn，但 `:143` `OwnedRole::new(role)` 用的是**原始 role**，`clamped` 从未赋回。
- **影响**：声明了 `allowed_tiers` 的 Role，mode 触发的 tier 越界时只打日志，实际请求仍带越界 tier。
- **跨仓库注意**：此 bug 在 `auto-musk` 仓库，不在 `auto-ai` 工程内。修复需在 `auto-musk` 进行。
- **修复方向**：给 `OwnedRole` 加 `override_tier: Option<ModelTier>`，钳制后用它构造。

#### F2. Plan 008 — Budget 从 hardstop 悄悄降级为 advisory
- **计划承诺**：§3.1 明确要求 "budget hardstop → Failed"，单测要求 "budget HardStop → Failed"
- **代码现状**：`orchestration/pipeline.rs:276-282` 注释 "Budget is now advisory (monitoring), not a hard stop"；测试 `:466-473` 断言超限**不**失败。但 `budget.rs:50` 的 `BudgetStrategy::HardStop` 枚举仍存在。
- **影响**：文档/枚举/计划与实际行为三方不一致。声明 hardstop 的配置实际不生效。
- **修复方向**：二选一——① 恢复 hardstop 行为（按计划）；② 把 `HardStop` 标 deprecated 并更新计划文档为 advisory。不能维持现状的"行为悄悄改了"。

#### F3. Plan 003 — `run_with_progress` 跳过全部新功能
- **计划承诺**：Plan 003 的四项能力（循环/验证器/工具守卫/门控）
- **代码现状**：`workflow.rs:369-427` 的 `run_with_progress`（流式变体）**没有**调 `filter_tools_for_step`、**没有**跑 validators、**没有**处理 on_fail/gate。流式路径下 Plan 003 等于没做。
- **缓解**：整个 `workflow` 模块已 `#[deprecated]`（Plan 008 的 `PipelineEngine` 替代），实际生产路径走的是 PipelineEngine。所以影响有限，但 deprecated 代码仍有调用方就会暴露。
- **修复方向**：要么补齐 `run_with_progress`，要么直接删除/deprecate 并引导到 PipelineEngine。

#### F4. Plan 008 — `driver.rs` 的 `build_handoff` 有 path 提取 bug
- **代码现状**：`orchestration/driver.rs:256-258` 提取 work_product 时 `tc.args.to_string()` 把**整个 JSON args 当成 path 字符串**（应是 `tc.args["path"]`）。
- **影响**：泛型 driver 的 handoff work_product 字段是错误的 JSON dump，下游 role 拿不到真实路径。
- **修复方向**：解析 `tc.args.get("path")`。

### 🟡 计划偏离 / 未实现任务

#### D1. Plan 008 Phase 2.2 — `StepValidator` + `ToolGuard` 未下沉
- **计划承诺**：Phase 2.2 要求在 `orchestration/` 新建 `validator.rs`，`FlowStep` 加 `validators`/`tool_guard` 字段。
- **代码现状**：`orchestration/` 下**没有** `validator.rs`，`FlowStep` 没有这两个字段。它们仍留在 deprecated 的 `workflow.rs`/`workflow_validator.rs`。
- **修复方向**：要么补齐下沉，要么从计划文档移除该 Phase 并说明"generic pipeline 不做内容验证，留给 app 层"。

#### D2. Plan 009 — spawn_pipeline 的 flow 与 Plan 007b 冲突
- **计划承诺**：Plan 007b 定义 superpowers 用 super-roles（super-advisor/super-coder/super-tester）。
- **代码现状**：`auto-ai-cli/src/spawn_pipeline.rs:21-28` 用的是 `assistant`/`coder`/`reviewer`。同一个 "superpowers" 模式名在 auto-ai-cli 和 auto-musk 语义不同。
- **修复方向**：统一两边的 flow 定义（提取到共享 crate，或在 spawn_pipeline.rs 注释这是 demo 简化版）。

#### D3. Plan 005 — Assistant `allowed_tools` 未按计划限制
- **计划承诺**：§1.2 要求 assistant 只读（read_file/search/list_dir/run_command，不含 write/edit）。
- **代码现状**：`builtin_roles/assistant.rs:38-43` 返回 `Vec::new()`（不过滤），把限制责任推给 mode。
- **修复方向**：要么按计划返回 read-only 列表，要么在计划文档说明"由 mode 负责"。

#### D4. Plan 001 Phase 6/7 — Forge/Ash 迁移未实施
- **计划承诺**：Phase 6（Forge 切到 auto-ai-agent）、Phase 7（Ash F3 升级）。
- **代码现状**：`auto-forge` 无 `auto-ai-agent` 引用；`auto-shell` 的 `ask_ai` 仍直接调 client。`auto-shell/plans/027` 明确写 F3 "unchanged"。
- **修复方向**：在 Plan 001 文档标注 "已废弃，见 auto-shell Plan 027"。

### 🟢 workaround / 死代码 / 过时注释（清理项）

| 位置 | 问题 | 建议 |
|---|---|---|
| `auto-ai-cli/src/spawn_pipeline.rs:204-207` | `final_summary_clone` 是 no-op 死代码 placeholder | 删除函数和调用 |
| `orchestration/driver.rs:242-265` | `build_handoff` 是 lossy stub（summary 暴力截断、to="next" 占位） | 加注释说明泛型层固有限制，或让 AgentFactory 负责 build_handoff |
| `ai-config/src/provider.rs:39-52` | 无 key 时返回 `"no-key-needed"` 占位串 | 改 `auth_required: bool` 显式表达 |
| `auto-ai-client/src/daemon.rs:5,34` | 注释引用已删的 direct mode | 清理注释 |
| `auto-ai-daemon/src/server.rs:96` | "future enhancement" 但 tier_router 已实现 | 更新注释 |

### 测试缺口（精选）

| 范围 | 缺失测试 | 优先级 |
|---|---|---|
| Plan 008 `driver.rs` | 0 测试（计划明确要求 MockAgentFactory 单测） | 中 |
| Plan 006 流式 tool_call | provider 的 SSE delta 累积单测、agent `run_stream` 单测 | 中 |
| Plan 003 循环/工具守卫 | 缺端到端集成测试（重试超限、reviewer 拿不到 write_file） | 低（模块已 deprecated） |
| Plan 009 `spawn_pipeline.rs` | 0 测试 | 低 |

---

## 已完整实现的计划（无遗留）

- **Plan 002**（三库重构）：核心全部落地，仅有少量过时注释（见上表）。
- **Plan 006**（流式 tool_calls）：四任务全部实现，且 malformed args 处理优于计划。唯一缺口是测试。
- **Plan 007b**（三种工作模式 / 6 个 super-roles + musk flows）：完全实现。

---

## 按 ROI 排序的改善优先级

| 序号 | 项 | 仓库 | 难度 | 价值 |
|---|---|---|---|---|
| 1 | F1 Tier 钳制无效 | auto-musk | 低 | 高（安全/计费） |
| 2 | F2 budget hardstop 行为对齐 | auto-ai | 低 | 高（消除文档/代码矛盾） |
| 3 | F4 driver build_handoff path bug | auto-ai | 低 | 中（handoff 正确性） |
| 4 | D1 StepValidator 下沉或移除 | auto-ai | 中 | 中（消除"计划写了没做"） |
| 5 | 清理死代码/过时注释（5 项） | auto-ai | 低 | 低（整洁度） |
| 6 | D2 spawn_pipeline flow 对齐 | auto-ai | 低 | 低（语义一致性） |
| 7 | 补 driver.rs / 流式 tool_call 测试 | auto-ai | 中 | 中（回归保护） |

**跨仓库提示**：F1（Tier 钳制）在 `auto-musk` 仓库，其余均在 `auto-ai`。若要在 `auto-ai` 内修，优先做 F2/F4（低难度、消除真实矛盾）和 D1（消除计划-代码漂移）。

---

## 审查覆盖的计划清单

| 计划 | 状态 | 主要遗留 |
|---|---|---|
| 001 auto-ai-agent 实施 | 基本完成 | Phase 6/7（Forge/Ash 迁移）未做，建议标注废弃 |
| 002 三库重构 | 完整实现 | 仅过时注释 |
| 003 workflow 循环/验证器/门控 | 部分实现 | run_with_progress 跳过新功能；模块已 deprecated |
| 004 Agent Roles | 基本完成 | Tier 钳制无效（auto-musk，真实 bug） |
| 005 Assistant Profession | 基本完成 | allowed_tools 未限制（设计偏离） |
| 006 流式 tool_calls | 完整实现 | 仅测试缺口 |
| 007a auto-ai-cli | 已实现 | write/edit 超出原计划范围（合理演进）；0 测试 |
| 007b 三种工作模式 | 完整实现 | 无 |
| 008 编排下沉 | 大部分实现 | Phase 2.2 未下沉；budget 行为降级；driver path bug；driver 0 测试 |
| 009 执行模式 | 已实现 | spawn_pipeline flow 与 007b 冲突；死代码 placeholder |
