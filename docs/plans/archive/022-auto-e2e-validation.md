# Plan 022: Auto 版 e2e 验证与行为补全 — 让转译版 agent 与 Rust 原生版行为等价

> **状态**：✅ 完成（2026-08-07 归档；follow-up 2026-08-07 完成）— 转译版 agent 行为等价验证 + 2 缺陷修复 + 2 架构性限制根因修复并消费
> **仓库**：auto-ai（主）—— 纯 auto-ai 侧工作，**不依赖 auto-lang**
> **前置**：Plan 021（已归档，三大功能缺口已解决，tag `auto-complete-v0.1`）
> **目标**：让 Auto 版（转译版 `rust/`）的 agent 后端能 e2e 完整运行，并通过系统化的 mock 套件 +
> live e2e runner 验证其行为与 Rust 原生版（`rust-ref/`）完全一致；补全 2 个已确认的真行为缺陷。
> **非目标**（明确排除）：转正（翻转 `[lib] path`）—— 那是 Plan 021 之后的独立计划，本计划双树并存。
> **验证方式**：Mock 套件（离线、可重复）+ Live E2E runner（真实 daemon + LLM）并重。

---

## 0. 背景与关键认知（调研结论）

### 0.1 当前架构事实（调研确认）

```
auto-ai-react 二进制（转译版）
  → 转译版 Agent / ReAct 循环（rust/src/agent.rs）        ← 本计划验证主体
  → 手写胶水 StreamingAiClient（rust/src/client_impl.rs）  ← 桥接层
  → 真实 rust-ref AiClient（已被 131+ 测试覆盖）            ← 非本计划范围
  → HTTP → aaid daemon → LLM 提供商                         ← 非本计划范围
```

**关键发现**：转译版 `auto-ai-client/rust/` 和 `ai-config/rust/` 的代码是**死代码**（不在运行链路里）——
转译版 agent 通过 `client_impl.rs` 直接用真实 rust-ref 的 `AiClient`/`ai-config`。因此：

- **e2e 验证的主体是转译版 agent 层**（`crates/auto-ai-agent/rust/src/`）
- client/config 层已由 rust-ref 的 131+ 测试覆盖，本计划**不重复验证**，只在 e2e runner 中间接验证它们的协同

### 0.2 行为差异分类（调研确认的 5 项）

| # | 差异 | 性质 | 处理 |
|---|---|---|---|
| 1 | `register_skill_tool` 只存 block 不注册 tool（agent.at:302-304） | **真 bug，.at 可修** | **Phase 3 修复** |
| 2 | `preferred_provider` 硬编码 None（agent.at:517） | **真 bug，.at 可修** | **Phase 3 修复** |
| 3 | `Thinking`/reasoning 事件缺失 | 架构性（第 5 项下游） | 记录为主（Phase 5） |
| 4 | `register` vs `register_shared` 签名不同 | 非问题（行为等价） | 不处理 |
| 5 | ReAct 循环用非流式 `complete()`，无 per-token 流式 | 架构性（Auto 不能表达 `Arc<dyn Fn>`） | 记录为主（Phase 5） |

### 0.3 固有的 API 分叉（Auto 语言限制，非 bug，本计划接受）

转译版 trait 形状与 rust-ref 不兼容，这是 Plan 021 既定架构，本计划**不改**：

- `Tool`/`Role`/`Client`：`&str`→`String`、`&JsonValue`→`JsonValue`（owned）、无 `Send+Sync`
- `Agent` 用 `Box<dyn>` 而非 `Arc<dyn>`、`new_shared` 而非泛型 `new`
- 枚举用 tuple variant 而非 struct variant；整数宽度 `u64`/`usize`→`u32`
- → **测试必须针对转译版 API 全新编写**，不能复用 rust-ref 的测试

### 0.4 测试基础设施现状

- rust-ref：131+ 测试（37 ai-config + 89 agent + 5 client），用 `ScriptedClient` mock 模式（无真实 LLM）
- 转译版：**几乎为 0**（仅 docstring 里的伪测试）
- 转译版有可运行二进制 `auto-ai-react`（rust/src/main.rs），但**无 e2e runner 脚本**，无任何转译版测试
- 转译版 `rust/Cargo.toml` 已含 `async-trait`/`tokio`/`serde_json`，**写 mock 测试无需加 dev-deps**
- 转译版 mock 模式已确认可适配（ScriptedClient 按值取 `CompletionRequest`，返回 owned `CompletionResponse`）

---

## 实施路线（按依赖与可控性排序）

### Phase 1 — 转译版 Mock 测试套件（离线，验证行为等价）✅ 核心交付

> 不依赖 LLM/daemon。建立转译版的第一个自动化测试体系，逐场景对标 rust-ref 的 `mvp_harness.rs` 行为。

**新建** `crates/auto-ai-agent/rust/tests/transpiled_harness.rs`（对标 `crates/auto-ai-agent/tests/mvp_harness.rs`，但用转译版 API）。

**1.1 共享 mock 基础设施**（文件头部）

- `ScriptedClient { responses: Mutex<Vec<CompletionResponse>> }` impl 转译版 `Client`（`complete(&self, req: CompletionRequest)` 按值，弹队列）
- `ErrorClient`（恒返回 `Err(ClientError)`，测错误传播）
- `EchoTool` impl 转译版 `Tool`（`name()->String`、`execute(&self, args: JsonValue)->Result<String,ToolError>` 按值）—— 复用 `echo_tool.rs` 模式
- `TestRole` impl 转译版 `Role`（`name()->String`、`max_turns()->u32`）
- helpers：`text_response(s)`、`tool_call_response(tool, args)`

**1.2 ReAct 循环核心场景**（对标 rust-ref agent.rs 内联测试）

- [ ] T1 **工具调用→反馈→结束**：mock 返回 tool_call → agent 执行 EchoTool → 喂回 → mock 返回最终文本；断言 `result.tool_calls.len()==1` + output 正确
- [ ] T2 **纯文本对话**：mock 直接返回文本，无工具调用；断言单轮结束
- [ ] T3 **max_turns 限制**：mock 每轮返回 tool_call 永不收敛；断言 `AgentError::MaxTurnsExceeded`
- [ ] T4 **client 错误传播**：ErrorClient 恒错；断言 `Err(AgentError::Client(...))`
- [ ] T5 **多轮工具链**：tool_call → result → 再 tool_call → result → 结束；断言 2 轮、2 个 tool_calls

**1.3 Tool 注册与执行**（对标 rust-ref tool.rs 测试）

- [ ] T6 `register_tool(Box::new(tool))` 后工具可执行（name 查找、execute 调用）
- [ ] T7 `register_shared(Arc::new(tool))` 与 register_tool 行为等价
- [ ] T8 未注册工具的 tool_call → 返回 `"[tool error: ...]"`（对齐 rust-ref 行为）

**1.4 Skill 工具**（Phase 3 修复后验证）

- [ ] T9 `register_skill_tool(SkillTool)` 后 skill 工具可执行（**依赖 Phase 3.1 修复**）

**1.5 内置角色**

- [ ] T10 `load_builtin("assistant")` 返回有效 Role；`builtin_names()` 覆盖全部 14 个角色
- [ ] T11 每个内置角色的 `system_prompt()` 非占位符（>50 字符，非 "Soul of the X"）

**1.6 流式事件（EventSink actor）**

- [ ] T12 `run_stream(task, cancel, sink)` 执行后 sink 收到事件序列（StepStarted/Tool/Done 等）
- [ ] T13 cancel=true 时 run_stream 提前返回 `Cancelled`

**1.7 preferred_provider（Phase 3 修复后验证）**

- [ ] T14 TestRole 重写 `preferred_provider()` 返回 `Some("zhipu")`，构造捕获型 client 断言 request 携带偏好

**验证标准**：`cd crates/auto-ai-agent/rust && cargo test --test transpiled_harness` 全绿（14+ 测试）。

---

### Phase 2 — Live E2E Runner（真实 daemon + LLM 全链路）

> 建立 e2e 运行器，验证转译版 agent 的完整运行链路。Mock 测不到的：真实 HTTP/SSE、daemon 协议、端到端 token 流。

**2.1 新建** `scripts/e2e-transpiled.sh`（Bash runner，对齐项目既有 `scripts/` 风格）

- [ ] 2.1.1 检查前置：`ZHIPU_API_KEY`（或 `OPENAI/ANTHROPIC`）env 存在；`auto-ai-react.exe` 已构建
- [ ] 2.1.2 拉起 daemon：后台启动 `aaid`，等待 `/v1/status` 200
- [ ] 2.1.3 跑转译版 agent：通过管道喂一个固定 prompt 给 `auto-ai-react`，捕获 stdout
- [ ] 2.1.4 断言：stdout 包含预期输出；工具调用路径用 "echo the word: hello" 触发 EchoTool
- [ ] 2.1.5 清理：kill daemon 进程（trap EXIT 保证清理）
- [ ] 2.1.6 失败诊断输出：daemon 日志 + agent stderr

**2.2 双轨对照**（关键：行为一致性实证）

- [ ] 2.2.1 同一 prompt 分别跑转译版（`auto-ai-react`）和原生版（`auto-ai-cli run`，用 rust-ref agent）
- [ ] 2.2.2 对比两者输出结构（不是逐字——LLM 有随机性）：工具是否被调用、轮数、stop_reason、token 用量量级
- [ ] 2.2.3 记录到 `docs/reviews/004-transpiled-e2e-parity.md`（新建，参照 001-003 review 风格）

**2.3 live 测试纳入 CI 友好形态**

- [ ] 2.3.1 不把 live 测试加入默认 `cargo test`（需 API key，CI 不稳定）
- [ ] 2.3.2 在 `rust-ref/tests/live_run.rs` 旁建转译版 `rust/tests/live_run.rs`（`#[ignore]`，对标 rust-ref 的 live 测试形态），手动 `cargo test --test live_run -- --ignored` 触发；daemon 不可达时 soft-skip

**验证标准**：本地运行 `scripts/e2e-transpiled.sh` 成功（需 API key）；转译版与原生版输出结构对照一致。

---

### Phase 3 — 行为缺陷补全（2 个真 bug，.at 源码修复）✅ 核心交付

> 修复调研确认的 2 个真行为缺陷。修后必须 retranspile 重验。

**3.1 修复 `register_skill_tool`（agent.at）**

- 现状：`register_skill_tool(tool: SkillTool)` 只存 `skills_block`，不注册 tool（agent.at:302-304）→ skill 工具调用会 "tool not found"
- 对标 rust-ref agent.rs:259-267：既注册又存 block
- [ ] 3.1.1 改 `src/agent.at` 的 `register_skill_tool`：在存 `skills_block` 后调用 `self.register_tool(tool)`（或 `self.tools.register(tool)`）
- [ ] 3.1.2 删除/更新 agent.at:297-301 的过时注释（"caller must also register" 不再成立）
- [ ] 3.1.3 Phase 1 的 T9 验证修复生效

**3.2 修复 `preferred_provider`（agent.at）**

- 现状：`build_request` 硬编码 `preferred_provider: None`（agent.at:517）→ Role 的 provider 偏好丢失
- 对标 rust-ref agent.rs:609：`preferred_provider: self.role.preferred_provider()`
- 注意：Role trait 的 `preferred_provider()` 方法**已存在**（role_def.at:103 已正确转译），只是 build_request 没调用
- [ ] 3.2.1 改 `src/agent.at` build_request：`preferred_provider: None` → `preferred_provider: self.role.preferred_provider()`
- [ ] 3.2.2 Phase 1 的 T14 验证修复生效

**3.3 Retranspile + 回归验证**

- [ ] 3.3.1 用当前 auto.exe 重跑 `crates/auto-ai-agent/retranspile.sh`
- [ ] 3.3.2 `cd crates/auto-ai-agent/rust && cargo check` 0 错
- [ ] 3.3.3 `cd crates/auto-ai-agent/rust && cargo test` 全绿（Phase 1 套件含新增 T14）
- [ ] 3.3.4 workspace 回归：`cargo check --workspace` + `cargo test --workspace` 0 错（rust-ref 不受影响）

---

### Phase 4 — 行为等价审计矩阵（系统性对标）

> 产出一份结构化的"功能点 × 转译版状态 × 验证方式"矩阵，确保无遗漏。

**新建** `docs/reviews/005-transpiled-parity-matrix.md`

- [ ] 4.1 逐功能点对标（参照 rust-ref 的公开 API + 测试场景）：
  - ReAct 循环：工具调用、多轮、max_turns、错误传播 ✓（Phase 1 T1-T5）
  - Tool 注册：register/register_shared/register_skill_tool ✓（T6-T9）
  - Role 系统：load_builtin、内置角色 souls、preferred_provider ✓（T10-T11, T14）
  - Memory：消息历史、memory_limit 截断
  - 流式：EventSink 事件序列、cancel ✓（T12-T13）
  - Orchestration：pipeline/driver/handoff/budget/flow（对标 rust-ref 5 集成测试）
- [ ] 4.2 每个功能点标注：✅ 行为等价（有测试）/ 🟡 行为等价无测试（补测试）/ 🔴 有缺陷（Phase 3 修）/ ⚫️ 架构性限制（记录）
- [ ] 4.3 识别 Phase 1 未覆盖的功能点，决定是否补充测试

**4.4 Orchestration 场景测试补充**（如矩阵发现缺口）

- 转译版 driver/pipeline 的 mock 测试（对标 rust-ref driver.rs 的 5 集成测试）：构造 mock AgentFactory + ScriptedClient，跑 pipeline 驱动，断言 PipelineEvent 序列
- [ ] 视 Phase 4.3 结果决定是否新增 `rust/tests/transpiled_orchestration.rs`

---

### Phase 5 — 架构性限制文档化（KNOWN-DEBT 转正）

> 把调研确认的架构性限制（第 3、5 项 + 固有 API 分叉）正式记录，供未来转正计划引用。

**5.1 更新** `docs/plans/KNOWN-DEBT-AND-RISKS.md`

- [ ] 5.1.1 追加 022 行：转译版 ReAct 循环非流式（complete vs complete_stream）—— 根因 Auto 不能表达 `Arc<dyn Fn>`，转译版用 EventSink actor + 非 stream 循环；per-token Delta/Thinking 不经 StreamEvent（侧信道 `StreamingAiClient` 部分补偿）。转正前置（需 auto-lang dyn-Fn 或侧信道正式化）
- [ ] 5.1.2 追加 022 行：转译版 trait API 分叉（owned/String/u32/Box dyn/无 Send+Sync）—— 固有语言限制，转正需全量适配下游或 auto-lang 增强
- [ ] 5.1.3 追加 022 行：`Thinking`/reasoning 事件缺失（第 5 项下游）

**5.2 明确不在本计划范围的后续工作**（写入 KNOWN-DEBT "未来增强"区）

- 转正（翻 `[lib] path`）
- 第 5 项的 dyn-Fn 支持或侧信道正式化（auto-lang 工程）

---

## 完成判定

- [x] **Phase 1**：`crates/auto-ai-agent/rust/tests/transpiled_harness.rs` 15 测试全绿（`cargo test` 离线跑通，含 follow-up T15）
- [x] **Phase 2**：`scripts/e2e-transpiled.sh` + `rust/tests/live_run.rs`（`#[ignore]`）设施就绪；本环境无 API key 未实跑（review 004 记录，用户具备时运行）
- [x] **Phase 3**：`register_skill_tool` + `preferred_provider` 两 bug 修复（.at 源码 + retranspile）；T9/T14 验证通过；workspace 回归 0 错
- [x] **Phase 4**：`docs/reviews/005-transpiled-parity-matrix.md` 产出，所有功能点有明确状态标注，无 🔴 遗漏（架构性 ⚫️ 除外）
- [x] **Phase 5**：KNOWN-DEBT 更新完整
- [x] **Phase 6**：两个架构性限制经 auto-lang Plan 397 根因修复（spec supertrait + Arc<Fn> spec-param golden），并在 follow-up 由 auto-ai 侧消费（complete_stream 流式 + Send+Sync）
- [x] workspace `cargo check` + `cargo test`（rust-ref）全绿，无回归
- [x] 打 tag `auto-e2e-v0.1` 标记 e2e 验证达成

---

## 风险与注意

- **retranspile 依赖当前 auto.exe**：Phase 3 的 .at 改动需 retranspile。若当前 PATH 的 auto.exe 是旧版可能触发已知 a2r 缺陷——用 Plan 021 既定的 retranspile.sh（含 sed 兜底）即可；若新 a2r 缺陷出现，记入 KNOWN-DEBT，不阻塞（功能等价可手动验证）。
- **LLM 随机性**：Phase 2 双轨对照不能逐字比对，只比对结构（工具是否调用、轮数、stop_reason）。用确定性 prompt（"Say exactly: ..."）降低方差。
- **Live 测试稳定性**：`#[ignore]` + daemon soft-skip 模式（对标 rust-ref live_run.rs），不进默认 CI。
- **只构建 debug**（Plan 021 既定要求）：全程 `cargo build`，不跑 release。
- **转正是后续**：本计划只做验证 + 补全 + 记录，不翻 `[lib] path`，双树并存。

---

## 后续（不在本计划范围）

- **转正（翻 `[lib] path` 删 rust-ref）**：独立计划。本计划产出 parity matrix + KNOWN-DEBT 为其提供就绪度评估。
- **转译版 client/config 死代码清理或转正**：当前不在运行链路，转正时一并处理。

---

## Phase 6 — 架构性限制的 auto-lang 根因修复（2026-08-07 追加）

> **背景**：Phase 5 把两个架构性限制（限制一 per-token 流式 / 限制二 trait API 分叉）记为
> "架构性，待 auto-lang"。本 Phase 对这两者做 auto-lang 侧的**根因调研**，并据调研结果
> 重新定性、划定 worktree 工作范围。调研结论显著改变了原判断——见 6.1/6.2。

### 6.1 限制一（per-token 流式 / `complete_stream`）— 🔁 重新定性：非 auto-lang 语言缺陷

**原判断（Phase 5）**："Auto 不能表达 `Arc<dyn Fn(serde_json::Value)>` 回调参数，是架构性限制。"

**调研结论（auto-lang 侧实证）**：**此判断已过时**。auto-lang Plan 390 §15.10（2026-08-07 落地）
**已交付** `Box<Fn(...)>` / `Arc<dyn Fn(...)>` 类型机制，覆盖**所有位置**：

| 位置 | golden 验证 | 证据 |
|---|---|---|
| 结构体字段 | `12_specs/007_box_fn` | ✅ |
| free-fn 参数 | `12_specs/007_box_fn` `set_cb(cb Box<Fn(int)>)` | ✅ |
| free-fn 返回 | `12_specs/007_box_fn` | ✅ |
| actor task state 字段（闭包字面量默认值） | `22_actors/022_closure_cb` | ✅ |
| **spec 方法参数** | ❌ **无 golden**（codegen 路径与 free-fn 参数共享） | 待确认 |

- **parser 路径已通**：`parser.rs:9719` `Fn`+`(` → `parse_fn_signature` → `Type::Fn(params, ret)`，
  包在 `Arc<...>` 里得 `Type::GenericInstance{Arc, [Type::Fn(...)]}`。
- **codegen 路径已通**：`trans/rust.rs:1272-1320` 的 `Box|Arc` + `Type::Fn` 分支输出
  `Arc<dyn Fn(...) + Send + Sync>`；spec 方法参数经 `rust_param_type_name`（rust.rs:1629）→ 同一渲染器。
- **auto-ai 侧注释过时**：`agent.at:102-107, 19-34` 仍写 "Auto can't express"——这是 §15.10 之前的判断。

**真缺口（仅 2 项，都很小）**：
1. **golden 未覆盖 spec 方法参数位置**：`Arc<Fn(...)>` 作 `pub spec` 方法参数 + `#[async_trait]` 的
   组合未被任何 golden 测试验证。需补一个 golden（`12_specs/009_arc_fn_param`）锁定。
2. **auto-ai 未消费**：`agent.at` 的 `spec Client` 未加 `complete_stream` 方法；`client_impl.rs` 的
   `StreamingAiClient` 侧信道是 §15.10 之前的 workaround，可回归 spec 路径（可选清理）。

**worktree 工作范围（auto-lang，小）**：仅补 golden 确认 spec-param + async_trait 组合。
若组合渲染异常（codegen 路径共享，预期不会），才需改 `trans/rust.rs`。**无 parser/AST 改动。**

### 6.2 限制二（trait API 分叉）— 🔁 重新定性：6 子项中 3 项非缺口

**原判断（Phase 5）**："固有 API 分叉（owned/String/u32/Box dyn/无 Send+Sync）是 Auto 语言限制。"

**调研结论（6 子项逐一实证分类）**：

| 子项 | 原判断 | 真实分类 | 证据 |
|---|---|---|---|
| ① 无 `Send+Sync` supertrait | 架构性 | **✅ 真 auto-lang 缺口**（AST 无 supertrait 概念） | `ast/spec.rs:17-22` SpecDecl 无 bounds 字段；`parser.rs:8494` spec_decl_stmt `:` 后是硬解析错误 |
| ② `String` 而非 `&str` 返回 | 架构性 | **🟡 故意 codegen 选择**（可 opt-in 改） | `rust.rs:1455` 返回位 `str`→`String`（"owned, safe default"）；参数位 `str`→`&str`（rust.rs:1629），非对称是故意的 |
| ③ owned 而非引用 | 架构性 | **❌ 非缺口**（`@T` 语法已支持） | `parser.rs:10246` `@T`→`Type::Reference`；auto-ai 源码只是没用 `@` |
| ④ `u32` 而非 `usize`/`u64` | 架构性 | **❌ 非缺口**（`usize`/`u64`/`i64` 关键字已有） | `parser.rs:962`；auto-ai 源码用 `uint`→`u32`，改写 `usize` 即可 |
| ⑤ `Box<dyn>` 而非 `Arc<dyn>` | 架构性 | **🟡 codegen 默认 + 源码可控** | 裸 `Type::Spec`→`Box<dyn>`（rust.rs:1226）是默认；源码写 `Arc<T>`→`Arc<dyn T>`（§15.11，已工作） |
| ⑥ 无泛型方法（spec 内） | 架构性 | **✅ 真 auto-lang 缺口**（SpecMethod 无 type_params） | `ast/spec.rs:53-59`；`parser.rs:8559` spec_method 无 `<...>`/`#[with]`；inherent impl 方法已有泛型（`ast/fun.rs:30`） |

**结论**：6 子项中
- **① 是唯一值得在 worktree 修的真语言缺口**（Send+Sync supertrait）
- ③④ 是 auto-ai 源码层面可改的（非 auto-lang 工作，归入转正计划）
- ②⑤ 是可记录的 codegen 选择（⑥ + `'static` bound 是大工程，workaround 可接受）
- ⑥ 留作文档化限制（`*_shared` workaround 功能可用）

#### 6.2.1 子项①（Send+Sync supertrait）修复方案

**目标**：让 `.at` 能写 `pub spec Tool: Send + Sync { ... }`，a2r 输出 `pub trait Tool: Send + Sync { ... }`。

**最小改动集**（auto-lang，中等工作量，contained）：
1. **AST**（`ast/spec.rs`）：`SpecDecl` 加 `bounds: Vec<String>`（或 `Vec<Type>`）字段。`Send`/`Sync`
   当作不透明标识符处理——无需引入 Auto 侧 marker trait 概念。
2. **Parser**（`parser.rs:8494-8557` `spec_decl_stmt`）：名字后、`<...>` 后、`{` 前允许可选 `: Bound1 + Bound2`。
   用单 token 前瞻判断有无 `:`。bounds 列表用 `+` 分隔的标识符序列。
3. **Codegen**（`trans/rust.rs:13349-13487` `spec_decl`）：第 13380 行 `trait Name` 后、generics 后、
   `{` 前（13411）插入 `: <bounds verbatim>`（若有）。
4. **golden**：`12_specs/010_spec_supertrait`（`spec Foo: Send + Sync { }` → `trait Foo: Send + Sync { }`）。
5. **回归**：既有 spec 全部无 `:`，bounds 为空 Vec，行为不变（无回归风险）。

#### 6.2.2 子项⑥（spec 内泛型方法）— 记录为后续，不在本次 worktree

SpecMethod 加 `type_params` + parser 支持 `<...>`/`#[with]` + codegen（镜像 inherent 方法 rust.rs:10173）。
另需 `'static` lifetime bound 语法（目前完全不可表达）。**工作量大，`*_shared` workaround 功能可用，
ROI 低**。留作文档化限制，不在本次 worktree（除非转正时成为硬阻塞）。

### 6.3 auto-lang worktree 工作范围（最终划定）

**worktree 名**：`auto-ai-022`（前缀 `auto-ai` + 计划 id `022`，按用户约定）
**位置**：`D:/autostack/auto-lang/.worktree/auto-ai-022`
**新 plan id**：397（`397-send-sync-supertrait-and-arc-fn-spec-param.md`）

**两项实施**：
1. **子项① Send+Sync supertrait**（主工作，中量）：AST + parser + codegen + golden（§6.2.1 改动集）
2. **限制一 golden 确认**（小）：`12_specs/009_arc_fn_param`——`pub spec` 方法取 `Arc<Fn(T)>` + async，
   锁定 spec-param + async_trait + Send+Sync 组合。若渲染异常才改 codegen。

**不在本次 worktree**（记录为 KNOWN-DEBT 后续）：
- 子项②（&str 返回 opt-in）、⑤（Arc 默认）——codegen 选择，转正时再议
- 子项⑥（spec 泛型方法 + `'static`）——大工程，workaround 可用
- auto-ai 侧消费（agent.at 加 complete_stream、删侧信道 workaround）——auto-lang 合并 + 重建 auto.exe 后，
  回 auto-ai 作为 Plan 022 的 follow-up（非本 Phase 范围，因依赖 auto-lang 合并）

### 6.4 回 auto-ai 验证（2026-08-07，用 worktree auto.exe）✅

- [x] worktree auto.exe 构建（含 Plan 397，`auto-lang/.worktree/auto-ai-022/target/debug/auto.exe`）
- [x] **集成验证**：`AUTO=<worktree auto.exe> crates/auto-ai-agent/retranspile.sh check` → **0 错**，
      且转译产物**无任何变化**（git status 空）——新 auto.exe 对既有 auto-ai 代码完全向后兼容
      （supertrait 语法可选，既有 spec 无 `:` 不受影响）
- [x] KNOWN-DEBT 的 022 行更新：限制一/二 标记"已解除（auto-lang 侧）"，剩余 auto-ai 消费工作标注为 follow-up
- [ ] **auto-ai 侧消费（未做，依赖 Plan 397 合并 master）**：
      - `agent.at` 的 `spec Tool`/`spec Role` 加 `: Send + Sync`（可选，转正时必需）
      - `agent.at` 的 `spec Client` 加 `complete_stream(req, on_event Arc<Fn(JsonValue)>)` + run_inner 改用流式 + 删侧信道 workaround
      - 删 `agent.at:102-107,19-34` 过时注释（"Auto can't express" 已不成立）
      - 这些是独立 follow-up，不阻塞 Plan 022 完成（本 Phase 只做 auto-lang 根因修复 + 验证）

---

## 实施记录

### Phase 3 — 行为缺陷补全（2026-08-07）✅

**3.1 register_skill_tool 修复**：`agent.at:302-304` 原只存 `skills_block`。改为对齐
rust-ref（agent.rs:259-267）：空 block→None，非空→Some(block)，然后 `self.register_tool(Box(tool))`。
- **关键技术点**：`.at` 里 `Box(tool)`（tool 是 SkillTool 变量）转译成 `Box::new(tool)`，
  参数类型 `Box<dyn Tool>` 触发 unsizing coercion。a2r 对**变量实参**不做自动装箱（只对字面量
  结构体实参装箱，Plan 390 §11），故必须显式 `Box(tool)` 包装。
- 更新过时注释（原 "caller must also register" 不再成立）。

**3.2 preferred_provider 修复**：`agent.at:517` 硬编码 `None` → `self.role.preferred_provider()`。
Role trait 方法本已存在（role_def.at:103），只是 build_request 未调用。

**3.3 Retranspile 验证**：retranspile.sh 重跑，转译版 `cargo check` **0 错**；转译产物
`agent.rs:300` `self.register_tool(Box::new(tool))`、`agent.rs:464` `preferred_provider: self.role.preferred_provider()`
均正确。workspace 回归全绿（ai-config 37 + agent 100 + mvp_harness 5 + client 5 + daemon 36 + cli 2 = 185 测试 0 失败）。

### Phase 1 — 转译版 Mock 测试套件（2026-08-07）✅

新建 `crates/auto-ai-agent/rust/tests/transpiled_harness.rs`，**14 测试全绿**：
- 共享 mock 基础设施：`ScriptedClient`（转译版 Client，按值取 req）、`ErrorClient`、
  `CapturingClient`（共享 Arc<Mutex> 捕获 request）、`EchoTool`、`TestRole`（可配 max_turns/preferred）
- T1-T5 ReAct 循环、T6-T9 Tool 注册（含 Phase 3 修复验证）、T10-T11 内置角色、
  T12-T13 流式事件（spawn_event_sink_with 注入捕获闭包 + drain_all）、T14 preferred_provider
- **T3 偏差修正**：原预期 max_turns=2 触发 MaxTurnsExceeded，实测发现 max_turns 是 **soft target**，
  hard cap = max_turns×5（与 rust-ref 语义一致）。改用 max_turns=1（hard=5）+ 6 个 tool_call 验证 hard cap。
- mock 设计要点：CapturingClient 用 `Arc<Mutex<Vec<CompletionRequest>>>` 共享（client 移入 agent 后无法向下转型）。

### Phase 2 — Live E2E Runner（2026-08-07）✅（设施就绪，live 待 API key）

- `scripts/e2e-transpiled.sh`：拉起 daemon→轮询就绪→跑 auto-ai-react 两条路径（纯文本+工具）
  →断言→trap 清理。无 key 时正确退出码 1（已验证）；语法检查通过。
- `crates/auto-ai-agent/rust/tests/live_run.rs`：两个 `#[ignore]` 测试（one_turn + tool_call），
  daemon soft-skip。编译通过（`--no-run` 0 错）。
- **本次环境无 API key，未实跑**。详见 `docs/reviews/004-transpiled-e2e-parity.md`。

### Phase 4 — Parity Matrix（2026-08-07）✅

`docs/reviews/005-transpiled-parity-matrix.md`：9 区功能点对标。**无 🔴 遗留缺陷**。
核心 agent 行为等价（14 测试覆盖）；orchestration 经 Plan 021 审计等价（无独立测试，低优先）；
架构性限制（per-token 流式 / trait API 分叉）记录于 §8。

### Phase 5 — KNOWN-DEBT（2026-08-07）✅

追加 4 条 022 行：转译版非流式（架构性）、trait API 分叉（架构性）、register_skill_tool（已修复）、
preferred_provider（已修复）+ 1 条待验证（live e2e）。

### 完成判定核对

- [x] Phase 1：transpiled_harness.rs **14 测试全绿**（离线）
- [x] Phase 2：e2e runner + live 测试设施就绪；review 004 产出（live 待 API key）
- [x] Phase 3：两 bug 修复（.at + retranspile 0 错）；T9/T14 验证通过
- [x] Phase 4：review 005 parity matrix 产出，无 🔴 遗漏
- [x] Phase 5：KNOWN-DEBT 更新完整
- [x] workspace cargo check + cargo test 全绿（185 测试，0 失败）
- [x] 打 tag `auto-e2e-v0.1`

### Follow-up 实施记录（2026-08-07）✅

Phase 6 调研后，两个架构性限制在 auto-lang 根因修复（Plan 397）并经 auto-ai 侧消费：

**auto-lang 侧（Plan 397，worktree `auto-ai-022` 已合并 master）：**
- spec supertrait bounds（限制二①）：`spec Name: Send + Sync {}` → `trait Name: Send + Sync`（AST/Parser/Codegen + golden 010）
- Arc<Fn> spec-param golden（限制一确认）：golden 009 锁定 `Arc<dyn Fn(...)>` spec 方法参数位（Plan 390 §15.10 能力，无 codegen 改动）
- TaskRef Clone（`a6abb487`，Plan 397 follow-up）：解除 sink move 重用 E0382

**auto-ai 侧消费（follow-up，commit `5cb79a6`）：**
- complete_stream 流式：`spec Client` 加 `complete_stream(req, on_event Arc<Fn(JsonValue)>)`；`run_inner` 改用 complete_stream，回调 `Arc(move (ev) => forward_sse_delta(ev, sink_cb))` 把 SSE text delta 转 `StreamEvent.Delta`（per-token Delta 流式复活，保守增量——统一 Delta，Thinking 区分留后续）
- Send+Sync：4 个 spec（Tool/Role/Client/AgentFactory）加 `: Send + Sync`
- client_impl.rs：AiClient + StreamingAiClient impl 加 complete_stream 桥接真实 SSE
- transpiled_harness T15 验证 Delta 转发；15 测试全绿

**关键技术决策**：spec 默认体不调 self（a2r DIV-TRAIT-A2R-3）→ complete_stream 无默认体，mock 用 helper；闭包块体不支持 → 提取 forward_sse_delta 自由函数保持单表达式箭头闭包；`@TaskRef` 引用参数 + `sink.clone()` 避开 a2r move-reuse clone 推断。

**留作文档化限制（转正时再议，非阻塞）**：②`&str` 返回（codegen 故意选择，medium-large auto-lang 工程）；⑥spec 内泛型方法 + `'static`（大工程，`*_shared` workaround 可用）。详见 `docs/reviews/005-transpiled-parity-matrix.md` §8.2。
