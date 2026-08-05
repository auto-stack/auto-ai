# Plan 018: a2r codegen 漂移修复（第四波 4.5/4.6 硬前置之一）

> **状态**：🟢 已完成（2026-08-04）
> **仓库**：**auto-lang（主，bug 修复）** + auto-ai（验证消费方）
> **来源**：Plan 016 第四波调研结论——4.3/4.5/4.6 大多卡在 auto-lang 跨仓前置；本 plan 聚焦
> **唯一可独立执行、ROI 最高**的工作：修 a2r 转译器的系统性 codegen bug，消除 auto-ai-agent
> 转译版的 E0404/E0252 编译错误。
> **对应 016 路线图**：4.5（第二轮 Auto 化）/ 4.6（转正）的硬前置 H4。不是 4.5/4.6 本身，
> 是它们的前置条件之一（转正还需 H1 serde 同步、H2 role_config 迁移、H5 orchestration 审计）。
> **编号说明**：016 路线图 4.6 曾把"转正"占位为 plan 018。但转正前置远未满足。本 plan 先占用 018
> 做 codegen 修复；转正计划顺延为 plan 019+（执行时同步修订 016 路线图 4.6 行）。

---

## 背景（2026-08-04 调研结论）

auto-ai-agent 转译版 `crates/auto-ai-agent/rust/` 有 **63 个编译错误**，分三个独立 a2r codegen 根因：

| 根因 | 错误码 | 数量 | 位置 | 触发 |
|---|---|---|---|---|
| **Bug A**：返回类型启发式把导入的具体类型当 trait，加 `impl` 前缀 | E0404 | ~53 | `rust.rs` `rust_return_type_name` 的 `Type::User` 分支 | `~Result<CompletionResponse, ClientError>` → `Result<impl CompletionResponse, impl ClientError>`；导入的 enum/struct/alias（ModelTier/AgentError/PathBuf/JsonValue/PipelineEngine…）被误判 |
| **Bug B**：同函数，`None`（Auto 单元类型）首字母大写被判 trait | E0404 | 6 | 同上 | `Result<None, AgentError>` → `Result<impl None, impl AgentError>` |
| **Bug C**：后处理去重 `contains` 匹配不到 brace 形式，重复注入 import | E0252 | 4 | `rust.rs` `fix_missing_trait_impl_uses` | 生成 `use crate::error::{AgentError};`（brace）后，注入器 `contains("use crate::error::AgentError;")`（无 brace）判定未导入 → 再注入一份 |

**前次修复未完成**：auto-lang commit `cf0b2e25`（同日）只给 `Type::User` 加了 `local_struct_types`
检查（仅识别本文件声明的类型），漏了导入类型 + `Type::GenericInstance` 分支。

---

## 修复设计（auto-lang `crates/auto-lang/src/trans/rust.rs`）

### Bug A/B：抽 `is_imported_concrete_type` helper + 复用 `self.uses` 模糊匹配

1. **新 helper**（复用 line 6152 既有模糊匹配逻辑）：判断名字是否在 `self.uses` 里
   （匹配裸名 / `::name` / `{name}` / `name,` / `, name` 形式）。
2. **`Type::User` 分支**：`local_struct_types || is_imported_concrete_type || name == "None"`
   → 走 `rust_type_name`（不加 impl）。
3. **`Type::GenericInstance` 分支**：同步加 `local_struct_types + is_imported_concrete_type` 守卫。

### 补充修复：容器泛型参数位置不加 impl

`GenericInstance`（Result/Option 等容器）的 args 递归从 `rust_return_type_name` 改为
`rust_type_name`——因为 Rust 语法禁止 `impl Trait` 嵌套在泛型参数里（`Result<impl T, E>` 非法）。
保留 str→String 映射，但不加 impl。修复了 glob 导入的具体类型（如 `Node` 经 `use.rust auto_atom::*`）
在 `Result<Node, E>` 位置被误判（glob 导入的名字不在 `self.uses`，但容器参数位置本就不该有 impl）。

### Bug C：`fix_missing_trait_impl_uses` 去重兼容 brace 形式

从 `use_stmt` 字符串 derive 出 brace 形式（`use crate::error::AgentError;` → `use crate::error::{AgentError};`），
`already_via_rust` 同时检查裸形式和 brace 形式。

### 必须保留的约束

- **Golden `015_impl_trait_return`**：`fn health() ~IntoResponse` → `impl IntoResponse`。
  `IntoResponse` 无 use 导入（裸引用），不在 `self.uses`，模糊匹配不命中，impl 前缀保留。

---

## 实施记录

- [x] 1. 建 auto-lang worktree（`fix/a2r-impl-type-e0404`）+ 复现基线（a2r 测试需 `--features test-trans`）
- [x] 2. 抽 `is_imported_concrete_type` helper（复用 line 6152 模糊匹配）
- [x] 3. 修 Bug A/B：`Type::User` + `Type::GenericInstance` 两分支 + `None` 白名单
- [x] 4. 修 Bug C：`fix_missing_trait_impl_uses` derive brace 形式去重
- [x] 5. 补充修复：容器泛型参数 args 递归改用 `rust_type_name`（`Result<Node,E>` 不再变 `impl Node`）
- [x] 6. 加 golden 回归测试：`022_imported_concrete_return`、`023_unit_none_return`
- [x] 7. auto-lang 全量 `cargo test`：**301 passed; 0 failed**（299 原有 + 2 新增，015 不回归）
- [x] 8. 合并到 auto-lang master（`a8b99df3` + `47d1d676`）+ 重建 auto.exe；清理 worktree

## 验证记录（auto-ai 消费方）

- [x] 9. 回 auto-ai 重跑三个 crate 的 `retranspile.sh`
- [x] 10. **auto-ai-agent/rust/ 错误 63→26**：E0404/E0252 全部清零；剩余 26 个是借用迭代器（E0308/E0507）
      类既有缺陷（016 A.3 节记录的 `for x in expr` 系统性 gap），非本 plan 范围
- [x] 11. ai-config/rust/：0→2（`impl Node` 已修；剩余 2 个是 `for m in &models` 借用迭代器 E0308，
      cf0b2e25 后 auto.exe 的既有行为，非本 plan 引入）；auto-ai-client/rust/：9（E0308/E0507 既有）
- [x] 12. 主版本无影响：`cargo check --workspace` 0 错误；`cargo test -p auto-ai-agent`
      100 单测 + 5 mvp_harness 全绿（rust-ref 不经 a2r）

---

## 完成判定

- [x] auto-lang：`cargo test` 301 passed 0 failed，`015_impl_trait_return` 不回归
- [x] auto-lang：commit `a8b99df3`（主修复）+ `47d1d676`（补充），auto.exe 重建
- [x] auto-ai：auto-ai-agent/rust/ **E0404 + E0252 全部清零**（63→26，剩余是借用迭代器类）
- [x] auto-ai：主版本 `cargo check` + `cargo test` 全绿

## 后续（非本 plan）

转正（plan 019+）剩余硬前置（按依赖序）：
1. **借用迭代器 a2r 修复**（016 A.3，`for x in expr` → `for x in &expr`）：消除剩余 26+2+9 个
   E0308/E0507 错误的主力，auto-lang 主导。
2. **Plan 381 serde 迁移同步到 .at/转译器**（H1/H2）：`auto-val/serde` feature 在转译 Cargo.toml
   开启 + role_config.rs/loader.rs 的 `#[derive(Deserialize)]` 写回 .at。
3. **orchestration 层功能对齐审计**（H5）：driver/flow/handoff/pipeline 的 rust/ 版 vs rust-ref 等价性。
4. 全部满足后才能安全翻转 `[lib] path` 并删 rust-ref。
