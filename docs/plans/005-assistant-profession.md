# Plan 005: 添加 Assistant Profession

> **Status**: Draft — 待实施
> **仓库**: `auto-ai`(crates/auto-ai-agent)
> **背景**: auto-ai-agent 有 7 个内置 profession(coder/architect/tester/reviewer/documenter/translator/runner),缺少 `assistant`——用户对话的第一入口角色。musk 及未来所有 app 的 chat 默认应使用 assistant,而非 coder。

---

## 1. 改动文件

### 1.1 新建 `crates/auto-ai-agent/resources/souls/assistant.md`

通用对话助手的灵魂文件。参考 auto-forge 的 assistant soul,去掉编排工具(handoff/relay/spawn),保留对话型人格(Nicole)。

### 1.2 新建 `crates/auto-ai-agent/src/professions/assistant.rs`

仿照 `coder.rs` 结构,实现 `Profession` trait:
- `model_tier`: Mid(分流/对话不需最强模型)
- `max_turns`: 12(对话型)
- `allowed_tools`: 只读 + 轻量(read_file/search/list_dir/run_command),不含 write/edit
- `temperature`: 0.3

### 1.3 修改 `crates/auto-ai-agent/src/professions/mod.rs`

在 `load_builtin` 和 `builtin_names` 注册 Assistant(放第一个,作为默认入口)。

## 2. 不做
- ❌ 不加编排工具(bring_in/dispatch/spawn_relay)——app 层职责
- ❌ 不加 handoff_to/dispatchable_to——musk relay 层元数据
- ❌ 不改 auto-forge 的编排型 soul——auto-ai-agent 的 assistant 是精简对话型

## 3. 验收
- `cargo test -p auto-ai-agent` 通过(含 assistant_identity 测试)
- `load_builtin("assistant")` 返回 Some
- 完成后 musk 的 `superpowers.at` 可把 profession 改成 `"assistant"`

---

## 实施状态复核（2026-07-20，见 docs/reviews/002）

- **核心已完整实现**：`assistant.md` soul、`assistant.rs` Role trait 实现、`load_builtin("assistant")` 注册、`assistant_identity` 测试、musk `superpowers.at` 改为 `role: "assistant"` 全部到位。
- **🟡 D3（计划偏离）— `allowed_tools` 未限制**：§1.2 要求 read-only（read_file/search/list_dir/run_command，不含 write/edit），但 `assistant.rs:38-43` 实际返回 `Vec::new()`（不过滤），注释说"由 mode 的工具白名单约束"。这是设计决策的演进——把工具限制责任交给 mode 层。**处理方式：更新本计划 §1.2 说明此偏离**（见修复），而非改回 read-only（因为 TUI 的 assistant 现在需要 write/edit 能力来执行文件操作）。
- **`max_turns` 从 12 改成 20**：`assistant.rs:32-37` 注释解释"12 太紧，read-then-answer 工作流会过早耗尽"。是有据调优，回写文档即可。
- **`handoff_to()`**：实际加了（`assistant.rs:46-48`），但 §2 "不做" 排除了 handoff——这是 Plan 008 后加的，不算 Plan 005 违规，但文档可注明。

