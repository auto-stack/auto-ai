# 代码审查 003：整体架构 + 各模块实现

- 日期：2026-07-20
- 审查员：代码审查（AI 辅助，人工复核关键结论）
- 范围：`auto-ai-agent`（核心 ReAct 引擎 + orchestration + skill）+ `ai-config`（共享底座）+ 跨 crate 架构
- 方法：三个并行子审查（agent 核心 / orchestration+skill / ai-config+架构）+ 人工复核 4 个最严重结论
- 与前两轮的关系：review 001 覆盖 daemon+client，review 002 覆盖历史计划，本次覆盖**之前没深入的 agent 核心 + 共享配置层 + 架构全局**

## 审查方法与结论可信度

三个子审查覆盖了 agent 核心（agent.rs/memory.rs/tool.rs/relay.rs）、编排层（orchestration/）、skill 系统、ai-config 全量、跨 crate 依赖。完成后**人工复核了 4 个最严重结论**：

| 结论 | 复核结果 |
|---|---|
| S1 Memory 截断破坏 tool_use/tool_result 配对 | ✅ 属实，`memory.rs:83-111` trim 按位置删，不感知配对 |
| C1 daemon 对 client 是死依赖 | ✅ 属实，`src/` 下引用全是注释，无代码引用 |
| L2 resolve_key 测试与实现不一致 | ✅ 属实，**实测 `cargo test` 失败**（测试套件有已知红） |
| S3 run 忽略 resp.error | ✅ 属实，`run` 无 error 检查，`run_stream:438` 有 |

---

## 🔴 严重

### S1. Memory 截断破坏 tool_use/tool_result 配对 → provider 400 错误
**位置**：`crates/auto-ai-agent/src/memory.rs:83-111`（`trim`）

trim 按位置删除最旧的非系统消息，**完全不感知 tool_use ↔ tool_result 的配对关系**。一个 ReAct turn 在 memory 里是「assistant(ToolUse) + user(ToolResult)」两条消息，trim 可能只删一半，产生孤儿 ToolResult 或未应答的 ToolUse——Anthropic/OpenAI 都要求配对完整，否则返回 `400 invalid_request_error`。

**潜伏性**：内置 Role 默认 `memory_limit = Some(20)`（40 条非系统消息后裁剪）。coder 做几轮 read/edit 就触发。一旦触发，Agent 因 ClientError 终止，且 provider 400 错误对用户不可读。

**修复方向**：trim 以语义单元为粒度——把「一条 assistant(含 ToolUse) + 其后所有 user(ToolResult)」视为不可分割的原子组，按组删除最旧的组。

### S2. tool_result 无大小限制，单次工具调用可永久撑爆上下文
**位置**：`agent.rs:324,504`（`memory.add_message(Message::tool_result(...))`）+ `auto-ai-cli/src/tools.rs:18-22`（ReadFile 无截断）

tool 返回的 `String` 原样进 memory，trim 只数条数不看字节。ReadFile 读 10MB 文件 → 10MB 的 ToolResult 永久驻留 → 下一轮 build_request 序列化整个 memory 发给 daemon → 撞上下文上限，400。且 memory 不自愈，这条记录跟到 run 结束。

**这是 Agent 最常见的崩溃路径**。

**修复方向**：Agent 层统一加 `max_tool_result_chars`（如 20k），超长截断 + 追加 `…[truncated {N} more chars]`。Search 工具已有 `.take(50)`，证明模式正确，应提到 Agent 层强制执行。

### S3. `run`（非流式）完全忽略 `CompletionResponse.error`，`run_stream` 却检查了
**位置**：`agent.rs:271-333`（run）vs `agent.rs:438`（run_stream）

`CompletionResponse.error` 承载「HTTP 200 但业务错误」。run_stream 检查并转 `AgentError`；run **从不检查**——拿到带 error 的响应会继续当正常处理：无 tool_calls 就把 error message 当「模型回答」返回（result.output 被污染）；有 tool_calls 就继续循环。

**根因**：run 和 run_stream 是两套重复代码（见 M6），error 检查只在 run_stream 加了，run 没跟上。

**修复方向**：在 run 的 complete 之后立即加 error 检查；或抽 `run_inner` 统一两套代码（见 M6）。

### S4. resolve_key 测试与实现不一致 —— 测试套件有已知红
**位置**：`crates/ai-config/src/provider.rs:95-98`（测试 `resolve_key_none_when_nothing_set`）

实现（provider.rs:58）在无 key 无 key_env 时返回 `Some("no-key-needed")`，测试断言 `None`。**实测 `cargo test -p ai-config` 失败**。这是为本地无鉴权 provider（如 Ollama）留的口子，但实现和测试没对齐。

**修复方向**：明确语义——若要支持无鉴权 provider（返回 placeholder），修测试；若要 fail-fast（返回 None），修实现。结合 review 002 规划的 `auth_required: bool` 字段一起做。

### S5. Skill 系统完整实现但从未被任何 pipeline 接入 —— 事实死代码
**位置**：`auto-ai-cli/src/spawn_pipeline.rs:72-77`（CliAgentFactory 不调 register_skill_tool）

`SkillTool`/`SkillRegistry` 在 auto-ai-agent 实现完整、单测完备，但 CLI 唯一的 agent 工厂从不注册 skill。superpowers 等 flow 实际拿不到 skill 能力。

**修复方向**：CliAgentFactory.build_agent 里，若 `~/.config/autoos/skills/` 存在则扫描注册；或 Role trait 加 `skills_dir()`/`skill_whitelist()` 自治。

---

## 🟡 中等

### M1. relay.rs 用 `block_on` 嵌套 runtime —— 定时炸弹且无人调用
**位置**：`relay.rs:39-44`

`RelayTarget::delegate` 是同步方法，实现里 `new_current_thread().enable_all().build()` + `block_on`。若调用方已在 tokio worker 上 → panic。grep 全 crate 无实际调用方（仅 relay.rs 自己的测试）。

**修复方向**：删除这个 impl（保留 trait 给未来），或改 `async fn`（trait 加 `#[async_trait]`）。

### M2. Agent 是 `&mut self`，无并发支持，跨步骤 memory 无法共享
**位置**：`agent.rs:256,365`（run/run_stream 都是 `&mut self`）

同一 Agent 实例不能并发跑两个请求。workflow.rs 每步 new 一个 Agent 回避，代价是步骤间 memory 完全不共享，只能靠字符串 context 传递。多轮 chat 场景下连续两次 `run` 的 memory 会累积（无清空 API）。

**修复方向**：中期——把 memory 从 Agent 字段改成 run 参数 `run(&mut self, memory: &mut Memory, task)`，让调用方决定 memory 生命周期和共享方式。

### M3. 循环检测累积计数（非连续），误杀长任务
**位置**：`agent.rs:300,474`（`seen` HashMap 跨整个 run 累积）

第 10、50、90 turn 各调一次相同 (tool,args)，第 90 turn 被判 loop 终止——哪怕中间做了大量不相关工作。且 key 用 `format!("{}", Value)` 不规范（字段序不同的同义 JSON 漏判）。

**修复方向**：改「连续计数」（维护 last_key，仅连续相同才 +1）；key 用 canonical JSON 或 hash；触发时先软警告（memory 注入提示）而非硬失败。

### M4. run 和 run_stream 高度重复 —— 一致性 bug 的根源
**位置**：`agent.rs:256-342`（run）vs `365-530`（run_stream）

S3（error 检查不一致）、M5（near-cap 提示只在 run_stream）都是这种分叉的产物。

**修复方向**：抽内部 `async fn run_inner(&mut self, task, on_event: Option<...>, cancel: Option<...>)`，run = run_inner(None,None)，run_stream = run_inner(Some,Some)。

### M5. run_stream 的 near-cap 警告污染长期 memory + 误用 Delta 事件
**位置**：`agent.rs:396-407`

`memory.add("system", &msg)` 插入「⚠️ N turns until hard stop」，但 system 消息永不被 trim；若 Agent 复用做第二次 run，这些警告带进新会话。且用 `StreamEvent::Delta` 发送——TUI 会当「模型回答」显示给用户。

**修复方向**：用 transient system message（不写回 memory）；新增 `StreamEvent::Warning` 变体承载 meta 信息。

### M6. gate_handler 是同步阻塞闭包 —— 常驻 task 设计下整条 pipeline 卡死
**位置**：`orchestration/driver.rs:64,192-197`；CLI 调用点 `main.rs:551-573`

gate_handler 是 `Fn(&str)->GateDecision` 同步闭包，CLI 实现用 `io::stdin().lock().read_line()` 全阻塞。在 tokio 常驻 task 模型下，阻塞期间整个 executor 线程卡死（含渲染/心跳等任务）。

**修复方向**：gate_handler 改 async；或 drive() 在 WaitForHuman 时返回 AdvanceResult，由调用方在 await 点异步处理后再 resume。

### M7. build_handoff 信息严重丢失 —— 下游 role 拿不到上下文
**位置**：`orchestration/driver.rs:250-279`

summary 截断 200 字符；decisions/open_questions/context_for_next 完全没填；token_usage.cumulative/budget_remaining 恒为 0 但 render 仍输出。handoff 的「token-efficient structured handoff」承诺未达成。

**修复方向**：让 AgentFactory 负责产出 HandoffDocument（app 知道 role 语义）；或 driver 加 HandoffExtractor trait。token_usage.cumulative 从 engine.cumulative_tokens 填充。

### M8. `parse_tier` 逻辑在 5 处重复，语义还不一致
**位置**：ai-config/loader.rs:312 / daemon/server.rs:119,652 / tier_router.rs:40 / agent/role_config.rs:238

同一张 tier 名表复制 5 份。且 serde（tier.rs:25-28）只认 `large`/`heavy`，手写 parse 都加了 `light`——同一名在「serde 路径」和「手写路径」行为不同。

**修复方向**：ai-config 暴露单一 `pub fn parse_tier_name(s) -> Option<ModelTier>`，5 处共用。

### M9. DaemonConfig 字段重叠 + daemon 缺少跨字段校验
**位置**：`ai-config/loader.rs:48-66`（DaemonConfig 复制 ClientConfig 字段而非嵌套）；`validate.rs` 只有 client 视角

DaemonConfig 把 providers/default_provider/default_model 原样再列，不嵌套 ClientConfig。无 `validate_daemon_config()`：max_concurrency 可填 0、idle_timeout_min 可填 0、tier_routing 引用不存在的 provider 无校验、default_provider 指向不存在只兜底无 warning。

**修复方向**：DaemonConfig 嵌套 ClientConfig；加 validate_daemon_config 覆盖上述 4 类；启动期强制调用。

### M10. `"tier:xxx"` 协议藏在字符串前缀，无类型保护
**位置**：产生端 `agent.rs:552-556`，消费端 daemon server.rs:112/118/651

`CompletionRequest.model: String` 既能装 `"tier:max"` 又能装 `"glm-5.2"`，拼错前缀（`"Tier:max"`）静默当 concrete model 路由到错误 provider。协议解析只存 daemon 侧，ai-config 无任何类型/常量声明。

**修复方向**：ai-config 暴露 `ModelSpec { Tier(ModelTier), ModelId(String) }` + to/parse 方法，解析逻辑收敛一处。

---

## 🟢 轻微（精选）

| 位置 | 问题 | 建议 |
|---|---|---|
| `daemon/Cargo.toml:13` | daemon 依赖 auto-ai-client 但零代码引用（C1 死依赖） | 删除依赖 |
| `client/lib.rs:21` | `pub use ai_config::*` 全量 re-export，暴露了 daemon 专属类型 | 精确 re-export |
| `daemon/server.rs:580` | API key 明文落盘 + .at.bak 备份也含明文 | 优先 key_env，备份脱敏 |
| `loader.rs:229-247` | 未知配置字段静默丢弃（typo 无 warning） | 白名单外字段 warn |
| `loader.rs:96,152` | default_provider 用 HashMap.keys().next()（部分已修） | IndexMap 或排序 |
| `handoff.rs:103` | Decision.render 把 status 输出两次（`**made** (made): title`） | 改 enum + 修 render |
| `budget.rs:111,123` | Warning{remaining} 可能下溢（warning_at>limit 时） | with_strategy 校验 |
| `skill.rs:167-197` | parse_frontmatter 对 BOM/CRLF/`key : value` 不稳健 | 剥 BOM + 宽松匹配 |
| `pipeline.rs:355-362` | resume() 清零所有 step 的 loop_counters（无限循环风险） | 只清 current_step |
| `pipeline.rs:23-34` | PipelineMode::Interactive 是 dead variant | 删除或实现 |
| `agent.rs:26` | LOOP_DETECT_THRESHOLD 等硬编码，无 Role 级覆盖 | 放 Role trait 默认值 |
| `driver.rs:43` | PipelineEvent::BudgetWarning 定义但从不发出 | driver emit 或删 |
| `validate.rs` | validate_role_model 无实际调用方（孤儿） | 接入或删 |

### run/run_stream 不支持并行工具调用
`agent.rs:299,473` 的 `for tc in &resp.tool_calls` 顺序 await，慢工具串行执行。对网络 IO 工具是吞吐损失。建议 `join_all`。

### StreamEvent::Thinking 定义但从不发出
`agent.rs:80-84` 定义了，但 run_stream 全发 Delta。中间轮的推理文本和最终答案混在 Delta 流。要么实现（tool-use turn 的 text 改 Thinking），要么删变体+更新文档。

---

## 跨 crate 架构评估

**依赖图（核实后）**：
```
ai-config  ← {client, daemon, agent}
client     ← {agent, cli}
daemon     ← cli（⚠ 死依赖，见 C1，实际无代码引用）
agent      ← cli
```
方向整体合理，无循环。唯一异常：daemon→client 是死依赖（C1）。

**分层合理性**：
- `ai-config` 作为共享底座 ✅，但混入了 daemon 专属字段（max_concurrency，见 review-002 + 上方 M9）
- client 瘦化为纯 HTTP 客户端 ✅（review 001 已确认）
- agent 依赖 client 作为可替换传输层 ✅
- cli 不直接依赖 daemon（通过子进程）✅

**演化风险**：
- 无 `[workspace.dependencies]`，共享依赖版本（serde/reqwest 等）各 crate 各写，有漂移风险
- ai-config 版本 0.1.0 无 CHANGELOG，未来若被 workspace 外部依赖需 semver 意识

---

## 按 ROI 排序的修复优先级

| 序号 | 项 | 类型 | 难度 | 价值 |
|---|---|---|---|---|
| 1 | S1 Memory 配对截断 | 严重 bug | 中 | 高（防长对话崩溃） |
| 2 | S2 tool_result 无大小限制 | 严重 bug | 低 | 高（最常见崩溃） |
| 3 | S4 resolve_key 测试红 | 严重（测试红） | 低 | 高（测试套件可信） |
| 4 | C1 删 daemon 死依赖 | 死代码 | 低 | 中（减误导+编译） |
| 5 | S3 run 漏检 error | 一致性 bug | 低 | 中 |
| 6 | M8 parse_tear 去重 | 重复代码 | 低 | 中（防协议漂移） |
| 7 | M4 run/run_stream 统一 | 重构 | 中 | 中（消除分叉根因） |
| 8 | S5 Skill 接入 pipeline | 功能缺失 | 中 | 中（激活死代码） |
| 9 | M2 memory 生命周期 | 架构 | 高 | 中（解锁并发） |
| 10 | M6 gate_handler async | 架构 | 中 | 中（解锁 TUI pipeline） |

**建议先做 1-4（低难度、高价值的严重项），再按需推进中等问题**。1/2/4 几乎是「改几行」的修复，3 是「改一行」。

---

## 审查覆盖的文件清单

**auto-ai-agent**：
- `src/agent.rs`（857 行，S1/S2/S3/M3/M4/M5 + 并行工具/Thinking 死变体）
- `src/memory.rs`（163 行，S1）
- `src/tool.rs`、`src/relay.rs`（M1）、`src/role_def.rs`、`src/error.rs`、`src/lib.rs`
- `src/orchestration/{pipeline,driver,handoff,budget,flow}.rs`（M6/M7 + 多项轻微）
- `src/skill.rs`（473 行，S5 接入 + frontmatter 健壮性）
- `src/validate.rs`、`src/workflow_validator.rs`（孤儿模块）

**ai-config**：
- `src/{lib,loader,provider,tier,validate,wire}.rs`（S4/M8/M9/M10 + 配置健壮性）

**跨 crate**：
- 依赖图、re-export、版本管理（C1/client re-export/workspace deps）
