# Review 005 — 转译版 agent 行为等价矩阵（Plan 022 Phase 4）

> **日期**：2026-08-07
> **计划**：Plan 022 Phase 4 — 行为等价审计矩阵
> **目标**：系统性对照转译版（`auto_ai_agent_a2r` / `rust/src/`）与原生版
> （`auto_ai_agent` / `rust-ref/src/`）的每个功能点，标注状态与验证方式，
> 确保无遗漏。

## 状态图例

| 标记 | 含义 |
|---|---|
| ✅ | 行为等价，有自动化测试覆盖 |
| 🟡 | 行为等价，无独立测试（Plan 021 审计或代码审查确认） |
| 🔴 | 有行为缺陷（Plan 022 Phase 3 已修复） |
| ⚫️ | 架构性限制（Auto 语言约束，记录为主） |
| ⬜ | 需 live e2e 验证（需 API key） |

---

## 1. ReAct 循环（agent.rs）

| 功能点 | 状态 | 转译版测试 | rust-ref 对标 | 说明 |
|---|---|---|---|---|
| 工具调用→反馈→结束 | ✅ | T1 | `run_tool_then_finish` | 完整 ReAct 单工具循环 |
| 纯文本单轮对话 | ✅ | T2 | `run_plain_text` | 无工具路径 |
| max_turns 限制（soft→hard×5） | ✅ | T3 | `run_exceeds_hard_turn_cap` | soft 目标 + hard cap 语义一致 |
| client 错误传播 | ✅ | T4 | ErrorClient 场景 | `AgentError::Client` |
| 多轮工具链 | ✅ | T5 | 多轮测试 | 连续工具调用 |
| 容量预警（near-cap warning） | 🟡 | — | agent.rs:370 | 逻辑转译一致（remaining≤5），无独立测试 |
| 循环检测（loop detection） | 🟡 | — | agent.rs seen-map | 逻辑转译一致（LOOP_DETECT_THRESHOLD） |

## 2. Tool 系统（tool.rs）

| 功能点 | 状态 | 转译版测试 | rust-ref 对标 | 说明 |
|---|---|---|---|---|
| `register_tool` 注册可调用 | ✅ | T6 | tool.rs 注册测试 | `Box<dyn Tool>` 入口 |
| `register_shared` 等价 | ✅ | T7 | tool.rs 注册测试 | `Arc<dyn Tool>` 入口，与 register 行为一致 |
| register vs register_shared 签名差异 | ⚫️ | — | — | Auto 无泛型方法（`<T:Tool>`），用 Box/Arc 双入口替代；**行为等价**（Plan 021 缺口 2） |
| 未注册工具→错误消息 | ✅ | T8 | tool.rs 未注册 | `exec_or_msg` 吞错为消息 |
| `register_skill_tool` 注册工具 | ✅ | T9 | agent.rs:259 | **Plan 022 Phase 3.1 修复**（原只存 block） |
| SkillTool 执行 | ✅ | T9 | skill.rs | 空注册表 SkillTool 可执行 |
| `tool_to_definition` 序列化 | 🟡 | — | tool.rs | 转译逻辑一致 |

## 3. Role 系统（role_def.rs / builtin_roles）

| 功能点 | 状态 | 转译版测试 | rust-ref 对标 | 说明 |
|---|---|---|---|---|
| `load_builtin` 加载 | ✅ | T10 | builtin_roles 测试 | 14 个角色 |
| `builtin_names` 完整性 | ✅ | T10 | builtin_roles | 14 名全覆盖 |
| 内置角色真实 souls | ✅ | T11 | mvp_harness 灵魂检查 | >50 字符，非占位符 |
| 未知角色返回 None | ✅ | T10 | builtin_roles | |
| `preferred_provider` 传播 | ✅ | T14 | agent.rs:609 | **Plan 022 Phase 3.2 修复**（原硬编码 None） |
| Role trait 其余方法 | 🟡 | — | role_def.rs | model_tier/temperature/allowed_tools 等转译一致 |
| trait API 分叉（String/&str, u32/usize） | ⚫️ | — | — | Auto 语言限制，固有分叉（Plan 021 既定） |

## 4. Memory（memory.rs）

| 功能点 | 状态 | 转译版测试 | rust-ref 对标 | 说明 |
|---|---|---|---|---|
| 消息添加（add/add_message） | 🟡 | — | memory.rs | 转译逻辑一致 |
| memory_limit 截断（trim） | 🟡 | — | memory.rs:trim | 转译逻辑一致（Role.memory_limit） |
| to_messages 序列化 | 🟡 | — | memory.rs | |

> **注**：Memory 在 ReAct 循环中被间接测试（T1-T5 都经过 memory.add/to_messages），
> 但无独立的截断行为测试。建议未来补充。

## 5. 流式事件（EventSink actor）

| 功能点 | 状态 | 转译版测试 | rust-ref 对标 | 说明 |
|---|---|---|---|---|
| `run` 内部 spawn 丢弃 sink | 🟡 | T1-T5 经 run | agent.rs:307 | run 路径用内部 sink，事件被丢弃 |
| `run_stream` 事件外发 | ✅ | T12 | driver.rs 流式 | Done 事件到达 sink |
| cancel 响应 | ✅ | T13 | driver.rs cancel | 提前返回 |
| per-token Delta 流式 | ⚫️ | — | — | **架构性限制**：转译版 ReAct 循环用非流式 `complete()`，无 per-token Delta（详见 §8） |
| `Thinking`/reasoning 事件 | ⚫️ | — | agent.rs:384 | **架构性限制**：下游于非流式循环（详见 §8） |
| Transport SSE（侧信道） | ⬜ | — | — | `StreamingAiClient` 侧信道部分补偿（main.rs printer thread），需 live 验证 |

## 6. Orchestration（driver/pipeline/flow/handoff/budget）

> Plan 021 Phase 5 已审计：**转译版无功能缺失**（业务代码差异为 a2r codegen 膨胀，
> rust/ 比 rust-ref 业务代码更多）。本节确认状态。

| 功能点 | 状态 | 转译版测试 | rust-ref 对标 | 说明 |
|---|---|---|---|---|
| PipelineDriver.drive 驱动 | 🟡 | — | driver.rs 5 集成测试 | Plan 021 Phase 5 审计等价 |
| gate 处理（resolve_gate_auto） | 🟡 | — | driver.rs | 转译版方法更多（13 vs 7） |
| Delta/Tool→PipelineEvent 转发 | 🟡 | — | driver.rs | Plan 021 Phase 6.6 已接通 |
| PipelineEngine 状态机 | 🟡 | — | pipeline.rs | AdvanceResult/Status/Gate 逻辑一致 |
| budget 跟踪 | 🟡 | — | budget.rs | 转译一致 |
| flow spec / handoff | 🟡 | — | flow.rs/handoff.rs | 转译一致 |
| API 兼容性差异 | ⚫️ | — | — | PipelineDriver 非泛型+`Box<dyn AgentFactory>`；drive 用 `fn` 指针非闭包（Plan 021 Phase 5.3） |

> **建议**：orchestration 场景无转译版独立测试。Plan 022 §4.4 建议视缺口决定是否
> 新增 `rust/tests/transpiled_orchestration.rs`（对标 rust-ref driver.rs 5 集成测试）。
> 当前定级为低优先（Plan 021 已审计功能等价，且 orchestration 不在 `auto-ai-react`
> 二进制运行链路内）。

## 7. 配置与序列化

| 功能点 | 状态 | 转译版测试 | rust-ref 对标 | 说明 |
|---|---|---|---|---|
| ai-config wire 类型 | ⚫️ | — | — | 转译版 wire.rs 无 serde derive（**死代码**，不在运行链路——转译版用真实 rust-ref ai-config） |
| role_config.at serde 风格 | ✅ | — | Plan 021 Phase 4 | Plan 021 已完成迁移（loader.at/role_config.at） |
| ClientConfig 解析 | ✅ | — | Plan 021 Phase 4 | provider 错误硬传播已对齐 |

## 8. 架构性限制汇总（⚫️ 项，记录为主）

这些是 Auto 语言固有限制导致的、无法在纯 `.at` 源码修复的分叉：

### 8.1 ReAct 循环非流式（complete vs complete_stream）
- **根因**：Auto 不能表达 `Arc<dyn Fn(serde_json::Value)>` 回调参数
- **现状**：转译版 `run_inner` 用 `client.complete()`（非流式）；agent 层事件流式
  经 EventSink actor 恢复（turn/tool 级），但 **per-token Delta 不经 StreamEvent**
- **补偿**：`StreamingAiClient`（client_impl.rs）侧信道转发原始 SSE 到 mpsc channel，
  `auto-ai-react` 的 printer thread 实现实时 token 显示（绕过 StreamEvent 系统）
- **影响**：`StreamEvent::Delta`/`Thinking` 在转译版是"死"变体（不被 ReAct 循环 emit）
- **转正前置**：需 auto-lang 支持 dyn-Fn 参数，或侧信道正式化（见 KNOWN-DEBT）

### 8.2 trait API 分叉
- **根因**：Auto 无 `&self` 返回 `&str`、无 `Send + Sync` bound、无泛型方法
- **现状**：`Tool`/`Role`/`Client` 用 owned/String/u32；`Agent` 用 `Box<dyn>` 而非 `Arc<dyn>`
- **影响**：转译版与 rust-ref API 不兼容，下游消费者（CLI、测试）无法直接替换
- **转正前置**：需全量适配下游，或 auto-lang 增强（见 KNOWN-DEBT）

### 8.3 枚举表示
- tuple variant（转译版）vs struct variant（rust-ref）——代码风格差异，序列化行为
  依赖 serde derive（转译版 wire 类型是死代码，实际用 rust-ref，故无影响）

---

## 9. 结论

**无 🔴 遗留缺陷**（Phase 3 已修复 2 个真 bug）。

转译版 agent 层的行为与 rust-ref **核心等价**：
- ReAct 循环、Tool 系统、Role 系统、流式事件（turn/tool 级）均有自动化测试覆盖（14 测试）
- 2 个真缺陷（register_skill_tool / preferred_provider）已修复并验证
- Orchestration 经 Plan 021 审计功能等价（无独立测试，低优先）
- 架构性限制（per-token 流式 / trait API 分叉）已记录，转正前置

**转正（翻 `[lib] path`）就绪度**：核心 agent 行为等价已验证；阻塞项是架构性限制
（8.1/8.2）需 auto-lang 工程或下游全量适配，属独立计划范畴。
