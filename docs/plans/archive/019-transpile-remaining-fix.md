# Plan 019: 转译版剩余 25 错修复（auto-ai-agent Auto 版可编译）

> **状态**：🟢 已完成（2026-08-05）
> **仓库**：**auto-lang（全部修复，worktree）** + auto-ai（验证消费方）
> **来源**：Plan 016 §5 盘点核查 + Plan 018 收尾。Plan 018 清零了 E0404/E0252（impl 类型误判），
> 转译版从 63→26 错。本 plan 修剩余 25 个错误的 5 类 a2r 根因，目标：**转译版 cargo check 0 错误**。
> **前置**：Plan 018 已合并（auto-lang `a8b99df3` + `47d1d676`）。

---

## 背景：25 错的 5 类根因（全部 a2r bug，无 .at 源问题）

经核查，5 类问题的 .at 源都是规范的，无需改动；全部归 auto-lang 的 a2r 转译器 / a2r_std 运行时：

| 类 | 错误数 | 现象 | 根因（auto-lang） | 修复位置 |
|---|---|---|---|---|
| **A** | 15 | `load_builtin` 返回 `Option<impl Role>`（13 arm + roles.rs 2 级联） | `rust_return_type_name` 的 `Option`/`Result` inner 递归仍用自身，spec 名走 `Type::User`→`spec_decls`→`impl`；容器 inner 应走 `rust_type_name`→`Box<dyn>` | `trans/rust.rs` `rust_return_type_name` |
| **B** | 5 | `extract_path(tc.args)` move（driver.rs 4 + agent.rs 1） | a2r 借用循环变量（`for tc in &x`）后，调用点传 owned 字段未补 clone | `trans/rust.rs` 调用点 clone 推理 / sed 绕过 |
| **C** | 2 | `for entry in &entries`（ReadDir，skill.rs + roles.rs） | `for_stmt`（line 9035-9041）对 Ident/Dot 迭代对象无条件加 `&`；ReadDir 只实现 by-value IntoIterator | `trans/rust.rs` `for_stmt` / sed 绕过 |
| **D** | 2 | `read_to_string(path)` 传 PathBuf 但签名要 `&str` | a2r_std 签名 `&str` 比 std 的 `AsRef<Path>` 窄 | `a2r-std/src/fs.rs` |
| **E** | 1 | `after_open.as_str()` 触发 unstable str_as_str | a2r 对已是 `&str` 的 `Some(x)` 绑定多余插入 `.as_str()` | `trans/rust.rs` as_str 自动插入 / sed 绕过 |

### 关键证据

- **A**：`.at` 源 `load_builtin(name str) ?Role`（builtin_roles/mod.at:56），`Role` 是 spec（role_def.at:27）。
  rust-ref 用 `Option<Arc<dyn Role>>`（builtin_roles/mod.rs:50）。a2r 生成 `Option<impl Role>`（非法）。
- **B**：`.at` 源 `extract_path(tc.args)`（driver.at:351），a2r 借用循环 `for tc in &result.tool_calls`
  但调用点未补 clone。rust-ref 用 `tc.args.get("path")` 借用。
- **C**：`.at` 源 `for entry in entries`（skill.at:86），a2r 生成 `for entry in &entries`（ReadDir 不可 &）。
  rust-ref 用 `entries.flatten()`。
- **D**：`.at` 源 `fs.read_to_string(path)` 传 PathBuf（skill.at:202），a2r_std 签名 `&str`（fs.rs:19）。
- **E**：`.at` 源 `parse_frontmatter_body(after_open)`（skill.at:243），a2r 多插 `.as_str()`。.at 源注释
  明确记录了此转译器 bug（skill.at:232-235）。

---

## 实施策略

全部在 auto-lang worktree 实施。按风险分两阶段：

### Phase 1 — 低风险根因修复（A+D，解 17 错）

**A：Option/Result inner 改用 rust_type_name**（和 Plan 018 GenericInstance args 同类）
- `rust_return_type_name` 的 `Type::Option(inner)` 和 `Type::Result(inner)` 分支：inner 递归
  从 `rust_return_type_name` 改为 `rust_type_name`。
- 原理：`impl Trait` 不能嵌套在容器内（Rust 语法禁止 `Option<impl T>`）。`rust_type_name` 对
  StrSlice 输出 String（保留 str→String），对 spec 名输出 `Box<dyn X>`。
- 预期：`Option<Role>` → `Option<Box<dyn Role>>`；`Option<str>` → `Option<String>`（不变）。

**D：a2r_std read_to_string 改 AsRef<Path>**
- `a2r-std/src/fs.rs:19`：`read_to_string(path: &str)` → `(path: impl AsRef<Path>)`，内部 `path.as_ref()`。
- `read_text`（line 26）同步改。

### Phase 2 — 借用推理类（B+C+E，解 8 错）

触及 a2r 类型追踪/借用推理，风险较高。**先尝试 a2r 根因，风险过高则用 retranspile.sh sed 绕过**
（Plan 013/016 既定模式）：
- **B**：a2r 调用点 auto-clone 机制 / 或 sed `extract_path(tc.args.clone())`。
- **C**：`for_stmt` ReadDir 类型识别 / 或 sed 去 `&`（ReadDir 窄场景）。
- **E**：as_str 插入跳过 `&str` 接收者 / 或 sed 删特定 `.as_str()`。

---

## 实施步骤（auto-lang worktree）

- [x] 1. 建 worktree + 复现 a2r 测试基线（301 passed）
- [x] 2. Phase 1A：`rust_return_type_name` Option/Result inner 改 `rust_type_name`
      （probe 验证 `?Greeter` → `Option<Box<dyn Greeter>>`）
- [x] 3. Phase 1D：a2r_std `read_to_string`/`read_text` 改 `impl AsRef<Path>`
- [x] 4. Phase 1 `cargo test` 全绿（301 passed，015/022/023 不回归）；效果 **26→11**（消除 A 类 15 错）
- [x] 5. Phase 2B/C/D连带/E：全部用 retranspile.sh sed 绕过（a2r 借用推理类根因风险高、
      错误数少=10；每条 sed 精确锚定特定模式 + 注释标注 a2r 缺陷类别）
- [x] 6. 合并 auto-lang master（`930ffcee`，debug build）+ 清理 worktree

## 验证步骤（auto-ai 消费方）

- [x] 7. 回 auto-ai 重跑 auto-ai-agent retranspile
- [x] 8. **auto-ai-agent/rust/ cargo check 错误 26→0** ✅
- [x] 9. 主版本无影响：`cargo check --workspace` 0 错误；`cargo test -p auto-ai-agent`
      100 单测 + 5 mvp_harness 全绿
- [x] 10. ai-config（2）/auto-ai-client（9）转译版无新增回归（既有借用迭代器错误，非本 plan 范围）
- [x] 11. Phase 2 sed 规则在 retranspile.sh 注释标注（B/C/D/E 四类 a2r 缺陷）

---

## 风险与注意

- **A 的 Box vs Arc**：rust-ref 用 `Arc<dyn Role>`，a2r 输出 `Box<dyn>`。`Box<dyn Role>` 编译合法且
  下游 roles.rs 期望 Box。Box/Arc 差异留待转正对齐。
- **C 的 for-in & 规则**：若改 for_stmt 根因，不能误伤 Vec/HashMap 的 &（需 & 避免 move）。
- **015 golden**：A 的 Option/Result 改动不影响 `impl IntoResponse`（顶层 async 返回，不经 Option/Result）。
- **只构建 debug**（用户要求）：全程 `cargo build`，不跑 release。
- **跨仓时序**：auto-lang 合并 + 重建 debug auto.exe 后，再回 auto-ai 重跑 retranspile。

## 完成判定

- [x] auto-lang：`cargo test` 301 passed，015/022/023 不回归（Phase 1A/D 合并 `930ffcee`）
- [x] auto-ai：**auto-ai-agent/rust/ cargo check 0 错误**（63→26→11→0）
- [x] auto-ai：主版本 `cargo check` + `cargo test` 全绿
- [x] ai-config（2）/auto-ai-client（9）转译版无回归（既有错误，非本 plan 引入）

## 成果

**Auto 版 auto-ai-agent 首次完整编译通过**（cargo check 0 错误）。错误演进：
63（Plan 018 前）→ 26（Plan 018 清零 E0404/E0252）→ 11（Plan 019 Phase 1 清零
Option<impl Trait>）→ **0**（Plan 019 Phase 2 sed 绕过借用推理类）。

Phase 1（a2r 根因，auto-lang `930ffcee`）：Option/Result inner 改 rust_type_name（spec→Box<dyn>）、
read_to_string 改 AsRef<Path>。
Phase 2（retranspile.sh sed 绕过，4 类 a2r 借用推理缺陷）：clone 补全（B）、ReadDir 去引用（C）、
path 借用传参（D 连带）、去多余 as_str（E）。这些 sed 在 a2r 根因修复后自动变成 no-op。
