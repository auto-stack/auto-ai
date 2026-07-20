# Plan 009: auto-ai-cli 三种执行模式 + 自动路由

> **状态**：已批准，实施中
> **仓库**：`auto-ai`（auto-ai-cli + auto-ai-agent）
> **前置**：Plan 007（auto-ai-cli）、Plan 008（orchestration 下沉）

## 4 个 Phase
1. 改写 assistant soul（路由规则）
2. spawn_pipeline 工具
3. 内置 flow + AgentFactory
4. chat 集成 + --mode

---

## 实施状态复核（2026-07-20，见 docs/reviews/002）

4 个 Phase 均已实现，但有 **2 个遗留问题**：

- **🟡 D2 — spawn_pipeline 的 flow 与 Plan 007b 冲突**：`auto-ai-cli/src/spawn_pipeline.rs:21-28` 的 superpowers flow 用 `assistant`/`coder`/`reviewer`，而 Plan 007b + musk `flows.rs` 定义的是 super-roles（`super-advisor`/`super-coder`/`super-tester`）。同一个 "superpowers" 模式名在 cli 和 musk 语义不同。**处理方式：在 spawn_pipeline.rs 注释说明这是 demo 简化版**（因为 auto-ai-cli 是独立 demo，不依赖 musk 的 super-roles）。
- **🟢 死代码 placeholder**：`spawn_pipeline.rs:204-207` 的 `final_summary_clone` 是 no-op 死代码（函数体空，调用点无用）。**处理方式：删除**（见修复）。
- **Phase 4 部分实现**：`--mode superpowers/relay` 走 legacy 文本 REPL，不走 TUI；TUI 仅支持 normal（自动路由）。这是设计取舍（TUI 的 spawn_pipeline 走 assistant 自动路由，而非强制模式），可接受。
- **测试**：`spawn_pipeline.rs` 0 测试，建议补 `flow_for` 的返回值单测。

