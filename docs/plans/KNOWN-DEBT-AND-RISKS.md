# Known Debt and Risks

归档复审中发现的 workaround、一致性遗漏、已知限制与未来增强。按严重度分级。

---

## 🟢 已知限制（设计决策，非 bug）

| Plan | 类别 | 描述 | 参考 |
|---|---|---|---|
| 019 | 已修复 | ~~`load_builtin` 返回 `Option<Box<dyn Role>>`，rust-ref 用 `Option<Arc<dyn Role>>`。转译版 `resolve_role` 因此返回 `Option<Arc<Box<dyn Role>>>`（双层包装）~~ — **已修复（2026-08-06，auto-lang §15.11-followup 跨模块 spec 单层化）**：a2r 现在对跨模块 import 的 spec（`use role_def: Role`）也渲染单层 `Arc<dyn Role>`。`resolve_role` 现返回 `Option<Arc<dyn Role>>`（与 rust-ref 对齐）；`roles.at` 值侧 `Arc(Box(role))` → `Arc(role)`（用户角色路径），`Arc(b)` → `Arc::from(b)`（builtin 路径，Box→Arc 单层转换）。`load_builtin` 仍返回 `Box<dyn Role>`（其声明在 builtin_roles.at，是返回类型本身，非容器包裹——保持 Box 合理）。retranspile + 独立 crate build + 全测试绿。 | `crates/auto-ai-agent/rust/src/roles.rs`（resolve_role 单层）|
| 021 | 已修复 | ~~EventSink actor 的 handler 只能 is-解构单个变体字段，无法把整个 `StreamEvent` 转发给 app~~ — auto-lang Plan 389（R1/R2/R3）已修复：(a) on-block TypeBinding 绑定入 name scope（`forward(ev)` / `(self.cb)(ev)` 可用）；(b) fn 指针可作 task state 字段（`cb: fn(...)` 类型推导）；(c) f-string 自引用 task state 字段可解析。**剩余**：app 注入 cb 到 actor 的机制（spawn 无参 + 无状态写入消息，见 plan 021 §6.7）→ 📌 已立项 auto-lang **Plan 390**（`actor-state-injection`，draft），推荐机制 M1（spawn 带初始化参数）。 | `crates/auto-ai-agent/src/agent.at`（EventSink task） |
| 019 | 已知限制 | Phase 2 的 4 类 sed workaround（clone 补全/ReadDir 去引用/path 借用/去多余 as_str）是 a2r 借用推理缺陷的临时绕过。每条 sed 精确锚定模式并注释标注缺陷类别；a2r 根因修复后自动变成 no-op。属 Plan 013/016 既定的 workaround 模式（同 SOUL const 修复）。 | `crates/auto-ai-agent/retranspile.sh` Plan 019 Phase 2 段 |
| 021 | 已修复 | ~~a2r 对 `Arc(x)`/`Box(x)` 构造函数在**实参位置**（`fn(Arc(x))` / `self.m(Arc(x))`）渲染为 `Arc(x)` verbatim（应为 `Arc::new(x)`）~~ — auto-lang **Plan 390 §14 L1 根因已修（1317d91c）**：parser `node_or_call_expr` 补 Arc/Box + `(` 识别 → `Expr::ArcExpr` → `Arc::new(x)`，实参位与 let 位一致。**已回收（cfa1515）**：`tool.at` 已去 `let a = Arc(tool)` workaround，直写 `self.tools.set(n, Arc(tool))`。 | `crates/auto-ai-agent/src/tool.at`（register，let-bound Arc workaround）|
| 021 | 已修复 | ~~Auto/a2r **缺少闭包类型**（`Box<dyn Fn>`/`impl Fn`/`Arc<dyn Fn>`）。EventSink cb 是裸 `fn(StreamEvent)` 指针，能持不捕获的 fn（noop_event），但不能持捕获了 `on_event` 的闭包。阻塞 driver Delta/Tool 转发（Plan 021 Phase 6.6）~~ — **类型机制已交付（Plan 390 §15.10）+ auto-ai 侧已落地（Plan 021 Phase 6.6，2026-08-06）**：`spec Fn` + `Box<Fn>` → `Box<dyn Fn + Send + Sync>`（含带签名 `Box<Fn(A)>`），闭包字段默认值自动 `Box::new(move |..|)`。EventSink cb 改 `cb = fn(e StreamEvent) { }` 闭包字段；driver drive_step 用 `Box(move (e) => forward_stream_event(e, on_event_local))` 注入捕获 `on_event` 的闭包，`run_stream` 流式转发 Delta/Tool → PipelineEvent 全部接通。rust-ref 用 `Arc<dyn Fn>`，Auto 现表达 `Box<dyn Fn>`（语义等价：box 而非 arc——单 sink 不需共享所有权）。端到端 smoke PASS（闭包捕获 `Arc<Mutex<Vec>>` 转发 Delta "hello world"）。driver 流式与非流式事件均工作。 | `crates/auto-ai-agent/src/agent.at`（EventSink task cb 字段）、`crates/auto-ai-agent/src/orchestration/driver.at:303`（drive_step run_stream 流式）|
| 021 | 已修复 | ~~a2r **跨模块 import 的 spec 不享 §15.11 单层化**。`Arc<Tool>` 在 `tool.at`（`pub spec Tool`，同模块）渲染单层 `Arc<dyn Tool>`，但在 `agent.at`（`use tool: Tool`，跨模块 import）仍渲染双层 `Arc<Box<dyn Tool>>`~~ — **已修复（2026-08-06，auto-lang §15.11-followup）**：a2r 的 `Box<Spec>`/`Arc<Spec>` 单层化现覆盖**跨模块 import 的 spec**（`use mod: Spec` 的 spec 名在 `spec_decls` 中时，`Type::User` 也走单层 `Arc<dyn T>` 分支，不再仅 `Type::Spec`）。`agent.rs` `register_shared` 现原生渲染 `Arc<dyn Tool>`；retranspile.sh 的 §15.11 sed 已删除（变 no-op 后清理）。同根因的 `Role` 双层也一并解除（见上 019 行）。验证：三转译 crate 独立 build 0 错（无 sed）、workspace 全绿、端到端 smoke（`Arc::new(PingTool)` 单层直注）PASS。 | `crates/auto-ai-agent/retranspile.sh`（§15.11 sed 已删）、`crates/auto-ai-agent/rust/src/agent.rs:288`（register_shared 原生单层）|

## 📋 未来增强

| Plan | 类别 | 描述 | 参考 |
|---|---|---|---|
| 019 | 未来增强 | a2r 借用推理的 4 类根因（B: 借用**循环变量**字段传 owned 参数未 clone——注 `self.field` 已正确 clone，仅 loopvar 字段未覆盖；C: for-in 对 ReadDir 无条件加 &；D: 函数参数 move 后重用未 clone；E: 对 &str 多余插 .as_str()）应到 auto-lang 根因修复，届时删除 retranspile.sh 的对应 sed 规则。每类是独立的 codegen 借用分析缺陷，需逐个设计修复（非单一根因）。**由用户在 auto-lang 单独推进。** | `crates/auto-ai-agent/retranspile.sh` Plan 019 Phase 2 段 |
| 021 | 已完成 | ~~a2r **跨模块 import 的 spec 单层化**（§15.11 扩展）~~ — **已修复（2026-08-06，auto-lang §15.11-followup）**：a2r 现对 `use mod: Spec` import 的 spec 也单层化。retranspile.sh 的 §15.11 sed 已删除（见上 021 已修复行）。 | — |
| 020 | 已完成 | ~~ai-config/rust + auto-ai-client/rust 转译版错误清零~~ — Plan 020 已通过 retranspile.sh sed 绕过清零（ai-config 1→0、client 8→0）。sed workaround 同 019 模式，a2r 根因修复后自动 no-op。 | `crates/ai-config/retranspile.sh`、`crates/auto-ai-client/retranspile.sh` |
