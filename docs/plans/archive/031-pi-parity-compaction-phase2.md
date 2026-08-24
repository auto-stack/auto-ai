# Plan 031: 压缩体系二期——真实 context_window 贯穿、溢出恢复、机器文件清单与测试债清理

> **状态**：✅ implemented（2026-08-24，feat/031-compaction-phase2 worktree；全部 10 任务落地，两轨测试 + live e2e 绿）
> **仓库**：auto-ai（auto-ai-agent 压缩主改；auto-ai-daemon 元数据透传 + 溢出识别；auto-ai-client 接线）
> **目标**：补齐 PLAN-028 一期核实出的四个缺口：①压缩窗口写死 128k，与 daemon 模型元数据脱节；②只有 run 前主动压缩，无溢出后的被动恢复；③摘要文件清单依赖 LLM 而非机器提取、无增量摘要；④一期遗留测试债。
> **参考实现**：pi-mono 本地克隆 `D:\github\pi`（main @ a1f955e9f）。
> **前置**：PLAN-028（一期已合，commit d910af6）；Phase 2 复用 026 的 TurnEnd/usage。
> **关联计划**：auto-musk PLAN-042（消费 details，无依赖关系）、PLAN-043（树投影与压缩锚点共存，见其风险节）。

---

## 0. 问题（一期核实结论，2026-08-24）

1. **窗口脱节**：`CompactionSettings` 默认 `context_window: 128_000`
   （`crates/auto-ai-agent/rust-ref/src/compaction.rs:32-40`），而 daemon 的
   模型元数据（PLAN-028 已建：`config.rs:111-135` 的 `with_meta`、
   `/v1/models` 的 `server.rs:467-502`）停在 daemon 侧——`TierCandidate`
   无元数据字段（`tier_router.rs:19-22`），`auto-ai-client` 全 crate 无
   `context_window` 类型。tier 路由到小窗口模型（如 32k）时，压缩阈值按
   128k 计算，会在触发前就爆窗。
2. **无溢出恢复**：`should_compact` 只在 run 开始前检查（`agent.rs:432-465`）；
   run 进行中上下文增长爆窗 → provider 报错 → 整个 run 失败。pi 是双轨：
   主动阈值 + 溢出识别→压缩→重试。
3. **摘要质量依赖 LLM**：`## Files` 清单完全靠摘要模型遵守模板（一期未声明
   的弱化——pi 的文件清单是机械提取）；且每次全量摘要，无 `previousSummary`
   增量。
4. **测试债**（一期核实清单）：cache 解析（`anthropic.rs:168/:285`、
   `openai.rs:155/:245`）与 tracker `record_full`、`/v1/usage` 零断言；
   `is_quota_exhausted`/`is_retryable` 与"配额不消费候选链"门控零测试；
   转译轨缺 auto-compact 集成对拍（仅 t21，无 `compaction_on` 用例）；live
   e2e 双脚本未跑（026/028 共欠）；ScriptedClient 的 `with_abort_after`
   是死 API（`mvp_harness.rs:83`）。

## 1. pi 参考实现索引

pi 仓库路径前缀 `D:\github\pi\packages\`：

| 关注点 | pi 位置 | 移植要点 |
|---|---|---|
| 溢出识别 `isContextOverflow(message, contextWindow)`：29 条 provider 文案正则 | `ai/src/utils/overflow.ts:134`（入口）、`:178`（getOverflowPatterns） | 三家各写 1-3 条保守正则（Anthropic "prompt is too long"、OpenAI "maximum context length"、Ollama "context window"，执行时按实际响应校准） |
| 溢出恢复决策点：`sameModel && isContextOverflow(...)` 才压缩重试 | `coding-agent/src/core/agent-session.ts:2079`；重试可行性 `isRecoverableLength`（`overflow.ts:171`） | 我们对应：仅当错误可归为溢出且 compaction 开启时恢复，重试上限 1 次 |
| 文件清单机器提取：扫消息流中的工具调用提取 read/modified 路径 | `agent/src/harness/compaction/utils.ts:24`（extractFileOpsFromMessage）、`:54`（computeFileLists）、`:62`（formatFileOperations） | 从 wire 的 tool_use（name + args.path/file_path）机械收集；read 类与 write/edit 类分列 |
| 增量摘要：`previousSummary` 存在时换 UPDATE 模板 | `agent/src/harness/compaction/compaction.ts:508-545`（UPDATE_SUMMARIZATION_PROMPT 分支） | `compact()` 加 `previous_summary: Option<String>`；锚点更新时传上次摘要 |
| contextWindow 随模型走（目录 → harness 消费） | `ai/src/types.ts` 的 `Model.contextWindow` + harness `shouldCompact` 消费点 | 我们链路：daemon 响应内嵌元数据 → client 缓存 → Agent 刷新 settings（见 §2.1） |
| 会话序列化喂摘要 | `agent/src/harness/compaction/utils.ts:91`（serializeConversation） | 已有类似实现（一期 compact 的消息序列化），增量改造 |

## 2. 方案

### Phase 1：真实 context_window 贯穿

链路选择（**响应内嵌**，准确跟随 tier fallback 的实际换模）：

1. daemon：`/v1/chat/completions`（含 SSE 尾帧）响应附带
   `model_meta: { id, context_window, max_output_tokens }`（serde default，
   旧 client 可忽略）。tier fallback 换候选后元数据随最终成功的模型发；
2. client：解析并缓存"最近一次实际模型元数据"；
3. agent：每轮从响应刷新（与 `last_usage` 刷新同点，`agent.rs:559-562`），
   `CompactionSettings.context_window` 若未显式设置则跟随实际值；
   `set_compaction_settings` 显式设置优先（测试/特殊场景兜底）。

### Phase 2：溢出恢复（被动压缩）

1. daemon/provider 错误归类增加 `ContextOverflow`（`LlmError` 新变体或
   `is_context_overflow(body)` 判定函数；正则保守匹配，未知错误不触发）；
2. `run_inner` 捕获 LLM 错误 → 判定溢出且 compaction 开启 →
   `compact()`（同款流程）→ 以压缩后记忆**重试本轮请求一次**（上限 1 次，
   防循环）→ 仍溢出则以错误收尾；
3. 恢复路径发 `Warning "context overflow recovered, compacted and retried"`
   事件（对齐一期 "context compacted" 的模式）。

### Phase 3：摘要强化

1. `extract_file_ops(messages) -> FileOps`：机械扫描 wire 消息中的
   tool_use，按工具名 + args 路径字段分列 read / modified；
2. 摘要 prompt 的用户内容追加机器清单：`## Files` 段由
   `format_file_operations` 生成后**直接拼入摘要锚点**（LLM 的清单输出仅作
   复述校对，锚点以机器清单为准）；
3. `compact(..., previous_summary)`：非 None 时用 UPDATE 模板（增量改写
   Goal/Progress/Decisions），锚点替换而非叠加。

### Phase 4：测试债清理（一期核实清单逐项）

1. cache 解析断言 ×4 处（anthropic/openai × 流式/非流式）+ tracker
   `record_full` + `/v1/usage` 输出；
2. `is_quota_exhausted`/`is_retryable` 单测 + "配额错误不消费 tier 候选链"
   的 fallback 门控测试（ScriptedClient 或 daemon 单元层模拟候选序）；
3. 转译轨补 auto-compact 集成对拍（对齐 rust-ref 的
   `harness_agent_auto_compacts_before_run`）；
4. live e2e：`scripts/e2e-transpiled.sh`、`e2e-daemon-a2r.sh` 实跑，断言扩
   展至 TurnStart/Thinking 序列（026 任务 8 一并闭环）；
5. `with_abort_after`：补一个消费测试（第 N delta 后取消，断言 Cancelled
   事件）或删除该 API——二选一，倾向补测试（mid-batch 取消的转译轨对拍
   缺口可顺带补上）。

## 3. 任务分解

1. ✅ daemon 响应内嵌 `model_meta`（非流式 + SSE 尾帧）+ client 解析缓存。
2. ✅ Agent 接线：窗口跟随刷新（显式设置优先）+ 单测（换模型后窗口变化）。
3. ✅ 溢出判定函数（三家保守子串匹配，含 rate-limit 排除）+ daemon 错误
   归类 + 单测。
4. ✅ `run_inner` 溢出恢复路径（压缩 → 重试一次 → Warning）+ ScriptedClient
   单测（模拟首轮溢出错误、次轮成功；二次溢出失败不循环）。
5. ✅ `.at` 轨同步 1-4 + 6-7（agent.at/compaction.at/client lib.at/daemon
   server.at+error.at+provider .at）+ retranspile（新增 sed 兜底 6 处）。
6. ✅ `extract_file_ops` + `format_file_operations` + 单测。
7. ✅ 锚点机器清单注入 + `previous_summary` UPDATE 模板 + 单测（二次压缩增量
   正确）。
8. ✅ Phase 4 测试债 1-2（cache/quota/门控：LlmError 分类 ×4、tracker
   record_full、/v1/usage 投影、配额不消费候选链 + 5xx fallback 对照）。
9. ✅ Phase 4 测试债 3-5（转译对拍 t22-t24、live e2e 双脚本实跑 +
   TurnStart/Thinking 断言、with_abort_after 消费测试；顺带修复
   rust/tests/live_run.rs 的既有坏导入）。
10. ✅ 全量回归：workspace 227 测试 + 转译轨 24 测试 + live e2e 双脚本绿。

### 实施备注（偏离与补充）

- 溢出判定用保守**子串匹配**（非正则），避免给 agent/daemon/client 引入
  regex 依赖与 .at 转译风险；marker 清单对齐 pi 三家 + 通用错误码形态。
- `compact()` 返回值改为 `(Memory, String)`（新 Memory + 本次摘要文本），
  供 Agent 的 `last_summary` 增量喂给；调用点同步更新。
- client 的 `last_model_meta` 缓存 API 仅 rust-ref 轨提供；`.at` 轨 Auto 的
  Mutex 无法写 Option 内部值，降级为仅透传 `resp.model_meta`（主链路
  agent 消费 resp.model_meta 不受影响）——已登记 KNOWN-DEBT。
- `auto-ai-react` REPL（rust/src/main.rs 手写 glue）改用 run_stream + 事件
  sink，stderr 输出 `[event] turn N start/end` / `[event] thinking` 标记，
  供 live e2e 断言事件序列（闭环 026 任务 8）。

## 4. 验收标准

- tier 路由到 32k 窗口模型时：上下文约 30k 即触发压缩（而非 128k 才触发），
  ScriptedClient 断言压缩请求的窗口值来自响应元数据。
- 构造"run 中途溢出"场景：自动压缩并重试成功，事件流含恢复 Warning；二次
  溢出以错误收尾不死循环。
- 二次压缩后的锚点含增量更新的 Goal/Progress 与机器提取的 Files 清单
  （清单与消息流中的工具调用严格一致，不依赖 LLM 输出）。
- Phase 4 清单全部落地，live e2e 两个脚本绿。

## 5. 风险与边界

- **响应字段兼容**：`model_meta` serde default；旧 client/旧 daemon 混部署
  时窗口回退默认 128k（与现状同，不劣化）。
- **溢出正则误判**：保守匹配 + 只在错误路径触发；误判的代价是多一次压缩
  （可接受），漏判的代价是 run 失败（与现状同，不劣化）。
- **增量摘要漂移**：多次 UPDATE 后摘要可能失焦——机器 Files 清单兜底关键事
  实；极端长会话的全量重摘要作为设置选项（默认增量）。
- **与 musk 树投影的叠加**（PLAN-043 风险节的镜像项）：musk with_history
  喂历史时应只喂"压缩锚点之后的保留尾部"，两仓实现时对齐一次口径。
