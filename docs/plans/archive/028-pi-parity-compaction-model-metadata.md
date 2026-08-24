# Plan 028: 上下文压缩与模型元数据——长会话生存与成本计量

> **状态**：✅ 已实施（2026-08-23，分支 feat/028-compaction-metadata，三阶段双轨落地，workspace 203 绿）
> **实施记录与偏差**：
> 1. `cost_per_mtok` 用整数微美元（u64 micro-USD/Mtok，如 3000000=$3.00/Mtok）——f64 会破坏 a2r 对
>    ProviderConfig 自动生成的 Eq/Ord derive；/v1/models 手写轨输出时换算回小数。
> 2. client→Agent 的元数据透传未做（compaction 的 window 来自 CompactionSettings，默认 128k 可配）——
>    全链路透传留待 musk 消费端需要时再接。
> 3. compact() 的 client 参数在 rust-ref 是 `&Arc<dyn Client>`、.at 轨是 `&Box<dyn Client>`（两轨
>    Client spec 包装不同）；摘要请求走 Client::complete（独立 system_prompt，session 隔离）。
> 4. find_cut_point 在"尾部起点非边界且前方无边界"时回退到前一个边界（尾部略超 keep_recent 也比
>    拒绝压缩、放任环形截断丢历史安全）——pi 行为的对齐取舍。
> 5. e2e 对拍（任务 9）未跑 live（需 daemon 实跑）；离线两轨对拍（t21 vs harness_compact）已全等。
> **仓库**：auto-ai（auto-ai-daemon 模型元数据主改；auto-ai-agent Memory 压缩主改）
> **目标**：①给 tier 路由补模型元数据（context_window/cost/能力）；②Memory 从"环形截断"升级为"结构化摘要压缩"（compaction）；③usage 补 cache 维度；④重试前先分类，配额类错误不走 fallback 链。
> **参考实现**：pi-mono 本地克隆 `D:\github\pi`（main @ a1f955e9f），`packages/agent/src/harness/compaction/` 与 `packages/ai/`。
> **前置**：Phase 1（元数据）无前置；Phase 2（压缩）依赖 Phase 1 + PLAN-026 的 `TurnEnd.usage`。
> **关联计划**：PLAN-026（turn 事件/usage）、auto-musk PLAN-039（工具输出截断——互补但不依赖）。

---

## 0. 问题

1. **Memory 只有截断**（`crates/auto-ai-agent/rust-ref/src/memory.rs`）：turn 上限的环形缓冲，pairing-aware（ToolUse/ToolResult 原子删除，避免孤儿 ToolResult 导致 provider 400——这点做得对）。但被删的上下文彻底丢失：长会话里 agent 忘掉自己在干什么、改过哪些文件。musk 的 HandoffDocument 只解决 agent 间交接，不解决单会话膨胀。
2. **tier 不携带元数据**：`tier_router.rs` 把 `tier:mid` 解析到具体模型，但模型表没有 `context_window`——压缩的触发条件（token 超限）无从判断；预算控制、"非 vision 模型别发图"同样缺依据。
3. **usage 丢 cache 维度**：Anthropic/OpenAI 响应里的 `cache_read_input_tokens`/`cached_tokens` 没进计量，成本核算失真。
4. **fallback 不分类**：429/超时时 tier 候选链依次 fallback（`tier_router.rs`），但配额耗尽/账单问题的错误换一家也一样撞墙，白耗重试窗口。

## 1. pi 参考实现索引

pi 仓库路径前缀 `D:\github\pi\packages\`：

| 关注点 | pi 位置 | 移植要点 |
|---|---|---|
| 压缩决策 `shouldCompact(tokens, window, settings)`：`tokens > window - reserve` | `agent/src/harness/compaction/compaction.ts` 的 `shouldCompact` 与 `DEFAULT_COMPACTION_SETTINGS`（reserve 16384 / keepRecent 20000） | 直接抄数值起步 |
| 切点选择：只在 turn 边界（user/toolResult 等 turn 起点）落刀，保留近 `keepRecentTokens` | 同文件 `findCutPoint` | 我们的 Memory 已有 turn 原子性概念，切点对齐 turn 边界 |
| token 估算：优先最近一条 assistant usage（真实数据），其后 chars/4 启发式 | 同文件 `estimateContextTokens` | `TurnEnd.usage`（PLAN-026）提供真实数据 |
| 结构化摘要模板：`## Goal / ## Progress / ## Key Decisions / ## Next Steps`，支持 previousSummary 增量 | 同文件摘要 prompt 构造 | 摘要末尾机器提取 read/modified 文件清单（`compaction/utils.ts` 的文件操作提取） |
| 摘要是独立 LLM 请求 + 新 session（不污染主会话缓存） | 同文件 `compact()` 的请求构造 | 经 Client spec 发出即可 |
| 压缩作为自包含检查点：上下文投影从它开始，绝不越过 | `agent/src/harness/session/context.ts` 的 `buildSessionContext` | Memory 侧等价物：summary 消息作为记忆头部 |
| 手动压缩入口与自动阈值双轨 | `coding-agent/src/core/compaction/`（编排层） | musk 后续加 `/compact` 命令时参考；本计划只做 auto-ai 侧原语 |
| Model 元数据形状：contextWindow/maxTokens/cost(input,output,cacheRead,cacheWrite)/input 能力 | `ai/src/types.ts` 的 `Model` 接口 | 轻量手写版，**不抄** 3000 行生成器（`ai/scripts/generate-models.ts`） |
| Usage 统一含 cacheRead/cacheWrite/cacheWrite1h + 集中计价 `calculateCost` | `ai/src/types.ts` 的 `Usage`、`ai/src/models.ts` 的 `calculateCost`（1h 写入按 2x input、分层阈值） | daemon `tracker.rs` 扩展同形字段 |
| 重试分类：配额/账单耗尽 = 不可重试；429/5xx/网络 = 可重试 | `ai/src/utils/retry.ts` 的 `isRetryableAssistantError`（两条大正则） | Rust 侧按 error body 关键词分类，几十行 |
| 溢出错误识别（30+ provider 文案正则） | `ai/src/utils/overflow.ts` | 三家 provider（Anthropic/OpenAI/Ollama）手写对应规则即可 |

## 2. 方案

### Phase 1：模型元数据（daemon）

- `auto-ai-daemon` 模型表每条增加：`context_window`、`max_output_tokens`、`cost_per_mtok { input, output, cache_read }`、`capabilities { vision, thinking }`。
- 手写维护（当前 3 provider × ~10 模型，规模可控）；`tier_router.rs` 的 tier→模型推导填充这些字段到响应元信息。
- `/v1/models` 响应携带元数据；client spec 透传给 Agent。

### Phase 2：Memory 压缩（auto-ai-agent）

新模块 `memory/compaction.rs`（两轨：`src/` + `rust-ref/`）：

```rust
pub struct CompactionSettings { pub reserve_tokens: usize, pub keep_recent_tokens: usize }
pub fn should_compact(tokens: usize, window: usize, s: &CompactionSettings) -> bool;
pub async fn compact(memory: &Memory, client: &dyn AiClient, s: &CompactionSettings)
    -> Result<Memory, CompactionError>;
```

流程（照搬 pi）：

1. `estimate_tokens`：最近 TurnEnd 的 usage 之和，其后追加消息 chars/4；
2. `should_compact` 触发（在 run 开始前检查，不 mid-run 打断）；
3. `find_cut_point`：从记忆尾部保 `keep_recent_tokens`，向前找最近 turn 边界（pairing-aware，与现有 trim 同规则）；
4. 被切掉的历史发独立 LLM 请求生成结构化摘要（Goal/Progress/Key Decisions/Next Steps + 文件清单）；
5. 新 Memory = `[summary 消息, 保留尾部]`；摘要请求不占主会话 token 预算。

### Phase 3：usage cache 维度 + 重试分类（daemon）

- `tracker.rs` 与 wire usage 结构加 `cache_read_tokens/cache_write_tokens`；Anthropic（`cache_read_input_tokens`/`cache_creation_input_tokens`）与 OpenAI（`prompt_tokens_details.cached_tokens`）响应解析各加一处。
- `LlmError` 分类函数：`quota_exhausted`（402/insufficient_quota/billing）→ 直接失败上抛，**不走 tier fallback**；`transient`（429/5xx/timeout）→ 走现有候选链。

## 3. 任务分解

1. daemon 模型表元数据 + `/v1/models` 透出 + client spec 类型。
2. `CompactionSettings/should_compact/estimate_tokens` 纯函数 + 单测。
3. `find_cut_point`（复用 pairing 规则）+ 单测（含 split-turn 拒绝：切点必须落在 turn 边界）。
4. 摘要请求构造 + 结构化模板 + Memory 重建 + 单测（ScriptedClient 模拟摘要响应）。
5. Agent 集成：run 前 should_compact 检查（可配置开关，默认开）。
6. `.at` 轨同步 + retranspile。
7. usage cache 字段：wire + provider 解析 + tracker 记账。
8. 错误分类函数 + tier_router fallback 门控 + 单测。
9. e2e 对拍：两轨压缩后上下文一致性。

## 4. 验收标准

- 构造 100-turn 会话（ScriptedClient），压缩后：模型收到的请求 = 摘要 + 近 20k token 尾部；摘要请求只发了一次、且与主请求 session 隔离。
- 压缩前后 agent 能回答"你刚才改了哪些文件"（文件清单在摘要里）。
- usage 报表区分 cache 命中；配额类错误不再触发 fallback（测试断言候选链只被 transient 错误消费）。

## 5. 风险与边界

- **摘要质量**是 LLM 依赖项，模板再好也可能丢关键信息；缓解：文件清单机器提取（不依赖摘要 LLM）+ `keep_recent_tokens` 给足。
- **两轨成本**：compaction 是纯逻辑（字符串/Vec 操作 + 一次 client 调用），无 `tokio::select!` 类阻塞模式，.at 可行性风险低。
- 压缩有损且不可逆（Memory 层无持久化历史）；musk 的 ConversationStore 有全量 jsonl，回放不受影响。auto-ai 侧不做压缩撤销（pi 的做法是历史留在会话文件里，Memory 层同样不回滚）。
- 与 PLAN-026 的依赖：`TurnEnd.usage` 未落地前，`estimate_tokens` 退化为全量 chars/4（可用但保守），不阻塞 Phase 2 开工。
