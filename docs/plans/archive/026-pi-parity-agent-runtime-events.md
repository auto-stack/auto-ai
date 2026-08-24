# Plan 026: agent 运行时对齐 pi-agent-core——turn 级事件、Thinking 一等公民、steering/follow-up 队列与应用层取消

> **状态**：✅ 已实施（2026-08-23，双轨落地，离线两轨对拍全等；live e2e 对拍待 daemon 实跑）
> **仓库**：auto-ai（auto-ai-agent 主改；ai-config wire 小改）+ auto-musk（消费端适配，影响见文末，**不在本计划内实施**——已确认本计划只改 auto-ai）
> **实施记录与计划偏差**：
> 1. `StreamEvent` 实际定义在 auto-ai-agent（agent.rs / agent.at），不在 ai-config wire.at（计划文档笔误）；
>    `TurnEnd.usage` 复用既有 `ai_config::Usage`，未新建 UsageSummary → ai-config 仅补 rust-ref `Usage` 的 `PartialEq, Eq` derive。
> 2. TurnEnd 只在 turn 正常完成时发出（工具批落库 / 终答 / follow-up 复活）；取消与错误不发（turn 未完成）。
> 3. 取消的"每工具前后"检查点实现为工具循环每轮迭代顶部单检查（前=后一等效）；中途取消时为未应答的
>    tool_use 补写占位 tool_result（"[cancelled by user]"）保 wire 合法——计划原文"结果不再注入记忆"会留
>    孤儿 tool_use（1214 教训），实现改为占位。
> 4. steering 计入 turn 上限（按 §5 决策）；取消时清空 steering 并发 Warning 报告丢弃数。
> 5. 转译轨新增 parking_lot 依赖（.at 的 Mutex 惯例）；`.at` 源码注意 `go` 是保留字（循环变量撞关键字会报
>    误导性的 "Expected term, got Go"）。
> **目标**：把 agent 运行时的三个语义缺口补齐——①事件流缺 turn 层；②Thinking 在 .at 轨退化为 Delta；③用户无法中途插话（steering）或叫停（取消）。全部对齐 pi-agent-core 的成熟语义。
> **参考实现**：pi-mono 本地克隆 `D:\github\pi`（main @ a1f955e9f），`packages/agent/`。
> **关联计划**：PLAN-027（content/details 分离）、PLAN-028（压缩，消费 turn 边界与 usage）；auto-musk PLAN-039/040（消费端）。

---

## 0. 问题与动机

现状（`crates/auto-ai-agent/rust-ref/src/agent.rs` + `src/agent.at`）：

1. **`StreamEvent` 扁平无 turn 层**：`Delta | ToolStart | Warning | Tool | Done | Cancelled | Error`（rust-ref 另有 `Thinking`）。UI 和 usage 计量分不清"一次 run 里有几轮 LLM 调用"，每轮消耗多少 token 无从挂靠。musk 前端按轮折叠展示、PLAN-028 的压缩切点判断都依赖 turn 边界。
2. **Thinking 语义双轨不一致**：daemon SSE 已区分 `{"type":"delta"|"reasoning"}`，但 `.at` 移植版把 reasoning 一律当 `Delta` 转发（KNOWN-DEBT 登记在案的缺口）。musk 的 `chats.rs` 持久化了 thinking（ThinkBlock 存活），但 agent 事件流丢失它导致 CLI REPL 染色展示缺失。
3. **无 steering / follow-up**：ReAct 循环 `run_inner` 只有 3 个取消检查点，用户在长任务中途发的消息无处安放——要么排队到整个 run 结束，要么丢失。
4. **取消只在协议层存在**：`CancellationToken` + `CancelOnDrop` 在 daemon 侧工作良好（断连即停拉 token），但 musk 应用层没接（`server.rs` 注释自认 "No cancellation endpoint yet"）。本计划在 auto-ai-agent 侧把取消语义定义清楚，musk 侧的端点接线由 auto-musk PLAN-040 一并处理。

## 1. pi 参考实现索引（移植蓝本）

pi 仓库路径前缀 `D:\github\pi\packages\agent\`：

| 关注点 | pi 位置 | 移植要点 |
|---|---|---|
| 三层事件模型 `AgentEvent`（agent/turn/message+tool_execution 共 10 种） | `src/types.ts` | 我们只补 turn 层：`TurnStart`/`TurnEnd`；message/tool 层已有对应物 |
| turn 事件携带的载荷（本轮 toolResults、usage） | `src/types.ts` 的 `turn_end` 定义 | `TurnEnd { turn_index, usage, tool_count }` |
| 主循环中 turn 事件的发出时机（LLM 调用前/工具批后） | `src/agent-loop.ts` 的 `runAgentLoop` | 在 `run_inner` 的对应位置插入 |
| steering 轮询点：**工具批执行完后、下次 LLM 调用前** | `src/agent-loop.ts` 内层循环末尾 `getSteeringMessages` 轮询 | 关键语义：不打断进行中的工具，注入下一轮上下文 |
| follow-up 语义：agent 本要停止时才轮询 | `src/agent-loop.ts` 外层循环 `getFollowUpMessages` | run 自然结束后若有 follow-up 则继续 |
| `PendingMessageQueue`（steer/followUp 两队列，`QueueMode: all | one-at-a-time`） | `src/agent.ts` | Rust 用 `std::sync::Mutex<VecDeque<Message>>` 即可 |
| `Agent.steer()/follow_up()/abort()/waitForIdle()` API 面 | `src/agent.ts` | 挂到我们的 `Agent` 结构上 |
| abort signal 贯穿到每个工具的 `execute(signal)` | `src/agent-loop.ts` 工具执行段 | 我们的工具 trait 无 signal 参数——**不动 trait**，用 Agent 级 `CancellationToken` 在循环检查点消费（沿用现有 3 检查点模式，加密到工具执行前后） |
| steering 队列在 abort 后的恢复 | `src/agent.ts` 的 `Agent.continue()` drain 逻辑 | 参考，简化为：Cancel 时清空 steering 队列并在事件流报告 |
| Thinking 一等公民（thinking delta 事件） | `packages/ai/src/types.ts` 的 `AssistantMessageEvent.thinking_*` | 修 `.at` 轨：reasoning delta 映射回 `Thinking` 变体而非 `Delta` |

## 2. 方案

### 2.1 事件协议（wire + 两轨同步）

`crates/ai-config/wire.at` 的 `StreamEvent` 增加：

```
| TurnStart { turn: u32 }
| TurnEnd { turn: u32, usage: Option<UsageSummary>, tool_count: u32 }
```

`UsageSummary { input_tokens, output_tokens }` 从 assistant 响应的 usage 字段提取（daemon 已解析，client 透传）。`Thinking` 变体在 rust-ref 已存在，本计划修复 `.at` 轨的退化映射（`src/agent.at` 中 reasoning→Delta 的转换点），使两轨一致。

事件顺序约定（与 pi 对齐）：`TurnStart → Delta*/Thinking* → ToolStart* → Tool* → TurnEnd → … → Done`。

### 2.2 steering / follow-up 队列

`Agent` 增加两个方法与内部队列：

- `steer(msg: Message)`：推入 steering 队列。`run_inner` 在**每个工具结果落回记忆之后、下一次 LLM 调用之前**轮询该队列，有则作为 user 消息注入（不打断当前工具批——与 pi 语义一致）。
- `follow_up(msg: Message)`：推入 follow-up 队列。仅在 run 即将结束时（软上限未触发、无 pending 工具）轮询，非空则以新 user 消息继续循环。

队列用 `Arc<Mutex<VecDeque<_>>>`，`Agent` 与外部（musk 的 axum handler）共享句柄。

### 2.3 取消语义

- `Agent::run` 接受 `CancellationToken`（沿用 daemon 已验证的模式）。
- 检查点从 3 个加密为 5 个：新 turn 前 / LLM 响应后 / **每个工具执行前** / **每个工具执行后** / steering 注入前。
- 取消时：已在执行的工具不中断（工具无 signal 是现状约束），但结果不再注入记忆，直接发 `Cancelled` 收尾。
- **.at 可行性**：`Mutex<VecDeque>` 与检查点模式均有 golden 证据（Plan 025 的 `003_sync`）；`tokio::select!` 不可用，但本方案不需要它——轮询式检查点即可。

### 2.4 ScriptedClient 增强（测试基建）

`crates/auto-ai-agent/tests/mvp_harness.rs` 的 ScriptedClient 增加：

- delta 切块：脚本化响应按 3-5 token 随机切块吐 `Delta`，模拟真实流式；
- `Thinking` 步：脚本可插入 reasoning delta；
- abort 注入：可编程地在第 N 个 delta 后触发 cancel，验证检查点行为。

参考 pi 的 faux provider（`D:\github\pi\packages\ai\src\providers\faux.ts`，708 行）：它把"可脚本化假流"做成了真 provider，支持流式切块、缓存 usage 模拟、abort——我们只需其中流式/abort 子集。

## 3. 任务分解

1. wire.at 增加 `TurnStart`/`TurnEnd`/`UsageSummary`；retranspile；两轨编译过。
2. rust-ref `agent.rs`：`run_inner` 插入 turn 事件发出点；提取 usage 填 `TurnEnd`。
3. `.at` 轨同步（`src/agent.at`）：turn 事件 + 修复 reasoning→`Thinking` 映射。
4. `Agent` 增加 steering/follow-up 队列与 `steer()`/`follow_up()` 方法；`run_inner` 接轮询点。
5. 取消检查点加密 + 取消时清空 steering 队列。
6. ScriptedClient 增强（delta 切块 / thinking / abort 注入）。
7. 测试：turn 事件序列断言；steering 注入时机（工具批完成后）；follow-up 触发条件；取消后状态一致性；`.at` 轨对拍（turn 事件序列与 rust-ref 全等）。
8. e2e：`scripts/e2e-*.sh` 对拍原生 vs 转译 daemon 的事件序列含新事件。

## 4. 验收标准

- 一次 3 轮工具调用的 run，事件流严格为 `TurnStart×3 … TurnEnd×3` 且 `TurnEnd.usage` 与 daemon 计量一致。
- steering 消息在当前工具结果之后、下一轮 LLM 请求之前进入上下文（用 ScriptedClient 断言请求序）。
- `.at` 轨 reasoning 内容以 `Thinking` 事件流出，CLI REPL 与 musk SSE 均可染色。
- 全部现有测试不回归；两轨对拍全等。

## 5. 风险与边界

- **wire 兼容**：`StreamEvent` 是 client/daemon/agent 共享真源，加变体需要三端同步发版（musk 的 `RunEvent` 桥接随后）。旧 musk 收到未知事件类型应跳过——确认 serde 反序列化用 `#[serde(other)]` 或宽容模式。
- steering 与 `max_turns` 软上限交互：注入 steering 不应计入 turn 上限（决策：计入，简单且防死循环；在计划执行时再评估）。
- 本计划**不改** `Tool` trait 签名（signal 参数留给 content/details 计划一并考虑，见 PLAN-027）。

## 6. 对 auto-musk 的影响（不在本计划内实施）

- `server.rs` 增加 `POST /api/chats/session/{id}/steer|follow|cancel` 三个端点，直连 Agent 队列句柄与 CancellationToken；
- `relay/mod.rs` 的 `StreamEvent→RunEvent` 桥接增加 `TurnStart/TurnEnd/Thinking` 透传；
- 前端 Chats 视图按 turn 折叠。归入 auto-musk 侧后续计划（与 PLAN-040 的 ToolUpdate 一并接线）。
