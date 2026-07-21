# 修复计划 012：agent 核心架构 + 配置层（review 003）

- 日期：2026-07-20
- 对应审查：`docs/reviews/003-architecture-review.md`
- 范围：review 003 的严重（S1–S5）+ 中等（M1–M10）+ 精选轻微项
- 原则：每个阶段独立可交付、独立可验证、独立可回滚
- 依赖：本计划与计划 011（daemon/client）独立，可并行/穿插执行

## 阶段总览

| 阶段 | 主题 | 覆盖问题 | 风险 |
|---|---|---|---|
| 1 | 快速止血（低难度高价值） | S2, S3, S4, C1, M8 | 低 |
| 2 | Agent 核心健壮性 | S1, M4, M5, M3 | 中 |
| 3 | 编排层接入（激活死代码） | S5, M6, M7 | 中 |
| 4 | 架构清理（选做） | M1, M9, M10 + 轻微项 | 低-中 |

阶段 1 优先做——S2/S4 几乎是「改几行」，立即消除最常见崩溃和测试红。阶段 2 的 S1 是最重要的中期修复（防长对话崩溃），M4（run/run_stream 统一）顺带消除 S3 的根因。阶段 3 激活 Skill 和 Handoff 等已造好但未接入的基础设施。阶段 4 按需推进。

---

## 阶段 1：快速止血（低难度高价值）

目标：用最小改动消除 2 个真实崩溃路径 + 1 个测试红 + 1 个死依赖。

### 任务 1.1 — tool_result 大小限制（S2）
- **位置**：`crates/auto-ai-agent/src/agent.rs:324,504`（tool_result 回填 memory 处）
- **改动**：
  - 在 Agent 加常量 `MAX_TOOL_RESULT_CHARS: usize = 20_000`（约 5k token）。
  - run 和 run_stream 拿到 tool outcome 后，若 `outcome.chars().count() > MAX`，截断为前 MAX 字符 + 追加 `\n…[truncated {N} more chars]`。
  - 提一个 helper `fn truncate_tool_result(s: &str) -> String` 两处复用。
- **验证**：单测 `truncate_tool_result` 边界（正好=MAX、超 MAX、空）；手动模拟 read 大文件后 agent 不再崩溃。

### 任务 1.2 — run 检查 resp.error（S3）
- **位置**：`crates/auto-ai-agent/src/agent.rs:271-333`（run 循环，complete 之后）
- **改动**：在 `let resp = self.client.complete(...).await?;` 之后立即加：
  ```rust
  if let Some(err) = &resp.error {
      return Err(AgentError::Config(err.clone()));
  }
  ```
  与 run_stream:438 对齐。
- **验证**：mock 一个返回 error 的 Client，断言 run 返回 Err。

### 任务 1.3 — 修 resolve_key 测试红（S4）
- **位置**：`crates/ai-config/src/provider.rs:95-98`（测试）vs `:58`（实现）
- **决策**：语义上「无 key 无 key_env」应返回 None（让 daemon fail-fast 而非发假 key 给上游）。**改实现**：去掉 58 行的 `Some("no-key-needed")`，无 key 无 key_env 时返回 `None`。对无鉴权 provider（Ollama）的支持，留待 `auth_required: bool` 字段（review 002 已规划），或 daemon 侧对 None key 的 provider 跳过 Authorization 头。
- **改动**：
  - resolve_key 末尾 `Some("no-key-needed")` → `None`。
  - 同步删掉 44 行、55 行的 `or_else(|| Some("no-key-needed"))`（空 api_key 和 env 未设都返回 None）。
  - provider/mod.rs::build（daemon）对 `resolve_key() -> None` 的处理：不再当 NoApiKey 错误（除非该 provider 需要鉴权）——暂留 TODO 注释指向 auth_required 方案。
- **验证**：`cargo test -p ai-config` 全绿（之前红的测试现在过）。

### 任务 1.4 — 删 daemon 对 client 的死依赖（C1）
- **位置**：`crates/auto-ai-daemon/Cargo.toml:13`
- **改动**：删掉 `auto-ai-client = { path = "../auto-ai-client" }` 行。`cargo build -p auto-ai-daemon` 确认无引用（已核实 src/ 下引用全是注释）。
- **验证**：daemon 编译通过 + 测试通过。

### 任务 1.5 — parse_tier 去重（M8）
- **位置**：5 处重复（ai-config/loader.rs:312 / daemon/server.rs:119,652 / tier_router.rs:40 / agent/role_config.rs:238）
- **改动**：
  - 在 `ai-config/src/tier.rs` 加 `pub fn parse_tier_name(s: &str) -> Option<ModelTier>`，合并所有别名（含 serde 漏掉的 `light`）。
  - 5 处改调这个单一来源。
- **验证**：单测 parse_tier_name 覆盖所有别名；cargo test 全绿。

### 阶段 1 验收
- `cargo test`（全工作区，含 ai-config）全绿 —— **特别确认之前红的 resolve_key 测试通过**
- 手动：read 大文件不再崩溃；错误响应不再被当回答

---

## 阶段 2：Agent 核心健壮性

目标：修复最严重的潜伏 bug（S1 长对话崩溃），并重构消除 run/run_stream 分叉（M4，顺带根治 S3 类问题）。

### 任务 2.1 — Memory 配对感知截断（S1）
- **位置**：`crates/auto-ai-agent/src/memory.rs:83-111`（trim）
- **改动**：trim 改为「以语义单元为粒度」：
  - 识别「一条 assistant（可含 ToolUse）+ 其后所有 user（ToolResult）」为一个原子组。
  - 按组删除最旧的组（而非按位置删单条）。
  - 实现思路：扫描 messages，用 ToolUse 的 id 和 ToolResult 的 tool_use_id 建映射；删一条时连带删其配对。或更简单：把连续的 [assistant, user*] 段视为不可分割，整段删。
- **验证**：单测构造「memory 满 + 含多组 tool_use/result」，trim 后断言无孤儿 ToolResult、无未应答 ToolUse；且条数降到 limit*2 以下。

### 任务 2.2 — run/run_stream 统一（M4）
- **位置**：`crates/auto-ai-agent/src/agent.rs:256-342`（run）+ `365-530`（run_stream）
- **改动**：抽内部方法统一两套循环：
  ```rust
  async fn run_inner(
      &mut self,
      task: &str,
      on_event: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
      cancel: Option<Arc<AtomicBool>>,
  ) -> Result<AgentResult, AgentError>
  ```
  - run = run_inner(task, None, None)
  - run_stream = run_inner(task, Some(cb), Some(flag))
  - 所有 event 发送包在 `if let Some(on_event)`；所有 cancel 检查包在 `if let Some(cancel)`
  - 顺带：S3 的 error 检查、M5 的 near-cap 提示、tool_result 截断（1.1）在 run_inner 里统一实现，run 和 run_stream 共享。
- **风险**：这是较大的重构，需充分回归测试。先确保阶段 1 的 S2/S3 已加，重构时逻辑收敛到 run_inner。
- **验证**：现有所有 agent 单测通过；新增「run 和 run_stream 行为一致性」测试（同输入两者结果相同）。

### 任务 2.3 — near-cap 警告不污染 memory（M5）
- **位置**：`crates/auto-ai-agent/src/agent.rs:396-407`
- **改动**（在 2.2 的 run_inner 内一并做）：
  - 不再用 `memory.add("system", ...)`，改用 transient 字段（每轮 build_request 时临时插入、不写回 memory）。
  - 不再用 `StreamEvent::Delta` 发警告，新增 `StreamEvent::Warning { text }` 变体。
  - TUI（tui.rs handle_stream_event）加 Warning 分支：显示为灰色系统消息。
- **验证**：长 run 触发 near-cap 后，memory 里无残留 system 消息；TUI 显示「⚠️ 剩余 N turn」而非混入回答。

### 任务 2.4 — 循环检测改连续计数（M3）
- **位置**：`agent.rs:300,474`（seen HashMap）+ `agent.rs:26`（常量）
- **改动**：
  - 把「跨 run 累积 HashMap」改为「连续相同才计数」：维护 `last_key: Option<String>` + `consecutive_count: usize`，仅当当前 key == last_key 才 +1，否则清零并更新 last_key。
  - key 用 `serde_json::to_string(&Value)`（canonical）替代 `format!("{}", Value)`。
  - 触发 loop 时（consecutive_count >= 阈值）：先在 memory 注入「你似乎在重复调用 X，请换思路」system 消息 + 给一次机会，第二次才 return Err。
- **验证**：单测「连续 3 次相同调用 → 软警告；第 4 次 → 失败」；单测「非连续相同调用不误判」。

> **状态（2026-07-21 复核）**：⏸️ **暂缓，未实施**。阶段 2 实施时评估认为：当前的累积式计数虽然偏激进（可能误杀长任务），但漏判的代价（runaway 烧 token）远高于误判代价（提前终止可重试），保守是更安全的选择；软警告 + 阈值调优需要谨慎设计避免反而放行真循环。**此任务在阶段 2 完成时被标 completed 但实际未做——这是文档与实现的不一致，现修正为暂缓**。归入阶段 5 之后的小改进队列，择机实施。

### 阶段 2 验收

### 阶段 2 验收
- cargo test（agent）全绿
- 手动长对话测试（多轮工具调用，触发 trim）不崩溃
- run 和 run_stream 行为一致

---

## 阶段 3：编排层接入（激活死代码）

目标：把已造好但未接入的 Skill 系统、Handoff 字段接通，让 pipeline 真正能产出有意义的多 agent 协作。

### 任务 3.1 — Skill 系统接入 CLI pipeline（S5）
- **位置**：`auto-ai-cli/src/spawn_pipeline.rs:72-77`（CliAgentFactory）+ `auto-ai-cli/src/main.rs:214-221`（build_agent）
- **改动**：
  - 在 build_agent 里，若 `~/.config/autoos/skills/` 存在且非空，扫描后 `agent.register_skill_tool(SkillTool::new(registry))`。
  - 无 skills 目录则跳过（不报错）。
  - 顺带修 skill.rs:167-197 的 parse_frontmatter 健壮性（剥 BOM、宽松 key-value 匹配）。
- **验证**：放一个测试 skill 到 ~/.config/autoos/skills/，TUI chat 里 agent 能调用 `/skill <name>`；无目录时正常工作。

### 任务 3.2 — build_handoff 信息补全（M7）
- **位置**：`orchestration/driver.rs:250-279`（build_handoff）+ `pipeline.rs:224`（submit_handoff）
- **改动**：
  - token_usage.cumulative 从 engine.cumulative_tokens 填充（在 submit_handoff 里、push StepRecord 前）。
  - budget_remaining 从 engine.budget_tracker 计算。
  - 提供 HandoffExtractor trait（默认实现 = 当前 lossy 版本），让 app 可注入 role 专属的 handoff 构造逻辑。AgentFactory 加 `fn extractor(&self) -> Option<Box<dyn HandoffExtractor>>`。
- **验证**：单测 build_handoff 的 cumulative 非零；driver 测试（见 3.4）。

### 任务 3.3 — gate_handler 改异步（M6）
- **位置**：`orchestration/driver.rs:64,192-197`；CLI 调用 `main.rs:551-573`
- **改动**：
  - gate_handler 类型改为 `Box<dyn Fn(&str) -> Pin<Box<dyn Future<Output=GateDecision> + Send>> + Send + Sync>`，或用 `#[async_trait]` 定义 async trait。
  - drive() 命中 gate 时 `.await` handler。
  - CLI 的 pipeline demo（main.rs:551）改异步 stdin 读取（spawn_blocking 包 read_line，或用 tokio 异步 stdin）。
- **验证**：pipeline demo 的 human gate 仍可工作；阻塞期间不卡死。

### 任务 3.4 — 补 driver.rs 单测（review 002 遗留）
- **位置**：`orchestration/driver.rs`（当前 0 测试）
- **改动**：加 `MockAgentFactory`（返回 canned 回复）+ 简单两步 flow，断言 StepStarted/StepCompleted/Completed 事件序列。
- **验证**：driver 单测通过。

### 阶段 3 验收
- Skill 在 TUI 可用
- pipeline 的 handoff 含真实 token 统计
- driver 有回归测试保护

---

## 阶段 4：架构清理（选做，按需推进）

低优先，不阻塞前 3 阶段。按 ROI 挑选。

### 任务 4.1 — 删 relay.rs 的 block_on impl（M1）
- grep 确认无实际调用方后，删除 `impl RelayTarget for Agent` + lib.rs re-export。保留 trait 定义供未来。

### 任务 4.2 — DaemonConfig 嵌套 + 校验（M9）
- DaemonConfig 嵌套 ClientConfig（而非复制字段）。
- 加 validate_daemon_config（max_concurrency>0、idle_timeout>0、tier_routing 引用存在性、default_provider 存在性）。
- 启动期强制调用。

### 任务 4.3 — tier: 协议枚举化（M10）
- ai-config 暴露 `ModelSpec { Tier(ModelTier), ModelId(String) }` + to/parse。
- CompletionRequest 保留 model: String 兼容，加 `model_spec()` 方法。
- agent 和 daemon 改用 ModelSpec。

### 任务 4.4 — Agent memory 生命周期重构（M2）
- 把 memory 从 Agent 字段改成 run 参数。
- 较大改动，建议单独立项评估。

### 任务 4.5 — 轻微项批次
- client `pub use ai_config::*` → 精确 re-export
- API key 落盘策略（优先 key_env + .at.bak 脱敏）
- 未知配置字段 warning
- handoff.rs Decision.render status 重复 bug
- pipeline.rs resume() 清零范围、PipelineMode::Interactive 删死变体
- BudgetWarning 事件 emit 或删
- validate.rs 孤儿模块存废
- 并行工具调用（join_all）
- StreamEvent::Thinking 死变体（实现或删）

### 阶段 4 验收
- 视具体任务而定

---

## 风险与回滚

- **阶段 2.2（run_inner 统一）**是最大的重构，触及 agent 核心循环。建议：先做 2.1（S1 trim），单独验证通过；再做 2.2，此时 S2/S3 已在两处都加了，重构只是收敛。2.2 必须有「同输入 run 和 run_stream 一致」的回归测试。
- **阶段 3.1（Skill 接入）** 改动面小但涉及文件系统扫描，注意无目录时的降级。
- **阶段 3.3（gate async）** 改动 driver 的核心签名，可能影响 musk 仓库的同名接口——需检查 musk 是否也实现 gate_handler。

## 阶段 5：实施复核发现的 workaround 回修

阶段 1-4 完成后做了一次自我复核，发现 3 处「功能正确但实现是 workaround / 不优雅」的地方。本阶段回修，让实现配得上 review 003 的彻底性。

### 任务 5.1 — resolve_key 去掉 `"no-key-needed"` placeholder（W1）

**问题**：阶段 1 任务 1.3（S4）原计划是「改实现返回 None」，但实际做的是**反过来改测试迁就实现**——保留了 `"no-key-needed"` 字符串 placeholder。理由是「Ollama 需要无 key 支持」（commit `ed72fb5`），但 placeholder 把「无 key」伪装成「有 key」：它会被原样塞进 `Authorization: Bearer no-key-needed` 发给上游——对会校验 key 格式的上游就是 401。review 002/003 一直规划的 `auth_required: bool` 才是正解。

**改动**：
- `ai-config/src/provider.rs`：`ProviderConfig` 加 `auth_required: bool` 字段（默认 true，serde `#[serde(default = "default_true")]`）。
- `resolve_key` 按 `auth_required` 决定：true 且无 key → `None`（fail-fast）；false → `Some("no-key-needed")`（仅用于 daemon 跳过 Authorization 头的占位）。
- `ProviderConfig` 的所有构造点（loader、daemon config.rs、tests）补 `auth_required` 字段。
- daemon `provider/mod.rs::build`：对 `auth_required=false` 的 provider 不因 `resolve_key()=None` 报 NoApiKey 错误；provider 发请求时 `auth_required=false` 则跳过 Authorization 头。

**验收**：`cargo test`（全工作区）通过；测试覆盖「无 key + auth_required=true → None」「无 key + auth_required=false → 占位」两种。

### 任务 5.2 — run_inner 的 emit 改用 no-op trait 对象（W2）

**问题**：阶段 2 任务 2.2（M4）统一 run/run_stream 时，`emit` 写成了需要显式传 `&on_event` 参数的闭包，19 处调用都带着 `emit(Event, &on_event)` 的冗长形式——因为 complete_stream 的 on_delta 回调里不能借用外层 on_event，只能用参数传入。

**改动**：把 `on_event: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>` 统一成 `Arc<dyn Fn(StreamEvent) + Send + Sync>`——None 时用一个 no-op 的 `Arc::new(|_| {})`。这样 `emit(ev)` 就是简单的 `on_event(ev)`，无需 Option 参数；complete_stream 的 on_delta 回调直接 clone 这个 Arc。`cancelled` 闭包同理可简化（或保留，它没有借用冲突）。

**验收**：现有 agent 测试全过；run_inner 里的 19 处 `emit(Event, &on_event)` 全部简化为 `on_event(Event)`。

### 任务 5.3 — near-cap 警告加 StreamEvent::Warning + 删 Thinking 死变体（U1）

**问题**：阶段 2 任务 2.3（M5）计划加 `StreamEvent::Warning` 变体，实际却降级为「继续用 Delta 发送」；阶段 4 又把 `StreamEvent::Thinking` 标注为「保留不删」。结果是 near-cap 提醒在 UI 里和模型正文混在一起，`Thinking` 变体定义着但从不 emit——连续两次妥协留了双重设计债。

**改动**：
- `agent.rs`：删 `StreamEvent::Thinking` 变体（从不 emit）；加 `StreamEvent::Warning { text }`。
- `run_inner`：near-cap 提醒从 `StreamEvent::Delta` 改为 `StreamEvent::Warning`。
- `main.rs` 旧 chat_loop + `tui.rs::handle_stream_event`：删 Thinking match 分支，加 Warning 分支（显示为灰色系统提示）。
- `driver.rs` 的 stream_cb match 也要同步（它 match StreamEvent）。

**验收**：`cargo check`（全工作区）零错误；agent/cli 测试全过。

### 阶段 5 验收
- `cargo test`（全工作区）全绿
- `cargo check` 零错误
- run_inner 里不再有 `emit(Event, &on_event)` 的重复参数

---

## 不在本计划范围

- review 001/002 的延后项（M7 SseParser 共享 / M4 services 子进程 / M6 client async）——见计划 011
- F1 Tier 钳制（auto-musk 仓库）
- 新功能开发

## 与现有文档的关联

- 对应审查：`docs/reviews/003-architecture-review.md`
- 延续的修复模式：参考计划 011（daemon/client）的分阶段 + 每阶段独立提交模式
