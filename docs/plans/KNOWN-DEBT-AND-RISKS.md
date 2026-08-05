# Known Debt and Risks

归档复审中发现的 workaround、一致性遗漏、已知限制与未来增强。按严重度分级。

---

## 🟢 已知限制（设计决策，非 bug）

| Plan | 类别 | 描述 | 参考 |
|---|---|---|---|
| 019 | 已知限制 | `load_builtin` 返回 `Option<Box<dyn Role>>`，rust-ref 用 `Option<Arc<dyn Role>>`。转译版 `resolve_role` 因此返回 `Option<Arc<Box<dyn Role>>>`（双层包装）。编译/功能可用（Deref 链），但精度有损。Box/Arc 差异留待转正对齐（需 a2r 对 spec 在返回位置生成 `Arc<dyn>` 而非 `Box<dyn>`）。 | `crates/auto-ai-agent/rust/src/builtin_roles.rs:23` |
| 019 | 已知限制 | Phase 2 的 4 类 sed workaround（clone 补全/ReadDir 去引用/path 借用/去多余 as_str）是 a2r 借用推理缺陷的临时绕过。每条 sed 精确锚定模式并注释标注缺陷类别；a2r 根因修复后自动变成 no-op。属 Plan 013/016 既定的 workaround 模式（同 SOUL const 修复）。 | `crates/auto-ai-agent/retranspile.sh` Plan 019 Phase 2 段 |

## 📋 未来增强

| Plan | 类别 | 描述 | 参考 |
|---|---|---|---|
| 019 | 未来增强 | a2r 借用推理的 4 类根因（B: 借用循环变量字段传 owned 参数未 clone；C: for-in 对 ReadDir 无条件加 &；D: 函数参数 move 后重用未 clone；E: 对 &str 多余插 .as_str()）应到 auto-lang 根因修复，届时删除 retranspile.sh 的对应 sed 规则。 | `crates/auto-ai-agent/retranspile.sh` |
| 019 | 未来增强 | ai-config/rust（2 错：`for m in &models` 借用迭代器）+ auto-ai-client/rust（9 错：str_as_str/borrow/mismatched）仍有既有 a2r 错误，非本 plan 范围。转正（plan 019+ 的后续）需一并清零。 | `crates/ai-config/rust/src/tier.rs:154` 等 |
