# Plan 017: workflow 引擎退役 — 最后一个消费者迁移 + 物理删除

> **状态**：🟢 已完成 — **Phase 1 + Phase 2 全部完成**（2026-08-04）
> **仓库**：auto-ai（Phase 2 主）+ auto-musk（Phase 1 协作）
> **前置**：Plan 016 第一/二波已完成（auto-ai-cli ✅、auto-shell ✅、auto-musk relay ✅ 均已切到
> PipelineEngine）；auto-musk 的 `relay/` 模块已基于 `auto_ai_agent::orchestration`。
> **目标**：把**最后一个**仍调用 deprecated `auto_ai_agent::workflow` 的消费者（auto-musk 的
> `/api/workflow/run` + `/api/workflow/run/stream`）迁到 PipelineEngine，然后从 auto-ai
> **物理删除** `workflow` + `workflow_validator`（rust-ref + `.at` 源 + rust/ 转译产物 + lib.rs 导出）。
> **对应 016 路线图**：4.1（musk 端点迁移，M）+ 4.2（物理删除，S，4.1 完成后）。
> **编号说明**：016 路线图 4.6 曾用 "plan 017" 占位指代"转正"计划。本计划占用 017；转正计划顺延为
> plan 018（执行 Phase 2.6 时同步修订 016 路线图 4.6 行的编号）。

---

## 背景与现状（2026-08-04 核查结论）

用户疑问"workflow.rs 似乎还在使用，没有替换成新架构" —— **属实，且原因明确**：

### 1. 新架构（PipelineEngine）已存在，且是生产主线

- **引擎**：`auto_ai_agent::orchestration::PipelineEngine`（严格超集），由
  `rust-ref/src/orchestration/{pipeline,driver,flow,handoff,budget}.rs` 组成，
  经 `rust-ref/src/orchestration.rs` + `lib.rs` 导出。
  Auto 版源在 `src/orchestration/*.at` → 转译 `rust/src/{pipeline,driver,flow,handoff,budget}.rs`。
- **迁移指南**：`docs/workflow-migration.md`。
- **已迁移消费者**：auto-ai-cli（`pipeline` 子命令 + `spawn_pipeline` 工具 → `PipelineDriver`）、
  auto-shell（无 Workflow 调用）、auto-musk **relay**（`relay/mod.rs` 已 re-export orchestration 类型，
  走 `/api/run` + SSE）。

### 2. 但 workflow 引擎仍未退役，原因 = 一个外部消费者

- **auto-musk 的遗留端点**：`backend/crates/musk/src/workflow.rs` 仍 `use auto_ai_agent::{parse_at_workflow, Workflow}`，
  `server.rs:129-131` 注册 `/api/workflows`、`/api/workflow/run`、`/api/workflow/run/stream`，
  内部用 `wf.run(...)` / `wf.run_with_progress(...)` + `WorkflowEvent`（SSE）。
- 只要 musk 还在编译调用这个 API，auto-ai 的 crate 就必须继续导出 deprecated 模块 →
  **016 路线图 4.2（物理删除）被 4.1 阻塞，正是当前状态**。

### 3. auto-ai 侧现状（本仓无需改业务代码）

- `rust-ref/src/lib.rs:31-50`：`pub mod workflow;`（`#[deprecated]`）+ `pub use workflow::{parse_at_workflow, Workflow, WorkflowContext, WorkflowEvent, WorkflowResult, WorkflowStep}`。
- `rust-ref/src/workflow.rs`（1181 行）+ `rust-ref/src/workflow_validator.rs`（192 行）仍编译进 crate；
  `cargo check -p auto-ai-agent` 通过、**零 deprecation 警告** → 仓库内无内部调用方（CLI/daemon/tests 全用 pipeline）。
- Auto 版：`src/workflow.at` 是 22 行占位文档（plan 013 明确"NOT YET PORTED"，因模块已 deprecated 且依赖
  Agent ReAct 循环 + Auto 解析器缺失，收益最低而推迟）→ `rust/src/workflow.rs` 是 1 行空 stub，
  但 `rust/src/lib.rs:52-53` 仍声明 `pub mod workflow; pub mod workflow_validator;`（Assembly 脚本
  `retranspile.sh` 对每个 `.rs` 文件自动生成 `pub mod`）。即 Auto 版暴露了一个**空壳 workflow 模块**。

### 4. 结论

迁移在 auto-ai 内部**架构上已完成**（新代码全走 PipelineEngine），但**物理删除未做**，唯一阻塞点就是
auto-musk 的遗留 `/api/workflow/*` 端点。该迁移跨仓库 + 有概念映射（DAG→线性、`$var`→HandoffDocument、
`profession`→`role_id`、`condition` 跳过→无对应、SSE 事件形状变化），**属复杂迁移**，故立本计划。

---

## 迁移映射（feature-dev → FlowSpec）

`backend/crates/musk/workflows/feature-dev.at`（architect→coder→tester→reviewer）是旧引擎唯一内置工作流：

| 旧 Workflow 概念 | PipelineEngine 对应 |
|---|---|
| `workflow { steps:[relay{}] }` + `parse_at_workflow` 解析 `.at` | Rust `FlowSpec::new("feature-dev")` + `add_step()`（无需 .at 解析） |
| `profession` 字段 | `FlowStep::new(id, role_id)` 的 `role_id` |
| `depends_on`（DAG 拓扑排序） | 线性顺序（feature-dev 本就是线性链） |
| `input : "...$var"` / `output : "$var"` 字符串替换 | `HandoffDocument` 结构化传递；driver/AgentFactory 侧按模板渲染 step 任务 |
| `condition : "$test_report"`（为假则跳过 reviewer） | FlowStep **无 condition 字段** → reviewer 恒执行（tester 必有输出，行为等价）或 driver 层判断 |
| `Gate::Human`（仅 log，自动继续） | `GateType::Human`（真正暂停）— feature-dev 未用，无需处理 |
| `on_fail` / validators（review 失败回跳） | FlowStep 无 validators（plan 008 D1 缺口：generic pipeline 不做内容验证，留给 app 层）— feature-dev 未用 |
| `WorkflowEvent`（4 变体） | `PipelineEvent`（10 变体）→ relay 的 `RunEvent` 总线 / SSE |
| `wf.run()` / `run_with_progress()` | relay driver 的 `advance()` 循环 + RunEvent 广播 |

---

## Phase 1 — auto-musk：迁移 `/api/workflow/*` 到 relay（016 4.1，M）✅ 2026-08-04

> **实现**：新增 `backend/crates/musk/src/relay/feature_dev.rs`（`FlowSpec` + 每步提示词模板 +
> `$var` 替换 + reviewer `condition` 跳过，驱动 `PipelineEngine` 状态机）；`server.rs` 三个端点改走
> `feature_dev::{run,run_stream}`，响应/SSE 形状逐字不变；删除 `workflow.rs` + `feature-dev.at` +
> `pub mod workflow` + `auto_generated/workflow.rs`。
> **行为变更（有意）**：自定义 `.at` workflow 文件不再支持（旧解析器随引擎退役），仅保留内置
> `feature-dev`。测试：197 单测（+7：feature_dev 4 + server 3）+ 全部 parity 套件全绿。

- [x] 1.1 把 feature-dev 改写为 `FlowSpec`（`relay/feature_dev.rs`），步骤 architect→coder→tester→reviewer，
      提示词模板逐字保留（`$design`/`$code`/`$test_report` 多步回看），替换 `workflows/feature-dev.at`
- [x] 1.2 `server.rs::workflow_run` 走 `feature_dev::run`（PipelineEngine）；**响应形状不变** `{steps, outputs}`
- [x] 1.3 `server.rs::workflow_run_stream` 走 `feature_dev::run_stream`；SSE 事件形状兼容旧格式
      （`step_start`/`step_done`/`step_skipped`/`finished`，serde tag 逐字段对齐）
- [x] 1.4 删除 `backend/crates/musk/src/workflow.rs` + `workflows/feature-dev.at`；
      移除 `musk/src/lib.rs pub mod workflow`；清理 `auto_generated/{mod.rs,extern_impl.rs,workflow.rs}`
- [x] 1.5 Auto 源契约不变：`src/back/api.at` 的 `WorkflowResult`/路径与 Rust 端仍一一对应（TS client 生成无感）
- [x] 1.6 前端 `src/front/pages/relay.at` 消费的 `{steps,outputs}` 形状不变，无需改动
- [x] 1.7 验证：`cargo build` 0 错误；`cargo test` 197 单测 + 全部 parity 套件全绿；
      新增端到端测试 `workflow_run_end_to_end_runs_four_steps`（4 步全跑）+ 流式事件序列测试

## Phase 2 — auto-ai：物理删除 workflow（016 4.2，S）✅ 2026-08-04

> 前置：Phase 1 完成后（musk 不再 import 该 API）— 已核查：auto-musk `src/lib.rs` 无 `pub mod workflow`、
> `src/auto_generated/` 无 workflow 残留、`src/` 编译路径零 `auto_ai_agent::workflow` 符号引用。

- [x] 2.1 删 `rust-ref/src/workflow.rs`、`rust-ref/src/workflow_validator.rs`
- [x] 2.2 `rust-ref/src/lib.rs` 移除 `pub mod workflow;`、`pub mod workflow_validator;`、
      `pub use workflow::{...}`（连同 `#[deprecated]` 标注）；crate 顶层 doc 去掉 workflow 段
- [x] 2.3 `rust-ref/src/error.rs:3-5` 的 doc 链接 `[crate::workflow]` 修正（改为只提 `crate::Agent`）；
      `src/error.at` 同步修正（驱动转译产物 `rust/src/error.rs`）
- [x] 2.4 删 `.at` 源：`src/workflow.at`、`src/workflow_validator.at`；重跑 `./retranspile.sh`
      → `rust/src/workflow.rs` 与 `rust/src/workflow_validator.rs` 消失、`rust/src/lib.rs` 的
      `pub mod workflow;` 声明自动消失（`read_pub_mods()` 按 `rust/src/*.rs` 现存文件生成）
- [x] 2.5 `docs/workflow-migration.md`：删除 Rollback 一节，改为"engine 已删除、无回退路径"说明；
      表头 "Workflow (deprecated)" → "Workflow (removed)"；顶部导语加 Plan 017 Phase 2 标注
- [x] 2.6 修订 `docs/plans/016-auto-mvp-roadmap.md`：4.2 标记 ✅ 完成（4.1 已标、4.6 已是 plan 018）
- [x] 2.7 全仓核查：`grep` 引擎符号（`Workflow::`/`parse_at_workflow`/`workflow_validator::`）**全仓归零**；
      `cargo check --workspace`（0 错误）+ `cargo check -p auto-ai-agent`（0 错误，2 个无关 unused-import 警告）
      + `cargo test -p auto-ai-agent`（100 单测 + 5 mvp_harness 全绿）。转译版 `rust/` 错误 67→64
      （减少 3 个：workflow 自身转译错误消失；剩余 64 个是既有的 a2r codegen 漂移 `impl Trait`，与本计划无关）

---

## 验证清单（完成判定）

- [x] auto-musk：`cargo check`/`cargo test` 通过；3 个 workflow 端点行为与旧版等价（含 SSE 事件）— Phase 1
- [x] auto-musk：frontend relay 页运行 feature-dev 全流程输出正常 — Phase 1
- [x] auto-ai：workspace `cargo check -p auto-ai-agent` 无 error（含 deprecation 归零）
- [x] auto-ai：`cargo test`（mvp_harness 等）全绿（100 单测 + 5 集成）
- [x] auto-ai：`cargo check`（`crates/auto-ai-agent/rust/` 转译版）错误数不因本计划增加（67→64，减少 3）
- [x] 符号级确认：`auto_ai_agent` 导出的 `Workflow`/`WorkflowEvent`/`parse_at_workflow` 等已不存在

## 风险与注意

- **SSE 兼容**：旧事件只有 4 种，新 `PipelineEvent` 有 10 种 —— 1.3 需显式映射或让前端升级消费新格式；
  一次性改前端更省维护，但会扩大 1.6 的范围，需与 musk 侧确认是否有存量外部调用方。
- **`condition` 语义**：feature-dev 的 reviewer `condition:"$test_report"` 依赖 tester 输出非空；
  转成恒执行 reviewer 是行为近似（tester 步骤必产出 report），若需严格等价需在 driver 层判断。
- **`profession` vs `role_id`**：旧 `.at` 用 `profession`，新 `FlowStep` 用 `role_id`；musk 的
  ProfessionRegistry 需在 AgentFactory 里把 role_id 解析成 musk 的 Profession（relay/driver.rs 已有此逻辑）。
- **转译产物**：Phase 2.4 重跑 `retranspile.sh` 会整体重写 `rust/src/`，需确认当前转译零错误基线
  （2026-08-04 时 `cargo check` rust/ 有 7 个既有错误，与本计划无关，删除 workflow 后应保持不新增）。
- **auto-musk 是独立仓库**：Phase 1 改动在 musk 提交；Phase 2 在 auto-ai 提交。两者顺序必须
  先 musk 后 auto-ai（删除会破坏还在 import 的 musk 编译）。
