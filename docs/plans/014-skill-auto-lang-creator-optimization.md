# Plan 014: auto-lang-creator 技能优化（基于 plan 013 + auto-lang plan 373/376 的移植经验）

> **状态**：✅ 已实施（2026-07-31：技能更新完成 + error.rs 重新生成验证通过）
> **仓库**：auto-ai（本计划）+ auto-lang（a2r 经验来源）+ skills（技能位于 `D:/autostack/skills/auto-lang-creator/`）
> **前置**：auto-ai plan 013（Auto 移植 G1-G4 全部达成）、auto-lang plan 373/376S/T/U/V/W（re-transpile 136→0）
> **目标**：把 plan 013 + auto-lang 373/376 过程中积累的 Auto 代码编写经验回灌到
> `auto-lang-creator` 技能，使将来生成的 Auto 代码从一开始就避免这些问题。
> **原编号**：auto-lang plan 377（已迁移到 auto-ai 重新编号为 014）

## 背景

plan 013 把 auto-ai 的 3 个 Rust crate 用 Auto 语言复刻（`.at` 文件），
plan 373 修了 343 个 a2r codegen 错误（手修 + post_process），plan 376S/T/U/V/W
把 a2r 生成器 + .at 源码持续改进到 **re-transpile 0 错误**（从 136 降到 0），
并实现了 lib.rs 自动生成。Auto 版 ReAct 端到端跑通（GLM-5.2 真实回答 + echo 工具调用）。

这些工作暴露了大量"Auto 代码编写时应该注意的模式"——它们目前散落在
计划文档、commit messages 和 a2r post_process 函数里。本计划的目标是
把这些经验系统化地整理进技能，使将来的代码生成直接产出正确模式。

## 三方对比方法

### 数据来源

| 对比项 | 来源 | 说明 |
|---|---|---|
| **Auto 源码（原始版）** | `git show 0c2c630b:crates/auto-ai-agent/src/*.at` | plan 013 首次移植的手写版本（未经修复） |
| **Auto 源码（修复后）** | master HEAD 的 `crates/auto-ai-agent/src/*.at` | 经 plan 373/376 修复后的版本 |
| **Rust 原版** | `auto-ai/crates/auto-ai-agent/src/*.rs` | 手写 Rust 参照基准 |
| **a2r 差异** | `git diff 0c2c630b HEAD -- crates/auto-ai-agent/src/*.at` | 原始版 → 修复版的所有 .at 源码变更 |

### 分析流程

```
1. 提取所有 .at 源码变更（git diff）
2. 分类每处变更：
   A. "Auto 代码编写规范"（应在技能中教）
   B. "a2r 生成器缺陷"（应修生成器，非技能问题）
   C. "桥接类型 API 差异"（应在技能中记录）
3. 把 A 类和 C 类经验整理进 auto-lang-creator 技能
```

## 已知的经验分类（已系统化 → skill.md 的 A/B/C 三节 + Bridge Types）

### A. Auto 代码编写规范（应在技能中教）

从 plan 373/376 的修复中，以下模式**必须在 Auto 代码中显式写**：

| 模式 | 错误症状 | 正确写法 | 为什么 |
|---|---|---|---|
| `.len()` 返回 `usize` | E0308 usize/uint | `s.len() as uint` | Rust 的 `.len()` 返回 usize，Auto 的 `uint` 是 u32 |
| `for m in self.field`（&self 方法中） | E0507 move | `for m in self.field.clone()` | &self 不能 move 字段 |
| `names.push(str_param)` | E0308 String/&str | `names.push(name.to_string())` | `str` 是 &str，Vec 需要 String |
| `HashMap.get(key)` 用作值 | E0308 Option | `self.map.get(key) ?? default` 或 `.clone()` | `.get()` 返回 Option<&T> |
| `is self.cfg.field`（两层 self） | E0507 move | `self.cfg.field ?? default` 或 bind 局部 var | a2r 的 auto-clone 只覆盖 `self.X` 一层 |
| `Some(map.get(key))` | E0308 Option 嵌套 | `Some(map.get(key).clone())` | `.get()` 返回引用，需 clone |
| `pair[0]` tuple 索引 | E0608 | `pair.0` | Auto tuple 用 `.N` 不用 `[N]` |
| `step.id` 多次使用 | E0382 use after move | `step.id.clone()`（除最后一次使用） | String 是 owned，move 后不可再用 |
| `e.message()` on 外部类型 | E0599 | `e.to_string()` 或 `format!("{}", e)` | 外部 crate 类型没有 Auto 的 `.message()` 方法 |
| `is last_handoff { None -> "" }` | E0308 match arms 不一致 | `None -> "".to_string()`（或 `""`，a2r 会补）；**不要 `Str.new()`** | `""` 是 &str，`Some(h) -> h.from` 是 String；`Str.new()` 转译成不存在的 `Str::new()`（Layer 1 修正） |
| `c.load()` + `v != 0` | E0277 bool/int 比较；裸 `c.load()` 渲染成占位注释 E0061 | `c.load(Ordering.SeqCst)` + `v` 直接用 | `AtomicBool.load()` 返回 bool；需显式 Ordering（Layer 1 修正） |
| `soft_limit * 5`（uint * int 字面量） | E0308 uint/int 混用 | `soft_limit * 5u`（用 uint 字面量） | 默认字面量是 int，需显式 `5u` |
| `s.len() as i32` 残留 | E0308 i32/u32 | 避免 `as i32`，用 `as uint` 或不 cast | a2r 的 `as i32` 习惯导致类型不匹配 |
| **局部变量与模块导入同名** | E0433 scope 错误 | 避免用 `agent`/`error`/`handoff` 等模块名做局部变量名 | a2r 误判为模块路径，渲染 `agent::method()` 而非 `agent.method()` |
| **函数体不支持 `::` 路径表达式** | 解析失败 | 不写 `std::time::SystemTime::now()`，改用 `time.now_sec()`；**需 `use.rust a2r_std::time`** | Auto 函数体不解析 `::`，需走 a2r 的 stdlib 方法分发；缺桥接则 `time` 不在作用域（Layer 1 修正） |
| **`use.rust` 不能导入 const** | 解析失败 | 不写 `use.rust std::time::UNIX_EPOCH`（const），只导入 type | const 导入后仍报 `undefined variable` |
| **spec 类型进 Arc 需先装箱** | E0308 | role 是 `has Role` 类型时写 `Arc(Box(role))` | 需先 `Box` 成 `Box<dyn Trait>` 才能进 `Arc`（plan 379 起 a2r 对任意 `has Spec` 生成真实 impl，通用装箱可用） |
| **Arc 内的值需解引用** | E0308 | 读取场景直接字段访问 `registry.entries`（最简）；或 `(*registry).clone()`（plan 379 起 parser 支持 `(*` 前缀解引用） | `Arc<T>` 不是 `T`；`*x.clone()` 会解析成 `*(x.clone())`，括号不可省（plan 379 起发射端自动补） |
| **返回 &str 切片但签名是 str** | E0308 | `fn f() str { return s.trim() }` → 补 `.to_string()` | a2r 把 `str` 返回类型渲染为 Rust `String`，但 `trim()` 返回 `&str` |
| **`pair.0.as_str()` 链式点丢失** | E0308 | 用 `pair[0].as_str()`（经 fix_tuple_index 转换保留 .as_str）；**不能直接做 `is` 匹配条件**，先 `let key = ...` 再 is | a2r 渲染 `pair.0.as_str()` 时丢失 `.as_str()`（链式点解析 bug）（Layer 1 修正） |

### B. a2r 生成器缺陷（不教技能，应修生成器）

这些是 a2r 生成器的 bug，不应在技能中教用户绕开：

| 缺陷 | 已有修复 | 状态 |
|---|---|---|
| `has Spec` 生成空 `ToolTrait` | plan 373 G2: 生成真正的 `impl Trait for Type` | ✅ |
| `~Result` 方法不标 async | plan 373 G2: `Type::Result` → `async fn` | ✅ |
| 多余 `.as_str()` on &str | plan 376B P7: 检查 `local_var_types` | ✅ |
| `#[async_trait]` 未加到 impl | plan 373 G2: `has_async` 检测 | ✅ |
| `&mut self` 未检测 | plan 373: `method_mutates_self` 扫描 | ✅ |
| `.await` 未自动插入 | plan 373: `fn_ret_types` + `call_needs_await` | ✅ |
| ContentBlock struct-variant | plan 373: `seed_known_struct_enum_variants` | ✅ |
| `(Uint, Int)` unify 返回 Int | plan 376G: 改为返回 Uint | ✅ |
| `dyn Trait` derive Clone/Debug 失败 | plan 376V: `fix_dyn_trait_derives` 降级为 `#[allow(dead_code)]` | ✅ |
| `Box<dyn Trait>` 未装箱 | plan 376V: `fix_spec_trait_boxing`（`Some(X{})` → `Some(Box::new(X{}))`） | ✅ |
| PathBuf 无 `.as_str()` | plan 376V: `fix_pathbuf_as_str`（→ `.to_str().unwrap()`） | ✅ |
| fn 字段调用 `self.on_event(x)` | plan 376V: `fix_fn_field_calls`（→ `(self.on_event)(x)`） | ✅ |
| tuple 索引 `pair[0]` | plan 376V: `fix_tuple_index`（→ `pair.0`） | ✅ |
| fs Result 嵌套括号 | plan 376W: `fix_a2r_std_fs_result_patterns`（`[^)]*`→`.*?`） | ✅ |
| enum `#[derive]` 不生效 | plan 376S: `EnumDecl.attrs` + parser 分支 | ✅ |
| lib.rs 不自动生成 | plan 376U: `A2R_CRATE_ROOT=1` → `pub use` + shims 注入 | ✅ |

### C. 桥接类型 API 差异（应在技能中记录）

| 差异 | 说明 | 正确写法 |
|---|---|---|
| `auto_val::Node` 不可 deref | a2r 生成 `*(*node).clone()` | `node.clone()` |
| `ClientError` 无 `.message()` | 用 `thiserror` 的 Display | `format!("{}", e)` |
| `PathBuf` 无 `.as_str()` | 用 `to_str().unwrap()` 或 `to_string_lossy()` | `path.to_str().unwrap()` |
| `a2r_std::fs::read_to_string` 返回 String 非 Result | 桥接函数签名差异 | 用 `Ok()` 包装或不 match |
| `HashMap.get()` 返回 `Option<&T>` | Rust 标准库 API | `?? default` 或 `.clone()` |

## 实施步骤

### 步骤 1：提取完整 diff（plan 376 完成后）

```bash
cd D:/autostack/auto-lang
git diff 0c2c630b HEAD -- crates/auto-ai-agent/src/*.at > /tmp/at_changes.diff
# 分类每处变更（A/B/C）
```

### 步骤 2：更新 auto-lang-creator 技能

在 `D:/autostack/skills/auto-lang-creator/skill.md` 中：

1. **新增"Rust→Auto 移植 gotchas"节**：把 A 类经验（21 条，含 376V/W 新增 8 条）写成明确的规则
2. **更新"桥接类型"节**：把 C 类经验（5 条）加入已知桥接 API 差异列表
3. **更新"常见错误"节**：把最容易犯的错误排到前面

### 步骤 3：验证

用一个简单的 Rust 文件（如 `auto-ai/crates/auto-ai-agent/src/error.rs`），
让技能（或手工）重新生成 `.at`，检查是否避免了已知问题。

### 步骤 4：更新 a2r 生成器（B 类）

确认 plan 373/376 的 B 类修复都在 post_process 链或生成阶段中，
不需要在技能里教用户绕开。

## 依赖

- ~~plan 376 re-transpile 错误降到 ~100 以下（当前 180）后再启动~~
  **✅ 已满足（2026-07-31）：re-transpile 达到 0 错误，经验数据已沉淀完毕。**
- ~~每次 .at 修复都在产生新的经验，提前做会漏掉后续~~
  **经验积累已趋于稳定（376V/W 是最后一批大批量修复）。**

## 预期效果

- 技能生成的 Auto 代码减少 ~50% 的 a2r 编译错误（A 类 + C 类经验）
  **✅ Layer 2 实测：基线 343 → 74（-78%）**
- 移植新 crate 时不需要重走 plan 373 的 343→0 手修过程
  **✅ Layer 2 盲迁移：held-out json_helpers.rs 74 → 0（4 处 a2r 缺陷修复后）**
- a2r 生成器的 post_process 链进一步简化（因为源码侧更规范了）
  **✅ Layer 3：fix_u32_i32_casts / fix_push_move / fix_for_in_self_field_borrow
  触发率归零；剩余 21 个 fix 命中均为合法转译（fix_mutable_params 等）**

---

## 实施记录（2026-07-31）

### 步骤 1：diff 提取与核对 ✅

```bash
cd D:/autostack/auto-lang
git diff 0c2c630b HEAD -- crates/auto-ai-agent/src/*.at   # 35 文件，+4325/-56
```

逐处核对 A/B/C 分类，A 类全部 20 条 + diff 新增 3 条均有 commit 佐证
（376V/W batch 系列）。已修的 error.at 恰好示范 A9/A23：`ClientError` 分支
去掉 `.message()`、AgentError 显式 `#[derive(Debug)]`。

### 步骤 2：技能更新 ✅（`D:/autostack/skills/auto-lang-creator/skill.md`）

1. **新增「Rust→Auto Porting Gotchas」节**：A 类 23 条规则表（20 条来自本计划 +
   从 diff 补充 3 条：A21 str 的 `is` 匹配 → `if ==`；A22 保留字改名
   `to`→`up_to`/`task`→`task_msg`；A23 外部类型显式 `#[derive(Clone, Debug)]`）。
2. **新增「Bridge Types（桥接类型）」节**：C 类 5 条已知桥接 API 差异表。
3. **「常见错误」节置顶**：Gotcha Checklist 顶部新增「Porting Quick Wins」8 条
   高频错误清单。
4. B 类仅以一句话在 A 类节后注明「已修复、勿绕写」——按计划 B 类不教技能。

### 步骤 3：验证 ✅（error.rs 重新生成 → a2r → cargo check）

用新技能规则从 `auto-ai/crates/auto-ai-agent/src/error.rs` 重新生成 `error.at`
（含 A9/A23 规则注释），a2r 转译 0 错误，临时 crate `cargo check` 0 错误
（16s），输出与 0 错误基线 `rust/src/error.rs` 语义逐行一致。验证临时目录
已清理，未触碰 auto-lang 工作区（其上有 plan 378 在途改动）。

### 步骤 4：B 类修复确认 ✅

a2r 生成器 `crates/auto-lang/src/trans/rust.rs` 中全部 16 项修复均在位：
`fix_dyn_trait_derives` / `fix_spec_trait_boxing` / `fix_pathbuf_as_str` /
`fix_fn_field_calls` / `fix_tuple_index` / `fix_a2r_std_fs_result_patterns` /
`impl Trait for Type` / `has_async` / `method_mutates_self` /
`call_needs_await` / `seed_known_struct_enum_variants` / `A2R_CRATE_ROOT`
lib.rs 自动生成等。无需在技能中教用户绕开。

### 追加：Layer 1 技能验证（2026-07-31，tests/ 框架 + 6 处规则修正）

按用户建议把验证放进了技能同仓：`D:/autostack/skills/auto-lang-creator/tests/`
（`verify.sh` + `probes/trap23.{rs,at}` + `README.md`）。23 陷阱探针覆盖
21/23 条 A 类规则（A9/A17 由语料 golden 覆盖，见 tests/README.md），
**30/30 断言 + a2r 转译 0 错误 + cargo check 0 错误**。

验证直接修正了 6 条从未被验证过的规则（详见 tests/README.md「Layer 1 发现」）：
- A10 `Str.new()` → `"".to_string()`（`Str.new()` 转译为不存在的 `Str::new()`）
- A11 `c.load()` → `c.load(Ordering.SeqCst)`（裸调用渲染成占位注释 E0061）
- A15 `time.now_sec()` 需 `use.rust a2r_std::time` 桥接（否则 E0425）
- A18 `(*registry).clone()` → Arc 字段直访（`(*` 前缀解引用从未能转译）
- A20 `pair[0].as_str()` 不能直接做 `is` 条件（先 bind）
- A7 裸 tuple 参数 `(str, str)` 解析失败 → 用 `List<(str, str)>`

并暴露 2 个 **a2r 生成器缺陷**（B 类）——**已在 auto-lang plan 379 修复并
合并到 master（`9c5a32a2`）**：
1. `known_spec_traits` 硬编码 Tool/Role/Client/AgentFactory 四个 spec，其余
   `has Spec` 退回空 `{Name}Trait`（plan 373 G2 修复不通用）→ 已泛化
2. `skill.at:402` 的 `(*registry)` 从未成功转译——`rust/src/skill.rs` 是
   失败后的手写回退，"0 错误"统计含此回退 → parser 支持一元 `*` 后首次可转译

修复后探针覆盖升至 **22/23**（A17 装箱重新纳入），verify.sh 断言随之更新。
真实全量 re-transpile 132 → 56 错误（-58%）；剩余 56 个在手写回退文件
（driver/client_impl/memory/validate 等）——plan 376"0 错误"从未覆盖它们，
待 plan 380+ 处理。

~~待办：Layer 2 盲迁移 + Layer 3 回归/fix 计数器~~ **✅ 已完成（2026-08-01）**：
见下文「Layer 2 + Layer 3 完成」节（盲迁移 74→0、fix 计数三 paper-over 归零）。

---

## 本计划续：真实 re-transpile 错误修复（2026-07-31）

> plan 379 合并后，去掉手写回退做全量真实 re-transpile，得到 **56 个错误**
> （132 → 56，-58%）。分析结论：**与 auto-ai 无关**——auto-ai 的 Rust 原版是
> 语义基准，错误全部在 auto-lang 侧（.at 移植源码 + a2r 转译器）。修复在
> auto-lang 用 worktree 方式实施（分支名沿 auto-lang 惯例取 plan-380，
> 但计划文档只在本文件维护）。

### 错误分布（按文件）

| 文件 | 错误数 | 主要错误 |
|---|---|---|
| skill.rs | ~18 | DirEntry 未导入、str_as_str ×2、char.as_str、Arc Copy、E0195、E0308 多处、JsonValue |
| driver.rs | ~16 | AgentError Clone/== 级联、GateDecision Display、&mut 借用、moved、match arms |
| roles.rs | ~14 | dirs 模块分发、DirEntry ×2、summary on Option ×3、Box\<dyn Role\>.clone、moved ×2、E0282 |
| pipeline.rs | ~4 | `time` 未导入、JsonValue、moved ×2、self.engine |
| workflow_validator.rs | 3 | E0733 异步递归、validators moved |
| agent.rs | 3 | E0782（trait `Future<...>`） |
| error.rs | 3 | 级联（driver 的 derive 需求） |
| validate.rs | 2 | str_as_str、ClientConfig.as_ref |
| memory.rs | 2 | void 调用被加 `.to_string()` |
| client_impl.rs | 2 | E0195（手写胶水，疑似 trait 注解缺失的连锁） |
| flow/tool/role_config/builtin_roles | 各 1-2 | E0782、moved |

### 分类与责任方

**类别 1：.at 移植源码问题（改 `crates/auto-ai-agent/src/*.at`，约 15-20 个）**
- 缺桥接导入：pipeline.at 缺 `use.rust a2r_std::time`（技能 A15）；skill.at/roles.at
  缺 `use.rust std::fs::DirEntry`；JsonValue 导入缺失
- A4 gotcha 复发：roles.at `self.roles.get(name).clone()` 直访字段（应 `is` 解构）
- validate.at `cfg.as_ref()`（struct 无 as_ref）；skill.at `'"'.as_str()`（char 无 as_str）
- roles.at 克隆 `Box<dyn Role>`（spec 未 extend Clone，设计问题）

**类别 2：a2r 转译器缺陷（改 `crates/auto-lang/src/trans/rust.rs`，约 25-30 个）**
1. **异步 spec trait 声明缺 `#[async_trait]`**（tool.rs/agent.rs E0782；疑似
   client_impl.rs E0195 根因）——plan 373 G2 只补了 impl 没补 trait 声明
2. 模块方法分发在 `is` 匹配位置缺失（roles.rs `dirs.home_dir()` E0423，return
   位置却正确）——同一调用不同位置行为不一致
3. void 调用被加 `.to_string()`（memory.rs）
4. 残留 `.as_str()` on &str（str_as_str ×3）——plan 376B 修复未覆盖
   `trim()`/`strip_prefix()` 链
5. Arc 字段赋值 auto-clone 未触发（skill.rs:374）
6. driver/pipeline 借用与 derive 级联（E0382、E0596、fn 值 `.to_string()`）

**类别 3：手写胶水**——client_impl.rs（疑似类别 2.1 连锁）

**类别 4：硬限制**——workflow_validator E0733 异步递归（需 Box::pin 或改迭代）

### 修复顺序（worktree 实施）

1. **类别 1（.at 源码）**：技能 A 类规则直接指导，风险低，先修
2. **类别 2.1（trait `#[async_trait]`）**：优先级最高，可能连锁消掉 client_impl
3. 逐批 retranspile + cargo check 回归（目标：56 → 尽可能接近 0）

---

## 修复完成记录：40 → 0 错误（2026-07-31 续）

> worktree `plan-380-at-and-a2r-fixes`（分支基于 master 92b2bd70 + plan-379 修复），
> 5 笔提交：`6261ad47` → `50991837` → `f0ef2e5f` → `9a7b99a7`。最终
> **`cargo check`（lib + bin）0 错误**，skills `tests/verify.sh` 30/30，
> 自举 `auto/` 源转译冒烟通过。

### 剩余 40 错误的文件簇与清零顺序

| 簇 | 错误数→0 | 提交 | 主要手法 |
|---|---|---|---|
| driver.rs | 10→0 | 6261ad47 | DriveOutcome 显式 `#[derive(Debug)]`（A23）；`mut fn` 链（dispatch/drive_step/resolve_gate_auto/handle_after_submit）；drive_step 预算警告改用 `handoff_doc.from.as_str()`；truncate_chars substring 加 `.to_string()` |
| agent.rs | 6→0 | 50991837 | `?` 改 `is Ok/Err` 显式映射 `AgentError.Client`；run/run_stream/run_inner 改 `mut fn`；工具循环克隆链；转译器新增 **fix_mutable_params**（fn 参数体内被改则加 `mut`） |
| skill.rs | 7→0 | f0ef2e5f | strip_prefix 内联（is 绑定记为 &str）；take_lines_joined 改收 `text str`；get/registry 传 `.as_str()`；转译器：**Plan 376 Pass 1 预注册 TypeDecl 方法 ret 类型**（trim-void 检查不再依赖声明顺序）+ **is_trim_method_call**（&str 参数位渲染 trim* 时去掉 `.to_string()`） |
| memory.rs | 1→0 | f0ef2e5f | 同上 trim-void 顺序修复 |
| roles.rs | 9→0 | 9a7b99a7 | load_one_builtin 改**返回 `?RoleDetail`**（by-value 参数无法回传 map 变更——原实现注册表为空）；PathBuf clone；cfg.name `.clone()` 解构；转译器：**spec_bound_idents**（`Some(prof)` 绑定 Box\<dyn\> 跳过 auto-clone）+ 兄弟扫描**递归进子目录并注册 fn_ret_types**（CLI 单文件跨模块）+ `is_spec_returning_scrutinee` 支持 `User(Role)` 形态 |
| workflow_validator | 2→0 | 9a7b99a7 | check/check_all/check_any 改**同步**（递归 async 需 Box::pin，E0733；与 Rust 原版一致）；check_any 循环克隆 |
| pipeline.rs | 1→0 | 9a7b99a7 | 语句位 is-match 由转译器补 `;`（值表达式臂 E0308） |
| role_config.rs | 1→0 | 9a7b99a7 | `is cfg.inherit` → `is result.inherit.clone()`（var result = cfg 后 cfg 已移动） |
| builtin_roles.rs | 1（新浮现） | 9a7b99a7 | `has_str_pattern` 对已是 `&str` 的目标不加 `.as_str()`（E0658 str_as_str） |
| bin 层 | 1（新浮现） | 9a7b99a7 | retranspile.sh read_shims 补 `pub mod echo_tool;`（main.rs 导入，之前被 lib 编译失败掩盖） |

### 顺带修掉的 a2r 缺陷（可回灌技能/plan）

1. **fix_some_str_to_string 过度应用**：旧 regex 对 `self.field = Some(任意 ident)` 都加
   `.to_string()` → `Option<u32>`（E0308）与 `Option<fn>`（E0599）载荷损坏。改为**类型感知**
   （先收集文本中 `&str`/`String` 类型的 ident，仅对 str ident 转换）。
2. **fn 参数 mut**：a2r 只为 `let` 局部加 `mut`；参数体内被 push/insert/set/字段赋值会 E0596。
   新增 `fix_mutable_params`（括号配平扫描 + 变更检测，处理 trait 方法声明与单行函数）。
3. **trim-void 顺序依赖**：`Memory.trim() void` 若声明晚于调用，`fn_ret_types` 查不到 Void →
   `.to_string()` on `()`（E0599）。Pass 1 预扫 TypeDecl 方法 ret 类型。
4. **trim* 传 &str 参数**：`clean_field_value(r.trim())` 被转成 `r.trim().to_string()` → E0308。
   &str 参数位改用 `expr_as_str` 渲染 trim*（去掉 `.to_string()` 后缀）。
5. **spec 值 auto-clone**：`is load_builtin(n) { Some(prof) }` 的 `prof` 是 `Box<dyn Role>`，
   无 Clone（E0599）。新增 `spec_bound_idents` + `is_spec_returning_scrutinee`
   （Option/Result\<Spec\> 或 User 形态命中 spec_decls）→ 跳过 auto-clone。
6. **CLI 单文件跨模块 ret 类型缺失**：`trans --path X.at` 无 parsed_modules，
   兄弟扫描只覆盖同目录/祖父目录。改为**递归进子目录** + 注册 `fn_ret_types`
   （build-in roles 的 `load_builtin ?Role` 可见）。
7. **has_str_pattern 对 &str 目标加 `.as_str()`** → E0658（str_as_str）。已是 &str
   （str 参数 / StrSlice 局部）时跳过。
8. **语句位 is-match 缺 `;`**：臂以 `map.insert(...)` 等值表达式结尾时 match 类型非 `()` →
   E0308。`Stmt::Is` 统一补 `;`（丢弃值）。
9. **fix_a2r_std_fs_result_patterns**：`Ok(x)` 无法推断 Err 类型（E0282）→
   `Ok::<String, std::io::Error>(x)`（Err 臂是死代码）。
10. **fix_spec_trait_boxing 正则**：`Some\((\w+)\s*\{\s*\})` 的 `)` 未转义 →
    "unopened group" panic，`unwrap()` 崩溃**静默丢弃 builtin_roles 模块**（之前
    retranspile 一直 [skip]）。转义 `\)`。

### 遗留的语义缺口（非编译错误，已定位）

- ~~**user-role 注册仍 by-value**：`load_user_roles → scan_roles_dir → load_user_at_file`
  链仍按值传 `roles`/`names`，参数内变更回传不到调用者 → 用户角色注册表为空。
  （内置角色已通过 load_one_builtin 返回式修复。）后续可用"返回 (roles, names) 元组"或
  把 scan 内联进 load() 解决。~~ → **G1 已修复（2026-08-01，见下方「语义缺口修复」节）**
- `AgentError::Client(#[from])` 在 a2r 转译下无 `#[from]`，`?` 转换需显式 `is Ok/Err`
  映射（agent.at 已按此写，文档化）。→ **保持为后续计划**（a2r 需支持变体属性发射）
- ~~`last_handoff_after` 仍是 stub（返回入参）——drive() 的 last_handoff 追踪未接真逻辑。~~
  → **G3 已修复（2026-08-01，见下方「语义缺口修复」节）**
4. 类别 4（异步递归）与 driver/pipeline 借用分析放最后

---

## Layer 2 + Layer 3 完成（2026-08-01，计划 014 全部闭环）

> worktree `plan-381-layer2-json-dispatch`（分支基于 master 77d6782a），2 笔提交：
> `9260381d`（Layer 2 的 4 处 a2r 修复）+ `f28795fa`（Layer 3 fix 计数）。
> skills 提交 `875d27a`（layer2 验证 + README + skill.md 回灌）。

### Layer 2 — 盲迁移（✅ 2/2）

目标：`auto-code-rs/auto/rust/src/json_helpers.rs`（308 行，跨仓库 held-out，
从未参与规则提炼）。按技能盲移植（未参考 auto-code-rs 既有 .at），
`tests/verify-layer2.sh` 验证。

- **初始 74 个 a2r 错误**（对比历史基线 343 → -78%），**全部根因于 4 处 a2r
  分发缺陷**，技能规则本身写对（.at 语义与 Rust 源逐行对应）：
  1. 两段式 `json.get(v,k)` 被加 `.to_string()`（返回 String 非 Value）→ 25 级联
  2. `json.as_int/as_string/as_bool` 无条件走 `*_str` 变体 + Value 参数 `.as_str()`
     → 37
  3. 枚举变体构造 String 载荷被加 `.as_str()`（静态方法路径误借用）→ 9
  4. `expr_contains_string` 未识别 `json.to_string/get_str` → 3
- **修复后 0 错误**；auto-ai-agent 全量回归 0 错误。
- 闭环回灌 skill.md Bridge Types 4 条（json.get 返回 Value / as_* 分派 /
  枚举变体载荷 / `trans --path` 裸文件名兄弟扫描失效）。

### Layer 3 — 回归 + 生成器简化（✅）

1. **回归**：全量 retranspile → cargo check 0 错误（无退化）。
2. **fix 计数**（`A2R_FIX_COUNTS=1`，40 个 fix_* 包装统计）：协议点名的
   `fix_u32_i32_casts` / `fix_push_move` / `fix_for_in_self_field_borrow`
   **全部 = 0**；剩余触发为合法转译（fix_mutable_params=35、fix_non_ord_derives=19
   等 21 个 fix 命中）。**Layer 2 held-out 只触发 1 次**（fix_mutable_params）。
   → 预期效果 3 达成（技能修正源码后缺陷掩盖类 fix 归零）。

### 计划 014 全部步骤状态

| 项 | 状态 |
|---|---|
| 步骤 1-4（经验分类/技能更新/验证/B 类确认） | ✅ |
| 本计划续：真实 re-transpile 56→0 | ✅ |
| Layer 1 探针验证 30/30 | ✅ |
| Layer 2 盲迁移 74→0 | ✅ |
| Layer 3 回归 + fix 计数（三 paper-over 归零） | ✅ |
| 遗留语义缺口 G1（user-role 注册链）+ G3（handoff 追踪） | ✅ 已修复 |
| 遗留语义缺口 G2（`AgentError::Client(#[from])`） | 文档化，后续计划 |

---

## 语义缺口修复（2026-08-01，G1 + G3 + 顺带 a2r 修复）

> worktree `plan-014-semantic-gaps`，2 笔提交 `6431ff8d`（G1/G3）+ `f729dfd3`
> （编译修复），合并进 master（`5c0030e4`、`5b38fa0b`）。

### G1 — user-role by-value 注册链（修复）

- 原状：`load_user_roles → scan_roles_dir → load_user_at_file` 按值传
  `roles`/`names`，参数内 `set/push` 回传不到调用者 → 用户角色永远加载不进注册表。
- 修复：`load()` 内联扫描循环（同 Rust 原版 roles.rs 106-161），逐文件解析抽成
  **返回式** `load_user_at_file(path) -> ?RoleDetail`（同 load_one_builtin 风格）；
  load() 内直接累积 + 去重（`roles.contains` 判重再 `names.push`/`roles.set`）。

### G3 — last_handoff_after stub（修复）

- 原状：`last_handoff_after` 返回入参 → drive() 的 last_handoff 从不更新，多步
  pipeline 第 2 步起输入仍是原始 task_msg。
- 修复：`DriveOutcome.Continue(?HandoffDocument)` 携带 handoff；`drive_step` 改为
  返回 `~Result<HandoffDocument, AgentError>`（Ok 时交回构建的 handoff）；删除
  stub。对齐 Rust 原版 drive() 的 `last_handoff = Some(handoff)`。
- 编译修正（`f729dfd3`）：① loop 内 `last_handoff` 需 `.clone()` 再传 dispatch
  （E0382 第二次迭代 use-of-moved-value）；② Continue 载荷是 `Option<...>`，
  `Ok(h)` 需显式 `Some(h)`（a2r 不自动包裹）。

### 验证

- 全量 retranspile → cargo check **0 错误**；roles.rs/driver.rs 重生成与提交版一致。
- G1 语义：用户 .at 角色按名覆盖内置、新名追加 names、list() 排序不变（与 Rust 一致）。

### 发现并顺带修复的 a2r 缺陷（均为预存在问题，非本次引入）

1. **a2r parser 递归深度限制（~9 层）**：`Parser::parse()` 对嵌套 >9 层的
   `is`/`if` 块递归爆栈（跨后端 ts/python 同样）。首版内联把 load() 解析做成
   ~10 层触发，故逐文件解析保留为 helper 规避。后续如需要更深的合法嵌套，需
   提高 parser 递归容量（新计划）。
2. **struct_init 空参位置构造回归（bd4c475e 引入，e7ec5eac 修复）**：plan-380
   P0 的位置构造分支把空参裸调用 `Type()` 也走位置构造；空成员结构体（如
   builtin_role_*.at 的 `pub type X has Role {...}`）不在 struct_fields →
   发射 `Assistant()` → E0423，且连带 fix_spec_trait_boxing 的 `Some(X{})`
   正则失效 → builtin_roles.rs 重生成后 cargo check 14 个 E0423。修复：位置
   构造分支加 `!args.args.is_empty()` 守卫。验证：全量 retranspile 0 错误，
   builtin_roles.rs 重生成与提交版逐字节一致。
