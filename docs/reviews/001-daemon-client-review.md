# 代码审查 001：auto-ai-daemon + auto-ai-client

- 日期：2026-07-20
- 审查员：代码审查（AI 辅助，人工复核关键结论）
- 范围：`crates/auto-ai-daemon`（全部，约 3000 行）+ `crates/auto-ai-client`（全部，约 470 行）
- 方法：三个并行子审查（流式核心 / 辅助模块 / client 全量）+ 人工复核 3 个最严重结论
- 对应修复计划：`docs/plans/001-daemon-client-fix-plan.md`

## 审查方法与结论可信度

三个子审查覆盖了 daemon 的流式核心（server/provider/sse/format）、辅助模块（config/pool/tier_router/tracker/services）、client 全量。完成后**人工复核了 3 个最严重的结论**：

| 结论 | 复核结果 |
|---|---|
| 辅助模块 C1：测试编译不过（3 处 fixture 缺 `tier_routing`） | ✅ 属实，`cargo check --tests` 实测 3 个 E0063 |
| 流式核心 C1：客户端断开后上游不停 | ✅ 属实，`server.rs:246-310` 无取消机制 |
| client 严重 2：SseBuffer 不处理 CRLF | ⚠️ 代码属实，但 daemon 实发 `\n\n`（`server.rs:255`），**当前未触发** → 降级为中等 |

---

## 严重程度图例

- 🔴 **严重**：资源泄漏 / 进程崩溃 / 数据错误，应尽快修复
- 🟡 **中等**：健壮性、正确性、架构缺陷，影响可靠性或维护成本
- 🟢 **轻微 / 建议**：改进项，非阻塞

---

## 🔴 严重

### S1. 客户端断开时，上游 provider 任务和 HTTP 连接不会停止 —— 资源/费用泄漏
- **位置**：`crates/auto-ai-daemon/src/server.rs:246-310`（`streaming_response`）
- **现象**：客户端断开 SSE 后，axum drop 了 stream，但内部的 `provider_task`（`tokio::spawn` 持续拉取 OpenAI/Anthropic 流）**没有任何取消机制**。它持续 `try_send` 到已无接收者的 channel（错误被 `let _ =` 静默忽略），一直拉完整个上游流。
- **后果**：长输出场景（大文件回传、未设 `max_tokens`）下一次断连会让上游空跑数分钟，持续消耗 token/费用；同时 semaphore permit 被无效占用，可能耗尽 provider 并发池。
- **根因**：`complete_stream`（`openai.rs`/`anthropic.rs`）是纯阻塞式 `while let Some(chunk) = stream.next().await` 拉取循环，无取消信号；`stream!` 宏循环里也没有 `select!` 监听 `tx.closed()`。
- **修复方向**：给 `complete_stream` 增加 `CancellationToken` 参数；在 stream body drop 时触发取消（自定义 Stream + Drop，或 `select!` 监听 `tx.closed()` 后 `abort()` provider task）。
- **关联**：与 TUI 取消功能预留的"方案 C（SSE 即时中断）"是同一架构缺口的两端——daemon 侧补上 CancellationToken 后，TUI 的即时取消也顺带实现。

### S2. 请求路径上多处 `unwrap()` 持有 `std::sync` 锁，单次 panic 永久毒化所有后续请求
- **位置**：`server.rs:52`（`cfg()`）、`tracker.rs:31/40/45`、`server.rs:627`
- **现象**：`RwLock`/`Mutex` 的 `.read().unwrap()` / `.lock().unwrap()`。一旦任一持锁线程 panic（锁中毒），**后续每个请求**调 `cfg()`/`record()` 都会 panic → axum 转 500，但 `std::sync` 锁中毒不可恢复，所有读配置/记用量的端点永久宕机。
- **修复**：换成 `parking_lot::RwLock`/`Mutex`（不传播 poisoning），或 `.lock().unwrap_or_else(|e| e.into_inner())`。
- **范围**：涉及 `AppState::config`、`AppState::current_model`、`UsageTracker::apps` 三处锁。

### S3. daemon 测试编译不过 —— 回归保护归零
- **位置**：`pool.rs:76`、`pool.rs:115`、`tier_router.rs:160`
- **现象**：`DaemonConfig` 新增 `tier_routing` 字段后（`ai-config/src/loader.rs`），3 处测试 fixture 未同步，`cargo check --tests -p auto-ai-daemon` 报 3 个 `E0063: missing field tier_routing`。生产路径（`config.rs:78`）已正确填，仅测试漏改。
- **已复核属实**。
- **修复**：3 处补 `tier_routing: TierRouting::default()`；CI 加 `cargo test --no-run` 守护。

---

## 🟡 中等

### M1. 多 provider fallback 形同虚设（架构承诺未兑现）
- **位置**：`tier_router.rs` 模块文档、`server.rs:133`（`resolve` 取首个）、`server.rs:217`（失败直接 502）
- **现象**：`tier_router` 设计了候选链（`candidates()` 返回多 provider）并文档承诺 "falling back on 429/timeout"，但 `chat_completions` 只用 `resolve()`（首个候选），失败直接返回 502，**从不迭代候选**。用户配置 `max: [zhipu, deepseek]` 期望容灾，实际主 provider 一抖动就 502。
- **根因**：`LlmError`（`lib.rs:35-41`）只有 `Http(String)/Api(String)` 字符串变体，无法区分"该 fallback 的错误"（429/超时/5xx）与"不该 fallback 的错误"（4xx 参数错）。
- **修复**：① `LlmError` 增加 `RateLimited`/`Timeout`/`Status(u16)` 变体；② `chat_completions` 循环 `candidates()` 按 `try_acquire` + 错误类型重试。

### M2. 流式请求的 token usage 永远丢失 —— 计费/统计不准
- **位置**：`openai.rs:319`、`anthropic.rs:290`
- **现象**：两个 provider 的流式路径都硬编码 `usage: None`。OpenAI 流式需发 `stream_options: {include_usage: true}` 才返回 usage；Anthropic 在 `message_delta.usage` 给出。当前都没提取，**所有流式请求的 token 不计入 `UsageTracker`**。
- **根因**：provider 抽象不足，两个 `complete_stream` 实现各自"忘了"提取。
- **修复**：OpenAI 加 `stream_options`；Anthropic 读 `message_delta`；trait 抽出公共骨架封装 usage 提取。

### M3. `pool::acquire` 永远阻塞，无超时 / fallback 触发点
- **位置**：`pool.rs:34`、`server.rs:162-177`
- **现象**：`Semaphore::acquire_owned().await` 在未 close 时永远返回 Ok，所以 "concurrency pool unavailable" 的 fallback 分支（`server.rs:171`）是**死代码**。provider 打满时请求无限排队，无排队超时，客户端可能等到自己的超时。
- **修复**：`acquire` 改 `try_acquire_owned` + 可选 timeout，让上层能在容量满时返回 503 或触发 M1 的 fallback。

### M4. `services.rs` 子进程管理三连问题：僵尸泄漏 + 非幂等 + 阻塞 runtime 风险
- **位置**：`services.rs:82-117`（ensure 非幂等）、`:179-208`（`drop(child)` 永不 wait → 僵尸）、`:104-111`（`thread::sleep` + `reqwest::blocking`）
- **现象**：
  1. 并发 `ensure` 同一服务 → 两个请求都发现 URL 不可达 → 都 spawn → 端口冲突。
  2. spawn 的子进程 detach 后，Unix 下父进程不 `wait` → 僵尸累积；daemon 长驻 → 持续泄漏。ready 检查失败也不 kill 已 spawn 的子进程。
  3. `ensure` 是同步阻塞函数（`thread::sleep` + `reqwest::blocking`），但无 `spawn_blocking` 强制约束，误用在 async handler 会卡住 tokio worker。
- **修复**：per-id Mutex 防重入；保留 `Vec<Child>` + `ServiceRegistry::Drop` 发终止信号；`ensure` 改 `async fn` 用 `tokio::time::sleep`。

### M5. `extract_fields_heuristic` 启发式拼回 tool_call 参数 —— 对 `cmd` 类有安全风险
- **位置**：`openai.rs:298, 332-377`
- **现象**：SSE 解析失败时用启发式正则提取 6 个固定字段名（path/content/cmd/old_string/new_string/task/flow）。**截断的 `cmd` 字符串可能被 shell 解释成完全不同的命令**（命令注入放大）。且只识别固定字段，其他工具参数丢失。
- **修复**：删除启发式，解析失败显式报 `malformed tool_call arguments` + 原始 args 进日志，让上层决定。

### M6. 客户端 `AiClient::new()` 阻塞调用，在 async 上下文会死锁/panic
- **位置**：`client/daemon.rs:18-60`、`client/lib.rs:33-40`
- **现象**：`is_running()` 用 `reqwest::blocking`，`ensure_daemon()` 含 `thread::sleep`。`AiClient::new()` 是公开 API 且文档当主路径推荐，但在 `#[tokio::main]` 内调用会触发 `Cannot drop a runtime in a concurrency thread` 或死锁。当前 CLI 用 `with_url` 绕过，但 API 陷阱仍在。
- **修复**：`ensure_daemon` 改 `async fn`（异步 client + `tokio::time::sleep`），或至少 `new()` 文档明确警告 + `#[track_caller]`。

### M7. 客户端 SseBuffer 与 daemon `sse.rs` 重复，且功能退化（不处理 CRLF）
- **位置**：`client/lib.rs:226`、`client/lib.rs:128-187`（解析逻辑复制两份）
- **现象**：客户端 SseBuffer 只 `find("\n\n")`，不识别 `\r\n\r\n`。**当前 daemon 实发 `\n\n`（已验证 `server.rs:255`）所以能工作**，但经 CRLF 规范化代理后会卡住——潜在隐患。另外解析逻辑在 `complete_stream` 的 drain 循环和 finish 循环里复制了两份。
- **修复**：把 `SseParser` 下沉到共享 crate（`ai-config` 或新建 `auto-ai-sse`），daemon/client 共用；客户端抽 `handle_data_line` 消除两份重复。

### M8. `pool::available()` 语义反义 —— 返回"已占用"数却命名"可用"
- **位置**：`pool.rs:37-42` vs `:50-55`
- **现象**：`available()` 返回 `limit - available_permits()`（已占用），但命名和文档承诺返回"可用数"，与 `status()` 直接矛盾。埋好的地雷，任何调用方做负载均衡/健康判断都会拿到反向语义。
- **修复**：改名 `in_use()` 或改实现返回 `available_permits()`。

### M9. 缺少请求超时 —— 慢上游会无限挂起并永久占用 permit
- **位置**：`openai.rs:183-191`、`anthropic.rs:164-173`、`server.rs:251-291`
- **现象**：两个 provider 在 `.send().await` 和 `stream.next().await` 上都没有 timeout。配合 S1，一个挂死的上游流永久占住 semaphore permit。`config_test` 里反而用了 `.timeout(15s)`（`server.rs:457`），说明项目知道这个 API，但生产路径漏了。
- **修复**：`reqwest::Client::builder().timeout(...)`；流式用 `tokio::time::timeout` 包裹每个 `stream.next().await` 做空闲超时（如 30s 无数据即断）。

---

## 🟢 轻微 / 建议

### L1. `config.rs:65` 默认 provider 选取不确定
`providers.keys().next()` 依赖 `HashMap` 迭代顺序。同时设 `ZHIPU_API_KEY` 和 `OPENAI_API_KEY` 时，每次启动可能选不同 provider，路由不可复现。改确定性优先级（如 `zhipu > anthropic > openai`）或用 `Vec` 收集后选。

### L2. `server.rs:473` `mask_key` 按 byte 切片，非 ASCII 边界会 panic
`&k[..6]` / `&k[k.len()-4..]` 无 `is_char_boundary` 检查。API key 几乎都 ASCII，低风险，建议用 `k.chars().take(6)`。

### L3. `server.rs:527` api_key 明文写入 `ai-daemon.at` 落盘
无转义、无 `chmod 600`。建议：key 不落盘只存 `key_env`；或写入前 JSON 转义；或写入后设权限。

### L4. `anthropic.rs:282` 与 OpenAI 路径对"解析失败"策略不一致
Anthropic 退化为 `Value::Null`（静默），OpenAI 用 `extract_fields_heuristic`（启发式）。两 provider 行为不对称。统一为显式报错。

### L5. 格式转换不一致：`ToolResult.is_error` 在 OpenAI 路径被丢弃
`format.rs:48` 丢弃 `is_error`，`anthropic.rs:318` 保留。同一条错误 tool result 经不同 provider 语义不同。建议 OpenAI 路径把 `is_error` 编进 content 前缀。

### L6. client `Default`（`lib.rs:205`）写死端口，绕过 `$AAID_URL`
`daemon_url()` 读 `$AAID_URL`，但 `Default` 直接硬编码 `127.0.0.1:17654`，测试场景下不一致。改 `Self::with_url(daemon_url())`。

### L7. 可观测性不足
关键路径（`chat_completions`/`streaming_response`/两个 `complete_stream`）无 `#[tracing::instrument]`，无法贯穿"路由→provider→SSE"链条；无 metrics（请求计数、provider 错误率、permit 占用率）。`config_test` 把上游错误体原样回显（`server.rs:465`），有信息泄漏风险。

### L8. `complete` 与 `complete_stream` 代码重复
`client/lib.rs:56-79` 的非流式实现和 `complete_stream` 的请求构造、状态码检查完全重复。建议 `complete` 复用 `complete_stream` + no-op 回调。

### L9. `on_event` 回调 panic 无隔离
`client/lib.rs:155, 185` 裸调 `on_event(value)`，回调 panic 会 unwind 掉整条流，agent 侧只能得到 `JoinError(Panic)` 而非 `ClientError`。建议包 `catch_unwind` 或回调返回 `Result`。

### L10. SSE 解析失败 / JSON 反序列化失败被静默吞掉
`client/lib.rs:129, 160` 的 `if let Ok(value) = ...` 无 Err 分支处理。daemon 发畸形 JSON 时客户端既不报错也不记录，丢的若是 `done` 事件则 `tool_calls`/`usage`/`model` 全空。

### L11. client `which()` Windows 分支是死代码
`client/daemon.rs:106-110`：candidate 已是 `aaid.exe`，`!ends_with(".exe")` 恒为 false。应处理 PATHEXT（`.bat`/`.cmd`）。或直接依赖 `which` crate。

### L12. `config.rs` env fallback 路径无 provider 时静默返回空配置
三个环境变量都没设时返回空 providers 配置，daemon 正常启动但每个请求都 `NoProvider`，问题被掩盖到运行时。建议启动期断言 `!providers.is_empty()`。

### L13. tier_router 相关小问题
- auto-derive 去重只看 provider 不看 (tier, model)，同 provider 多个同 tier 模型会重复入候选（`tier_router.rs:83-86`）。
- 显式路由写错 tier 名被静默丢弃（`tier_router.rs:40-47` 的 `_ => continue`），建议 warn。
- 显式路由不校验 provider/model 是否真实存在（`tier_router.rs:48-54`），配置错误被悄悄替换成默认 provider。

---

## 按 ROI 排序的修复优先级

| 序号 | 修复项 | 一次解决的问题 | 难度 |
|---|---|---|---|
| 1 | provider 加 `CancellationToken` + 超时 | S1 + M9 + M3 | 中 |
| 2 | 换 `parking_lot` 锁 | S2 | 低 |
| 3 | 修测试 fixture + CI 加 `test --no-run` | S3 | 低 |
| 4 | `LlmError` 结构化 + 实现 fallback 迭代 | M1 + M5 根因 | 中 |
| 5 | `SseParser` 下沉共享 + 客户端复用 | M7 + L10 | 中 |
| 6 | 流式 usage 提取 | M2 | 低 |
| 7 | `services` 子进程管理 | M4 | 中 |
| 8 | `extract_fields_heuristic` 删除 | M5 | 低 |
| 9 | `pool::available` 语义修正 | M8 | 低 |
| 10 | client `new()` async 化 | M6 | 中 |

**特别提示**：第 1 项（CancellationToken）与之前 TUI 取消功能预留的"方案 C"是同一架构缺口的两端——daemon 侧补上后，TUI 的即时取消也顺带实现，是一举两得的高 ROI 项。

---

## 审查覆盖的文件清单

**auto-ai-daemon**：
- `src/server.rs`（714 行）、`src/provider/openai.rs`（600 行）、`src/provider/anthropic.rs`（429 行）
- `src/provider/mod.rs`（118 行）、`src/sse.rs`（147 行）、`src/format.rs`（136 行）、`src/main.rs`（98 行）
- `src/config.rs`（94 行）、`src/pool.rs`（126 行）、`src/tier_router.rs`（213 行）
- `src/tracker.rs`（85 行）、`src/services.rs`（233 行）、`src/lib.rs`（60 行）

**auto-ai-client**：
- `src/lib.rs`（277 行）、`src/daemon.rs`（162 行）、`src/error.rs`（34 行）

**对照参考**：
- `crates/ai-config/src/loader.rs`（`DaemonConfig` 权威定义）
- `crates/ai-config/src/wire.rs`（`ToolCall`/`CompletionResponse`/`Usage` 定义）
- `crates/auto-ai-agent/src/agent.rs`（Client trait 与回调约束）
