# 006 — 流式 tool_calls 支持（消除 run_stream 双重请求）

> **状态**：实施计划，待执行。
> **日期**：2026-07-09
> **影响**：auto-ai-daemon + auto-ai-client + auto-ai-agent（三层）
> **前置**：无

## 0. 问题陈述

`Agent::run_stream`（auto-ai-agent `agent.rs:330-456`）当前用**双重请求**解决流式 + tool_calls 的矛盾：

1. 先 `complete_stream()` 拿流式文本 delta（用户看到实时输出）
2. 再 `complete()`（非流式）拿同一轮的 `tool_calls` + `usage`

**危害**：
- **双倍 LLM 请求**：每个 turn 发两次 API 调用，token 和延迟翻倍
- **行为不一致**：模型在两次请求中可能返回不同结果——流式那次说"让我看看..."并想调工具，非流式那次可能直接给了文本回答而不调工具，导致 agent 错误地认为"无工具调用→最终答案"而停止
- **用户困惑**：agent 说了一句话就停，不继续推理

**根因**：daemon 的 `complete_stream` provider 实现（`provider/openai.rs:174` + `provider/anthropic.rs:155`）**丢弃了 SSE 流中的 tool_calls 事件**——只解析 `delta.text`/`delta.content`，忽略 `delta.tool_calls`（OpenAI 格式）和 `content_block_start type=tool_use`（Anthropic 格式）。返回的 `CompletionResponse.tool_calls` 始终为空 `Vec::new()`。

## 1. 修复方案（三层端到端）

让 tool_calls 在 SSE 流中一路传递到 agent，消除第二次请求。

```
Provider (SSE upstream)
  → 解析 delta.tool_calls / content_block_start(tool_use)    [Task 1]
Daemon streaming_response
  → 在 done 事件中带上 tool_calls + stop_reason               [Task 2]
auto-ai-client complete_stream
  → 从 SSE done 事件提取 tool_calls，返回 CompletionResponse   [Task 3]
auto-ai-agent run_stream
  → 用 complete_stream 返回的 tool_calls，不再做第二次请求      [Task 4]
```

## 2. 详细改动

### Task 1: Provider — 流式解析 tool_calls

**文件**：
- `crates/auto-ai-daemon/src/provider/openai.rs` `complete_stream`（L174-235）
- `crates/auto-ai-daemon/src/provider/anthropic.rs` `complete_stream`（L155-214）

#### OpenAI 格式（GLM/智谱 也用这个）

OpenAI SSE 的 tool_calls 在 `delta.tool_calls` 数组里分片到达：

```jsonc
// 第一个 chunk — tool_call 开始
{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"read_file","arguments":""}}]}}]}
// 后续 chunks — arguments 增量
{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}
{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"" }}]}}]}
// 最后一个 chunk
{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}
```

修改 `complete_stream`：
1. 维护 `accumulated_tool_calls: Vec<AccumulatedToolCall>`（按 `index` 聚合）
2. 每个 SSE chunk：检查 `delta.tool_calls`，按 index 合并 id/name/arguments（arguments 是增量拼接）
3. 检查 `finish_reason`：`"tool_calls"` → 标记 `wants_tool = true`
4. 把累积的 tool_calls 转为 `Vec<ToolCall>` 放入返回的 `CompletionResponse`

```rust
// 在 complete_stream 的 SSE 循环里新增：
#[derive(Default)]
struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}

let mut tool_call_accum: Vec<AccumulatedToolCall> = Vec::new();
let mut finish_reason: Option<String> = None;

// 在 for data in data_events 循环里：
if let Some(finish) = json["choices"][0]["finish_reason"].as_str() {
    finish_reason = Some(finish.to_string());
}
if let Some(tcs) = json["choices"][0]["delta"]["tool_calls"].as_array() {
    for tc in tcs {
        let index = tc["index"].as_usize().unwrap_or(0);
        while tool_call_accum.len() <= index {
            tool_call_accum.push(AccumulatedToolCall::default());
        }
        let accum = &mut tool_call_accum[index];
        if let Some(id) = tc["id"].as_str() { accum.id = id.to_string(); }
        if let Some(name) = tc["function"]["name"].as_str() { accum.name = name.to_string(); }
        if let Some(args) = tc["function"]["arguments"].as_str() { accum.arguments.push_str(args); }
    }
}

// 返回时：
let tool_calls: Vec<ToolCall> = tool_call_accum.into_iter()
    .filter(|tc| !tc.name.is_empty())
    .map(|tc| ToolCall {
        id: tc.id,
        name: tc.name,
        input: serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null),
    })
    .collect();

Ok(CompletionResponse {
    content,
    tool_calls,  // 不再是 Vec::new()！
    stop_reason: finish_reason,
    usage: None,  // OpenAI 流式 usage 在最后一个 chunk（如果模型支持）
    ..
})
```

#### Anthropic 格式

Anthropic SSE 的 tool_use 在 `content_block_start` 事件里声明，`content_block_delta` 事件里增量传 `input_json_delta`：

```jsonc
{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_abc","name":"read_file","input":{}}}
{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}
{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"test.rs\"}"}}
{"type":"content_block_stop","index":1}
{"type":"message_delta","delta":{"stop_reason":"tool_use"}}
```

修改 `complete_stream`：
1. 维护 `tool_blocks: HashMap<index, (id, name, input_json_accum)>`
2. `content_block_start` type=tool_use → 记录 id + name
3. `content_block_delta` type=input_json_delta → 按 index 拼接 partial_json
4. `message_delta` stop_reason=tool_use → 标记
5. 返回时把累积的 tool_blocks 转为 `Vec<ToolCall>`

### Task 2: Daemon — SSE done 事件携带 tool_calls

**文件**：`crates/auto-ai-daemon/src/server.rs` `streaming_response`（L185-250）

当前 `done` 事件只带 `model` + `usage`。改为也带 `tool_calls` + `stop_reason`：

```rust
// L210-220 修改：
match provider.complete_stream(&req, on_delta).await {
    Ok(resp) => {
        // ... existing usage tracking ...
        let _ = tx.try_send(format!(
            "data: {}\n\n",
            json!({
                "type": "done",
                "model": resp.model,
                "usage": resp.usage,
                "tool_calls": resp.tool_calls.iter().map(|tc| json!({
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.input,
                })).collect::<Vec<_>>(),
                "stop_reason": resp.stop_reason,
            })
        ));
    }
    // ... error handling unchanged ...
}
```

### Task 3: Client — complete_stream 返回 CompletionResponse

**文件**：`crates/auto-ai-client/src/lib.rs` `complete_stream`（L91-141）

当前返回 `Result<String, ClientError>`（只有累积文本）。改为返回 `Result<CompletionResponse, ClientError>`（带 tool_calls + usage + stop_reason）。

```rust
pub async fn complete_stream(
    &self,
    req: &CompletionRequest,
    on_event: impl Fn(serde_json::Value) + Send + 'static,
) -> Result<CompletionResponse, ClientError> {
    // ... existing SSE reading ...
    let mut full = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut usage: Option<Usage> = None;
    let mut model = String::new();

    while let Some(chunk_result) = stream.next().await {
        // ... parse SSE ...
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data_line) {
            if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                full.push_str(text);
            }
            // Parse done event for tool_calls + usage
            if value.get("type").and_then(|t| t.as_str()) == Some("done") {
                if let Some(tcs) = value.get("tool_calls").and_then(|t| t.as_array()) {
                    tool_calls = tcs.iter().map(|tc| ToolCall {
                        id: tc["id"].as_str().unwrap_or("").to_string(),
                        name: tc["name"].as_str().unwrap_or("").to_string(),
                        input: tc["input"].clone(),
                    }).collect();
                }
                stop_reason = value.get("stop_reason").and_then(|t| t.as_str()).map(String::from);
                model = value.get("model").and_then(|t| t.as_str()).unwrap_or("").to_string();
                // parse usage if present
            }
            on_event(value);
        }
    }

    Ok(CompletionResponse {
        content: full,
        tool_calls,
        stop_reason,
        usage,
        model,
        error: None,
    })
}
```

**注意**：`CompletionResponse` 和 `ToolCall` 类型需要从 `ai-config` crate 导入或定义。检查 `auto-ai-client` 当前依赖什么类型——它可能需要新引入 `ai_config::ToolCall`。

同时更新 `Client` trait 的 `complete_stream` 默认实现（`agent.rs:41-67`）和 `auto_ai_client::AiClient` 的 `Client` impl。

### Task 4: Agent — run_stream 消除双重请求

**文件**：`crates/auto-ai-agent/src/agent.rs` `run_stream`（L330-456）

删除 L370-436 的"第二次非流式 complete"分支。改为使用 `complete_stream` 返回的 `CompletionResponse` 里的 `tool_calls`：

```rust
pub async fn run_stream(
    &mut self,
    task: &str,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
) -> Result<AgentResult, AgentError> {
    self.memory.add("user", task);
    let max_turns = self.role.max_turns();
    let mut result = AgentResult::default();
    let mut seen: HashMap<String, usize> = HashMap::new();

    for turn in 0..max_turns {
        result.turns = turn + 1;
        let req = self.build_request();

        // Single streaming request — text deltas + tool_calls both surface.
        let collected = Arc::new(std::sync::Mutex::new(String::new()));
        let on_delta = on_event.clone();
        let stream_resp = self
            .client
            .complete_stream(&req, Arc::new(move |ev| {
                if let Some(t) = ev.get("text").and_then(|t| t.as_str()) {
                    collected.lock().unwrap().push_str(t);
                    on_delta(StreamEvent::Delta { text: t.to_string() });
                }
            }))
            .await?;

        let content = stream_resp.content;
        if let Some(u) = &stream_resp.usage {
            result.total_tokens += u.total_tokens() as u64;
        }

        if !stream_resp.tool_calls.is_empty() {
            // Record assistant turn + execute tools.
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if !content.is_empty() {
                blocks.push(ContentBlock::Text { text: content.clone() });
            }
            for tc in &stream_resp.tool_calls {
                blocks.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.input.clone(),
                });
            }
            self.memory.add_message(Message { role: "assistant".into(), content: blocks });

            for tc in &stream_resp.tool_calls {
                // ... loop detection + execute + StreamEvent::Tool (unchanged) ...
            }
            continue;
        }

        // No tool calls → final answer.
        result.output = content.clone();
        self.memory.add("assistant", &content);
        on_event(StreamEvent::Done { result: result.clone() });
        return Ok(result);
    }

    // max turns exceeded
    on_event(StreamEvent::Error { message: format!("max turns ({max_turns}) exceeded") });
    Err(AgentError::MaxTurnsExceeded(max_turns))
}
```

**关键变化**：`complete_stream` 现在返回 `CompletionResponse`（而非 `String`），`run_stream` 直接从中取 `tool_calls`，不再做第二次 `complete()`。

## 3. 接口变更汇总

| 层 | 变更 |
|---|---|
| Provider `complete_stream` | 返回值不变（`CompletionResponse`），但 `tool_calls` 字段不再恒空 |
| Daemon `streaming_response` | `done` SSE 事件新增 `tool_calls` + `stop_reason` |
| Client `complete_stream` | 返回类型 `String` → `CompletionResponse` |
| `Client` trait `complete_stream` | 返回类型 `String` → `CompletionResponse` |
| Agent `run_stream` | 删除第二次 `complete()`，用 stream 返回的 `tool_calls` |

## 4. 向后兼容

- `Client::complete_stream` 的默认实现（fall back to `complete()`）也改为返回 `CompletionResponse`
- Mock client 测试：更新返回类型
- musk 的 `chat_stream` 和 `relay/driver.rs`：它们的 `on_event` 回调只消费 `StreamEvent`，不直接调 `complete_stream`，**无需改动**
- musk 的 `run` / `run_inner`（非流式路径）：不受影响

## 5. 测试计划

1. **Provider 单测**：构造 OpenAI/Anthropic SSE fixture（含 tool_calls delta），验证 `complete_stream` 正确解析出 `tool_calls`
2. **Daemon 集成测试**：`streaming_response` 的 `done` 事件包含 `tool_calls`
3. **Client 单测**：mock SSE 返回 done 带 tool_calls，验证 `complete_stream` 返回正确的 `CompletionResponse`
4. **Agent 单测**：mock `complete_stream` 返回 tool_calls，验证 `run_stream` 执行工具并继续循环（不再做第二次请求）
5. **端到端**：在 musk 里发一条需要工具调用的消息，验证 agent 正确流式 + 调工具 + 多轮

## 6. 实施顺序

Task 1 → Task 2 → Task 3 → Task 4（严格顺序，上层依赖下层）。每个 Task 一个 commit + 单测。
