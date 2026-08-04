# Plan 015: auto-ai 三 crate 的 Auto 版源码迁入（从 auto-lang 迁回）

> **状态**：✅ 已实施（2026-08-03：迁移内容落地 + 双版本编译通过 + 文档化）
> **仓库**：auto-ai（本计划）
> **来源**：auto-lang 仓库 `crates/{ai-config,auto-ai-agent,auto-ai-client}/` 的 Auto 移植版
> （auto-lang commit `91443c10` 删除了这些目录，本计划把它们迁回 auto-ai 作为权威源）
> **前置**：auto-ai plan 013（Auto 移植 G1-G4 + ReAct 端到端跑通）、auto-lang plan 373/376
> （a2r codegen 修复到 re-transpile 0 错误）、auto-ai SAFETY.md（跨仓库文件操作安全规范）
> **目标**：把 auto-lang 中的 3 个 AI crate 的 **Auto 版源码（`.at`）** 迁入 auto-ai 仓库，
> 与既有的**手写 Rust 参考实现**共存，确立"双版本"长期架构。

## 背景：为什么要迁回

plan 013 把 auto-ai 的 3 个 Rust crate（ai-config、auto-ai-agent、auto-ai-client）用 Auto 语言复刻，
产物当时落地在 `auto-lang/crates/`（因为 a2r 生成器在 auto-lang，开发期间就近存放）。
但 plan 013 本质上是 **auto-ai 项目**的目标——auto-lang 不应长期托管这些"auto-ai 的 Auto 移植版"。

auto-lang 已在 commit `91443c10`（message："refactor(plan-015): 删除 3 个 AI crate —— Auto 版源码已迁回
auto-ai"）删除了这 3 个 crate 目录。本计划就是 auto-ai 侧对应的接收动作：**把被删除的 Auto 源码
迁回 auto-ai**，与既有的手写 Rust 共存。

## 双版本架构（核心设计）

迁移后每个 crate 有**两个 Rust 源树**，定位明确：

| 源树 | 角色 | 编译入口 | 当前地位 |
|---|---|---|---|
| `rust-ref/src/*.rs` | **手写 Rust 参考实现** | 主 workspace 的 `crates/<crate>/Cargo.toml` → `[lib] path = "rust-ref/src/lib.rs"` | **当前主要版本**（生产运行主线） |
| `src/*.at` + `rust/src/*.rs` | **Auto 源 + a2r 转译产物** | `rust/Cargo.toml`（独立 `[workspace]`，包名 `-a2r` 后缀） | **跟随版本**（功能对齐后将转正） |

**版本控制策略**：
- `rust-ref/`（手写 Rust）、`src/*.at`（Auto 源）、`rust/`（含 Cargo.toml、retranspile.sh、转译产物 `.rs`）
  ——三者全部纳入 git。
- 仅 `*.a2r.rs`（a2r 转译中间产物）gitignore，因为它们由 retranspile.sh 随时重新生成。
- 设计意图：**当前 Rust 版是主要版本与参考实现，Auto 版作为跟随**；未来等 Auto 版能够完全复刻 Rust
  版的功能之后，再作为主要版本（届时切换主 workspace 的 `[lib] path` 并删除 rust-ref）。

**主 workspace 不变**：仍是 6 个成员（`Cargo.toml` 未改），编译 `rust-ref`（手写 Rust）。
`rust/` 目录通过自身 `Cargo.toml` 里的 `[workspace]` 表自我排除，不会被主 workspace 吸收。
当前 `auto-ai-cli`/`auto-ai-daemon`/`aictl` 仍是纯 Rust crate，**不在本次迁移范围**。

## 现状盘点（迁移内容已全部在工作区，待整理提交）

经核查，迁移所需内容**已 100% 就位**（与 auto-lang `91443c10~1` 删除前逐字节比对一致）：

| 项目 | ai-config | auto-ai-agent | auto-ai-client |
|---|---|---|---|
| `src/*.at`（Auto 源） | 6 个 ✅一致 | 36 个 ✅一致 | 3 个 ✅一致 |
| `rust-ref/` 重命名 | 6 个 src + 重命名 staged | 35 src + 12 souls + 1 test，全部 staged | 3 个 staged |
| `rust/Cargo.toml` | ✅ 独立 `[workspace]`，路径已修正(4-up) | ✅ 同左 + `[[bin]] auto-ai-react` | ✅ 同左 |
| `rust/src/*.rs`（转译产物） | ❌ 空（未转译） | ✅ 38 个，`cargo check` 0 错误 | ❌ 空（未转译） |
| `retranspile.sh` | ✅ | ✅ | ✅ |
| `Cargo.toml` 的 `path` 修正 | `src/lib.rs`→`rust-ref/src/lib.rs` | 同左 | 同左 |

## 实施步骤

### 阶段 A：修正版本控制配置

**A1. 更新根 `.gitignore`**：新增 `*.a2r.rs`（a2r 转译中间产物，参考 auto-lang 行 90）。
不忽略 `crates/*/rust/`（按双版本策略，转译产物纳入 git）和 `crates/*/src/*.at`（Auto 源）。

**A2. 暂存 3 个 `Cargo.toml` 的 `path` 修正**：
`[lib] path = "src/lib.rs"` → `path = "rust-ref/src/lib.rs"`（ai-config / auto-ai-agent / auto-ai-client）。

### 阶段 B：验证双版本可编译（提交前的安全闸）

**B1. 主 workspace（rust-ref）cargo check** —— 0 错误 ✅（仅 unused 警告）
**B2. auto-ai-agent/rust/（转译版）cargo check** —— 0 错误 ✅（55 警告均为 dead_code/unused）

> ai-config/rust/ 和 auto-ai-client/rust/ 的 src/ 目前为空，**本次不转译**——计划范围限定为落地现状，
> 这两个 crate 的转译作为本计划的后续工作（见下"剩余工作"）。

### 阶段 C：撰写本文档

### 阶段 D：分原子提交

**D1.** `refactor(plan-015): 迁入 3 crate 的 Auto 源 + rust-ref 重命名`
—— 45 个 `src/*.at` + 57 个 `rust-ref/` 重命名 + `retranspile.sh` + `rust/Cargo.toml`
+ auto-ai-agent 的 38 个 `rust/src/*.rs` 转译产物。

**D2.** `build(plan-015): Cargo.toml lib path → rust-ref + .gitignore a2r 中间产物`
—— 3 个 `Cargo.toml` 的 `[lib] path` 修正 + `.gitignore` 新增 `*.a2r.rs`。
（构建配置必须与源码迁移同步提交，否则主 workspace 编译失败。）

**D3.** `docs(plan-015): 迁移计划文档 + 剩余工作记录` —— 本文件。

## 跨仓库路径依赖（安全约束）

`rust/Cargo.toml` 声明了到 auto-lang 的 path 依赖：
```toml
a2r-std   = { path = "../../../../auto-lang/crates/a2r-std" }
auto-atom = { path = "../../../../auto-lang/crates/auto-atom" }
auto-val  = { path = "../../../../auto-lang/crates/auto-val" }
```
路径深度已修正（4-up 到 `autostack/`，对比 auto-lang 原版的 3-up bug）。这些是跨仓库 junction 依赖，
受 `SAFETY.md`（"跨仓库文件操作安全规范"）约束——任何对 `D:/autostack/` 下含 junction 目录的删除/镜像
操作必须先跑 `scripts/check-junctions.sh` 预检。本计划**不涉及任何跨仓库删除/镜像操作**，纯本地 git 提交。

> 2026-08-02 曾因 `robocopy /MIR` 跟随循环 junction 导致两个仓库的 `.git` 和 auto-ai worktree
> 被毁（详见 SAFETY.md §7 事故记录），故此处特别警示。

## 剩余工作（plan 015 后续，不在本次执行范围）

1. **ai-config 的 a2r 转译**：`crates/ai-config/` 下跑 `./retranspile.sh check`，目标 0 错误，
   填充 `rust/src/*.rs`（当前为空）。
2. **auto-ai-client 的 a2r 转译**：同上。
3. **功能对齐评估**：3 个 crate 的 Auto 版（rust/）逐个对照 rust-ref，记录功能缺口（参考 plan 013
   的 G1-G6 验证方法）。
4. **转正决策**：当 Auto 版功能完全对齐后，把主 workspace 的 `[lib] path` 从 rust-ref 切到 rust，
   删除 rust-ref（届时另起 plan-016）。

## 验证清单（计划完成的判定标准）

- [x] `cargo check`（根，rust-ref）0 错误
- [x] `cargo check`（auto-ai-agent/rust/，转译版）0 错误
- [x] 3 个 commit 创建成功，各自通过 cargo check
- [x] `docs/plans/015-auto-lang-migration.md` 存在且内容完整
- [x] `.gitignore` 包含 `*.a2r.rs`
- [x] `git status` 干净（无残留 untracked 的迁移文件）
