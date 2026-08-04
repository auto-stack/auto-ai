# Known Issues — 遗留缺陷与后续工作跟踪

> **用途**：汇总跨计划复审发现的、未完全闭环的缺陷与后续工作。每项注明所在仓库、来源计划、
> 触发条件和处理建议。便于后续会话接手，也避免已完成计划归档后这些问题被遗忘。
> **维护**：每次计划复审时更新；问题解决后移到「已解决」节并标注日期。
> **更新日期**：2026-08-04（首次建立，源自 001-015 全计划复审）

---

## 🔴 待修复缺陷（有功能影响）

### F4 — build_handoff path 提取（代码已修，缺回归测试）

| 字段 | 值 |
|---|---|
| 来源计划 | [008-orchestration-down](plans/008-orchestration-down.md)（活动） |
| 所在仓库 | auto-ai（本仓库） |
| 位置 | `crates/auto-ai-agent/rust-ref/src/orchestration/driver.rs:276-292 build_handoff` |
| 严重度 | 🟢 低（代码已修复，仅缺回归测试锁定） |

**状态**：代码已在 commit `3eecf4f`（2026-07-20）修复——`build_handoff` 现用
`tc.args.get("path").and_then(|p| p.as_str()).unwrap_or("?")`，不再 dump 整个 JSON。
.at 版（driver.at:344-370）同样已修。

**剩余工作**：缺回归测试（plan 016 第二波 2.1 会补——`build_handoff` 对 write_file/edit_file
工具调用断言 `work_product[0].path == args["path"]`）。.at 版 driver 0 测试，待补。

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

- **F1 — Tier clamp 不生效**（2026-08-04 解决，auto-musk `c0434f8`）：`OwnedRole` 新增
  `override_tier` 字段 + `with_override_tier()` builder，`model_tier()` 返回 override；
  `build_agent_from_mode` 的 clamp 逻辑改为通过 builder 应用 `clamped`。此前计算了 `clamped`
  却只用于日志导致 `allowed_tiers` 失效。附 3 单元测试。Plan 004 §5 错误的 ✅ 已纠正。
- **F2 — Budget HardStop 三方不一致**（2026-08-04 文档清理）：确认 `BudgetStrategy` 枚举的
  `strategy` 字段是死代码（全代码库无读取），pipeline 实际行为是 advisory（由 `BudgetAction::LimitReached`
  表达，已有测试锁定）。给 `BudgetStrategy` 加 deprecated-in-spirit 文档标注，`new()` 注释说明 advisory，
  消除误导。保留枚举以维持 API 兼容（auto-musk re-export 它但未使用）。rust-ref + .at 双版本同步。

---

## 复审参考

- 2026-08-04 全计划复审：覆盖 001-015 共 14 个计划文档，结论详见各计划文首状态行。
- 历史复审 `docs/reviews/002-historical-plans-review.md`（2026-07-20）：覆盖 001-009 的代码审计。
- `docs/reviews/001-daemon-client-review.md`、`docs/reviews/003-architecture-review.md`：
  分别对应 plan 011、012 的审查来源。
