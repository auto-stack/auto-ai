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
