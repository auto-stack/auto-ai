# Known Issues — 遗留缺陷与后续工作跟踪

> **用途**：汇总跨计划复审发现的、未完全闭环的缺陷与后续工作。每项注明所在仓库、来源计划、
> 触发条件和处理建议。便于后续会话接手，也避免已完成计划归档后这些问题被遗忘。
> **维护**：每次计划复审时更新；问题解决后移到「已解决」节并标注日期。
> **更新日期**：2026-08-04（首次建立，源自 001-015 全计划复审）

---

## 🔴 待修复缺陷（有功能影响）

### F1 — Tier clamp 不生效（跨仓库：auto-musk）

| 字段 | 值 |
|---|---|
| 来源计划 | [004-agent-roles-profession-upgrade](plans/004-agent-roles-profession-upgrade.md)（活动） |
| 所在仓库 | **auto-musk**（非本仓库） |
| 位置 | `auto-musk/backend/crates/musk/src/lib.rs:118-143` |
| 严重度 | 🔴 高（功能缺陷） |

**问题**：Tier clamping 计算了 `clamped` 值却从未赋回——`:118-140` 发出警告，`:143` 仍用原始 role。
声明的 `allowed_tiers` 实际被忽略，用户角色可突破 tier 限制。

**修复方向**：在 auto-musk 的 `OwnedRole` 增加 `override_tier` 字段，clamp 时写入并生效。
注意：plan 004 §5 错误地标此为 ✅，与代码矛盾——该 ✅ 需在修复后纠正。

**触发条件**：任何依赖 allowed_tiers 强制 tier 边界的场景（如受限用户角色调用 max tier 模型）。

---

### F2 — Budget 从 HardStop 降级为 advisory，三方不一致

| 字段 | 值 |
|---|---|
| 来源计划 | [008-orchestration-down](plans/008-orchestration-down.md)（活动） |
| 所在仓库 | auto-ai（本仓库） |
| 位置 | `crates/auto-ai-agent/rust-ref/src/orchestration/pipeline.rs:289,480` |
| 严重度 | 🟡 中（行为与设计文档不符） |

**问题**：Budget 静默从 HardStop 降级为 advisory。`BudgetStrategy::HardStop` 枚举仍存在（暗示可硬停），
但 pipeline 实际只监控不中止（注释 "advisory by design"）。设计文档、枚举、实现三方不一致。

**待决**：明确决策——要么实现真正的 HardStop（pipeline 在 LimitReached 时中止），要么移除
HardStop 枚举并更新设计文档为 advisory-only。当前是"代码已决定 advisory，文档/枚举未跟上"。

**触发条件**：依赖 token budget 硬停的长时间运行 pipeline（超预算时应停却继续跑）。

---

### F4 — build_handoff 用 to_string() 提取 path，dump 整个 JSON

| 字段 | 值 |
|---|---|
| 来源计划 | [008-orchestration-down](plans/008-orchestration-down.md)（活动） |
| 所在仓库 | auto-ai（本仓库） |
| 位置 | `crates/auto-ai-agent/rust-ref/src/orchestration/driver.rs:261 build_handoff`（及 :285 `.to_string()`） |
| 严重度 | 🟡 中（功能瑕疵：handoff 上下文质量差） |

**问题**：`build_handoff` 提取路径时用 `tc.args.to_string()`，把整个 args JSON 序列化塞进去，
而非取 `tc.args["path"]` 字段。导致 handoff 上下文包含冗余 JSON，下游 agent 收到混乱信息。

**修复方向**：改为 `tc.args.get("path")`（或对应字段名），只提取路径字符串。

---

## 🟢 有意暂缓项（低 ROI，记录触发条件）

这些是已完成计划里**主动决定推迟**的工作，非缺陷。记录在此以防遗忘。

### 来自 Plan 011（daemon-client-fix，已归档）

| 项 | 触发复活的条件 |
|---|---|
| M7 — SseParser 抽成共享 crate | 当 CRLF 问题在非 localhost 部署实际出现时 |
| M4 — services 子进程管理加 Drop 实现 | 当 services 子进程泄漏成为问题时 |
| M6 — client `ensure_daemon`/`new()` 改 async | 当阻塞调用实际造成性能问题（目前唯一调用方已绕过） |

### 来自 Plan 012（agent-architecture-fix，已归档）

| 项 | 触发复活的条件 |
|---|---|
| 任务 2.4 — M3 loop 连续计数（软警告） | 当保守计数策略误判真实循环时（当前更安全） |

### 来自 Plan 008（orchestration-down，活动）

| 项 | 说明 |
|---|---|
| driver 0 单元测试 | §5 要求 MockAgentFactory 单测，待补 |

---

## 🔵 后续大块工作（需新计划）

### Plan 015 后续 — 剩余 2 crate 的 Auto 转译

| 字段 | 值 |
|---|---|
| 来源计划 | [015-auto-lang-migration](plans/015-auto-lang-migration.md)（活动） |
| 所在仓库 | auto-ai（本仓库） |

**待做**：
1. `crates/ai-config/` 跑 `./retranspile.sh check`，目标 0 错误，填充 `rust/src/*.rs`（当前为空）
2. `crates/auto-ai-client/` 同上（当前为空）
3. 3 个 crate 的 Auto 版逐个对照 rust-ref 做功能对齐评估（参考 plan 013 的 G1-G6 方法）
4. Auto 版功能完全对齐后，把主 workspace `[lib] path` 从 rust-ref 切到 rust，删除 rust-ref（届时起 plan-016）

**当前状态**：仅 auto-ai-agent 完成转译并通过 cargo check；链路已验证通畅。

---

## ✅ 已解决

（暂无。问题解决后在此登记：编号 + 解决日期 + 简述 + 关联 commit。）

---

## 复审参考

- 2026-08-04 全计划复审：覆盖 001-015 共 14 个计划文档，结论详见各计划文首状态行。
- 历史复审 `docs/reviews/002-historical-plans-review.md`（2026-07-20）：覆盖 001-009 的代码审计。
- `docs/reviews/001-daemon-client-review.md`、`docs/reviews/003-architecture-review.md`：
  分别对应 plan 011、012 的审查来源。
