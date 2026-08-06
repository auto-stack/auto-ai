# Known Debt and Risks

归档复审中发现的 workaround、一致性遗漏、已知限制与未来增强。按严重度分级。

---

## 🟢 已知限制（设计决策，非 bug）

| Plan | 类别 | 描述 | 参考 |
|---|---|---|---|
| 019 | 已知限制 | `load_builtin` 返回 `Option<Box<dyn Role>>`，rust-ref 用 `Option<Arc<dyn Role>>`。转译版 `resolve_role` 因此返回 `Option<Arc<Box<dyn Role>>>`（双层包装）。编译/功能可用（Deref 链），但精度有损。Box/Arc 差异留待转正对齐（需 a2r 对 spec 在返回位置生成 `Arc<dyn>` 而非 `Box<dyn>`）。 | `crates/auto-ai-agent/rust/src/builtin_roles.rs:23` |
| 021 | 已修复 | ~~EventSink actor 的 handler 只能 is-解构单个变体字段，无法把整个 `StreamEvent` 转发给 app~~ — auto-lang Plan 389（R1/R2/R3）已修复：(a) on-block TypeBinding 绑定入 name scope（`forward(ev)` / `(self.cb)(ev)` 可用）；(b) fn 指针可作 task state 字段（`cb: fn(...)` 类型推导）；(c) f-string 自引用 task state 字段可解析。**剩余**：app 注入 cb 到 actor 的机制（spawn 无参 + 无状态写入消息，见 plan 021 §6.7）→ 📌 已立项 auto-lang **Plan 390**（`actor-state-injection`，draft），推荐机制 M1（spawn 带初始化参数）。 | `crates/auto-ai-agent/src/agent.at`（EventSink task） |
| 019 | 已知限制 | Phase 2 的 4 类 sed workaround（clone 补全/ReadDir 去引用/path 借用/去多余 as_str）是 a2r 借用推理缺陷的临时绕过。每条 sed 精确锚定模式并注释标注缺陷类别；a2r 根因修复后自动变成 no-op。属 Plan 013/016 既定的 workaround 模式（同 SOUL const 修复）。 | `crates/auto-ai-agent/retranspile.sh` Plan 019 Phase 2 段 |
| 021 | 已知限制 | a2r 对 `Arc(x)`/`Box(x)` 构造函数在**实参位置**（`fn(Arc(x))` / `self.m(Arc(x))`）渲染为 `Arc(x)` verbatim（应为 `Arc::new(x)`），仅 let 位（`let a = Arc(x)`）正确。缺口 2 Phase F 的 `register(tool Tool)` 体里用 `let a = Arc(tool)` 先绑定再 `.set(name, a)` 绕过（`tool.at:84` 注释标注）。根因：实参位 `Arc(...)` 未识别为 `Expr::ArcExpr`（rust.rs:1864 仅 let/直接表达式位识别）。a2r 根因修复后可去掉 let 绑定，`self.tools.set(n, Arc(tool))` 直写。**由用户在 auto-lang 单独推进**（独立于 Plan 390 Phase E）。 | `crates/auto-ai-agent/src/tool.at`（register，let-bound Arc workaround）|
| 021 | 已知限制 | Auto/a2r **缺少闭包类型**（`Box<dyn Fn>`/`impl Fn`/`Arc<dyn Fn>`）。EventSink cb 是裸 `fn(StreamEvent)` 指针，能持不捕获的 fn（noop_event），但不能持捕获了 `on_event` 的闭包。阻塞 driver Delta/Tool 转发（Plan 021 Phase 6.6）。`fn(params){}` 参数绑定已修（Plan 390 Phase H），但闭包→fn 指针的类型不匹配仍阻。rust-ref 用 `Arc<dyn Fn>`，Auto 无法表达。driver 非流式事件已正常工作，仅流式 Delta/Tool 受阻。需 Auto 增加闭包类型表达。 | `crates/auto-ai-agent/src/orchestration/driver.at:303`（drive_step，run 非流式）|

## 📋 未来增强

| Plan | 类别 | 描述 | 参考 |
|---|---|---|---|
| 019 | 未来增强 | a2r 借用推理的 4 类根因（B: 借用循环变量字段传 owned 参数未 clone；C: for-in 对 ReadDir 无条件加 &；D: 函数参数 move 后重用未 clone；E: 对 &str 多余插 .as_str()）应到 auto-lang 根因修复，届时删除 retranspile.sh 的对应 sed 规则。**由用户在 auto-lang 单独推进。** | `crates/auto-ai-agent/retranspile.sh` |
| 020 | 已完成 | ~~ai-config/rust + auto-ai-client/rust 转译版错误清零~~ — Plan 020 已通过 retranspile.sh sed 绕过清零（ai-config 1→0、client 8→0）。sed workaround 同 019 模式，a2r 根因修复后自动 no-op。 | `crates/ai-config/retranspile.sh`、`crates/auto-ai-client/retranspile.sh` |
