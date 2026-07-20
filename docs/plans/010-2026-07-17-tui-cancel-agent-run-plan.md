# TUI 取消 Agent 运行 — 实现计划

日期：2026-07-17
关联设计：`docs/designs/2026-07-17-tui-cancel-agent-run-design.md`
范围：方案 A（Esc 软中断，检查点取消）

## 实现顺序

按依赖关系自底向上：agent 层信号 → 调用方适配 → TUI 接入。每步独立编译验证。

---

## 步骤 1：agent 层 — StreamEvent + run_stream 信号

**文件：`crates/auto-ai-agent/src/agent.rs`**

### 1.1 新增 `StreamEvent::Cancelled` 变体

在 `StreamEvent` 枚举（agent.rs:72）加：
```rust
/// The run was cancelled by the user (partial results may already have been
/// produced and emitted via earlier events).
Cancelled { result: AgentResult },
```

### 1.2 `run_stream` 新增 `cancel` 参数

签名改为（agent.rs:357-361）：
```rust
pub async fn run_stream(
    &mut self,
    task: &str,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<AgentResult, AgentError> {
```
文件顶部确认 `use std::sync::atomic::{AtomicBool, Ordering};`（按需加 import）。

### 1.3 加 3 个检查点（agent.rs:368 起 ReAct 循环）

**检查点 1 — 循环顶部**（`for turn in 0..hard_limit` 循环体最开头，agent.rs:369 前）：
```rust
for turn in 0..hard_limit {
    // Cancel checkpoint: stop before starting a new ReAct turn.
    if cancel.load(Ordering::SeqCst) {
        on_event(StreamEvent::Cancelled { result: result.clone() });
        return Ok(result);
    }
    result.turns = turn + 1;
    ...
```
> `AgentResult` 需 `Clone`——确认它已 derive Clone（若无需加）。

**检查点 2 — complete_stream 返回后**（agent.rs:394 `let stream_resp = ...await?;` 之后、`if let Some(err)` 之前）：
```rust
let stream_resp = self.client.complete_stream(&req, ...).await?;
// Cancel checkpoint: stop after the LLM responded, before consuming it.
if cancel.load(Ordering::SeqCst) {
    on_event(StreamEvent::Cancelled { result: result.clone() });
    return Ok(result);
}
```

**检查点 3 — 每个工具执行前**（agent.rs:448 `on_event(StreamEvent::ToolStart{...})` 之前）：
```rust
for tc in &stream_resp.tool_calls {
    // Cancel checkpoint: stop before executing the next tool.
    if cancel.load(Ordering::SeqCst) {
        on_event(StreamEvent::Cancelled { result: result.clone() });
        return Ok(result);
    }
    let key = format!("{}::{}", tc.name, tc.input);
    ...
```

### 1.4 编译验证
`cargo check -p auto-ai-agent` —— 会因其他调用点未传 cancel 报错，记下错误，在步骤 2/3 修复。

---

## 步骤 2：调用方适配（dummy cancel token）

run_stream 新增参数，所有调用点必须更新。

### 2.1 pipeline driver
**文件：`crates/auto-ai-agent/src/orchestration/driver.rs`**（agent.rs 调用在 `agent.run_stream(&input, stream_cb).await?`，约 driver.rs:148）

改 `run_stream(&input, stream_cb)` → `run_stream(&input, stream_cb, Arc::new(AtomicBool::new(false)))`（永不取消的 dummy）。按需加 import。

### 2.2 旧文本 chat_loop
**文件：`crates/auto-ai-cli/src/main.rs`**（`agent.run_stream(input, on_event).await`，约 main.rs:413）

同样传 dummy `Arc::new(AtomicBool::new(false))`。

### 2.3 编译验证
`cargo check -p auto-ai-agent -p auto-ai-cli` 应全部通过（此时 tui.rs 还是 dummy，下一步替换为真实 cancel）。

---

## 步骤 3：TUI 接入 — input channel + App + Esc

**文件：`crates/auto-ai-cli/src/tui.rs`**

### 3.1 input channel 类型改为携带 cancel

tui.rs:144 `mpsc::unbounded_channel::<String>()` → `mpsc::unbounded_channel::<(String, Arc<AtomicBool>)>()`。

后台 task（tui.rs:149-159）解构 `(text, cancel)` 并透传给 run_stream：
```rust
while let Some((text, cancel)) = input_rx.recv().await {
    let tx = stream_tx.clone();
    let on_event = Arc::new(move |ev| { let _ = tx.send(ev); });
    let _ = agent.run_stream(&text, on_event, cancel).await;
}
```

### 3.2 主循环发 input 时创建 cancel handle

handle_key 的 Enter 分支（tui.rs:282-291 附近），发 input 时附带新建的 cancel：
```rust
let cancel = Arc::new(AtomicBool::new(false));
app.current_cancel = Some(cancel.clone());
app.is_streaming = true;
...
let _ = input_tx.send((text, cancel));
```

### 3.3 App 新增字段

tui.rs `App` 结构体加：
```rust
/// 当前正在执行的回合的取消句柄（streaming 时存在，结束后清空）。
pub current_cancel: Option<Arc<AtomicBool>>,
```
`App::new` 里初始化为 `None`。

### 3.4 Esc 处理（streaming 分支）

handle_key 的 `if app.is_streaming { match key.code {` 分支（tui.rs:259-267）加：
```rust
KeyCode::Esc => {
    if let Some(c) = &app.current_cancel {
        c.store(Ordering::SeqCst);
    }
    // 不立即置 is_streaming=false，等 Cancelled 事件统一收尾
}
```

### 3.5 Cancelled 事件处理

handle_stream_event（tui.rs handle_stream_event 函数）加分支：
```rust
StreamEvent::Cancelled { result } => {
    app.total_tokens += result.total_tokens;
    app.chat.finish_assistant();
    // 灰色提示行
    app.chat.add_system("⊘ 已取消");
    app.is_streaming = false;
    app.current_cancel = None;
}
```
> `add_system` 渲染为灰色，符合提示语义。或加一个专用 `ChatLine::Cancelled`，但用 system 行更简单。

### 3.6 Done/Error 事件也清空 current_cancel

现有 Done/Error 分支补 `app.current_cancel = None;`（回合结束统一清理）。

### 3.7 help bar 追加 Esc 提示

render_app 的 help bar（streaming 分支，tui.rs:492）改为：
```
"Esc=取消 │ Tab=toggle tool │ PageDown=scroll to bottom │ q=quit"
```

---

## 步骤 4：验证

1. `cargo check`（全工作区）—— 零编译错误
2. 独立 target 目录 `cargo build -p auto-ai-cli` —— 编译+链接通过（规避 exe 文件锁）
3. 手动测试（需 daemon 运行）：
   - 发一个会触发多轮工具的问题（如"读 README 并总结"）
   - 流式期间按 Esc → 观察立即停止、`⊘ 已取消` 提示、已生成内容保留
   - 立即输入下一轮消息 → memory 连贯（agent 记得之前的对话）
   - 边界：纯思考阶段按 Esc；工具执行中途按 Esc；连续按 Esc

---

## 风险与注意

- **`AgentResult::Clone`**：检查点要用 `result.clone()`，确认 `AgentResult` 已 derive Clone（agent.rs:97 附近，含 `ToolCallRecord` 需也 Clone）。若无需补。
- **AtomicBool 可见性**：用 `Ordering::SeqCst` 保证跨线程及时可见（store 和 load 都用 SeqCst）。
- **取消延迟**：方案 A 在"等当前 LLM 响应"时不能即时停——文档已说明，属预期行为。
- **exe 文件锁**：构建前确认无残留 auto-ai-cli 进程。

## 不改动

- daemon / provider / client 层（方案 A 不需要 SSE 中断，那是未来方案 C）
- agent memory 语义（软中断不回滚）
