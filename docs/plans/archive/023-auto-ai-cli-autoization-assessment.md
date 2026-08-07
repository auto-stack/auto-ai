# Plan 023: auto-ai-cli Auto 化可行性评估 — 不启动

> **状态**：📋 已评估，不启动（2026-08-07）—— 经三份独立调研，完整 Auto 化被 a2r 能力边界
> 硬阻塞；部分 Auto 化成本/收益比低，决策为选项 A（保持纯 Rust）。
> **决策**：选项 A——auto-ai-cli 保持纯 Rust 不动。
> **替代**："Auto 版 e2e 完整流程"已由 `auto-ai-react` 二进制覆盖（Plan 022 验证）。

---

## 1. 评估背景

Plan 022 完成后，考虑把第 4 个 crate `auto-ai-cli`（纯 Rust TUI/CLI 应用）也 Auto 化，
以实现"auto-ai 的 Auto 版 e2e 完整流程"。经三份独立深度调研（agent 调研 a2r TUI 表达力 +
agent 逐文件分析 + agent API 分叉核查），结论是**完整 Auto 化不可行，部分 Auto 化价值低**。

## 2. 核心结论：auto-ai-cli 是应用层，非纯逻辑库

已 Auto 化的 3 个 crate（ai-config / auto-ai-client / auto-ai-agent）都是**纯逻辑库**，
a2r 能完整覆盖。auto-ai-cli 完全不同——它是一个 **TUI/CLI 应用**（2607 行），绝大部分是
Rust 框架绑定：

| 文件 | 行数 | Auto 化可行性 | 原因 |
|---|---|---|---|
| tui.rs | 909 | ❌ 阻塞 | ratatui 的 `Frame<'_>`/`Line<'static>`/`TextArea<'static>` 生命周期参数 + `execute!` 宏 + `terminal.draw(\|f\|...)` render 闭包。a2r **零 lifetime 语法**，全测试套件无一个 lifetime golden |
| main.rs | 805 | 🟡 部分 | clap 结构 + 命令分发可进 .at（struct-style enum 变体已验证支持，golden `vm/10_types/011_enum_multi_field`）；但 tokio runtime + `cfg!` 浏览器启动 + `env!` 宏留 .rs |
| chat_model.rs | 302 | ✅ 可行 | 纯数据模型，chrono + serde_json 桥接 |
| spawn_pipeline.rs | 204 | 🟡 可行（有 API 适配成本） | 消费 driver API，闭包字段是硬阻塞 |
| tools.rs | 185 | 🟡 边界 | 6 个 Tool 实现，`cfg!(windows)` + `std::process::Command` |
| session.rs | 104 | ✅ 可行 | serde derive + std fs，loader.at 直接先例 |
| markdown.rs | 98 | ❌ 阻塞 | 纯 ratatui 版本互转，只服务 tui.rs |

**可 Auto 化：约 600 行（chat_model + session + spawn_pipeline + 部分 tools）**
**必须留 .rs：约 2000 行（tui + main 主体 + markdown）**

## 3. 两个硬阻塞（a2r 根本性限制）

### 3.1 ratatui 的 lifetime 参数（tui.rs 永久阻塞）
a2r 无 lifetime 语法（`<'a>`/`'_`/`<'static>`），全测试套件零 lifetime golden。
ratatui 的核心类型 `Frame<'_>`、`Buffer<'a>`、`Line<'static>`、`Span<'a>`、`Text<'a>`
全部带 lifetime 参数，无法在 .at 表达。`tui-textarea::TextArea<'static>` 同理。
→ tui.rs（909 行）+ markdown.rs（98 行）**永久不可 Auto 化**。

### 3.2 driver 的闭包字段（spawn_pipeline.rs 硬阻塞）
转译版 `PipelineDriver` 的 `gate_handler`/`on_event` 是 `fn` 指针（driver.rs:137-138），
因为 `.at` 不能表达闭包类型字段（仅 `Box<dyn Fn>`/`Arc<dyn Fn>` 作参数/字段值，由 Plan 390 §15.10
支持，但 driver 的字段是裸 `fn` 指针）。main.rs:612 的 `drive()` 回调**捕获 `current_turn`**
（用于格式化 Done 事件），plain `fn` 无法捕获状态。
→ 需手写 Rust 胶水包装，或改 a2r 支持闭包字段（大工程）。

### 3.3 附带：StreamEvent/PipelineEvent 的 tuple 变体
转译版用 tuple 变体（`Delta(text)`），CLI 的 ~35 个 match 臂用 struct 变体（`Delta { text }`）。
.at 源码本身是 tuple 声明，重转译不能消除——要么改 agent.at 枚举声明（影响 agent crate 全局），
要么逐臂改 CLI。机械但量大。

## 4. 消费转译版 agent 的 API 适配成本

若让 auto-ai-cli 消费转译版 `auto_ai_agent_a2r`（真正的 Auto 版 e2e），需适配 ~21 处 API 分叉：
- `Agent::new`→`new_shared`+Box 包装
- `register_tool`+Box（×12 处）
- `run_stream` 改 TaskRef sink（回调模型从 `Arc<dyn Fn>` 变 actor handle）
- 缺 `preload_messages`（session 恢复断裂）
- `with_context` 签名变（builder → `&mut self` mutator）
- `PipelineDriver::new`+Box factory、`AgentFactory::build_agent` 按值 handoff
- driver 闭包字段（§3.2 硬阻塞）
- StreamEvent/PipelineEvent ~35 match 臂改 tuple（§3.3）

## 5. "Auto 版 e2e 完整流程"已存在

`auto-ai-react` 二进制（`crates/auto-ai-agent/rust/src/main.rs`）就是完整的 Auto 版 agent e2e 入口：
- 转译版 Agent / ReAct 循环（rust/src/agent.rs）
- EventSink 流式（per-token Delta，Plan 022 follow-up）
- 工具调用（EchoTool）
- 手写胶水 StreamingAiClient 桥接真实 AiClient → daemon → LLM

Plan 022 已验证它 e2e 可运行（缺的只是 live API key 实跑）。它没有 TUI 界面，
但作为"Auto 版 agent 的完整 e2e 验证入口"已足够。因此不需要 Auto 化 auto-ai-cli 来"实现 Auto 版 e2e"。

## 6. 决策：选项 A（不启动）

三个选项经评估：

| | A 不启动（选定） | B Phase 0+1 | C Phase 0-3 |
|---|---|---|---|
| 工作量 | 0 | ~1 天 | ~3-5 天 |
| Auto 化行数 | 0 | ~400 行 | ~600 行 |
| 消费转译版 agent | 否 | 否（仍 rust-ref） | 是 |
| 硬阻塞处理 | 不涉及 | 不涉及 | 需闭包字段胶水 |
| "Auto 版 e2e" | 已由 auto-ai-react 覆盖 | 形式上部分 Auto | 混合体 e2e |

**选 A 的理由**：
1. auto-ai-cli 是应用层框架绑定，强行部分 Auto 化得到高复杂度混合体（600 行 Auto + 2000 行 Rust 胶水）
2. 两个硬阻塞（闭包字段、tuple 变体）需大量胶水或改 agent crate，工作量大且引入新复杂度
3. auto-ai-cli 无"转正后成为 workspace 默认"的清晰收益（不像 3 个库 crate）
4. "Auto 版 e2e"已由 auto-ai-react 覆盖，无需 Auto 化 CLI

## 7. 未来重启条件

本评估可在以下条件变化时重启：
- a2r 获得 lifetime 参数语法支持（解除 §3.1 ratatui 阻塞）——那将让 tui.rs Auto 化成为可能
- a2r 获得闭包类型字段支持（解除 §3.2 driver 阻塞）
- 有明确的"必须用 .at 写 TUI"的产品需求（非纯技术纯洁性驱动）

在上述条件未满足前，auto-ai-cli 保持纯 Rust 是正确的工程决策。

---

## 调研证据索引（三份独立调研）

1. **逐文件 Auto 化可行性分析**：7 文件逐一评估（EASY/MEDIUM/HARD/BLOCKED），含外部依赖、
   Rust 惯用法、option A 胶水先例（client_impl.rs/main.rs/echo_tool.rs）
2. **a2r TUI 表达力核查**：use.rust 机制、外部类型构造、derive 透传、事件循环、
   lifetime/泛型——逐项 SUPPORTED/PARTIAL/UNSUPPORTED + golden 证据
3. **API 分叉核查**：clap struct-style enum（✅ 支持，golden `vm/10_types/011_enum_multi_field`）、
   转译版 agent vs rust-ref 的 21 处 API 差异逐项列表
