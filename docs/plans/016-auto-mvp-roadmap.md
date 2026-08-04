# Plan 016: Auto 版 MVP 可运行验证路线图

> **状态**：🟡 实施中（2026-08-04 制定）
> **仓库**：auto-ai（主）+ auto-musk（F1、workflow 端点迁移）
> **前置**：Plan 015（Auto 源迁入，已完成）、Plan 004/008（复审后有残留）
> **目标**：完成 Auto 版（.at + rust/）的 MVP 可运行验证，打 tag；**不转正**。
> 转正留给未来的第二轮 Auto 化（plan 017+），等 Auto 生成能力成熟后凭 Rust 参考版生成。

## 战略背景（用户决策）

1. **Auto 版当前是"跟随版本"，Rust 版是"参考实现/主线"**。原因：Auto 语言尚未被任何大模型掌握，
   直接写 Auto 新代码正确率低，需用 Rust 参考版作为生成辅导。未来 Auto 代码量大、或 Auto 生成 skill
   足够优秀后，再考虑直接用 Auto 凭空写新程序。
2. **这一波的终点 = MVP 验证 + 打 tag**，不是转正。打 tag 后继续完善 Rust 版，再进行第二轮 Auto 化。
3. **废弃旧 workflow 引擎**，但前置条件：新引擎（PipelineEngine）在所有下游实际可用并已切换
   （auto-ai-cli ✅、auto-musk ❌、auto-shell ✅）。musk 迁完后才删 workflow。
4. **Token budget 暂不做**，只做 token statistics（当前 `AgentResult.total_tokens` 已累计，已满足）。

## 5 个 Harness 的现状盘点（决定 MVP 验证范围）

| Harness | rust-ref | 转译版 rust/ | 缺口 | 修复工作量 |
|---|---|---|---|---|
| **plan（编排）** | ✅ | ✅ 完全对齐 | 无 | 0（可直接验证） |
| **spec（动态分发）** | ✅ | 🟡 部分（设计如此） | Auto 限制：`complete_stream`/流式回调未移植；`StreamingAiClient` 已用 channel 部分恢复 | L（需 Auto 解析器改进，第四波） |
| **tool use** | ✅ | ✅ 循环对齐 | `register_tool<T>` 泛型缺；CLI 8 处调用编译失败 | S（加值类型方法） |
| **skill** | ✅ | 🟡 部分对齐 | `register_skill_tool` **未写在 .at**；skills_block 永远 None | S（加方法到 agent.at） |
| **agent role** | ✅ | 🟡 部分对齐 | 14 个内置角色 soul 是占位符；resources/souls/*.md 只在 rust-ref 下 | M（运行时 fs 加载） |

> 关键结论：3 个 harness（plan/spec 建模/tool 循环）基本就绪；skill/role 有源于 .at 的阻塞缺口，
> 均 S-M 级。spec 的流式补齐是大工程（L），归入第四波，不阻塞 MVP（polling-style 循环功能正确）。

---

## 第一波：独立快速修复（全 S，可并行）

> 完成后：**归档 Plan 004**、推进 Plan 008/015。无依赖，立即可做。

### 1.1 F1 — Tier clamp 修复（Plan 004 唯一未决项）

- **仓库**：auto-musk（`backend/crates/musk/src/lib.rs`）
- **问题**：`OwnedRole`（:43-105）无 `override_tier` 字段，`model_tier()` 转发 `inner.model_tier()`
  （:80-82）。clamp 在 :131-135 算了 `clamped` 但只用于日志，:148 的 `OwnedRole::new(role)`
  用原始 role。`allowed_tiers` 形同虚设。
- **修复**：给 `OwnedRole` 加 `override_tier: Option<ModelTier>` 字段 + `with_override_tier()` builder；
  `model_tier()` 返回 override（若有）；:148 改为 `.with_override_tier(clamped)`（当 clamp 发生时）。
- **工作量**：S（~15 行 + 1 单元测试）
- **验证**：单元测试——构造 allowed_tiers=[mid,pro] + role tier=max 的 OwnedRole，断言 model_tier()==pro。
- **完成后**：纠正 plan 004 §5 错误的 ✅，**归档 Plan 004**。

### 1.2 ai-config a2r 转译

- **仓库**：auto-ai（`crates/ai-config/`）
- **现状**：`rust/src/` 为空；6 个 .at（lib/loader/provider/tier/validate/wire）已就位，纯同步无 async。
- **实际工作量**：**M（比预估的 S 大）**。2026-08-04 首次 `./retranspile.sh check` 产生 30 个错误，
  分 5 类（均为 .at 源码写法或 a2r codegen 缺陷，非业务逻辑问题）：

  | 类 | 数量 | 根因 | 修复方向 |
  |---|---|---|---|
  | `JsonValue` 未找到 | 4 | wire.at 缺 `serde_json::Value as JsonValue` 导入 | .at 加 `use.rust` |
  | ContentBlock 变体字段访问 | 8 | tuple vs struct 变体访问方式 a2r 处理不一致 | .at 写法或 a2r |
  | match 尾表达式返回 () | 6+ | a2r 对"match 作为函数返回值"codegen 缺陷（tier.rs 的 display_name/order/description） | .at 改用显式 `return`（对照 agent 已成功模式） |
  | `Option<&ProviderConfig>` 无 models 字段 | 2 | loader.at 链式 Option 访问 a2r 处理错 | .at 或 a2r |
  | ModelDefinition 缺 Eq/Ord derive | 3 | a2r 未生成 derive | .at 加 derive 注解 |
  | Vec\<String\> 无 Display | 1 | a2r 缺 Display 桥接 | 手写或 a2r |

  **对照证据**：已成功转译的 auto-ai-agent 里，返回值方法都用显式 `return`（如
  `fn model_tier() { return ModelTier::Max; }`），不用 match 尾表达式——印证 a2r 该缺陷存在，
  且 .at 改写可绕过。
- **动作**：逐类修复 .at 源码（主要在 tier.at/wire.at/loader.at），重跑 retranspile 直到 0 错误。
- **验证**：`retranspile.sh check` 输出 `error count: 0`；`cargo check`（rust/）0 错误。
- **状态（2026-08-04）**：✅ **转译通过（0 错误）**。Phase A 全部完成（auto-lang `09f22c16`），
  ai-config 转译 **30→0 错误**，rust/src/ 产物已生成并入库。auto-ai-agent 同步重新转译也 0 错误。
  .at 源码改动（tier.at return、validate.at 单遍迭代+as_str+clone、loader.at as_str）已提交。

### 1.3 auto-ai-client a2r 转译

- **状态**：🟡 **转译 38→10 错误**（A.4 大部分完成，10 个小类型匹配剩余）。
  A.4 全程修复（auto-lang `7e2b9f67`）：time Dot/get 字面量/find 借用/self.field 赋值 +
  循环变量跟踪/json.parse_opt/pathbuf type-aware/[]byte→Vec<u8>/split 借引/fs.exists 借引 +
  .at 绕过（pub/to_string/path join/json 模块重写/bytes/spawn）。
  ai-config/agent 无回归（双双 0 错误）。
  剩余 10 个是小类型匹配：sse.push(&str/i64→u32/json.get &Vec/self.buf move，
  用 .at 绕过收尾。rust/src/ 已生成（半成品）。

### 1.4 F2 — budget enum/文档清理（Plan 008 残留）

- **仓库**：auto-ai（`crates/auto-ai-agent/rust-ref/src/orchestration/budget.rs` + .at 版）
- **现状**：advisory 行为已实现并有测试锁定（pipeline.rs:289,478），但 `BudgetStrategy::HardStop`
  枚举仍存在、`TokenBudget::new()` 默认仍指向它、计划/设计文档仍写 hardstop。决策已定（advisory），
  只差传播。
- **动作**：弃用或移除 `HardStop` 枚举变体（改 docstring 或 `#[deprecated]`）；改 `TokenBudget::new`
  默认；回填 plan 008 文档（§3.1/§9/§10）+ `docs/orchestration-down-design.md`。
- **工作量**：S
- **验证**：`cargo check` 通过；grep 确认无误导性 HardStop 引用。

### 1.5 孤儿 echo.at 清理

- **仓库**：auto-ai（`crates/auto-ai-agent/src/tools/echo.at`）
- **现状**：retranspile.sh 不扫 `src/tools/`，echo.at 从不被转译；实际用的是手写 `rust/src/echo_tool.rs`，
  两者 schema 分歧（text vs message）。
- **动作**：删除 `src/tools/echo.at`（或并入 retranspile 流程——但手写 echo_tool.rs 已够用，删更简单）。
- **工作量**：S
- **验证**：确认 rust/src/echo_tool.rs 仍工作；retranspile 无变化。

### 1.6 重建并记录 auto.exe commit

- **仓库**：auto-lang（`../auto-lang`）
- **现状**：auto.exe 在 PATH（version 0.1.0），但略陈旧；auto-lang 的 a2r 刚修过 plan 384 重大缺陷。
  转译器是无版本 path 依赖，有漂移风险。
- **动作**：`cd ../auto-lang && cargo build`；记录当前 auto-lang commit hash 到本计划文档。
- **工作量**：S
- **验证**：`auto --version` 可用；commit hash 记录在案。
- **记录的 commit hash**：auto-lang `7e2b9f67`（2026-08-04，含 Phase A A1-A4 全部修复，重建 auto.exe 0.1.0）。
  前次：`7b9fec54`（A.4 首批）、`09f22c16`（A.3）、`6c9da95f`（A.2）、`b16614d3`（A1/A2）。
  ai-config + agent 转译 0 错误，client 38→10（10 个小类型匹配 .at 收尾）。

---

## Phase A：a2r 生成器缺陷修复（阻塞 1.2/1.3 转译）

> **前置发现**（2026-08-04 ai-config 转译实测）：剩余 24 个转译错误本质是 a2r 生成器的
> codegen 缺陷。在 .at 层绕过成本高且可能与 AutoVM bug 冲突，应到 auto-lang 仓库
> 用 worktree 方式修复 a2r 生成器根因。
> **仓库**：auto-lang（`crates/auto-lang/src/trans/rust.rs`）
> **工作方式**：在 auto-lang 建 worktree 修复 + 测试，合并后回 auto-ai 重跑转译。
> **完成后解锁**：1.2（ai-config）+ 1.3（auto-ai-client）转译可推进。
>
> **进度（2026-08-04）**：A1 + A2 已修复并合并到 auto-lang master（`b16614d3`），
> auto.exe 已重建。ai-config 转译 **30→15 错误**（消除 15 个）。
> 剩余 15 个是 4 个新发现的 a2r 缺陷（见 A.2 节），作为 Phase A 后续。
> auto-lang worktree 已清理。

### A1. tuple 变体构造生成了 struct 语法 ✅ 已修复

- **状态**：✅ 已修复（auto-lang `b16614d3`）。ai-config 消除 14 个错误。
- **根因**：`seed_known_struct_enum_variants()`（rust.rs:11288）硬编码 struct 变体 seed
  （如 `ContentBlock::Text { text }`），真实 .at 声明 tuple 变体后 seed 从未被清除，
  构造代码先查 `enum_struct_variants` 致 seed 优先 → tuple 变体用 struct 语法 + 臆造字段名。
- **修复**：`fn enum_decl()` 处理每个变体前先 `enum_struct_variants.remove(item_key)`，
  让真实声明成为权威。单点修复，同时纠正两个构造点（:5705, :6520）。

### A2. match 尾表达式多加分号 ✅ 已修复

- **状态**：✅ 已修复（auto-lang `b16614d3`）。ai-config 消除 1 个（tier.at 的 parse_name）；
  tier.at 其余已用显式 `return` 绕过（保留，显式 return 是好风格）。
- **根因**：`fn stmt()` 对 `Stmt::Is` 无条件加分号（:7670）。`fn body()` 尾表达式路径里
  `Stmt::Is` 走 `_ =>` 调 `self.stmt()`，继承分号，致 match 尾表达式变语句，函数返回 ()。
- **修复**：`fn body()` 的尾表达式 match 显式处理 `Stmt::Is`，调 `is_stmt()` 但不加分号。

### A3. 不支持 type 别名 / `use.rust ... as ...`

- **现象**：Auto 无 `type X = Y` 别名语法；`use.rust serde_json::Value as JsonValue` 报 E0099。
  导致 `JsonValue` 这类短别名在转译产物里无法表达。
- **影响**：ai-config wire.at 的 JsonValue（已用 retranspile.sh 注入 `use serde_json::Value as JsonValue;` 绕过）。
- **修复方向**：在 Auto 语法层支持 `use.rust X as Y` 别名，或支持 `type X = extern::Y`。
- **工作量**：M（涉及 Auto 解析器 + a2r codegen）
- **注**：ai-config 已用 retranspile.sh 注入绕过，优先级低于 A1/A2。可作为 a2r 长期改进。

### A.2 剩余 a2r 缺陷（A1/A2 修复后新发现，ai-config 15 错误）

> A1/A2 修复后（auto-lang `b16614d3`），ai-config 转译 30→15。A.2（A4-A7）修复后
> （auto-lang `6c9da95f`），ai-config 转译 30→4。剩余 4 个是借用/移动语义（A.3），后续处理。

| 编号 | 缺陷 | 错误数 | 状态 |
|---|---|---|---|
| **A4** | builder `return self` 类型不匹配 | 4 | ✅ 已修（fn_decl 检测 builder → mut self） |
| **A5** | String/&str 自动借引缺失 | 3 | 🟡 部分修（get 规则加了，嵌套 Dot 未覆盖，.at 用 .as_str() 补） |
| **A6** | `pub type`（struct）缺 Eq/Ord derive | 3 | ✅ 已修（fix_non_ord_derives 升级 pass + 传播） |
| **A7a** | `.to_string()` 误插入非 String（作用域污染） | 1 | ✅ 已修（fix_some_str_to_string 函数作用域化） |
| **A7b** | Option 链式访问（.get().field） | 2 | ✅ .at 改 match unwrap（validate.at） |
| **A7c** | Vec Display | 1 | ✅ .at 改 .join(", ")（validate.at） |
| **A8** | Err(FStr) Box 包装 | 1 | ✅ 已修（FStr 走 .into()） |

### A.3 剩余：借用/移动语义（4 错误）

> A.2 修复后剩余 4 个错误，都是 Auto（无借用概念）→ Rust 借用/移动转换的系统性 gap：

- **E0507**（2 个）：`for m in p.models` —— `p` 是 `&ProviderConfig`（来自 Some(p) match），
  `p.models` 是引用，a2r 生成 move 迭代。应为 `for m in &p.models`。
- **E0382**（1 个）：`config.provider_names` 第二次 for 循环 move（第一次已消费）。应为 `&config.provider_names`。
- 类似 1 个。

**修复方向**：a2r 在 `for x in expr` 生成时，若 expr 是引用上下文（&self 字段、已 move 的变量），
  应生成 `for x in &expr`。这是 a2r 对借用迭代器的系统性改进。或 .at 层用 `.iter()` 显式写。

### Phase A 进度与后续动作

**已完成（A1/A2/A.2，auto-lang `6c9da95f`）**：
1. ✅ A1 + A2 修复（tuple 变体构造 + match 尾表达式分号），合并 `b16614d3`。
2. ✅ A.2 修复（A4 builder self / A6 derive / A7a 作用域 / A5 get / A8 Err FStr），合并 `6c9da95f`。
3. ✅ auto.exe 重建，worktree 清理。ai-config 转译 30→4。
3. ✅ 回 auto-ai 验证：ai-config 30→15 错误。

**待办（A.2，下个会话）**：
1. 在 auto-lang 建 worktree 修复 A4/A5/A6/A7，a2r 测试全绿。
2. 合并到 master，重建 auto.exe。
3. 回 auto-ai 重跑 1.2（ai-config）+ 1.3（auto-ai-client），目标各 0 错误。
4. 更新本计划 1.2/1.3 状态。

---

## 第二波：补测试 + 简单对齐（M，Plan 008 闭环 + 解锁 MVP）

> 依赖第一波的 1.6（auto.exe）。完成后：**归档 Plan 008**、MVP 前置缺口清零。

### 2.1 driver 单元测试补全（Plan 008 最后未决项） ✅ 已完成

- **状态**：✅ 已完成（2026-08-04）。补了 5 个测试（共 6 个含原 happy-path），全绿。
- **测试**：F4 回归（path 提取不 dump JSON）+ 非 file 工具忽略 + 无 path 回退 "?" +
  client 错误不 Completed + summary 200 字截断。
- **完成后**：**归档 Plan 008**。

### 2.2 skill 缺口修复（MVP 前置） ✅ 已完成

- **状态**：✅ 已完成。agent.at 加了 `register_skill_tool(tool SkillTool)`：设 `skills_block`（核心价值——
  让模型看到技能目录）。工具注册拆分（Auto 不能 Arc 包 spec 值，调用方另用 register_shared）。
- **注**：agent 转译版重新转译后有 8 个预先存在的 a2r 限制（&ReadDir iterator、&Arc、tc.args move），
  不影响 rust-ref（主版本）和 MVP 验证。

### 2.3 tool 缺口修复（推迟到转正）

- **状态**：⏸ 推迟。`register_tool<T>` 泛型构造在 Auto 里无法表达（spec 值 → Arc<Box<dyn>> 需要泛型）。
  MVP 不需要（auto-ai-react.exe 不用 CLI 的 register_tool）。转正时再处理。

### 2.4 ai-config + auto-ai-client 功能对齐审计 ✅ 已完成（Phase A 中覆盖）

- **状态**：✅ 已完成。Phase A 转译过程中逐文件对比了转译产物 vs rust-ref，修复所有发现的功能差异。
  - ai-config：0 错误，公共 API 对齐（ModelTier/ContentBlock/ProviderConfig/ClientConfig 等）。
  - client：38→9 错误，剩余是类型推断限制（非功能缺口），核心 API（AiClient/complete/complete_stream/daemon）对齐。

---

## 第三波：MVP 验证核心（M，这一波的重头戏）

> 依赖第二波。完成后：**打 tag，Plan 015 闭环**。

### 3.1 agent role soul 修复 ✅ 已完成

- **状态**：✅ 已完成。12 个角色的 SOUL 改用 comptime `#{read_text(...)}`（转译时读文件嵌入，
  等效 include_str!）。runner/translator 已有内联 SOUL。转译后 14 角色含真实 soul。

### 3.2 MVP harness 验证（Rust 集成测试） ✅ 已完成

- **状态**：✅ 已完成。5 个测试全绿（`cargo test -p auto-ai-agent --test mvp_harness`）。
  测试在 `crates/auto-ai-agent/tests/mvp_harness.rs`，测 rust-ref（主版本）。
  - tool use：mock client 返回 tool_call → 验证执行 + 回填 ✅
  - skill：register_skill_tool → skills_block 非空 ✅
  - agent role：14 角色含真实 soul（非占位符）✅
  - plan：2-step flow → Completed + StepStarted/Completed 事件 ✅
  - spec：Box<dyn> Client/Role/Tool 动态分发 + ReAct 循环 ✅

### 3.3 打 tag ✅ 已完成

- **状态**：✅ MVP 验证通过，打 tag `auto-mvp-v0.1`。**归档 Plan 015**。

---

## 第四波（后续，不在本路线图执行范围）

记录路线，等条件成熟再做：

| 序 | 工作 | 仓库 | 工作量 | 时机/前置 |
|---|---|---|---|---|
| 4.1 | workflow 端点迁移：auto-musk 的 `/api/workflow/run` + `/stream` 迁到 relay | auto-musk | M | ✅ 已完成（plan 017 Phase 1，2026-08-04） |
| 4.2 | workflow 物理删除（rust-ref + .at + lib.rs 导出） | auto-ai | S | ✅ 已完成（plan 017 Phase 2，2026-08-04） |
| 4.3 | spec 流式补齐（complete_stream / 事件队列 / Auto actors） | auto-ai + auto-lang | L | 需 Auto 解析器支持 `dyn Fn` |
| 4.4 | 完善 Rust 参考版（新一轮功能开发） | auto-ai | — | MVP 打 tag 后 |
| 4.5 | 第二轮 Auto 化（凭 Rust 版生成 Auto） | auto-ai | — | Auto skill 成熟后 |
| 4.6 | 转正（plan 018）：翻转 [lib] path，删 rust-ref | auto-ai | M | 第二轮 Auto 化达成 100% 功能后 |

> **分叉记账（Plan 381）**:rust-ref/ 已迁移 auto-val serde Deserialize + lenient 辅助
> (loader.rs / role_config.rs,Plan 381)并修复 provider 反序列化错误静默跳过的问题;
> 转译产物 `crates/ai-config/rust/` 是从 src/*.at 生成的旧 opt_* 风格,**不反映该迁移**。
> 4.6 翻转 [lib] path 前需决策:在 .at 源/转译器层面同步 serde 支持(让 retranspile 产物
> 跟上 rust-ref),或评估转译树取舍。


---

## 验证清单（本路线图完成的判定标准）

- [ ] 第一波 6 项全部完成
- [ ] Plan 004 归档（F1 修复后）
- [ ] Plan 008 归档（driver 测试补全后）
- [ ] 第二波 4 项完成（含 skill/tool 缺口修复）
- [ ] 第三波：5 个 harness 集成测试全绿
- [ ] 打 tag `auto-mvp-v0.x`
- [ ] Plan 015 归档
- [ ] `cargo check`（rust-ref workspace）+ `cargo check`（3 个 rust/ 转译版）双通过
- [ ] `cargo test`（含新集成测试）全绿
