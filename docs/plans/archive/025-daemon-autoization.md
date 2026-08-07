# Plan 025: auto-ai-daemon Auto 化（直接 use.rust axum/tokio 方案）

> **状态**：🟡 Phase 0 完成（axum 可行性已验证），Phase 1-5 待推进
> **仓库**：auto-ai（daemon）+ 可能 auto-lang（select! 语法若决定支持）
> **目标**：把 auto-ai-daemon（3165 行纯 Rust HTTP 网关）Auto 化——`.at` 源码 + 转译树，
> 直接用 `use.rust axum/tokio/reqwest` 调用 Rust 框架（不用 a2r-std 包装）。
> **技术路线**（用户决定）：直接 use.rust axum/tokio，绕过 a2r-std 的 HTTP server 缺失。
> **前置调研**：daemon 逐文件可行性 + axum/tokio 在 .at 的表达能力（golden 证据充分）。

---

## 0. 关键调研结论

直接 `use.rust axum/tokio` 方案**远优于** a2r-std 方案。axum 的核心模式在 .at 都有 golden 证据：
- **Router 方法链** ✅（`Router::new().route(...).with_state(...)`）
- **解构 extractor 参数** ✅（golden `16_interop/016_extractor_destructure`：`fn multi(Path(id) Path<String>, Json(data) Json<String>)`）
- **`impl IntoResponse` 返回** ✅（golden `015_impl_trait_return`：`fn health() ~IntoResponse` → `async fn health() -> impl IntoResponse`）
- **`#[tokio::main]`** ✅（golden `018_dotted_attrs`）
- **async stream** ✅（golden `21_generators/002_stream_yield`：`yield` 自动包 `async_stream::stream!`）
- **`Arc<dyn Fn + Send + Sync>` 回调参数** ✅（Plan 397 golden 009）

**唯一硬阻塞**：`tokio::select!`（多臂 `pat = fut => body` 宏）——a2r 完全无此语法。
仅影响 openai.rs/anthropic.rs 的流式 cancel-race 循环（各 1 处）。

## 1. 文件拆分（8 个 .rs → .at / 胶水）

### 可 Auto 化（6 文件，~1162 行 → .at）

| 文件 | 行数 | 难度 | 关键模式 |
|---|---|---|---|
| config.rs | 122 | EASY | 配置加载 + env fallback，委托 ai-config |
| tracker.rs | 86 | EASY | usage 记账，Mutex<HashMap>（golden `003_sync`） |
| sse.rs | 147 | EASY | 纯字节 SSE 解析器，无 I/O |
| format.rs | 136 | EASY-MEDIUM | canonical↔OpenAI JSON 转换（`json!` 宏 → 手动 Value 构造） |
| tier_router.rs | 315 | EASY | 纯逻辑，tier→candidate 路由（最有价值的业务逻辑） |
| pool.rs | 160 | MEDIUM | Semaphore + acquire_with_timeout（golden `003_sync` Semaphore 模式） |

### 部分可 Auto 化（2 文件，需小胶水）

| 文件 | 行数 | .at 部分 | 胶水部分 |
|---|---|---|---|
| lib.rs | 113 | LlmError 枚举 + is_retryable/Display（golden `004_variant_from_attr` 近 1:1） | `From<reqwest::Error>` 可放 .at（用 `#[from]`） |
| main.rs | 98 | `#[tokio::main]` + TcpListener::bind + axum::serve + arg 解析（全 viable） | 无 |

### 需胶水（BLOCKED 文件）

| 文件 | 行数 | 阻塞点 | 处置 |
|---|---|---|---|
| server.rs | 529 | `CancelOnDrop`（impl Drop，RAII guard，7 行） | 全文件可 .at，仅 CancelOnDrop 留 ~10 行 .rs helper 或重构 |
| provider/mod.rs | 168 | 无硬阻塞（AiProvider trait + Registry） | 可 .at（`Arc<dyn Fn + Send + Sync>` 回调已支持） |
| provider/openai.rs | 485 | `tokio::select!`（:276，流式 cancel-race） | 非 stream 的 complete() 可 .at；stream 循环留 .rs 胶水 |
| provider/anthropic.rs | 493 | `tokio::select!`（:280） | 同 openai |
| provider/ollama.rs | 80 | 无（纯委托） | 可 .at |

### Phase 0 — axum 可行性验证 ✅（2026-08-07）

**Spike 验证**：建立了 `crates/auto-ai-daemon/rust/`（Cargo.toml + lib.rs + main.rs），
写了一个极简 axum server（`/health` handler），转译 + 编译 + 运行 + curl 测试全通过。
**确认 use.rust axum/tokio 方案可行**——Router + async handler + serve + TcpListener 全链路工作。

**验证发现的 a2r 转译细节**（正式转译时需处理）：
1. `use.rust` 类型/函数在 `.at` 里用**点号**语法（`axum.Router`、`TcpListener.bind`），a2r 转 `::`
2. **三层路径**（`axum.routing.get`）转译有问题（中间层转 `::` 但末层保留 `.`）→ 绕过：用 `use.rust axum::routing::get` 直接导入末层，调用处只写 `get(health)`
3. **`#[tokio.main]` 重复**：`.at` 的 `#[tokio.main]` + `pub fn main` 导致重复输出 → 正式转译时 main 不加 `pub`，或用专门的 main 处理
4. **`tracing::info` 是宏**：a2r 把 `tracing.info(...)` 转成函数调用 `tracing::info(...)`，但实际是宏 → 需用 a2r 的宏调用语法（`#[macro]` 或 `tracing::info!`），或用 `println!` 替代
5. **axum handler 必须是 async fn**：`fn health() -> String` 不满足 `Handler` trait，需 `async fn`（这是 axum 约束，非 a2r 问题）

**结论**：方案可行，上述 1-4 是转译时需注意的细节，5 是 axum 本身的约束（.at 的 `~String` 方法会转 async fn，正好匹配）。

## 2. 技术决策

### 2.1 tokio::select! 的处理（唯一硬阻塞）
两个选择：
- **A（接受胶水）**：openai/anthropic 的流式循环留手写 .rs（~20-30 行/文件），其余转 .at。
  两个 provider 的 build_body/response 解析可移到 .at，胶水只保留 select! 循环。
- **B（扩展 a2r）**：给 .at 加 select! 语法（`select { biased; _ = cancel.cancelled() => {...} }`）。
  auto-lang 工程，解除阻塞后 openai/anthropic 全量 .at。
- **决策**：本计划先走 A（接受胶水），select! 语法扩展作为可选 follow-up（记入 KNOWN-DEBT）。
  理由：A 能让 daemon 80%+ Auto 化，select! 只影响 2 处流式循环。

### 2.2 CancelOnDrop（server.rs 小阻塞）
- `impl Drop` 在 .at 无语法。两个选择：留 ~10 行 .rs helper / 重构掉（用 channel close 替代）。
- **决策**：留小 .rs helper（CancelOnDrop 是安全网，重构风险高于收益）。

### 2.3 .at 与胶水的组装模式
- 建立 `crates/auto-ai-daemon/src/*.at`（源码）+ `rust/`（转译树，独立 crate `auto-ai-daemon-a2r`）
  + 手写胶水（`rust/src/server_glue.rs`、`rust/src/provider/stream_glue.rs` 等）
- 对齐既有 3 crate 的 retranspile.sh 模式
- daemon 是 binary（aaid）+ lib，转译树需 `[[bin]] aaid` + `[lib]`

## 3. 实施路线（分阶段，可控）

### Phase 0 — 建立转译树骨架
- [ ] 0.1 创建 `crates/auto-ai-daemon/src/`（.at 源码）+ `rust/`（转译树）+ `retranspile.sh`
- [ ] 0.2 `rust/Cargo.toml`：crate 名 `auto-ai-daemon-a2r`，独立 workspace，`use.rust` 的 Cargo dep
      声明（axum/tokio/reqwest 等，a2r 的 `dep` 语法或手写 Cargo.toml）
- [ ] 0.3 确认 .at 的 `use.rust axum` / `use.rust tokio` 能正确转译出 `use axum::...;`（Phase 0 验证）

### Phase 1 — EASY 文件转 .at（低风险）
- [ ] 1.1 config.rs → config.at
- [ ] 1.2 tracker.rs → tracker.at
- [ ] 1.3 sse.rs → sse.at
- [ ] 1.4 tier_router.rs → tier_router.at
- [ ] 1.5 format.rs → format.at（处理 `json!` 宏 → 手动 Value）
- [ ] 1.6 retranspile + 转译树 cargo check 0 错

### Phase 2 — MEDIUM 文件转 .at
- [ ] 2.1 pool.rs → pool.at（Semaphore 模式）
- [ ] 2.2 lib.rs → lib.at（LlmError + #[from]）
- [ ] 2.3 provider/mod.rs → provider/mod.at（AiProvider trait + Registry）
- [ ] 2.4 provider/ollama.rs → ollama.at（纯委托）
- [ ] 2.5 retranspile + cargo check

### Phase 3 — server.rs + main.rs（axum 层，核心验证）
- [ ] 3.1 main.rs → main.at（#[tokio::main] + TcpListener + axum::serve）
- [ ] 3.2 server.rs → server.at（Router 链 + 解构 extractor + impl IntoResponse + async_stream SSE body）
- [ ] 3.3 CancelOnDrop 留 .rs helper（server_glue.rs）
- [ ] 3.4 retranspile + cargo check（**关键里程碑**：axum 服务端从 .at 转译成功）

### Phase 4 — provider openai/anthropic（select! 胶水）
- [ ] 4.1 openai.rs/anthropic.rs 的非流式部分（build_body、response 解析）→ .at
- [ ] 4.2 流式 select! 循环 → 手写 .rs 胶水（provider/stream_glue.rs）
- [ ] 4.3 retranspile + cargo check

### Phase 5 — 端到端验证
- [ ] 5.1 转译版 daemon（auto-ai-daemon-a2r）build 出 aaid 二进制
- [ ] 5.2 启动转译版 daemon，用 auto-ai-react（转译版 agent + Plan 024 转译版 client）跑全链路 e2e：
      转译版 agent → 转译版 client → **转译版 daemon** → LLM
- [ ] 5.3 对比原生 daemon 的 /v1/status、/v1/chat/completions 行为

### Phase 6 — KNOWN-DEBT + 文档
- [ ] 6.1 tokio::select! 阻塞记录（openai/anthropic 流式循环留 .rs）
- [ ] 6.2 CancelOnDrop 的 impl Drop 记录
- [ ] 6.3 全链路 Auto 版 e2e 文档（agent + client + daemon 三层转译）

## 4. 风险

- **axum 端到端未验证**：goldens 证明各单项（Router 链、extractor、impl IntoResponse），但从未组合过
  完整 axum 服务端。Phase 3.4 是关键验证点，可能需要 transpile-then-fixup。
- **services.rs**（233 行）：进程管理（`std::process::Command` + cmd/sh + TCP probe）——OS 特定胶水，
  可能在 Phase 2 遇到 `cfg!` 问题，可能需部分留 .rs。
- **Cargo dep 声明**：.at 的 `use.rust axum` 需要在转译树 Cargo.toml 声明 axum 依赖。a2r 的
  `dep axum(version: "0.7")` 语法（golden `021_path_dep`）或手写 Cargo.toml。
- **select! follow-up**：若用户后续想要 openai/anthropic 全量 .at，需给 a2r 加 select! 语法。

## 5. 完成判定

- [ ] daemon 的 8 文件中 6+ 转 .at（config/tracker/sse/format/tier_router/pool/lib/main/server/ollama/provider-mod）
- [ ] openai/anthropic 的流式 select! 循环留 .rs 胶水（已知限制）
- [ ] 转译版 daemon 能 build 出 aaid 二进制 + 启动 + 响应 /v1/status
- [ ] **全链路 Auto 版 e2e**：转译版 agent + client + daemon 三层跑通
- [ ] workspace + 转译版测试无回归
- [ ] KNOWN-DEBT 记录 select! / CancelOnDrop / services.rs 限制

## 6. 里程碑意义

本计划完成后，auto-ai 的三层核心（agent + client + daemon）全部有 Auto 版，全链路 e2e 可跑
（auto-ai-react + 转译版 client + 转译版 daemon）。这是"Auto 版 e2e 完整流程"的真正实现
（ai-config 按架构决定不转译，auto-ai-cli 按 Plan 023 评估不转译）。
