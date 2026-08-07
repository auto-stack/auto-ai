# Plan 024: 激活转译版 auto-ai-client（agent→client 全链路 Auto 版）

> **状态**：📋 计划（待批准）
> **仓库**：auto-ai（纯 auto-ai 侧）
> **目标**：让转译版 agent（`auto-ai-react`）消费转译版 client（`auto-ai-client-a2r`），
> 使 agent→client 路线完整 Auto 化（不再经 rust-ref client）。
> **非目标**：不动 workspace 默认（CLI/daemon 仍用 rust-ref client）；不动 daemon。
> **前置调研**：client 激活可行性调研（三处阻塞已定位）。

---

## 0. 背景：当前运行链路 vs 目标

```
当前：auto-ai-react（转译版 agent）
  → client_impl.rs（手写胶水，impl Client for 真实 AiClient）
  → rust-ref AiClient（原生）              ← 这里仍是原生 Rust
  → daemon → LLM

目标：auto-ai-react（转译版 agent）
  → client_impl.rs（保留作 trait 适配器，但 impl 的对象改为转译版 AiClient）
  → 转译版 AiClient（auto-ai-client-a2r）  ← 全 Auto 版
  → daemon → LLM
```

转译版 client 的 HTTP 层（a2r-std http，基于 ureq）**已经实现并能工作**（Plan 013 G6 已交付，
`a2r-std/src/http.rs` 完整实现 POST/SSE）。所以 HTTP 不是存在性阻塞，而是质量阻塞（见 §1.3）。

## 1. 三个阻塞点（调研已定位）

### 1.1 AiClient 是 crate-private（致命编译阻塞）
- `rust/src/lib.rs:53` `struct AiClient`（无 `pub`）→ 外部 crate 无法命名该类型
- 根因：`src/lib.at:41` `type AiClient {`（无 `pub`）→ 转译产物无 pub
- 修复：`.at` 源码加 `pub`（type + ext 块 + 方法）

### 1.2 签名不匹配 agent 的 Client trait
- 转译版 `complete_stream` 的 `on_event: fn(JsonValue)`（plain fn 指针）≠ trait 要的 `Arc<dyn Fn(JsonValue) + Send + Sync>`
- 修复：`.at` 源码改 `complete_stream` 回调类型为 `Arc<Fn(JsonValue)>`（Plan 397 已确认 spec 方法参数支持），retranspile

### 1.3 HTTP 是 sync-in-async（性能/正确性）
- a2r-std http 用 ureq（同步）+ 后台线程；`pub async fn complete` 实际阻塞 tokio 执行器
- 影响：重载下可能饿死 executor / 死锁
- 修复选项：a) 转译版 client 用 `tokio::task::spawn_blocking` 包裹同步 http 调用；
  b) 接受当前状态（auto-ai-react 是单用户 REPL，非高并发，阻塞影响有限），记为 KNOWN-DEBT
- **决策**：本计划选 b（接受 + 记录）——auto-ai-react 是验证入口非生产服务器，sync-in-async 可接受。
  若后续 daemon 也 Auto 化并需要全异步链路，再处理。

## 2. 实施路线

### Phase 1 — 修 .at 源码 visibility + 签名（auto-ai-client）
- [ ] 1.1 `src/lib.at`：`type AiClient` → `pub type AiClient`
- [ ] 1.2 `ext AiClient` → `pub ext AiClient`
- [ ] 1.3 `new`/`with_url`/`default`/`url`/`is_daemon_mode` 加 `pub`
- [ ] 1.4 `complete_stream` 的 `on_event fn(JsonValue)` → `on_event Arc<Fn(JsonValue)>`
- [ ] 1.5 retranspile `crates/auto-ai-client/retranspile.sh`，确认 `rust/src/lib.rs` AiClient 变 pub、
      complete_stream 签名含 `Arc<dyn Fn(JsonValue) + Send + Sync>`

### Phase 2 — 依赖切换（agent 转译树指向转译版 client）
- [ ] 2.1 `auto-ai-agent/rust/Cargo.toml`：`auto-ai-client = { path = "../../auto-ai-client" }`
      → 改为指向转译版 client crate
- [ ] 2.2 **关键决策**：转译版 client crate 名是 `auto-ai-client-a2r`（lib 名 `auto_ai_client_a2r`），
      与 agent 当前 import 的 `auto_ai_client` 不同。两个选择：
      - **A（改 import）**：agent 的 .at 源码 + client_impl.rs + main.rs 的 `use auto_ai_client::` 改成
        `use auto_ai_client_a2r::`。改动面大但语义清晰。
      - **B（crate 别名）**：转译版 client 的 Cargo.toml 加 `package-name = "auto-ai-client"`？
        不行——crate 名冲突。
      - **C（path 指向 + lib name 对齐）**：agent/rust/Cargo.toml 用
        `auto-ai-client = { path = "../../auto-ai-client/rust" }` 但 Cargo 用目录名/包名识别，
        包名是 `auto-ai-client-a2r`，依赖键得是 `auto-ai-client-a2r`。
      - **推荐 A**：改 import。agent 的 .at 里 `use auto_ai_client: ...` → 用 a2r crate 的 re-export shim。
        实际上 agent.rs 的 shim `pub mod auto_ai_client { pub use ::auto_ai_client::*; }` 改成
        `pub use ::auto_ai_client_a2r::*`。这样 agent 的其余代码的 `use crate::auto_ai_client::` 不变！
- [ ] 2.3 重新 build `auto-ai-agent/rust/`，确认解析到转译版 client

### Phase 3 — client_impl.rs 适配（impl Client for 转译版 AiClient）
- [ ] 3.1 `client_impl.rs` 当前 `impl Client for AiClient`（rust-ref 的 AiClient）。
      改为 impl 转译版的 AiClient。但转译版 AiClient 已有 complete/complete_stream 方法——
      是否还需要 client_impl.rs？
      - 转译版 AiClient 的 complete_stream 用 `fn(JsonValue)`（Phase 1.4 改成 Arc<Fn> 后对齐）
      - 转译版 AiClient 的 HTTP 用 a2r-std（ureq），而非 reqwest——这是它自己的实现
      - 所以 client_impl.rs 可能**不再需要**（转译版 AiClient 自己 impl 了 Client trait？不——
        AiClient 是结构体，Client 是 agent 的 spec/trait。AiClient 需要有人 impl Client for AiClient）
- [ ] 3.2 确认：转译版 AiClient 是否能直接 `impl Client for AiClient`（在 client_impl.rs 胶水里）。
      需要 AiClient 是 pub（Phase 1 已修）。impl 体调 AiClient 自己的 complete/complete_stream。
- [ ] 3.3 StreamingAiClient 的处理：它包装真实 reqwest AiClient 做 SSE 侧信道。
      如果改用转译版 client，侧信道是否还需要？转译版 client 的 complete_stream 已用 a2r-std http
      做 SSE。StreamingAiClient 可能可以删除或改造。

### Phase 4 — 验证
- [ ] 4.1 `auto-ai-agent/rust/` cargo check 0 错
- [ ] 4.2 `auto-ai-react` 二进制重新 build
- [ ] 4.3 **e2e 实跑**（daemon 在运行 + 配置文件有 key）：确认转译版 client 经 a2r-std http 能与 daemon 通信
- [ ] 4.4 转译版测试套件（transpiled_harness 15 测试）仍全绿（mock 不受影响——它们不依赖 AiClient）
- [ ] 4.5 workspace 回归（rust-ref client 不受影响）

### Phase 5 — KNOWN-DEBT 记录
- [ ] 5.1 记录 sync-in-async（a2r-std http 用 ureq 同步，async fn 实际阻塞）作为已知限制
- [ ] 5.2 记录转译版 client 与 rust-ref 的行为差异（如有）

## 3. 风险

- **a2r-std http 的 SSE 实现质量**：ureq + 后台线程 + mpsc 的 SSE 流式，能否正确解析 daemon 的
  SSE 响应？Plan 013 G6 实现时验证过，但 daemon 协议可能已演进。Phase 4.3 的 e2e 实跑是关键验证。
- **依赖解析**：agent/rust 是独立 workspace，指向 client/rust（也是独立 workspace）。path dep 跨独立
  workspace 需确认能解析（既有先例：agent/rust 已依赖 client crate 根）。
- **client_impl.rs 角色**：从"桥接到真实 reqwest AiClient"变成"impl Client for 转译版 AiClient"——
  实质变化，需仔细处理。

## 4. 完成判定

- [ ] 转译版 agent（auto-ai-react）的运行链路里，client 层用的是转译版（auto_ai_client_a2r），
      不再经 rust-ref
- [ ] e2e 实跑通过（转译版 client → a2r-std http → daemon → LLM）
- [ ] workspace + 转译版测试全绿，无回归
- [ ] KNOWN-DEBT 记录 sync-in-async 限制
