# TUI 取消 Agent 运行（Esc 软中断）— 设计文档

日期：2026-07-17
状态：已批准（待实现）
范围：`auto-ai-cli` TUI + `auto-ai-agent::Agent::run_stream`

## 背景与动机

当前 TUI 在 agent 执行（思考 / 调工具 / 等待 LLM 响应）期间，用户无法中止当前回合。agent 的 ReAct 循环可能持续多轮（思考→工具→思考→工具→…→回答），若方向跑偏，用户只能等到整个回合结束才能继续输入。

本设计为 TUI 增加 **Esc 软中断**：按 Esc 终止当前回合，保留已生成的部分输出，agent memory 保持一致，用户可立即开始下一轮对话。

## 目标与非目标

**目标**
- 流式期间按 Esc → 终止当前 agent 回合（停止下一轮思考、停止下一个工具调用）
- 已生成的部分文本/工具结果保留在对话框内
- agent memory 保持一致（不留下半截的写入）
- 零新依赖（用 `std::sync::atomic`）

**非目标（明确排除）**
- 不在 LLM 正在吐 token 的过程中即时掐断 SSE 流（这是未来的方案 C，见"未来增强"）
- 不回滚 agent memory（软中断，非硬中断）
- 不清空已显示的内容

## 方案选型

经评估三个方案：
- **方案 A：CancellationToken 检查点**（本设计采用）—— 在 run_stream 的安全检查点响应取消
- 方案 B：spawn run_stream + abort —— 破坏 memory 一致性，排除
- 方案 C：SSE 即时中断 —— 体验更好但工程量大，作为**未来增强**（见下）

**选 A 的理由**：取消必须发生在 run_stream 的安全检查点（两个 await 之间），保证 memory 永远一致。A 是任何取消语义的地基，C 是其上的体验增强，两者是叠加而非二选一。

## §1 信号机制

**信号载体**：`Arc<std::sync::atomic::AtomicBool>`
- 取消是"一次性触发"，不需要多任务广播
- 零新依赖，所有调用方廉价 clone/share
- 未来演进到 C 时可平移为 `tokio_util::sync::CancellationToken`，调用方签名不变

**run_stream 新增参数**（取消是"本次运行"的属性，非 agent 实例属性）：

```rust
pub async fn run_stream(
    &mut self,
    task: &str,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    cancel: Arc<AtomicBool>,   // 新增
) -> Result<AgentResult, AgentError>
```

**检查点位置**（`agent.rs` ReAct 循环）——均为 memory 一致的安全点：
1. ReAct 循环顶部（`for turn in 0..hard_limit` 循环体最开头）
2. `complete_stream` 返回后（LLM 响应到达、解析前）
3. 每个工具执行前（`for tc in &stream_resp.tool_calls` 内、`tools.execute` 之前）

**被取消时的行为**：run_stream 在检查点检测到 `cancel.load(SeqCst) == true`，发新的 `StreamEvent::Cancelled`，然后 `return Ok(result_so_far)`（result 含已产生的 turns/tool_calls/output）。

**StreamEvent 新增变体**：
```rust
pub enum StreamEvent {
    ...
    /// The run was cancelled by the user (partial results may have been produced).
    Cancelled { result: AgentResult },
}
```

## §2 后台 task 与主循环改动

**信号持有方式**：主循环创建并持有 cancel handle（不额外加 channel）。input channel 类型从 `String` 改为 `(String, Arc<AtomicBool>)`：

```
主循环 Enter 时（发起新回合）:
   let cancel = Arc::new(AtomicBool::new(false));
   app.current_cancel = Some(cancel.clone());
   input_tx.send((text, cancel));

主循环 Esc 时:
   if let Some(c) = &app.current_cancel { c.store(true, SeqCst); }
   // 不立即置 is_streaming=false——等 Cancelled 事件统一处理
```

**后台 task**（tui.rs:149-159）：从 input_rx 收到 `(text, cancel)`，把 cancel 透传给 run_stream。

**App 新增字段**：
```rust
pub struct App {
    ...
    /// 当前正在执行的回合的取消句柄（streaming 时存在，结束后清空）。
    pub current_cancel: Option<Arc<AtomicBool>>,
}
```

**Esc 处理**（handle_key，streaming 分支）：
```rust
KeyCode::Esc => {
    if let Some(c) = &app.current_cancel {
        c.store(true, Ordering::SeqCst);
    }
    // 等 Cancelled 事件到来再收尾
}
```

**Cancelled 事件处理**（handle_stream_event）：
- `app.is_streaming = false`
- `app.current_cancel = None`
- 对话框内加灰色提示行 `⊘ 已取消`
- `finish_assistant()`（保留已生成内容）

**UI 反馈**：
- Cancelled 到达后：对话框底部（下边框前）加一行灰色 `⊘ 已取消`
- help bar 在 streaming 时追加 `Esc=取消` 提示

## §3 调用方适配 + 方案 C 扩展点

**调用方适配**（run_stream 新增 cancel 参数，所有调用点更新）：
- `tui.rs` 后台 task —— 主消费者，透传主循环创建的 cancel
- `main.rs` 旧文本 `chat_loop` —— 传 `Arc::new(AtomicBool::new(false))`（永不取消的 dummy）
- `crates/auto-ai-agent/src/orchestration/driver.rs` —— 同样传 dummy token

显式加参数优于隐式默认（调用方明确表达"我不需要取消"）。

## 未来增强：方案 C（SSE 即时中断）

当前方案 A 只能在 ReAct turn 边界生效（等当前 LLM 响应到达后才停）。未来要实现"在 LLM 吐 token 过程中即时中断"，扩展路径：

1. **client 层**（`auto-ai-client/src/lib.rs` 的 `complete_stream`）：把 SSE 字节流读取循环 `while let Some(chunk) = resp.bytes_stream().next().await` 用 `tokio::select!` 包裹——一分支读 chunk，另一分支监听取消信号。取消时 break，停止读 SSE（底层 HTTP 连接随之 drop 释放资源）。

2. **取消信号下沉到 client**：`complete_stream` 新增 `cancel: Arc<AtomicBool>` 参数（与 run_stream 同款）。此时建议把全局的 `Arc<AtomicBool>` 统一替换为 `tokio_util::sync::CancellationToken`——它的 `cancel()` 等效于 `store(true)`，但额外提供 `cancelled()` future 给 `select!`，更适合跨 await 点协作。

3. **run_stream 检查点逻辑无需改动**：已有的 3 个检查点保留，SSE 中断只是打破"检查点之间的等待"。两层协作——SSE 层即时停读，检查点层保证 memory 一致性收尾。

4. **演进提示**：C 是在 A 之上**叠加** SSE 层中断，而非重写 A。A 的 cancel 参数接口为 C 留好了扩展位。若决定上 C，AtomicBool → CancellationToken 是平移操作（`cancel()` 取代 `store(true, SeqCst)`），run_stream 签名不变。

## 组件与文件清单

| 文件 | 改动 |
|---|---|
| `crates/auto-ai-agent/src/agent.rs` | `StreamEvent` 加 `Cancelled` 变体；`run_stream` 加 `cancel` 参数 + 3 个检查点 |
| `crates/auto-ai-agent/src/orchestration/driver.rs` | `run_stream` 调用传 dummy cancel token |
| `crates/auto-ai-cli/src/tui.rs` | input channel 类型改为 `(String, Arc<AtomicBool>)`；App 加 `current_cancel`；Esc 处理；Cancelled 事件处理；help bar 提示 |
| `crates/auto-ai-cli/src/main.rs` | 旧 `chat_loop` 的 `run_stream` 调用传 dummy cancel token |

## 测试

- 手动：启动 TUI，发一个会触发多轮工具的问题，流式期间按 Esc → 观察立即停止、`⊘ 已取消` 提示、已生成内容保留、可立即输入下一轮
- 边界：在纯思考阶段（无工具）按 Esc；在工具执行中途按 Esc；连续按 Esc
- 编译：`cargo check` 全工作区（确认新参数不破坏其他消费者）

## 开放问题

无。
