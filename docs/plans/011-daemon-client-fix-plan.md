# 修复计划 011：auto-ai-daemon + auto-ai-client

- 日期：2026-07-20
- 对应审查：`docs/reviews/001-daemon-client-review.md`
- 目标：修复审查发现的 3 个严重问题 + 9 个中等问题，按 ROI 分阶段执行
- 原则：每个阶段独立可交付、独立可验证、独立可回滚

## 阶段划分

按"低风险快速止血 → 高 ROI 架构修复 → 收尾改进"分四阶段。阶段内任务可并行，阶段间有依赖（如阶段 3 依赖阶段 2 的 `LlmError` 结构化）。

---

## 阶段 1：快速止血（低风险、高紧迫）

目标：用最小改动消除 3 个严重问题和 1 个语义陷阱，恢复回归保护。

### 任务 1.1 — 修复测试编译（S3）
- **文件**：`pool.rs:76`、`pool.rs:115`、`tier_router.rs:160`
- **改动**：3 处 `DaemonConfig` 测试 fixture 补 `tier_routing: ai_config::loader::TierRouting::default()`。或改用 `DaemonConfig::default()` 后按需覆盖字段，避免再次漂移。
- **验证**：`cargo check --tests -p auto-ai-daemon` 通过；`cargo test -p auto-ai-daemon` 运行通过。
- **CI**：在 CI 配置加 `cargo test --no-run`（全工作区）作为编译守护。

### 任务 1.2 — 锁换 parking_lot（S2）
- **文件**：`server.rs`（`AppState::config`、`AppState::current_model`）、`tracker.rs`（`UsageTracker::apps`）
- **改动**：
  - `auto-ai-daemon/Cargo.toml` 加 `parking_lot = "0.12"` 依赖。
  - `std::sync::RwLock` → `parking_lot::RwLock`，`std::sync::Mutex` → `parking_lot::Mutex`。
  - 所有 `.read().unwrap()` / `.lock().unwrap()` 改为 `.read()` / `.lock()`（parking_lot 不返回 Result，无 poisoning）。
- **验证**：`cargo check -p auto-ai-daemon`；手动验证配置热重载、usage 统计仍正常。

### 任务 1.3 — `pool::available()` 语义修正（M8）
- **文件**：`pool.rs:37-42`
- **改动**：改名 `available()` → `in_use()`（返回 `limit - available_permits()`），与 `status()` 语义一致。或保留名字改实现返回 `available_permits()`。**推荐改名**（避免调用方误用旧语义）。
- **验证**：`cargo check -p auto-ai-daemon`；grep 确认无调用方仍用旧名。

### 阶段 1 验收
- `cargo check --tests`（全工作区）通过
- `cargo test -p auto-ai-daemon` 通过
- 手动验证 daemon 正常启动、配置热重载、usage 统计正常

---

## 阶段 2：流式资源安全（高 ROI）

目标：修复最严重的资源泄漏（S1）和超时缺失（M9），并打通 TUI 即时取消的 daemon 侧支撑。

### 任务 2.1 — provider 加 CancellationToken + 空闲超时（S1 + M9）
- **文件**：`provider/mod.rs`（trait）、`openai.rs`、`anthropic.rs`、`server.rs:246-310`
- **改动**：
  1. `Provider::complete_stream` 签名新增 `cancel: tokio_util::sync::CancellationToken` 参数。
  2. `auto-ai-daemon/Cargo.toml` 加 `tokio-util = { version = "0.7", features = ["rt"] }`。
  3. 两个 provider 的 SSE 读取循环用 `tokio::select!` 包裹：一分支 `stream.next().await`，另一分支 `cancel.cancelled().await`。取消时立即 break，drop 上游流。
  4. 每个 `stream.next().await` 另用 `tokio::time::timeout(Duration::from_secs(30), ...)` 做空闲超时（30s 无数据断流）。
  5. `reqwest::Client::builder().connect_timeout(...).build()` 加连接超时。
- **daemon 侧传递**：`streaming_response` 创建 token，spawn `provider_task` 时传入；在 `stream!` 循环里 `select!` 监听消费者 drop（`tx.closed()` 或 axum body 的 poll 返回 closed）→ `cancel.cancel()`。
- **client 侧联动**（可选，本任务先留扩展点）：`client/lib.rs` 的 `complete_stream` 同样接受 `CancellationToken`，透传给 daemon（通过 HTTP 头或客户端直接 select 断流）。
- **验证**：单元测试模拟"客户端中途 drop future"，断言 provider task 在合理时间内退出；手动测试：启动长输出请求，TUI 按 Esc 取消，观察 daemon 日志显示 provider 流中断。

### 任务 2.2 — `pool::acquire` 支持超时与 try_acquire（M3）
- **文件**：`pool.rs:32-35`、`server.rs:162-177`
- **改动**：
  - `acquire` 增加 `timeout: Option<Duration>` 参数。
  - 内部用 `tokio::time::timeout(dur, sem.acquire_owned()).await`；超时返回 `None`（或新增 `AcquireError::Timeout`）。
  - `server.rs:171` 的 "concurrency pool unavailable" 分支从此**不再是死代码**：容量满时返回 503，或触发阶段 3 的 fallback。
- **验证**：单测模拟 semaphore 打满，断言 acquire 在超时后返回 None。

### 阶段 2 验收
- 长输出请求中途断开，daemon 日志显示 provider task 退出 + permit 释放
- 上游 30s 无数据自动断流
- `cargo test` 通过；手动 TUI 取消立即停止 daemon 侧流式（呼应 TUI 设计文档的"方案 C"）

---

## 阶段 3：路由层兑现承诺（中 ROI）

目标：实现多 provider fallback（M1），并清理 tool_call 解析的安全隐患（M5）。依赖阶段 1 无，但与阶段 2 的 M3 配合。

### 任务 3.1 — `LlmError` 结构化（M1 根因）
- **文件**：`lib.rs:35-41`
- **改动**：`LlmError` 增加变体：
  - `RateLimited`（429）
  - `Timeout`（请求/连接超时）
  - `Upstream { status: u16, message: String }`（其他上游错误）
  - 保留 `Http(String)/Api(String)` 兼容，或迁移所有构造点。
  - `From<reqwest::Error>` 按 `is_timeout()`/`is_connect()` 分类映射。
- **验证**：`cargo check`；现有错误处理路径行为不变（通过 Display 兼容）。

### 任务 3.2 — 实现 fallback 迭代（M1）
- **文件**：`server.rs:130-220`（`chat_completions`）
- **改动**：
  - 改用 `state.tier_router.candidates(tier)` 获取候选链（Vec）。
  - 循环遍历候选：对每个候选 `try_acquire`（阶段 2 的 M3），失败则下一个；provider 调用失败时按 `LlmError` 类型判断——`RateLimited`/`Timeout`/`Upstream(5xx)` → fallback 到下一个候选；`Upstream(4xx)` → 直接返回（参数错，不该重试）。
  - 所有候选都失败 → 返回 502 + 汇总错误。
- **验证**：配置双 provider，主 provider 返回 429，断言请求自动 fallback 到备选；4xx 错误不 fallback。

### 任务 3.3 — 删除 `extract_fields_heuristic`（M5）
- **文件**：`openai.rs:298, 332-377`
- **改动**：删除启发式函数。tool_call arguments 的 `serde_json::from_str` 失败时，返回 `LlmError::Api("malformed tool_call arguments")`，原始 args 字符串记 `tracing::warn!`。
- **同步**：`anthropic.rs:282` 的 `unwrap_or(Value::Null)` 也改为同样的显式报错（L4）。
- **验证**：构造一个畸形的 tool_call delta，断言返回错误而非静默 Null/启发式结果。

### 阶段 3 验收
- 双 provider fallback 在主 provider 故障时生效
- tool_call 解析失败显式报错，不再有 `cmd` 截断的命令注入风险
- `cargo test` 通过

---

## 阶段 4：健壮性收尾（中低 ROI）

目标：剩余中等问题和精选轻微问题。可按优先级挑选执行，不阻塞前三阶段。

### 任务 4.1 — `SseParser` 下沉共享（M7 + L10）
- **文件**：新建 `crates/auto-ai-sse/`（或放 `ai-config`），移动 `daemon/src/sse.rs`
- **改动**：
  - daemon 和 client 共用同一个解析器（消除客户端 SseBuffer 退化版）。
  - 补 CRLF（`\r\n\r\n`）边界识别（消除当前未触发但潜在的隐患）。
  - 客户端 `complete_stream` 抽 `handle_data_line` 消除两份重复。
  - 解析失败加 `tracing::warn!`（L10）。
- **验证**：新增 CRLF 分帧的单测；daemon/client 共用后行为对齐。

### 任务 4.2 — 流式 usage 提取（M2）
- **文件**：`openai.rs`、`anthropic.rs`
- **改动**：
  - OpenAI：请求 body 加 `"stream_options": {"include_usage": true}`；解析 `delta.usage`（最后一帧）。
  - Anthropic：解析 `message_delta` 事件的 `usage` 字段。
  - 两个 provider 的流式路径返回真实 usage 而非 `None`。
- **验证**：流式请求后 `UsageTracker` 有 token 记录；`/v1/usage` 反映流式用量。

### 任务 4.3 — `services` 子进程管理（M4）
- **文件**：`services.rs`
- **改动**：
  - `ServiceRegistry` 加 `Mutex<HashMap<id, ()>>` 的"启动中"标记，`ensure` 前 check + insert，防并发重入。
  - 保留 `Vec<Child>`（或 PID），`ServiceRegistry` 实现 `Drop`：daemon 关停时发 SIGTERM/kill。
  - ready 检查失败时 kill 已 spawn 的子进程。
  - `ensure` 改 `async fn`（`tokio::time::sleep` + 异步 probe），保留 `ensure_blocking` 给 `spawn_blocking`。
- **验证**：并发 ensure 同一服务只 spawn 一次；daemon 退出时子进程被清理。

### 任务 4.4 — client `new()` async 化（M6）
- **文件**：`client/daemon.rs`、`client/lib.rs`
- **改动**：
  - `ensure_daemon` 改 `async fn`（异步 reqwest + `tokio::time::sleep`）。
  - `AiClient::new()` 改 `async`，或在文档明确警告"仅非 async 上下文调用" + `#[track_caller]`。
  - 去掉 `reqwest` 的 `blocking` feature（若 services 也 async 化）。
- **验证**：`AiClient::new().await` 在 `#[tokio::main]` 内可用，无 runtime panic。

### 任务 4.5 — 配置健壮性（L1 + L12）
- **文件**：`config.rs`
- **改动**：
  - `load_from_env` 的默认 provider 选取改确定性优先级（`zhipu > anthropic > openai`，或按字母序），用 `Vec` 收集后选。
  - env 路径无 provider 时，启动期 panic 或返回 `Result`（`assert!(!providers.is_empty())`）。
  - `ANTHROPIC_API_KEY` 空串 fallback 用 `.filter(|v| !v.is_empty())` 串起来。
- **验证**：多 key 环境下默认 provider 确定性；无 key 时启动失败而非运行时 503。

### 任务 4.6 — 轻微问题批次（L2/L3/L5/L6/L11/L13）
- **L2** `mask_key` 用 `k.chars().take(6)` 避免非 ASCII 边界 panic。
- **L3** `ai-daemon.at` 写入后 `chmod 600`（Unix）；或 api_key 不落盘只存 `key_env`。
- **L5** OpenAI 路径的 `ToolResult.is_error` 编进 content 前缀（`"[ERROR] {content}"`）。
- **L6** client `Default` 改 `Self::with_url(daemon_url())`。
- **L11** client `which()` 删除 Windows 死分支，或依赖 `which` crate。
- **L13** tier_router：去重键改 `(provider, model)`；未知 tier 名 warn；显式路由校验 provider/model 存在。

### 任务 4.7 — 可观测性（L7，可选）
- **文件**：`server.rs`、provider 实现
- **改动**：
  - `chat_completions`、`streaming_response`、两个 `complete_stream` 加 `#[tracing::instrument(skip(...), fields(model, stream, app))]`。
  - 关键路径加 `tracing::info!`/`warn!`：路由选择、provider 失败、token 用量。
  - `config_test` 的上游错误体做白名单字段提取，不原样回显（信息泄漏）。
- **验证**：一次请求在日志里能贯穿"路由→provider→SSE→完成"链条。

### 阶段 4 验收
- 流式请求 token 计入 UsageTracker
- daemon 退出无僵尸子进程
- client 在 async 上下文安全可用
- 可选：日志可贯穿单次请求全链路

---

## 风险与回滚

- **阶段 2**（CancellationToken）改动面较大，触及 provider trait 签名。建议在一个 feature 分支开发，充分测试（尤其 SSE 中途断开的各种时序）后再合并。
- **阶段 3** 的 fallback 改变了请求的路由行为，需确保现有单 provider 配置行为不变（候选链只有一个时退化为原逻辑）。
- **阶段 4 任务 4.1**（SseParser 下沉）涉及新建 crate + 移动代码，可能影响 `ai-config` 的依赖图，需评估是否值得（也可暂保留两份，仅补 CRLF 修复）。

---

## 阶段 4 延后项评估（2026-07-20 复核）

阶段 1–4 已全部实施并提交（`ab61027`…`d3a76eb`）。下列三项原本属于阶段 4，经复核**当前均无实际触发的 bug**，投入产出比不如新功能开发，**决定暂缓**。每项记录了触发条件，以便将来择机重启。

### M7 — SseParser 下沉共享（client `SseBuffer` 与 daemon `sse.rs` 重复）
- **当前状态**：两份代码并存。daemon 实发 `\n\n`（已验证 `server.rs:255`），client 只认 `\n\n`——**功能正常**。CRLF 风险是假设性的（本项目 daemon↔client 是 localhost 直连，无中间代理做行尾规范化）。
- **修复成本**：高（新建共享 crate + 改依赖图）。
- **何时该修**：当 client 经过反向代理/CDN（可能改写行尾），或 SSE 协议需要扩展（如 `event:` 类型路由、多行 `data:` 拼接）时。

### M4 — services 子进程管理（僵尸/非幂等）
- **当前状态**：`server.rs:749` 的 `services_ensure` **已用 `spawn_blocking` 正确包裹** `ensure`，"阻塞 runtime"风险已规避。`spawn_service`（`services.rs:203`）`drop(child)` detach，Unix 下确实会产生僵尸，但 services 仅在**用户手动 ensure** 时 spawn（非高频路径），累积很慢。非幂等需两个 ensure 请求同时到达才触发，概率低。
- **修复成本**：中（`Drop` 实现 + per-id 幂等锁 + `Vec<Child>` 生命周期）。
- **何时该修**：services 自动启动（如 daemon 启动时批量 ensure）、或并发 ensure 成为常态时。

### M6 — client `ensure_daemon` async 化
- **当前状态**：`is_running`/`ensure_daemon` 用 `reqwest::blocking` + `thread::sleep`，在 tokio runtime 内调用会 panic（嵌套 runtime）。**但唯一调用方（CLI `main.rs`）已用 `with_url` 绕过**——陷阱已被意识并回避。`AiClient::new()` 是公开 API 但实际无人用。
- **修复成本**：中。**没有干净的小修方案**——要么 `ensure_daemon`/`new()` 全 async 化（破坏 `new()` API 兼容，牵连调用方），要么内部建临时 runtime（正是要避免的嵌套）。
- **何时该修**：当有调用方需要 `AiClient::new()` 的 auto-discover 能力、或 async 上下文成为主用法时。届时建议直接把 `new()` 改 `async`，一次性做对。

## 不在本计划范围

- 前端 / TUI 代码（除非联动验证，如阶段 2 的 TUI 取消）
- agent 层（`auto-ai-agent`）的内部逻辑
- 新功能开发（本计划只修复审查发现的问题）

## 与现有文档的关联

- 阶段 2 的 CancellationToken 与 `docs/designs/2026-07-17-tui-cancel-agent-run-design.md` 的"未来增强：方案 C（SSE 即时中断）"对应——daemon 侧补上后，TUI 即时取消顺带实现。
