# Plan 025: auto-ai-daemon Auto 化（直接 use.rust axum/tokio 方案）

> **状态**：🟢 Phase 0 + 1 + 2 + 3 完成（server.at 含 AppState + status/models/usage/chat_completions 四个 handler 全从 .at 转译，
> config_test/services_*/streaming_response 留 server_glue.rs 手写，services.rs 复制，`./retranspile.sh check` cargo check = 0 错幂等），Phase 4-5 待推进
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

### Phase 1 — EASY 文件转 .at（✅ 完成）

**5 个 EASY 文件全部转译完成，`./retranspile.sh check` 全自动流水线 cargo check = 0 错（幂等、可复现）。**

sse.at 的初版用 `.substring()` + 自定义 helper（find_in_str / split_once / trim_start_spaces）
触发了 4 类 a2r codegen 缺陷（self.&buf 借用、单参 substring、Vec.first() on Option、
`i + 1 as usize` 优先级）。**解决方式不是逐个 sed，而是重写 sse.at 改用 auto-ai-client
SseBuffer 已验证可编译的 `.slice()` / `.find()` / `.trim()` 模式** —— 这样在源头规避了
全部 4 类缺陷，只剩一个机械的 `str_find(&self.buf, ...)` 借用 sed（Plan 019 D-class，
和 client 相同）。

转译过程新发现并记录的 a2r 缺陷（已用 retranspile.sh sed 兜底）：
- `-> impl StructName`：a2r 在 struct 返回类型前误加 `impl` 关键字（config）
- `dirs.home_dir()`：a2r 把关联函数调用渲染成方法调用（`.` 应为 `::`）（config）
- `const X = 4` 推断成 `i32` 而非目标字段的 `usize`（config）
- `ProviderConfig(...)`：a2r 把 struct 渲染成 positional tuple ctor（config）
- `.as_str()` on `&str`：用了 unstable `str_as_str` feature（config）
- `HashMap::get(&owned_key)`：借用/类型推断失败，需 `.as_str()` 或 turbofish（tracker / tier_router）
- `m.keys().collect()`：无类型提示导致 `HashMap::get` 的 `Q` 参数推断失败，需 turbofish（tier_router）
- `for x in &cands`（cands 已是 `&Vec`）：变成 `&&Vec` 不可迭代（tracker / tier_router / format）
- **`routes` 是 .at 保留关键字**：`config.tier_routing.routes` 在 .at 里写不出来 →
  tier_router 保留一份手写胶水 `tier_router_glue.rs`（暴露 routes 为 owned list，仿
  auto-ai-agent/client_impl.rs 模式）

**关键认知**：daemon crate 的 Cargo.toml 链接的是 **rust-ref 版 ai-config**
（`[lib] path = "rust-ref/src/lib.rs"`），所以 .at 的字段形状必须对齐 rust-ref
（DaemonConfig 无 `provider_names`、`idle_timeout_min: u64`、`max_concurrency: Option<usize>`），
而非 ai-config 自己的 .at 转译版。

**json! 宏**：a2r 不能 emit 宏。format.at 用 `serde_json::Map::new()` + `.insert()` +
`Value::Object(...)` 手动构建（已验证 a2r 干净转译）。

**ContentBlock 变体匹配**：`is block { ContentBlock.Text(t) -> ... }` 能正确转译成
`match block { ContentBlock::Text { text } => ... }`（对齐 rust-ref 的 named-field 变体）。

其余 EASY 文件（config/tracker/tier_router/format）已全部转译完成。
- [x] 0.2 `rust/Cargo.toml`：crate 名 `auto-ai-daemon-a2r`，独立 workspace，`use.rust` 的 Cargo dep
      声明（axum/tokio/reqwest 等，a2r 的 `dep` 语法或手写 Cargo.toml）
- [ ] 0.3 确认 .at 的 `use.rust axum` / `use.rust tokio` 能正确转译出 `use axum::...;`（Phase 0 验证）

### Phase 1 — EASY 文件转 .at（低风险）✅
- [x] 1.1 config.rs → config.at
- [x] 1.2 tracker.rs → tracker.at
- [x] 1.3 sse.rs → sse.at
- [x] 1.4 tier_router.rs → tier_router.at（+ 手写 `tier_router_glue.rs`：`routes` 保留关键字）
- [x] 1.5 format.rs → format.at（`json!` 宏 → 手动 `serde_json::Map` + `Value::Object`）
- [x] 1.6 retranspile + 转译树 cargo check 0 错 ✅（`./retranspile.sh check`，幂等可复现）

### Phase 2 — MEDIUM 文件转 .at（✅ 基本完成，ollama 推迟到 Phase 4）

**pool / error / provider(mod) 三个 MEDIUM 文件全部转译完成，`./retranspile.sh check` cargo check = 0 错。**

ollama.rs 与 openai.rs 强耦合（委托 `OpenAiProvider`），单独转译无法编译，整体推迟到 Phase 4。

- [x] 2.1 pool.rs → pool.at（`tokio::sync::Semaphore` + `Arc` + async acquire；用 `~?OwnedSemaphorePermit` 表达 async）
- [x] 2.2 lib.rs → **error.at**（LlmError 抽到独立模块；lib.rs 保持组装式 crate root）
      - Display/Error：a2r 从 `.message()` 方法**自动生成** `impl Display` + `impl Error`（无需 sed）
      - `impl From<reqwest::Error>`：Auto 无 impl From → 用 `.from_reqwest_error()` factory + retranspile.sh sed 注入真实 impl（Phase-4 providers 的 `?`/`.into()` 需要）
      - 结构体变体 `Upstream { status, message, retryable }`：`is self { Variant(f1, f2, f3) -> }` 干净转译；但字段绑定是引用（`retryable` 是 `&bool`）需 sed deref
- [x] 2.3 provider/mod.rs → **provider.at**（`pub spec AiProvider: Send + Sync` → `#[async_trait] pub trait`；`StreamDelta` enum；`ProviderRegistry`）
      - **关键发现**：spec 方法签名里的 `Arc<Fn(StreamDelta) + Send + Sync>` 会触发 .at 解析错误（`+ Send + Sync` 在参数位不允许）→ 只写 `Arc<Fn(StreamDelta)>`，a2r 自动补 `+ Send + Sync`
      - `from_daemon_config` 委托 `provider_glue.rs`（Phase-4 stub：返回 NoProvider；Phase 4 替换为真实 provider 构造）
- [ ] 2.4 ~~provider/ollama.rs~~ → **推迟到 Phase 4**（与 openai.rs 强耦合）
- [x] 2.5 retranspile + cargo check（`./retranspile.sh check`，幂等可复现）

Phase 2 新发现并记录的 a2r 缺陷（已 retranspile.sh sed 兜底）：
- **`+ Send + Sync` 在 spec 方法的 `Arc<Fn(..)>` 参数位是解析错误**（只能写在 spec 头 `: Send + Sync`，a2r 自动补到 Fn bound）——这是个 .at 写法约束，记入 .at 风格指南
- 结构体变体 match 的字段绑定是引用（需 deref）
- a2r 从 `.message()` 方法**自动生成** Display + Error impl（意外的好行为，省了 sed）
- `impl From` 完全不支持（Auto 无此语法）→ factory + sed 注入

**当前转译树状态**：8 个 `.at` 源文件（Phase 1 的 5 个 + Phase 2 的 pool/error/provider），
2 个手写胶水（tier_router_glue.rs / provider_glue.rs），lib.rs 组装式 crate root，
main.rs 仍是 Phase 0 spike（Phase 3 转）。

### Phase 3 — server.rs + main.rs（axum 层，核心验证）✅ 完成

**axum 服务端从 .at 完整转译成功**——`server.at` 含 `AppState` + `status`/`models`/`usage`/`chat_completions`
四个 handler 全部从 `.at` 转译（含候选链 fallback + permit + provider.complete），`config_test`/`services_*`/
`streaming_response` 留 `server_glue.rs` 手写（reqwest 链式 + spawn/mpsc/stream! 无 .at 先例），
`services.rs` 直接复制（OS 胶水），`./retranspile.sh check` cargo check = 0 错（三次幂等可复现）。

**Phase 3 实施过程中的关键决策与发现**：

1. **main.rs 保持手写**（非 main.at）。a2r 把 `println`/`eprintln` 渲染成函数调用（应为宏 `println!`），
   `env.args()` 路由到不存在的 `a2r_std::env::args`，且 `#[tokio.main]` 双重输出。bin 是纯胶水
   （arg 解析 + bind + serve），axum 验证里程碑在 server.at，故 main.rs 手写引用转译版 lib
   （同 auto-ai-agent/rust/src/main.rs 模式）。

2. **AppState 不能 derive**。`ProviderRegistry` 含 `Arc<dyn AiProvider>`（不实现 Debug/Clone），
   a2r 的默认 derive 全失败；`#[allow(dead_code)]` 抑制默认 derive（rust-ref 的 AppState 也无 derive）。

3. **.at for 循环只支持 2-tuple 解构**。`pool.status()` 返回 3-tuple `(name, available, max)`，
   .at 里用 `entry.0/.1/.2` 索引访问代替解构。

4. **`ext` 方法的构造器陷阱**：`pub fn from_config(config)` 在 `ext` 块里被 a2r 加 `&self`
   （当实例方法），但它是关联函数（构造器）。retranspile.sh sed 把 `from_config`/`from_daemon_config`/
   `complete`/`complete_stream` 的签名改成正确的 `&self`/`&DaemonConfig`/`&CompletionRequest` 形式
   （rust-ref 签名）。Phase 3 首次调用这些方法才暴露此 latent 问题。

5. **`~IntoResponse` 在有 extractor 参数时丢失 `impl`**。golden 015 证明 `~IntoResponse` →
   `async fn -> impl IntoResponse`，但当参数列表含 `State(...)`/`Json(...)` 解构时 a2r 输出
   `-> IntoResponse`（无 impl，E0782）。retranspile.sh sed 补 `impl`。

6. **`json!` 宏 → `serde_json.Map` + `Value` 手动构造**（沿用 format.at 模式）。整数需
   `serde_json.Number.from(n)` 包装（`Value.Number(int)` 类型不匹配，json! 宏隐式转换）。

7. **streaming_response 留 server_glue.rs 手写**。裸 `tokio::spawn` + 双向 `mpsc` +
   `async_stream::stream!` 三件套无 .at 转译先例（仅 actor 抽象的 spawn + 单向 recv 有），
   整个 SSE 桥接函数手写实现（含 CancelOnDrop guard），server.at 的 chat_completions 流式分支
   委托它。Arc 闭包回调（on_delta）虽低风险，但 spawn/channel/stream 组合决定了整体留 glue。

8. **config_test + services_* 留 server_glue.rs**。config_test 用 reqwest 客户端链式
   （项目无 reqwest 客户端调用先例，用 VM 内建 http）；services_* 委托手写的 services.rs
   （OS 胶水，cfg!windows/Command/reqwest::blocking）。

9. **tracker.at 的 `names` 字段改为 `Mutex<List>`**。原 `names: List<str>` 在 `record(&self)` 下
   无法 push（Vec 需 `&mut self`，但 record 经 `Arc<AppState>` 调用只能是 `&self`）。包 Mutex 后
   `&self` + 内部可变性工作，all() 用 `names.lock().iter()`。

10. **services.rs（233 行）直接复制**（非转译）。含 `cfg!(windows)`、`std::process::Command`、
    `reqwest::blocking`、`spawn_blocking` 等 OS 胶水，转译风险高且与 axum 核心无关。Cargo.toml
    补 reqwest `blocking` feature。

- [x] 3.1 main.rs 手写（bin 保留 .rs，a2r 不能 emit println!/args 宏）—— cargo check 绿
- [x] 3.2 server.at 最小版（AppState + new/cfg + router 委托 glue + status）—— axum 基础链路 0 错
- [x] 3.3 models/usage/resolve_tier_model（json! → serde_json.Map 手动构造）—— 0 错
- [x] 3.4 chat_completions（候选链 fallback + permit + provider.complete + into_response）—— **关键里程碑：0 错**
- [x] 3.5 config_test + services_*（glue 手写，reqwest 链式 + services 委托）—— 0 错
- [x] 3.6 streaming_response（glue 手写真实版：spawn + mpsc + async_stream + CancelOnDrop）—— 0 错
- [x] 3.7 server_glue.rs（build_router 全路由 + CancelOnDrop + streaming + config_test + services_*）
- [x] 3.8 services.rs 复制 + Cargo.toml 补 reqwest blocking feature
- [x] 3.9 retranspile + cargo check（**里程碑达成**：axum 服务端从 .at 转译，0 错三次幂等）

**当前转译树状态**：9 个 `.at` 源文件（Phase 1-2 的 8 个 + server.at），3 个手写胶水
（tier_router_glue / provider_glue / **server_glue**），1 个直接复制（services.rs），
lib.rs 组装式 crate root，main.rs 手写 bin。server.at 覆盖 rust-ref server.rs 的
AppState + 4 个核心 handler；server_glue.rs 覆盖路由挂载 + 3 个 handler + streaming + CancelOnDrop。

### Phase 4 — provider openai/anthropic（select! 胶水）+ ollama ⬅️ 下一会话从这里继续
- [ ] 4.1 openai.rs/anthropic.rs 的非流式部分（build_body、response 解析）→ .at
- [ ] 4.2 流式 select! 循环 → 手写 .rs 胶水（provider/stream_glue.rs）
- [ ] 4.3 ollama.rs → ollama.at（纯委托 OpenAiProvider；Phase 2 推迟至此，与 openai.rs 一起转）
- [ ] 4.4 用真实 provider 构造替换 provider_glue.rs 的 build_registry stub
- [ ] 4.5 retranspile + cargo check

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

- [x] daemon 的核心文件转 .at（config/tracker/sse/format/tier_router/pool/error/provider/**server** = 9 个 .at）
- [x] server.rs 的 axum 层从 .at 转译（AppState + status/models/usage/chat_completions）
- [x] CancelOnDrop / streaming_response / config_test / services_* 留 server_glue.rs 手写（已知限制）
- [x] services.rs 直接复制（OS 胶水，非转译）
- [ ] openai/anthropic 的流式 select! 循环留 .rs 胶水（Phase 4 已知限制）
- [ ] 转译版 daemon 能 build 出 aaid 二进制 + 启动 + 响应 /v1/status（Phase 5）
- [ ] **全链路 Auto 版 e2e**：转译版 agent + client + daemon 三层跑通（Phase 5）
- [ ] workspace + 转译版测试无回归（Phase 5）
- [ ] KNOWN-DEBT 记录 select! / CancelOnDrop / services.rs / streaming 限制（Phase 6）

## 6. 里程碑意义

本计划完成后，auto-ai 的三层核心（agent + client + daemon）全部有 Auto 版，全链路 e2e 可跑
（auto-ai-react + 转译版 client + 转译版 daemon）。这是"Auto 版 e2e 完整流程"的真正实现
（ai-config 按架构决定不转译，auto-ai-cli 按 Plan 023 评估不转译）。
