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

### 缺口 2：`register_tool` 泛型 → 📌 重新定性（2026-08-06 实证），路线改走 spec-param + a2r call-site 修复

> **2026-08-06 实证推翻原始判断**：无需泛型语法。a2r 的 spec-param 机制（`fn register(tool Tool)`，
> `Tool` 是 spec）已转译为 `fn register(tool: Box<dyn Tool>)`，达到 rust-ref `register<T: Tool>(tool: T)`
> 的全部人体工学价值（调用方写 `register(my_tool)` 即可）。真缺陷是 **a2r call-site 不自动 `Box::new`
> 具体结构体实参**（`r.register(t)` 转译无包装 → E0308）。
>
> **泛型语法路线 ❌ 否决**（spec-param 已够，泛型 + `'static` 是 L 级过度设计）。

- **现状**：rust-ref 的 `ToolRegistry::register<T: Tool + 'static>(tool: T)` 内部 `Arc::new(tool)`。
  Auto 版只有 `register_shared(tool: Arc<Tool>)`——缺"接受任意 Tool 实现类型"的入口。
- ~~**阻塞**：Auto 语言没有 `<T: Tool>` 泛型方法语法~~ → **重新定性**：阻塞在 a2r call-site
  不自动装箱 spec 参数的具体值实参（三处协调缺陷，详见 auto-lang Plan 390 §11.2）。
- **影响**：下游（auto-ai-cli）若想 `register(MyTool{...})` 必须先 `Arc::new(Box::new(...))` 手包箱。
- ~~**修复路径**：需 auto-lang 支持泛型方法声明~~ → **新修复路径**：
  - **Phase E（auto-lang）**：a2r call-site spec 自动装箱修复（3 处 `trans/rust.rs` 改动）→ auto-lang **Plan 390 §11**
  - **Phase F（auto-ai）**：`tool.at`/`agent.at` 加 `register(tool Tool)` / `register_tool(tool Tool)` → auto-lang Plan 390 §12
  - 存储类型仍 `Arc<Box<dyn Tool>>`（双层包装，Plan 019 已知限制，功能可用）

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

### Phase 1b — 缺口 1 agent 层流式落地（actor TaskRef）✅ 2026-08-05

> **前置解除**：auto-lang Plan 387 §16/17 实现 a2r actor 转译（TaskDef/on/外部 enum 消息/
> `TaskRef<StreamEvent>` 作函数参数 move），三个 P0 阻塞全部解决。1.3 的否决点不再成立，
> Phase 1 按 §16 预期用法重启（用户选定 actor 方案）。

- [x] 1b.1 agent.at 新增 `task EventSink`（on-block is-解构全部 7 个 StreamEvent 变体，
      状态 `log` 累计文本；应用侧转发需 a2r 增强，见 KNOWN-DEBT）
- [x] 1b.2 `run_stream` 签名改为 §16 规格：`(task_msg str, cancel Arc<AtomicBool>, sink TaskRef<StreamEvent>)`
- [x] 1b.3 `run_inner(task_msg, cancel ?Arc, sink TaskRef)` 事件点全部改 `sink.send(ev)`（10 处），
      移除局部 `events` 缓冲（run 路径由内部丢弃 sink 消费，对齐 rust-ref "on_event 恒为 sink"）
- [x] 1b.4 `run` 内部 `Task.spawn("EventSink", 16)` 丢弃 sink（无 Arc 构造，避开 a2r `as_str` 误插缺陷）
- [x] 1b.5 driver.at 改用 `run()`（转非流式，行为不变；PipelineEvent.Delta/Tool 转发留后续——
      a2r 无闭包，agent 事件转发到 driver.on_event 需 actor 持 fn 状态，见 KNOWN-DEBT）
- [x] 1b.6 验证：retranspile 重跑，rust/ 转译版 0 错误；独立 crate 端到端运行——spawn EventSink →
      run/run_stream → sink 收到并解构事件（stdout 确认）；workspace 0 错误 + rust-ref 100 单测/5 集成全绿

**遗留（auto-lang 侧，见 KNOWN-DEBT）**：a2r 缺陷——on-block 的 `ev` 绑定不能作表达式变量
（handler 无法整包转发事件）、fn 指针不能作 task state 字段（生成 `/* unknown */`）、f-string
自引用 task state 字段解析失败（已用 `+` 拼接绕过）。修复后 EventSink handler 可真正外发到
app 的 channel/SSE。

### Phase 2 — 缺口 2 工具注册 → ✅ 完成（2026-08-06，spec-param + a2r call-site 修复）

> 泛型语法否决（见"缺口 2"章节）。auto-lang **Plan 390 §11 (Phase E)** + **§12 (Phase F)** 承接。

- [x] 2.1 ~~评估泛型方法可行性~~ → **已重评估**：spec-param 机制已够，真缺陷在 a2r call-site 自动装箱
- [x] 2.2 auto-lang worktree：a2r call-site spec 自动装箱修复 → Plan 390 **Phase E**（已合并 master，commit `0126f846`）
- [x] 2.3 回 auto-ai：`tool.at`/`agent.at` 加 `register(tool Tool)` / `register_tool(tool Tool)` → Plan 390 **Phase F**（本次）
- [x] 2.4 retranspile 0 错（rust/ 独立 crate + workspace 双检）；`r.register(EchoTool())` →
      `r.register(Box::new(EchoTool {}))` 自动装箱，CLI 无需手包箱；rust-ref 100 单测全绿 →
      **缺口 2 完成判定勾选**

### Phase 3 — 缺口 1 完整路径（dyn-Fn，auto-lang，L 级，若 Phase 1 备选不可行）

- [ ] 3.1 auto-lang worktree：解析器加 `dyn Fn(...)` / `impl Fn(...)` 类型语法分支
- [ ] 3.2 a2r 加 Fn-trait object 的 Rust 输出（`dyn Fn(Value) + Send + Sync`）
- [ ] 3.3 回 auto-ai：agent.at 的 `Client` spec 加 `complete_stream`，run_inner 用回调 emit
- [ ] 3.4 删除 client_impl.rs 的 StreamingAiClient workaround（回归 spec 路径）

### Phase 4 — 缺口 3 serde 同步（转正前置）

> **由用户在 auto-lang 单独推进**（a2r 加 serde derive 注解转译）。

- [ ] 4.1 评估 a2r 对 `#[derive(Deserialize)]` 注解的支持现状
- [ ] 4.2 auto-lang worktree：a2r 扩展 serde derive 注解转译（或 retranspile.sh 注入绕过）
- [ ] 4.3 回 auto-ai：role_config.at / loader.at 用 serde 重写，对齐 rust-ref
- [ ] 4.4 同步 Plan 381 的 provider 错误硬传播行为

### Phase 5 — orchestration 功能对齐审计（auto-ai 内，转正前置）

> 不依赖 auto-lang。确认转译版 rust/ 的 orchestration 与 rust-ref 功能等价（无缺失）。

- [x] 5.1 行数/测试占比审计：五个文件（budget/driver/flow/handoff/pipeline）的业务代码差异
      **结论：转译版无功能缺失**——所有文件业务差为负数（rust/ 比 rust-ref 业务代码多），
      差异全是 a2r codegen 膨胀（显式类型注解、显式 return）+ rust-ref 内嵌测试。
      driver.rs：rust-ref 321 业务 + 267 测试 = 588；rust/ 404（无测试），业务多 83 行。
- [x] 5.2 方法等价性抽查：转译版 driver 有 13 个方法（含 dispatch/drive_step/resolve_gate_auto/
      handle_after_submit/build_step_input），rust-ref 有 7 个（部分逻辑内联在 drive 里）。
      **转译版方法更多，非更少**。
- [x] 5.3 API 兼容性差异记录（转正时需同步改下游）：
      - `PipelineDriver`：rust-ref 泛型 `<F: AgentFactory>` → rust/ 非泛型 + `Box<dyn AgentFactory>`
      - `PipelineDriver::new`：`factory: F` → `factory: Box<dyn AgentFactory>`（cli 需 `Box::new()`）
      - `drive(on_event)`：rust-ref 回调 `Arc<dyn Fn>` → rust/ `fn(PipelineEvent)` 指针
      - `AgentFactory` trait：rust-ref `: Send + Sync` bound → rust/ 无 bound

### Phase 6 — EventSink 外发：a2r 三个限制修复（auto-lang worktree，缺口 1 收尾）

> **2026-08-05 追加**。Phase 1b 落地后，agent 层事件已能进入 EventSink actor，但 handler
> 无法把事件转发给 app。根因是 a2r 的三个限制（Phase 1b 调研实证，见 KNOWN-DEBT）：
>
> | # | 限制 | 症状 | 影响 |
> |---|---|---|---|
> | R1 | on-block 的 TypeBinding（`ev StreamEvent`）绑定不能作表达式变量 | E0201 "undefined variable ev"（`forward(ev)` / `(self.cb)(ev)` 都报错） | handler 无法整包转发事件 |
> | R2 | fn 指针不能作 task state 字段 | `cb = noop_event` 生成 `cb: /* unknown */` | EventSink 无法持有"转发回调"状态（driver 的 Delta/Tool 转发同理被阻） |
> | R3 | f-string 自引用 task state 字段解析失败 | `log = f"${log}D:${t};"` 报 E0007（Phase 1b 已用 `+` 拼接绕过） | 事件文本累计只能用 `+` 拼接 |
>
> **修复后预期**：EventSink handler 可写 `fn forward(ev StreamEvent)` 或持 `cb fn(StreamEvent)`
> 状态做 app 转发；driver.at 可恢复 rust-ref 的 Delta/Tool → PipelineEvent 转发；f-string 恢复。
> 实施由用户在 auto-lang worktree 推进（spec 见 auto-lang Plan 387 §18）。

- [x] 6.1 auto-lang worktree（`plan-389/a2r-task-scope-fixes`）：修复 R1（on-block TypeBinding 入 name scope）
- [x] 6.2 auto-lang worktree：修复 R2（task state 字段 fn 指针类型推导）
- [x] 6.3 auto-lang worktree：修复 R3（f-string 自引用 task state 解析）
- [x] 6.4 回归：a2r 22_actors 001-015（含新增 013/014/015）全绿 + auto-lang 全量测试零新增失败
- [x] 6.5 回 auto-ai：重建 auto.exe（含 Plan 390 master）→ retranspile 0 错；
      EventSink 加 `cb = noop_event` state 字段 + `(self.cb)(ev.clone())` 转发；
      `run` 用 `Task.spawn("EventSink", 16, f"", noop_event)` 注入默认 cb（Plan 390 Phase B
      spawn 带参）。a2r spawn helper 修为 `_with` 双函数（Rust 不支持参数默认值，
      auto-lang commit `ad73cb06`）。rust/ 独立 crate + workspace 0 错；rust-ref 100 单测全绿。
- [ ] 6.6 driver.at 恢复 rust-ref 等价：Delta/Tool → PipelineEvent 转发
      **⏸ 架构阻塞**（2026-08-07 调研）：driver 的 `on_event` 回调需注入 EventSink 的
      `cb fn(StreamEvent)`，但 cb 只能接收单参数（事件），无法捕获 `on_event`。
      Auto 闭包不能捕获外部变量（`fn(ev) { forward(ev, outer_cb) }` 报 "Variable ev
      not defined"），EventSink actor 又无法持有 `on_event` 引用。rust-ref 用 `Arc<dyn Fn>`
      闭包捕获 `on_event`，Auto 无等价。需以下之一：(a) Auto 支持 `Arc<dyn Fn>` 闭包捕获
      （语言级），(b) 非 actor 的流式架构，(c) EventSink 能持额外上下文。非 15 行接线，
      属更深架构工作。**driver 的 StepStarted/Completed/Failed 等非流式事件已正常工作**，
      仅 Delta/Tool（流式内容）转发受阻。

### §6.7 EventSink cb 转发的外部设置机制 → ✅ 已落地（Plan 390 Phase B + Phase 6.5）

R1/R2/R3 修复后 `(self.cb)(ev)` 语法可用；Plan 390 Phase B（a2r spawn 带初始化参数）+
Phase 6.5（auto-ai EventSink 改 `cb = noop_event` + spawn 带参注入）已**完整解决 cb 注入**：
`Task.spawn("EventSink", 16, f"", cb)` 注入回调，EventSink `(self.cb)(ev.clone())` 转发。

> **✅ 已落地**：auto-lang Plan 390 Phase B（M1 spawn 带参，机制 A 自描述栈）+ Phase A（VM 侧）。
> 回 auto-ai 衔接（Phase 6.5）完成：rust/ 独立 crate + workspace 0 错，rust-ref 100 单测全绿。

**遗留（Phase 6.6，架构阻塞）**：driver 的 Delta/Tool → PipelineEvent 转发需 EventSink cb
捕获 `on_event`，但 Auto 闭包不能捕获外部变量，EventSink 无法持额外上下文 → 见 Phase 6.6
"架构阻塞"说明。需 Auto 支持 `Arc<dyn Fn>` 闭包捕获或非 actor 流式架构。

---

## 风险与注意

- **三个缺口全跨仓**：本计划的主体工作在 auto-lang（语言/a2r 扩展）。auto-ai 侧的消费验证
  依赖 auto-lang 合并 + 重建 auto.exe 后回 auto-ai 重跑 retranspile。
- **只构建 debug**（用户既定要求）：全程 `cargo build`，不跑 release。
- **Phase 1 探路结论已兑现**：actor 路径（缺口 1）在 auto-lang Plan 387 落地后实施完毕
  （Phase 1b）；缺口 1 不再依赖 dyn-Fn。剩余转发限制（EventSink 外发）记录于 KNOWN-DEBT。
- **转正（4.6）仍是后续**：本计划消弭功能缺口；转正还需 orchestration 功能对齐审计 +
  serde 同步（Phase 4）+ 翻转 [lib] path，是 plan 021 之后的独立计划。

## 完成判定

- [~] 缺口 1：agent 层流式事件能外发（complete_stream 或 channel 方案）
      **大部分完成（Phase 1b + 6.5）**：`Agent.run_stream(task, cancel, sink TaskRef<StreamEvent>)`
      已可向 EventSink actor 外发全部事件；EventSink 的 `cb fn(StreamEvent)` 已可经
      spawn 带参注入（Plan 390 Phase B），`(self.cb)(ev.clone())` 转发到注入的回调。
      **剩余**：driver 的 Delta/Tool → PipelineEvent 转发受阻于 Auto 闭包捕获限制
      （Phase 6.6，见上）——非流式事件（StepStarted/Completed/Failed 等）已正常工作。
- [x] 缺口 2：ToolRegistry 有泛型/装箱的 register 入口（或明确记录语言前置）
      **✅ 完成（Phase 2/E/F）**：`ToolRegistry.register(tool Tool)` + `Agent.register_tool(tool Tool)`
      已落地（auto-ai），spec-param 路径 + a2r call-site 自动装箱（auto-lang Plan 390 Phase E）。
      `r.register(MyTool{})` → `r.register(Box::new(MyTool{}))` 自动包装，无需 `Arc::new(Box::new(...))`。
      泛型语法路线否决（spec-param 已够）。转正后 CLI `register_tool(ReadFile)` 可直接用。
- [ ] 缺口 3：转译版的 role_config/loader 与 rust-ref 的 serde 行为对齐
- [ ] 三个转译 crate 仍 0 错；rust-ref 主版本测试全绿
- [ ] 打 tag `auto-complete-v0.1`（或 auto-mvp-v0.3）标记功能覆盖达成
