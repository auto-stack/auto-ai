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
- **状态（2026-08-04）**：**阻塞于 Phase A（a2r 生成器修复）**。已修 2 类（tier.at match→return 6 个、
  retranspile.sh JsonValue 注入 4 个），30→24 错误。剩余 24 个本质是 3 个 a2r codegen 缺陷（见 Phase A），
  在 .at 层绕过成本高且可能冲突 AutoVM。已做的部分进展（tier.at return 改动、retranspile.sh
  JsonValue 注入）已提交，rust/src/ 半成品产物未提交。

### 1.3 auto-ai-client a2r 转译

- **状态**：**阻塞于 Phase A**（与 1.2 同类 a2r 缺陷，待 a2r 修复后一并转译）。

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
- **记录的 commit hash**：auto-lang `896db196001999694f95efc9b5cce5204b212643`（2026-08-04 重建 auto.exe，version 0.1.0）。后续每次重新转译前应确认 auto-lang 仍在该 commit 或记录新 commit。

---

## Phase A：a2r 生成器缺陷修复（阻塞 1.2/1.3 转译）

> **前置发现**（2026-08-04 ai-config 转译实测）：剩余 24 个转译错误本质是 a2r 生成器的
> 3 个 codegen 缺陷。在 .at 层绕过成本高且可能与 AutoVM bug 冲突，应到 auto-lang 仓库
> 用 worktree 方式修复 a2r 生成器根因。
> **仓库**：auto-lang（`crates/auto-lang/src/trans/rust.rs`）
> **工作方式**：在 auto-lang 建 worktree 修复 + 测试，合并后回 auto-ai 重跑转译。
> **完成后解锁**：1.2（ai-config）+ 1.3（auto-ai-client）转译可推进。

### A1. tuple 变体构造生成了 struct 语法

- **现象**：.at 定义 `Text(str)`（tuple 变体），构造 `ContentBlock.Text(t)`（位置参数），
  a2r 生成 `ContentBlock::Text(String)` 定义但构造却生成 `ContentBlock::Text { text: t }`
  （struct 语法）→ E0559 "no field named" + E0769 "tuple variant written as struct variant"。
- **影响**：ai-config wire.at 的 ContentBlock 8 个错误。
- **根因位置**：`trans/rust.rs` 枚举变体构造 codegen（:150-155 缓存区分 tuple/struct，
  但构造路径未正确使用缓存判断）。
- **修复方向**：a2r 构造 `Enum.Variant(args)` 时，查缓存判断该变体是 tuple 还是 struct；
  tuple 用 `Enum::Variant(args)`，struct 用 `Enum::Variant { field: val }`。
- **工作量**：M（需深入 a2r 构造 codegen + 加测试）
- **注意**：wire.at 用 tuple 变体是为绕过 AutoVM 的 struct 解构 bug（plan 013 B3）。
  修好 a2r 后，可考虑把 wire.at 改回 struct 变体（与 rust-ref 一致），前提是 AutoVM 解构
  bug 也已修或 .at 改用字段访问而非解构。

### A2. match 尾表达式多加分号

- **现象**：.at 的 `fn f() T { is x { ... } }`（match 尾表达式），a2r 生成
  `fn f() -> T { match x { ... }; }`（match 后多了分号）→ 函数返回 `()` 而非 match 值。
- **影响**：曾致 ai-config tier.rs 6 个错误（已用 .at 改 `return` 绕过，但根因未除）。
- **根因位置**：`trans/rust.rs` match/`is` 表达式作为函数尾表达式的 codegen。
- **修复方向**：当 `is` 表达式是函数体的最后一条语句时，不加尾分号。
- **工作量**：S-M
- **注**：tier.at 已改用显式 `return` 绕过，此修复为根因清除（让其他 .at 不必绕过）。

### A3. 不支持 type 别名 / `use.rust ... as ...`

- **现象**：Auto 无 `type X = Y` 别名语法；`use.rust serde_json::Value as JsonValue` 报 E0099。
  导致 `JsonValue` 这类短别名在转译产物里无法表达。
- **影响**：ai-config wire.at 的 JsonValue（已用 retranspile.sh 注入 `use serde_json::Value as JsonValue;` 绕过）。
- **修复方向**：在 Auto 语法层支持 `use.rust X as Y` 别名，或支持 `type X = extern::Y`。
- **工作量**：M（涉及 Auto 解析器 + a2r codegen）
- **注**：ai-config 已用 retranspile.sh 注入绕过，优先级低于 A1/A2。可作为 a2r 长期改进。

### Phase A 完成后的动作

1. 在 auto-lang worktree 修复 A1（必做）+ A2（必做）+ A3（可选），a2r 测试全绿。
2. 合并到 auto-lang master，重建 auto.exe，记录新 commit hash。
3. 回 auto-ai，重跑 1.2（ai-config）+ 1.3（auto-ai-client）转译，目标各 0 错误。
4. 更新本计划 1.2/1.3 状态。

---

## 第二波：补测试 + 简单对齐（M，Plan 008 闭环 + 解锁 MVP）

> 依赖第一波的 1.6（auto.exe）。完成后：**归档 Plan 008**、MVP 前置缺口清零。

### 2.1 driver 单元测试补全（Plan 008 最后未决项）

- **仓库**：auto-ai（`crates/auto-ai-agent/rust-ref/src/orchestration/driver.rs`）
- **现状**：已有 1 个 happy-path 测试 + ScriptedClient/MockFactory 脚手架（:314-432）。缺：Failed 传播、
  Paused（loop cap）终止、WaitForHuman（gate reject）、BudgetWarning、**F4 回归测试**（最高价值）。
- **动作**：补 ~5 个 tokio 测试，复用现有脚手架。重点：F4 回归测试（build_handoff 提取 args["path"]
  而非 JSON dump）。
- **工作量**：M
- **验证**：`cargo test -p auto-ai-agent` 全绿。
- **完成后**：**归档 Plan 008**。

### 2.2 skill 缺口修复（MVP 前置）

- **仓库**：auto-ai（`crates/auto-ai-agent/src/agent.at` + retranspile）
- **现状**：`skills_block` 字段（agent.at:171）和 `build_system_prompt` 注入管线（:348,459）都在，
  但 `register_skill_tool` 方法**完全没写**（注释 :185-188 说"Set by register_skill_tool"却无定义）。
- **动作**：在 agent.at 加 `fn register_skill_tool(tool SkillTool)`：设 `self.skills_block =
  Some(tool.available_skills_block())` + 转发 `register_shared`。重跑 retranspile。
- **工作量**：S
- **验证**：转译后 `rust/src/agent.rs` 含该方法；grep 确认 skills_block 非 None。

### 2.3 tool 缺口修复（MVP 前置）

- **仓库**：auto-ai（`crates/auto-ai-agent/src/agent.at` 或 `auto-ai-cli/src/main.rs`）
- **现状**：转译版只有 `register_shared(Arc<Box<dyn Tool>>)`，缺泛型 `register_tool<T>`。
- **动作**：方案 A——在 agent.at 加值类型 `register_tool(tool Tool)`（Auto spec 可接受值类型，内部 box）。
  方案 B——改 CLI 的 8 处调用为 box。倾向方案 A（对调用方透明）。
- **工作量**：S
- **验证**：`cargo check -p auto-ai-cli`（临时指向转译版）通过。

### 2.4 ai-config + auto-ai-client 功能对齐审计

- **仓库**：auto-ai
- **动作**：对 1.2/1.3 转译产物，diff 公共 API（`pub fn/struct/enum`）vs rust-ref，逐项核对行为。
- **工作量**：S×2
- **验证**：API 一致；无功能缺口。这 2 个 crate 达"转正条件"（虽本轮不转正）。

---

## 第三波：MVP 验证核心（M，这一波的重头戏）

> 依赖第二波。完成后：**打 tag，Plan 015 闭环**。

### 3.1 agent role soul 修复（运行时 fs 加载）

- **仓库**：auto-ai（`crates/auto-ai-agent/`）
- **现状**：14 个内置角色 .at 的 SOUL 是占位符（"Soul of the Coder"）；`resources/souls/*.md`
  只在 rust-ref/ 下。Auto 无 `include_str!`。
- **方案**（用户选定：运行时 fs 加载）：把 14 个 md 拷到 .at 树（如 `src/resources/souls/`），
  在 `load_builtin` 时用 `a2r_std::fs::read_to_string` 运行时加载。
- **动作**：(a) 拷贝 14 个 souls md；(b) 改 14 个 builtin_roles/*.at 的 SOUL 为运行时加载；
  (c) 确认 a2r_std::fs::read_to_string 可用（或加 path 解析）；(d) retranspile。
- **工作量**：M
- **验证**：转译后角色 system_prompt 含真实 soul 内容；非占位符。

### 3.2 MVP harness 验证（Rust 集成测试）

- **仓库**：auto-ai（`crates/auto-ai-agent/rust/tests/`）
- **形式**（用户选定：Rust 集成测试）：每个 harness 一个测试函数，用 mock client 驱动，可纳入 CI。
- **验证的 5 个 harness**：
  1. **tool use**：mock client 返回 tool_call → 验证工具执行 + 结果回填 + 二次请求
  2. **skill**：注册 SkillTool → 验证 system_prompt 含 `<available_skills>` + 模型可调用
  3. **agent role**：load_builtin 各角色 → 验证 system_prompt 含真实 soul（非占位符）
  4. **plan**：构造 FlowSpec → PipelineDriver 驱动 → 验证 step/handoff 事件序列
  5. **spec**：验证 Client/Role 的 spec 动态分发（Box<dyn>）+ ReAct 循环基本跑通
- **工作量**：M（5 个测试 + 脚手架）
- **验证**：`cargo test`（rust/）全绿。

### 3.3 打 tag

- **动作**：`git tag auto-mvp-v0.1 -m "..."`（具体版本号执行时定）
- **完成后**：**归档 Plan 015**（MVP 达成，剩余转正工作归 plan 017+）。

---

## 第四波（后续，不在本路线图执行范围）

记录路线，等条件成熟再做：

| 序 | 工作 | 仓库 | 工作量 | 时机/前置 |
|---|---|---|---|---|
| 4.1 | workflow 端点迁移：auto-musk 的 `/api/workflow/run` + `/stream` 迁到 relay | auto-musk | M | 独立任务，可与本路线图并行 |
| 4.2 | workflow 物理删除（rust-ref + .at + lib.rs 导出） | auto-ai | S | 4.1 完成后 |
| 4.3 | spec 流式补齐（complete_stream / 事件队列 / Auto actors） | auto-ai + auto-lang | L | 需 Auto 解析器支持 `dyn Fn` |
| 4.4 | 完善 Rust 参考版（新一轮功能开发） | auto-ai | — | MVP 打 tag 后 |
| 4.5 | 第二轮 Auto 化（凭 Rust 版生成 Auto） | auto-ai | — | Auto skill 成熟后 |
| 4.6 | 转正（plan 017）：翻转 [lib] path，删 rust-ref | auto-ai | M | 第二轮 Auto 化达成 100% 功能后 |

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
