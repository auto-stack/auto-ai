# Plan 027: Tool 结果 content/details 分离——模型看到的与 UI 展示的解耦

> **状态**：📝 drafting（未开工）
> **仓库**：auto-ai（auto-ai-agent 的 Tool trait 主改；ai-config 不动——details 不进 LLM 上下文）
> **目标**：`Tool::execute` 返回 `String` 改为结构化 `ToolOutput { content, details }`：`content` 回喂模型，`details` 为 UI/日志/审批流的结构化载荷。对齐 pi 的三元分离设计（content / details / label）。
> **参考实现**：pi-mono 本地克隆 `D:\github\pi`（main @ a1f955e9f），`packages/agent/src/types.ts` 与 `packages/coding-agent/src/core/tools/`。
> **前置**：无硬前置；建议在 PLAN-026 之后做（turn 事件与 details 都要动 agent.rs，避免两次冲突合并）。
> **关联计划**：auto-musk PLAN-039（文件工具重写时直接采用新签名）、PLAN-040（run_command 的截断元数据走 details）。

---

## 0. 问题

现状：`Tool::execute(&self, args: &Value) -> Result<String, ToolError>`（`crates/auto-ai-agent` 的 Tool trait，musk 全部工具实现它）。

具体受害场景：

1. **edit 类工具**：替换成功后模型只需要"成功，改了 N 处"，但 diff 是 UI 最想展示的东西。现在 musk 的 `EditFile` 只返回 `"edited 'path' (1 replacement)"`——前端没有 diff 可渲染，用户看不到 agent 改了什么。
2. **run_command 类工具**：截断信息（显示了 100/5000 行）是给 UI 的状态提示，混在给模型的文本里浪费上下文 token。
3. **审批流**：musk 的 spec 写入工具有审批队列，审批 UI 想要结构化的"将写入哪个文件、diff 是什么"，现在只能从字符串里猜。

pi 的解法是数据级分离，不是渲染级分离——core 层根本不知道 UI 存在，只保证输出里有两个通道。

## 1. pi 参考实现索引

pi 仓库路径前缀 `D:\github\pi\packages\`：

| 关注点 | pi 位置 | 移植要点 |
|---|---|---|
| `AgentToolResult` 三元结构：`content`（Text/Image 块，回模型）+ `details`（泛型 `TDetails`，任意结构给 UI/日志）+ `usage?` | `agent/src/types.ts` 的 `AgentToolResult` | 我们简化为 `ToolOutput { content: String, details: Option<Value> }`；图片通道待多模态需求出现再加 |
| 泛型 details 如何避免泛型感染（pi 用 `TDetails` 泛型 + 注册时擦除） | `agent/src/types.ts` 的 `AgentTool<TParams, TDetails>` | Rust 侧直接用 `serde_json::Value` 更简单，泛型不必 |
| 工具结果的 details 在事件流中的位置（`tool_execution_end` 携带 result 整体） | `agent/src/types.ts` 的 AgentEvent 定义 | 我们的 `Tool` 事件变体增加 `details` 字段 |
| details 不进 LLM 上下文：`convertToLlm` 投影只取 content | `agent/src/agent-loop.ts` 的 LLM 调用边界 | 我们的 `exec_or_msg`/记忆组装处只把 content 包进 ToolResult block |
| 各工具的 details 形状范例 | `coding-agent/src/core/tools/edit.ts:83`（`EditToolDetails { diff, patch, first_changed_line }`）、`read.ts:34`（`{ truncation }`）、`bash.ts:53`（`{ truncation, full_output_path }`） | 直接抄这三个形状，musk 工具重写时用 |
| 截断元数据结构 `TruncationResult` | `coding-agent/src/core/tools/truncate.ts:15` | auto-musk PLAN-039 的共享截断模块产出此结构，放 details |
| UI 消费 details 的方式（renderResult 拿 details 渲染，模型只见 content） | `coding-agent/src/core/tools/edit.ts:414`（renderResult 读 `details.diff`） | musk 前端经 SSE 的 ToolResult 事件读 details |

## 2. 方案

### 2.1 新返回类型（auto-ai-agent）

```rust
pub struct ToolOutput {
    /// 回喂 LLM 的文本（进 ToolResult content block）
    pub content: String,
    /// 结构化载荷，给 UI/日志/审批流；不进 LLM 上下文
    pub details: Option<serde_json::Value>,
}

impl From<String> for ToolOutput { /* content = s, details = None */ }
```

`Tool` trait 签名改为 `async fn execute(&self, args: &Value) -> Result<ToolOutput, ToolError>`。

### 2.2 循环侧接线

- `exec_or_msg`：成功路径把 `output.content` 包进 ToolResult wire block（与现状同）；`output.details` 附加到 `StreamEvent::Tool` 变体（`details: Option<Value>`）后丢弃，**不进 Memory**——上下文膨胀零增加。
- 错误路径不变（`ToolError` 仍转字符串回喂）。
- ConversationStore（musk 侧）可选择把 details 持久化到 Turn 记录供前端回放——那是 musk 的事，本计划只保证事件流里有。

### 2.3 迁移策略

trait 改签名是破坏性变更，一次切完：

1. auto-ai-agent 内置工具/SkillTool 先改（多数直接 `Ok(s.into())`）；
2. musk 全部工具改签名（机械改动；趁 PLAN-039/040 重写时顺手给 edit/read/run_command 填真实 details）；
3. `retranspile.sh` 对拍两轨。

**不做** `execute_legacy` 双轨兼容——本项目无外部 Tool 实现者，一次切干净。

### 2.4 .at 可行性

`ToolOutput` 是普通 struct + Option<Value>，无新语法模式；serde derive 在 .at 有 golden 证据（wire.at 全是）。风险低。

## 3. 任务分解

1. `ToolOutput` 定义 + Tool trait 签名变更 + `exec_or_msg`/事件流接线（rust-ref）。
2. auto-ai-agent 内置工具与 SkillTool 迁移。
3. `StreamEvent::Tool` 增加 `details` 字段（wire.at 同步）。
4. `.at` 轨同步（src/agent.at 等）。
5. musk 侧工具签名机械迁移（`impl Tool for ...` 全部 `Ok(x.into())` 保持行为不变——真实 details 填充留给 PLAN-039/040）。
6. 测试：details 不出现在发给 LLM 的请求里（ScriptedClient 断言请求体）；事件流携带 details；两轨对拍。

## 4. 验收标准

- ScriptedClient 捕获的 LLM 请求中，ToolResult block 内容与改动前逐字节一致（details 零泄漏）。
- 工具事件携带 details，musk SSE 可透传。
- 全部测试与两轨对拍通过。

## 5. 风险

- musk 有 ~20 个 Tool 实现，签名变更是大面积机械改动；集中在一次提交，避免半迁移状态。
- `Value` 类型的 details 缺乏 schema 约束——约定各工具的 details 形状写入工具文档注释（edit: `{diff, patch, first_changed_line}`；run: `{truncation, full_output_path}`；read: `{truncation}`），前端按工具名分发解析。不引入强类型泛型，保持 trait 简单（pi 也是运行时弱类型 details）。
