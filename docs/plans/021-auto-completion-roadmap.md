# Plan 021: Auto 版完善化 — 覆盖 Rust 原生版 100% 能力

> **状态**：🟡 实施中（2026-08-05 制定）
> **仓库**：auto-ai（主）+ auto-lang（a2r/语言能力扩展，跨仓）
> **基线**：tag `auto-mvp-v0.2`（三个转译 crate 全 0 错，rust-ref 181 测试全绿）
> **目标**：让 Auto 版（.at + rust/ 转译树）覆盖 Rust 原生版（rust-ref）的全部能力，
> 消除三个已知功能缺口，为最终"转正"（翻转 [lib] path，删 rust-ref）扫清障碍。
> **对应 016 路线图**：4.3（spec 流式）+ 4.5（第二轮 Auto 化）的功能覆盖维度。
>
> **2026-08-05 分工调整**：三个缺口（complete_stream / register_tool<T> / serde 同步）
> 全部依赖 auto-lang 的语言/a2r 能力扩展。**a2r 侧工作（dyn-Fn 支持、TaskDef 转译、
> 泛型方法语法、serde derive 注解）由用户在 auto-lang 仓库单独推进**。本 agent 负责
> auto-ai 侧不依赖 auto-lang 的工作：转正前置（orchestration 功能对齐审计等）。

---

## 三大功能缺口（auto-mvp-v0.2 基线）

经核查，Auto 版与 Rust 原生版有三个功能缺口。**三者都依赖 auto-lang 的语言/a2r 能力扩展**，
无法纯 auto-ai 内解决：

### 缺口 1：`complete_stream` 流式（最重，L 级）

- **现状**：rust-ref 的 `Client` spec 有 `complete_stream(req, on_event: Arc<dyn Fn(Value)>)`，
  agent 的 ReAct 循环用它做实时流式输出。Auto 版的 `agent.at` 只有 `complete`（非流式），
  `run_stream` 退化为 polling-style（事件进本地 list 不外发）。
- **阻塞**：Auto 解析器 + a2r 双双不支持 `dyn Fn(...)` / `impl Fn(...)` 类型（Plan 018 调研确认）。
  `dyn Trait`（具名 trait）已支持，但 Fn-trait object 无对应语法分支。
- **当前 workaround**：rust/src/client_impl.rs 的手写 `StreamingAiClient` + std::sync::mpsc channel
  绕过 spec，恢复 client 层 token 流（但 agent 层 ReAct 事件不外发）。
- **修复路径**：
  - **A（完整，L）**：auto-lang 给解析器加 `dyn Fn(...)` 语法 + a2r 输出分支。
  - **B（备选，M）**：用 Auto actor/channel 机制（agent.at:42 注释已埋伏笔）在 spec 层做事件队列，
    绕开 dyn-Fn。不依赖解析器改动。

### 缺口 2：`register_tool<T>` 泛型（中，M 级）

- **现状**：rust-ref 的 `ToolRegistry::register<T: Tool + 'static>(tool: T)` 内部 `Arc::new(tool)`。
  Auto 版只有 `register_shared(tool: Arc<Tool>)`——缺"接受任意 Tool 实现类型"的泛型入口。
- **阻塞**：Auto 语言没有 `<T: Tool>` 泛型方法语法。Plan 016 §2.3 明确"推迟到转正"。
- **影响**：下游（auto-ai-cli）若想 `register(MyTool{...})` 必须先 `Arc::new` 包箱。
  功能可用但不便，且与 rust-ref API 不完全对齐。
- **修复路径**：需 auto-lang 支持泛型方法声明（或 Auto 层面的 trait object 自动装箱语法）。

### 缺口 3：Plan 381 serde 同步到 .at 源（中，M 级）

- **现状**：rust-ref 的 `role_config.rs` 用 `#[derive(Deserialize)] struct RoleDecl` +
  `node.deserialize()` + `auto_val::lenient_f64_opt` / `string_or_list_opt`。转译版 `rust/src/role_config.rs`
  是旧 `opt_str`/`opt_float`/`opt_uint` 手写风格。ai-config 的 loader.rs 同样分叉。
- **阻塞**：a2r 不支持 `#[derive(Deserialize)]` 和 `#[serde(deserialize_with = "...")]` 注解的转译。
- **影响**：转译版功能正确（opt_* 风格可用），但与 rust-ref 不一致；转正（翻 path）前需同步。
  另：rust-ref 的 provider 反序列化错误硬传播行为（Plan 381），转译版是静默跳过——行为分叉。
- **修复路径**：需 a2r 扩展 serde derive 注解支持（或 retranspile.sh 注入）。

---

## 实施路线（按可控性排序）

> 三个缺口都跨仓依赖 auto-lang。本计划从**最可能独立推进**的角度切入，同时记录 auto-lang 前置。

### Phase 1 — 缺口 1 备选路径评估（agent 事件队列，auto-ai 内）

不依赖 auto-lang 解析器改动，尝试用 Auto 现有能力（channel/actor）在 agent 层恢复事件外发。

### Phase 1 — 缺口 1 备选路径评估（agent 事件队列）— ❌ 否决（2026-08-05）

> **结论：actor 方案在 a2r 转译路径不可行。**
>
> 调研发现 Auto 有完整的 actor 模型（`task`/`Task.spawn`/`h.send`/`on{}`/`ask`+`ctx.reply`），
> 底层是 `tokio::sync::mpsc<Value>`，理论上可替代 `Arc<dyn Fn(StreamEvent)>` 回调。
> 但 **a2r 转译器（`trans/rust.rs`）没有 `Stmt::TaskDef` 的转译分支**（grep 零命中）——
> actor 是 AutoVM 运行时特性，a2r 从未实现 `task` 声明的 Rust 输出。
>
> - [x] 1.1 调研 agent.at 的 `run_inner` 事件缓冲机制 — 确认 `events: List<StreamEvent>` 局部缓冲，外发断点明确
> - [x] 1.2 评估 Auto 的 channel/actor 能力 — actor 模型（Task.spawn + h.send）存在且 VM 可用，
>       但 VM channel 只支持 i32；actor handle 是一等值（i32 opaque ID）
> - [x] 1.3 **关键否决点**：a2r 无 `TaskDef` 转译分支 — `task Foo { on {} }` 声明无法转译成 Rust，
>       转译版 rust/ 无法生成 actor 代码。actor 方案只对 VM 执行路径有用，转译路径不可行
> - [x] 1.4 转为依赖 Phase 3 的 dyn-Fn 路径（或先给 a2r 加 TaskDef 转译，也是 auto-lang 工程）
>
> **Phase 1 否决后的路径**：缺口 1 的修复统一走 Phase 3（a2r 加 dyn-Fn 支持），
> 或额外先给 a2r 加 TaskDef 转译（让 actor 方案对转译版也可行）。两者都是 auto-lang L 级工程。

### Phase 2 — 缺口 2 泛型工具注册（需 auto-lang 评估）

- [ ] 2.1 评估 Auto 语言能否表达"接受任意 Tool 实现"的语义（泛型方法 / trait object 自动装箱 / 其他）
- [ ] 2.2 若 auto-lang 可支持：在 auto-lang worktree 实现泛型方法语法（或装箱语法）
- [ ] 2.3 回 auto-ai 在 tool.at 加 `register` 方法，重跑 retranspile 验证
- [ ] 2.4 若 auto-lang 暂不支持：记录为语言能力前置，保持 `register_shared` workaround

### Phase 3 — 缺口 1 完整路径（dyn-Fn，auto-lang，L 级，若 Phase 1 备选不可行）

- [ ] 3.1 auto-lang worktree：解析器加 `dyn Fn(...)` / `impl Fn(...)` 类型语法分支
- [ ] 3.2 a2r 加 Fn-trait object 的 Rust 输出（`dyn Fn(Value) + Send + Sync`）
- [ ] 3.3 回 auto-ai：agent.at 的 `Client` spec 加 `complete_stream`，run_inner 用回调 emit
- [ ] 3.4 删除 client_impl.rs 的 StreamingAiClient workaround（回归 spec 路径）

### Phase 4 — 缺口 3 serde 同步（转正前置）

- [ ] 4.1 评估 a2r 对 `#[derive(Deserialize)]` 注解的支持现状
- [ ] 4.2 auto-lang worktree：a2r 扩展 serde derive 注解转译（或 retranspile.sh 注入绕过）
- [ ] 4.3 回 auto-ai：role_config.at / loader.at 用 serde 重写，对齐 rust-ref
- [ ] 4.4 同步 Plan 381 的 provider 错误硬传播行为

---

## 风险与注意

- **三个缺口全跨仓**：本计划的主体工作在 auto-lang（语言/a2r 扩展）。auto-ai 侧的消费验证
  依赖 auto-lang 合并 + 重建 auto.exe 后回 auto-ai 重跑 retranspile。
- **只构建 debug**（用户既定要求）：全程 `cargo build`，不跑 release。
- **Phase 1 是关键探路**：如果 Auto 的 channel 能力足以在 agent 层做事件队列，
  缺口 1 可以不依赖 dyn-Fn（最重的 auto-lang 改动），大幅降低整体工作量。
- **转正（4.6）仍是后续**：本计划消弭功能缺口；转正还需 orchestration 功能对齐审计 +
  serde 同步（Phase 4）+ 翻转 [lib] path，是 plan 021 之后的独立计划。

## 完成判定

- [ ] 缺口 1：agent 层流式事件能外发（complete_stream 或 channel 方案）
- [ ] 缺口 2：ToolRegistry 有泛型/装箱的 register 入口（或明确记录语言前置）
- [ ] 缺口 3：转译版的 role_config/loader 与 rust-ref 的 serde 行为对齐
- [ ] 三个转译 crate 仍 0 错；rust-ref 主版本测试全绿
- [ ] 打 tag `auto-complete-v0.1`（或 auto-mvp-v0.3）标记功能覆盖达成
