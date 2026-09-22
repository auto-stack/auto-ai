---
plan_id: PLAN-034
status: archived
feature_name: 角色直接绑定模型（有序候选链，替代 tier 间接绑定）
author: [agent]
created_at: 2026-09-22T00:00:00Z
updated_at: 2026-09-22T00:00:00Z
plan_revision: 1
completion_kind: delivered
current_step: 7
total_steps: 7
supersedes_spec_components: []
new_spec_components:
  - docs/specs/auto-ai/role-model-binding.md
touched_goals: []
---

# PLAN-034：角色直接绑定模型（有序候选链）

## 0. 变更摘要

把 agent 角色与模型的绑定从「角色 → tier → daemon tier_routing/自动派生 →
模型」的间接链，改为**角色直接声明有序模型候选链**：角色文件新增
`models : [{provider, model}, …]`，链首为主选，其余为备用；请求经
`CompletionRequest` 新字段 `model_chain` 直达 daemon，daemon 按序尝试候选，
可重试错误（429/超时）、provider 缺失、**并发池耗尽**时自动切到下一个候选。
tier 轨道整体保留为遗留兼容路径（builtin 角色、既有用户角色、老客户端不受
影响），不在本计划退役。

**绑定机制的位置结论（本次调查确认）**：tier 绑定机制实现在 **auto-ai**
仓库（`ai-config` 的 ModelTier/TierRouting/wire、`auto-ai-agent` 的 Role
spec 与角色文件解析、`auto-ai-daemon` 的 tier_router + server 候选链循环）；
**auto-os-config 不含任何绑定逻辑**，它只是 `.at` 配置文件的通用编辑器
（其 tier 相关代码仅为 UI 控件推断约定 + `/api/enums/tiers` 端点）。因此
本计划的机制改动全部落在 auto-ai，auto-os-config 仅需配套的编辑器约定。

## 1. 目标

1. 角色可在配置文件中直接、显式地绑定一个或多个模型，顺序即优先级。
2. 主模型不可用（可重试错误 / 并发池满）时自动使用备用模型，无需人工干预。
3. 模型选项来源于 daemon 配置中**所有 provider 的所有模型**（编辑器侧可
   选择），不再要求「同 tier 模型可互换」这一被实践证伪的假设。
4. 完全向后兼容：既有 `model_tier` / `model`（单模型 pin）角色、builtin
   角色、tier_routing 配置、老版本 daemon/客户端的行为逐比特不变。
5. auto-os-config 的 Roles 编辑器能编辑 `models` 字段（表格 + provider
   下拉），保存往返无损。

### 非目标（Non-goals）

- 不退役 tier 体系：`ModelTier`、`tier:` 请求 token、`tier_routing` 配置、
  `/api/enums/tiers`、TierRouter 全部保留（是否退役是后续独立决策）。
- 不改 auto-musk 的 roles-config-page（它有自己的 tier UI，读写同一 roles
  目录；新字段对其是加法不破坏——列入 §10 待澄清的后续项）。
- 不做模型能力/成本元数据驱动的自动选型（仍由人显式排序）。
- 不修复既有的 daemon `.at`/`.rs` 漂移（如 server.at 缺 warning 帧），只
  保证本计划新增代码双写两树；漂移记入已知债务。

## 2. 架构方案

现状（tier 间接绑定）：

```
role { model_tier : "mid" }                    ~/.config/autoos/roles/<name>.at
  └─ Role.model_tier()  (auto-ai-agent, role_def)
      └─ build_model_id → "tier:Mid" token     (agent.at build_request)
          └─ daemon server.at chat_completions
              └─ TierRouter.candidates_preferred(mid, preferred_provider)
                  ├─ 显式 tier_routing 配置（ai-daemon.at）
                  └─ 或从 providers 自动派生（tier 标签）
                      └─ 候选链 [(provider, model)…] → 逐个尝试
```

目标（直接绑定）：

```
role { models : [{provider : "zhipu", model : "glm-5.3"},
                 {provider : "local", model : "ornith"}] }
  └─ Role.models()  (新 trait 方法，默认 [])
      └─ build_request：models 非空 → req.model = 链首 model id
                          + req.model_chain = 全链（新 wire 字段）
         （否则回落现有 model pin / tier token，行为不变）
          └─ daemon：model_chain 非空 → 直接作为候选链（跳过 tier_router，
             不受 preferred_provider 重排——显式序优先）
              └─ 既有 fallback 循环：可重试错误 / provider 缺失 /
                 并发池 acquire 超时 → 下一候选
```

关键复用：daemon 的候选链循环（server.at:314-380）已经是通用的
「有序 (provider, model) 列表 + 逐个降级」机制，本方案只是给它增加了第三
种链来源（显式 model_chain），不动循环本身——除一处：**并发等待策略**。
现状所有候选统一 `acquire_with_timeout(30s)`；对显式链请求，链上非末位候
选改为短等待（常量 `CHAIN_SHORT_WAIT = 1s`），末位候选保持 30s 兜底。理
由：用户声明了链即表达了「并发不够就用备用」的意图，在非末位候选上等 30
秒违背该意图；末位无路可退，宁可等。tier/单模型路径的 30s 行为不变。

## 3. 技术栈

- **auto-ai**（Rust workspace；三棵树现实，见 §4）：
  - `crates/ai-config`：wire 类型（shipped = `rust-ref/src/wire.rs`；
    master = `src/wire.at`；a2r 验证树 = `rust/src/`，由
    `retranspile.sh` 重生成）。
  - `crates/auto-ai-agent`：Role spec / 角色配置 / 请求构造
    （shipped = `rust-ref/src/`；master = `src/*.at`；a2r 树 =
    `rust/src/`，含 133 个测试的 `rust/tests/transpiled_harness.rs`）。
  - `crates/auto-ai-daemon`：候选链网关（built from `src/`：a2r 组装的
    `*.rs` + 手写 `src/provider/`；`.at` master 同目录并存，已存在漂移）。
- **auto-os-config**（Vue3 + Vite 前端 `src/`，其中 `src/editor/types.ts`
  为手写宿主文件；Rust 后端 `auto-os-config-back/`；Auto 前端 master 在
  `auto/src/front/*.at`，经 `auto build` 生成）。
- 测试：`cargo test`（各 crate + a2r 子树）；os-config 侧
  `node test-*.mjs` 系列脚本。

## 4. 需求分析与背景调查

### 4.1 用户需求与授权（本次会话，2026-09-22）

- 需求原话要点：不同 model 差距大，难认定同 tier 的 model 可互换；改为
  agent 直接绑定 model——从所有 provider 获取所有 model 列表，选择一个或
  多个，更靠前的更优先（并发数不够就用备用）。
- 已授权范围：完成「tier 间接绑定 → 直接模型绑定」的转变。涉及仓库：
  auto-ai（机制）+ auto-os-config（编辑器配套，转变的必要组成部分）。
  未指定预算/自动续跑上限；未授权改动 auto-musk（见 §10）。

### 4.2 现状链路与证据（已核实）

| 环节 | 文件 | 要点 |
|---|---|---|
| tier 枚举与解析 | `crates/ai-config/src/tier.at`（shipped `rust-ref/src/tier.rs`） | Min<Lite<Mid<Pro<Max；`resolve_model_id` 就近档位回退 |
| daemon 配置 | `crates/ai-config/src/loader.at` | `DaemonConfig.tier_routing`（`TierRouteCandidate{provider,model}` 有序表）；空则 TierRouter 从 providers 按 tier 标签自动派生 |
| Role spec | `crates/auto-ai-agent/src/role_def.at:36-44` | `model_tier()` 默认 Mid；`model()` 单模型 pin（空=tier）；`preferred_provider()` |
| 角色文件 | `crates/auto-ai-agent/src/config/role_config.at` + `src/roles.at` | RoleDecl/RoleConfig 全字段 serde 往返；目录 `~/.config/autoos/roles/` |
| 请求构造 | `crates/auto-ai-agent/src/agent.at:803-840, 954-959` | `build_model_id`：pin 优先，否则 `f"tier:${…}"` token |
| daemon 网关 | `crates/auto-ai-daemon/src/server.at:263-380`（shipped `src/server.rs`） | `tier:` token → TierRouter 候选链；具体 id → 扫描 providers 找属主；fallback 循环覆盖 可重试错误/provider 缺失/并发池超时；quota 耗尽中止整链 |
| 实机配置 | `~/.config/autoos/ai-daemon.at`、`roles/assistant.at` | tier_routing 显式配置在用（zhipu/deepseek/local 三 provider）；用户角色用 model_tier |
| os-config 编辑器 | `auto-os-config/src/editor/types.ts`（手写宿主） | `inferField`：key `tier`/`model_tier` → select（`/api/enums/tiers`）；`models` 若为对象数组将自动渲染为 table（`inferColumn` 列目前均为文本） |
| os-config 后端 | `auto-os-config-back/src/core.rs` | 已有 `/api/enums/self/:mid/providers` 与 `/api/enums/self/:mid/models/:provider`（按模块名取，天然可跨模块引用 ai-daemon） |
| 次级消费者 | `auto-musk`（backend `crates/musk/src/server.rs`、frontend `roles-config-page.vue`） | 读写同一 roles 目录、展示 tier；新 JSON 字段对其加法无破坏 |

### 4.3 双树/三树构建现实（对任务分解至关重要）

- `ai-config`、`auto-ai-agent`、`auto-ai-client` 的 **Cargo 构建指向
  `rust-ref/`**（手写 Rust，shipped 行为）；`src/*.at` 是 master；
  `rust/src/` 是 a2r 转译验证树（`retranspile.sh` 重生成）。
- **既存缺口**：`src/wire.at` 的 CompletionRequest 落后于
  `rust-ref/src/wire.rs`（缺 `preferred_provider` 字段，rust-ref:174 有）。
  本计划 T-01 加 `model_chain` 时顺手补齐，避免新字段二次漂移。
- `auto-ai-daemon` 的 Cargo 构建 `src/lib.rs`（a2r 组装 `*.rs` + 手写
  `src/provider/`）；`server.at` 缺最近 commit 的 warning 帧（漂移先例）。
  本计划新增 daemon 代码**双写 `.at` 与 `.rs`**，既有漂移不处理。

### 4.4 兼容性判断依据

- `CompletionRequest` 无 `deny_unknown_fields`：老 daemon 反序列化带
  `model_chain` 的请求会忽略新字段、按 `req.model`（=链首 model id）服务
  ——优雅降级为「只用主模型」，不报错。
- serde trait 默认方法：`Role` 增 `models()` 默认实现不破坏任何现有实现
  者（含 auto-musk 的 MockRole）。
- RoleSummary/JSON API 新增字段是加法，前端忽略未知键。

## 5. 详细设计

### 5.1 角色文件字段与优先级

```at
role {
    name : "coder"
    models : [
        { provider : "zhipu",    model : "glm-5.3" },
        { provider : "deepseek", model : "deepseek-v4-pro" }
    ]
    # 其余字段不变；model / model_tier 仍可单独出现
}
```

- 解析优先级（agent 侧，`build_request`）：**`models`（非空）> `model`
  （单模型 pin，遗留）> `model_tier`（tier token，遗留）> builtin 默认**。
- `models` 与 `model_tier` 允许共存（迁移期），序列化往返两者都保留；
  spec 明确 models 胜出。
- builtin 角色继续只声明 tier（跨环境可移植的默认），用户角色用 `models`
  显式覆盖。
- 链语义：有序、允许同一 provider 出现多次（不同模型）；daemon 不校验
  model id 是否声明于该 provider 的 models 列表（与既有具体 id 请求路径
  一致，透传给 provider）；无效项（provider 不在配置）在 fallback 循环中
  天然跳过并计入 last_error，全链失败 → 502（沿用现有报文）。
- `preferred_provider` 与显式链：**链非空时忽略 preferred_provider**
  （显式序即用户意图）。

### 5.2 wire 扩展（ai-config）

```rust
/// 候选链条目：provider 名 + 该 provider 下的具体 model id。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelCandidate {
    pub provider: String,
    pub model: String,
}

pub struct CompletionRequest {
    // …既有字段…
    /// 有序模型候选链（链首为主选）。空 = 不启用（走 model/tier 既有路径）。
    /// 老 daemon 反序列化时忽略（serde default）；序列化空链时省略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_chain: Vec<ModelCandidate>,
}
```

- 约定：链非空时 `req.model` 必须等于 `chain[0].model`（供老 daemon 降级
  与展示）；daemon 路由只看链。
- 落点：`rust-ref/src/wire.rs`（shipped）+ `src/wire.at`（master，连同补
  `preferred_provider` 既存缺口）+ `rust/src/`（retranspile）。

### 5.3 agent 层（auto-ai-agent）

- `Role` spec（role_def.at / rust-ref role_def.rs）：新增
  `fn models() -> Vec<ModelCandidate> { vec![] }`。
- `RoleDecl`/`RoleConfig`（config/role_config）：新增
  `models : Option<Vec<ModelCandidate>>`（serde default/skip 同 5.2 风格），
  parse/serialize 往返；`merge_over`：Some 覆盖。
- `ConfigRole.models()`：`cfg.models ?? []`；`profession_to_config`
  透传（非空才 Some）。
- `RoleSummary` 增 `models : Vec<ModelCandidate>`（默认 `[]`，JSON 加法）。
- `build_request`（agent.at / rust-ref agent.rs）：
  1. `role.models()` 非空 → `model = chain[0].model`，
     `model_chain = chain`；
  2. 否则 `model()` pin 非空 → 现状不变（`model_chain` 空）；
  3. 否则 tier token，现状不变。
- compaction（compaction.at / rust-ref compaction.rs）：摘要请求的模型
  入参从 `model: &str` 扩为携带选择（model + chain，或直接传
  `&[ModelCandidate]`），调用点仅 crate 内，同步改签名；压缩请求同样获得
  链降级。

### 5.4 daemon 层（auto-ai-daemon）

`chat_completions`（server.at + server.rs 双写）候选链构建增一分支：

```text
if req.model_chain 非空:
    candidates = model_chain.map(c => (c.provider, c.model))
elif req.model 以 "tier:" 开头:   # 现状不变
else:                            # 具体 id 找属主，现状不变
```

fallback 循环内并发等待（唯一的行为改动，仅作用于显式链请求）：

```text
wait = (显式链 且 当前候选非末位) ? CHAIN_SHORT_WAIT(1s) : 30s
```

- 常量 `CHAIN_SHORT_WAIT: Duration = 1s` 定义于 server 模块，注释说明
  设计理由（§2）；后续如需可配置再提。
- 其余循环语义逐比特不动：quota 耗尽中止整链、可重试错误换下一个、非可
  重试立即 502、SSE 流式分支照旧走 streaming_response（流式请求链首即成
  功——流开始后无法换模型，与现状一致）。

### 5.5 auto-os-config（编辑器配套）

- **零改动部分**：roles 集合模块（registry 已注册 `roles`/`role`）+
  通用编辑器已自动把 `models : [{provider,model}…]` 渲染为表格（数组同构
  对象 → table），本计划上线当天角色文件即可被无损编辑。
- **T-05 增强**：`src/editor/types.ts` 的 `inferColumn`/`inferField` 新
  约定——`models` 表内 `provider` 列 → select，选项
  `optionsFrom {kind:'self-providers', moduleId:'ai-daemon'}`（端点已存
  在，跨模块按名引用）；`model` 列 v1 保持文本输入。若表格单元格尚不支持
  select 控件，扩展 `auto/src/front/config_editor.at`（master）并重生成。
  `model` 列行级联动（按所选 provider 过滤其模型下拉）为 T-07 可选增强，
  需要新的行级上下文机制，不阻塞验收。
- README 的 inferField 约定表补一行（models → 表格 + provider 下拉）。

### 5.6 规范增量

| delta_id | add/modify/retire | docs/specs/... target | before/after rule | rationale | acceptance IDs |
|---|---|---|---|---|---|
| SD-01 | add | `docs/specs/auto-ai/role-model-binding.md` | before：无该 spec（角色-模型绑定行为散落在 role_def/agent/server 代码注释）；after：成文规范——`models`/`model`/`model_tier` 三字段语义与优先级、候选链降级规则（含 CHAIN_SHORT_WAIT 并发策略）、wire 字段 `model_chain` 契约（链首=主选、req.model=chain[0]、preferred_provider 让位）、兼容矩阵（老 daemon/老角色/老客户端）、「tier 轨道保留不退役」决策 | 绑定行为是跨 crate（ai-config/agent/daemon）契约，现状无成文 spec，本次变更是沉淀时机 | AC-01…AC-05, AC-07 |
| SD-02 | modify | `D:/autostack/auto-os-config/README.md`（inferField 约定表） | before：约定表无 models/provider 列规则；after：补「`models`（provider/model 对象数组）→ 表格，provider 列下拉取 ai-daemon providers」一行 | 编辑器约定表是 os-config 通用编辑器的既成文档面 | AC-06 |

## 6. 测试设计

- **ai-config**（rust-ref 单测 + a2r 树重转后全绿）：
  - serde 往返：`model_chain` 序列化/反序列化；空链省略输出。
  - 兼容：带 `model_chain` 的 JSON 反序列化进「旧字段集结构体」不报错
    （模拟老 daemon，验 unknown-field 容忍）。
- **auto-ai-agent**（rust-ref 单测 + `rust/tests/transpiled_harness.rs`）：
  - `parse_at_role`：含 `models` 块解析正确；`serialize_at_role` 往返
    无损；models 与 model_tier 共存时两者都保留。
  - `build_request`（harness 捕获请求断言）：三档优先级（models>model>
    tier）；models 非空时 `req.model == chain[0].model` 且
    `req.model_chain` 全链；models 空时行为与现状一致（回归）。
  - compaction 请求携带链。
- **auto-ai-daemon**（src/server.rs 测试模块，现有 64 例保持绿）：
  - 链分支：model_chain 请求 → 候选顺序正确；链首可重试失败 → 次选成
    功，响应 model_meta 反映实际服务模型；provider 无效项跳过；全链失败
    → 502。
  - 并发降级：主 provider 池饱和（复用现有 pool 测试基建，占坑后短等待
    即切）→ 次选服务；末位候选仍走 30s 路径（不实测 30s，断言等待选择
    逻辑）。
  - 回归：无链的 tier 请求、具体 id 请求行为不变；空链等价无链。
- **auto-os-config**：
  - 后端：若 T-05 涉及端点/约定测试，沿用 core.rs 既有测试模式。
  - 前端：`node test-generic-editor.mjs` 既有模式新增用例（models 表格
    渲染 + provider 列 select 选项来自枚举 + 保存载荷形状）；README 表
    更新人工核对。

## 7. 验收标准

- **AC-01 显式链端到端降级**：角色声明 2 候选链，链首 provider 返回可重
  试错误（429/超时）→ 请求由次选服务成功。验证：daemon server 单测
  （mock provider 注入失败）+ agent 侧 build_request 单测。
- **AC-02 并发耗尽快速降级**：链非末位候选的并发池满时，daemon 在
  CHAIN_SHORT_WAIT（1s）内切换下一候选，而非等待 30s。验证：daemon 单测
  （池占坑 + 短等待断言）。
- **AC-03 既有行为逐比特回归**：仅 `model_tier`、仅 `model`、两者皆无
  （builtin 默认 Mid）的角色产生的请求与改动前一致；无 `model_chain` 的
  请求在 daemon 走原路径。验证：agent/daemon 现有测试全绿 + 新增回归例。
- **AC-04 wire 向后兼容**：带 `model_chain` 的请求 JSON 可被旧字段集结构
  体反序列化（老 daemon 忽略新字段不报错，按 `req.model` 服务）。验证：
  ai-config serde 单测。
- **AC-05 角色文件往返无损**：`models` 块经 `parse_at_role ↔
  serialize_at_role` 往返保持等价；与 `model_tier` 共存时两字段都保留。
  验证：agent 单测。
- **AC-06 os-config 可编辑**：Roles 编辑器中 `models` 渲染为表格，行可
  增删，provider 列为下拉（选项=ai-daemon providers），保存后 `.at` 文件
  含合法 `models` 块且重新打开无损。验证：os-config 前端测试脚本。
- **AC-07 全绿**：`cargo test --workspace`（auto-ai，含 daemon 64+、agent
  全部）+ a2r 子树（`crates/ai-config/rust`、`crates/auto-ai-agent/rust`
  的 133+）+ os-config 测试脚本全部通过。

## 8. 执行步骤

> 执行于专属 worktree（auto-ai：`D:/autostack/.wt/ai-034/auto-ai`，分支
> `plan-034-dev`；os-config 改动在其自身 worktree/分支，work 阶段建立）。

- **T-01 wire 扩展（ai-config）** [x]
  证据（work 94de4e5）：rust-ref wire.rs 增 ModelCandidate(Eq/Ord)+model_chain(serde default/空链省略)+with_model_chain(链首锚定)；wire.at 同步并补 preferred_provider 既存缺口；a2r 重转；`cargo test -p ai-config` 43 绿(+3)；workspace check 0 错。AC-04 证毕。
  依赖：无。落点：`crates/ai-config/rust-ref/src/wire.rs`、
  `crates/ai-config/src/wire.at`（含补 `preferred_provider` 既存缺口）、
  `crates/ai-config/rust/src/`（`retranspile.sh`）。
  内容：`ModelCandidate` + `CompletionRequest.model_chain`（serde
  default/skip）；`single()` 等构造默认空链。
  验证：`cargo test -p ai-config`；`cd crates/ai-config/rust && cargo
  test`。关联 AC-04。
- **T-02 角色配置层（auto-ai-agent）** [x]
  证据（work 89fd09b）：RoleDecl/RoleConfig.models serde 双向（auto-val 桥实测支持 Vec<struct>）；Role spec models() 默认空；ConfigRole/RoleSummary/profession_to_config/merge_over(整链替换)/inherit 透传；role_config.rs 4 新测试 + a2r role_ser_bridge 全字段往返扩链。`cargo test -p auto-ai-agent` 119+18 绿、a2r 6 绿。AC-05 证毕。
  依赖：T-01。落点：`src/config/role_config.at`、`src/role_def.at`、
  `src/roles.at` + `rust-ref/src/` 对应文件 + a2r 树重转。
  内容：RoleDecl/RoleConfig.models、Role spec `models()`、
  ConfigRole/`profession_to_config`/RoleSummary 透传、merge_over 规则、
  serde 往返。验证：`cargo test -p auto-ai-agent` + a2r 树测试。
  关联 AC-05。
- **T-03 请求构造与压缩链（auto-ai-agent）** [x]
  证据（work 89fd09b）：build_request 三档优先级（models>pin>tier；链非空 req.model=chain[0].model+全链，无链路径逐比特不变）；compaction/try_compact 签名扩链；mvp_harness +4、transpiled_harness +4 捕获断言。rust-ref 119+22 绿、a2r 28+6 绿。AC-01(agent 半)、AC-03(agent 半) 证毕。既存账：live_run.rs 基线缺 thinking_level 编译断裂一并修复。
  依赖：T-02。落点：`src/agent.at`、`src/compaction.at` +
  `rust-ref/src/agent.rs`、`rust-ref/src/compaction.rs` + a2r 树。
  内容：build_request 三档优先级与 model_chain 填充；compaction 签名
  扩链并同步 crate 内调用点。验证：transpiled_harness 新增捕获断言。
  关联 AC-01（agent 半）、AC-03。
- **T-04 daemon 候选链分支（auto-ai-daemon）** [x]
  证据（work e8ac394）：chat_completions 候选构建增 model_chain 分支（跳过 TierRouter/preferred_provider）；CHAIN_SHORT_WAIT=1s 非末位短等，permit_wait 纯函数；server.at/server.rs 双写；daemon 71 绿（64+7：链序降级/无效跳过/全链502/preferred让位/池饱和<15s/选择矩阵/空链回归），a2r 重转绿。AC-01(daemon 半)、AC-02、AC-03(daemon 半) 证毕。
  依赖：T-01。落点：`src/server.at` + `src/server.rs`（新增代码双写）。
  内容：model_chain 分支、CHAIN_SHORT_WAIT 非末位短等待、preferred
  _provider 让位、无效项跳过。验证：`cargo test -p auto-ai-daemon`
  （64+ 保持绿）。关联 AC-01、AC-02、AC-03。
- **T-05 os-config 编辑器约定** [x]
  证据（os-config work 9373f76，分支 plan-034-dev @ D:/autostack/.wt/ai-034/auto-os-config）：inferColumn provider 列→select（跨模块 ai-daemon 枚举，types.ts 与双份 api.ts byte 兼容三写）；warmEnumsText 顶层表格预热补齐（原 void c 占位）；config_editor.at 表格 select 单元格（镜像 cell-select 先例）重生成 ConfigEditor.vue（diff 16 行聚焦）；README 约定表补一行（SD-02）。vue-tsc+vite build 过、back 41 绿。浏览器 e2e 归 T-06。
  依赖：T-01（字段形状定稿）之后即可，与 T-02/03/04 并行。落点：
  `auto-os-config/src/editor/types.ts`（宿主手写）、必要时
  `auto/src/front/config_editor.at`（master）重生成、README 约定表。
  内容：`models` 表 provider 列 → select（self-providers of ai-daemon）
  约定；表格 select 单元格支持（若缺）。验证：`node
  test-generic-editor.mjs` 系列。关联 AC-06、SD-02。
- **T-06 端到端串联验证** [x]
  证据（os-config work 0d6671b）：隔离栈实测（未触真机 ~/.config/autoos）——
  (a) daemon 腿：worktree 构建 aaid :17998 + mock OpenAI :18444 + deadp(拒连)，
  链请求 deadp→mockp 降级成功 content=pong、model_meta 回填 {id:m2,window:128000}；
  tier:mid 走 tier_routing 降级回归不变；具体 id m2 属主扫描回归不变。
  (b) UI 腿：隔离 USERPROFILE 根起 back :17701 + vite :17700，
  test-plan034-models.mjs 四断言 PASS（models 表渲染/provider 列 select 喂
  [deadp,mockp] 枚举/保存往返无损/重开显编辑）。既存修复：collection_browser
  表格三处理器补 store.MarkDirty（单元格编辑从不置 dirty、Save 永不可达的
  既有缺口，挡 AC-06 保存路径）。测试角色建在隔离根，真机未动。
  依赖：T-01…T-05。内容：本地 `~/.config/autoos/roles/` 加一个双候选
  测试角色（主选指向可制造 429/池满的 provider，次选指 local），aaid 重
  启后实测降级与 model_meta 回填；确认既有 tier 角色不受影响后清理测试
  角色。验证：手工 smoke + 记录证据。关联 AC-01、AC-02、AC-03。
- **T-07 spec 沉淀与收尾** [x]
  证据（work 71df9bf）：docs/specs/auto-ai/role-model-binding.md（SD-01 全
  要素）；KNOWN-DEBT 增两条（server.at 缺 plan073 warning 帧漂移、musk
  roles 页未适配+整文覆盖丢链风险待确认）；AC-07 终跑全绿——workspace 308
  （43+119+22+49+4+71）+ 四 a2r 树绿 + os-config back 41 绿 + 浏览器 e2e
  PASS。顺手修 auto-ai-cli 两例 run_ash_script 缺 skip 守卫（PLAN-033 测试
  bug，无 ash 环境基线即红，AC-07 门禁暴露；主检出 main HEAD 复现实锤）。
  依赖：T-01…T-06。内容：撰写 `docs/specs/auto-ai/role-model-binding.md`
  （SD-01）；全仓测试终跑（AC-07）；更新 KNOWN-DEBT（daemon .at 漂移、
  musk roles 页未适配两项）。关联 AC-07、SD-01。

## 9. 复审记录

- 2026-09-22（**merge 回执 PLAN-034:r1**）：
  - `prepared` ✓：reviewed 基线 = r1 pass @ 71df9bf（auto-ai，spec 冻结
    blob 143415087384e178a3b7581d2436ee3b7b995c5c）+ 0d6671b（os-config）；
    main 超前核对——auto-ai 630a98d..main 仅本计划簿记
    （93051ef/57eb44a，只触 docs/plans/034-*.md）；os-config 8fe4cdc..main
    = PLAN-041 T-10（93b2d7b，只触 auto/src/front/desktop_page.at，与本计划
    9 文件零重叠）——review 证据无需刷新。
  - `landed` ✓：两仓各自 rebase（旧→新映射：auto-ai 94de4e5→3626f18、
    89fd09b→ab4a1db、e8ac394→997c2d7、71df9bf→**360ca89**；os-config
    9373f76→c3ea27b、0d6671b→**62ed10f**），`git range-diff` 六提交全部
    `=`（逐补丁等价，安全重写证明）；main 均以 `--ff-only` 落地无 merge
    commit——auto-ai main tip = 360ca89，os-config main tip = 62ed10f。
    spec `docs/specs/auto-ai/role-model-binding.md` 在 main 且 blob 哈希与
    冻结值一致。冒烟：auto-ai main 上 `cargo test -p ai-config -p
    auto-ai-daemon` = 43+71 绿；os-config main 上 `npm run build` 绿，
    back `cargo test` 在主检出被**外部 WIP** 挡住（os-config 主检出的
    `../../auto-lang` 解析到 auto-lang 主检出，其工作区有他会话 WIP：
    Cargo.toml 重复 `iced` 键致 manifest 解析失败）——已在同提交 62ed10f
    的 worktree（依赖解析到干净的 ai-034/auto-lang 组兄弟）复验 back 41 绿
    + 前端构建绿；落地内容与已验证内容逐字节相同（range-diff 全等）。
    auto-lang 主检出 WIP 属他会话所有，未触碰，已向用户汇报需其 own 路由。
  - `ledger_refreshed` = **N/A（有据，沿 PLAN-033 先例）**：本仓库无 musk
    式 ledger/SpecsDocument 派生视图；canonical（docs/specs/auto-ai/
    role-model-binding.md）与 os-config README 约定表（SD-02）均已随分支
    落地到各自 main。
  - `archived` ✓：2026-09-22 `git mv` 至
    `docs/plans/archive/034-role-direct-model-binding.md`（本仓库归档目录
    为 `archive/`），`status: archived` + `completion_kind: delivered`。
  - `cleaned`：（见下方补记）

- 2026-09-22 review（[agent]）：
  `stage: review | plan_id: PLAN-034 | plan_revision: 1 | outcome: pass`
  `reviewed_commit`: auto-ai `plan-034-dev` 71df9bf（base 630a98d = main）；
  os-config `plan-034-dev` 0d6671b（base 8fe4cdc = main）。
  `dependency_revisions`: ai-034/auto-lang = master bebd09387（零改动检出，
  wt-guard 三树 clean；两 worktree 无脏区，实现全部已提交）。
  `spec_inputs`: docs/specs/auto-ai/role-model-binding.md（新建，blob 冻结
  143415087384e178a3b7581d2436ee3b7b995c5c @ 71df9bf）；SD-02 = auto-os-config
  README.md inferField 约定表 @ 9373f76。
  `acceptance_results`（本会话重建重放，非采信实施期汇报）：
  AC-01 pass（daemon explicit_chain_routes_in_order_and_meta_follows ok +
  agent harness_model_chain_sets_head_and_full_chain/…wins_over_pin ok；
  隔离栈实测 deadp→mockp 降级+meta 回填见 work 账）；
  AC-02 pass（explicit_chain_saturated_non_last_falls_through_fast ok，本轮
  重放 1.03s——1s 短等待实证；permit_wait 选择矩阵 ok）；
  AC-03 pass（agent harness_no_chain_pin_and_tier_unchanged ok + daemon
  empty_chain_and_concrete_id_unchanged ok + 既有 tier/meta 回归例全绿）；
  AC-04 pass（ai-config model_chain_tolerated_by_old_field_set /
  empty_is_skipped_on_wire / roundtrip_and_head_pin 三例 ok）；
  AC-05 pass（role_config models 四例 ok + a2r role_ser_bridge 6 例 ok）；
  AC-06 pass（test-plan034-models.mjs 重放：四断言 PASS + 无页面错误）；
  AC-07 pass（workspace 308 全绿重放：43+119+22+49+4+71；ai-config/client/
  daemon a2r 树 ok、agent a2r t34 4+bridge 6 ok——同 commit 71df9bf 无变
  更，T-07 全量跑证据复用）。
  `findings`: 无阻断。备注三点：① 本 review 与实施同会话，独立性受限——
  已以工件重建裁定（全部 AC 重放 + diff 抽查 + spec 对照），未采信实施期
  自述；② musk 兼容性（Role 默认方法不破坏 MockRole）为 Rust trait 默认方
  法语义推证 + musk 本计划零改动，未实测 musk 构建（其 worktree musk-084
  归他人所有）；musk 整文覆盖写回丢链风险已登记 KNOWN-DEBT 待用户决策；
  ③ 顺带修复 4 项（wire.at preferred_provider 缺口 / live_run.rs 基线编译
  断裂 / os-config 单元格 dirty 缺口 / cli ash 测试守卫）均已核实为基线
  缺陷（main HEAD 复现）且修复最小化，不属范围收敛。
  `evidence`: 本记录所列命令与测试名均可在 worktree 复现；spec 冻结哈希
  如上；diff 范围核对 ai-config 3 / agent 20 / cli 1 / daemon 3 / docs 2 +
  os-config 9 文件，与 §8 落点一致。
  `next`: merge（auto-plan-merge；worktree 保留）。

- 2026-09-22 work handoff（[agent]）：
  `stage: work | plan_id: PLAN-034 | plan_revision: 1 | outcome: pass`
  `code_commit`: auto-ai `plan-034-dev` 71df9bf（基 630a98d，T-01 94de4e5 →
  T-02/03 89fd09b → T-04 e8ac394 → T-07 71df9bf）；auto-os-config
  `plan-034-dev` 0d6671b（基 8fe4cdc，T-05 9373f76 → T-06 0d6671b）；
  依赖组 ai-034/auto-lang = master bebd0938 零改动检出。
  `task_ids`: T-01…T-07 全清（7/7），AC-01…AC-07 全证，SD-01/SD-02 就位。
  `evidence`: 见各任务 [x] 证据行；AC-07 终跑 workspace 308 + 四 a2r 树 +
  back 41 + 浏览器 e2e PASS；隔离 e2e 链降级 + model_meta 回填实录。
  `blockers`: 无。
  `next`: review（auto-plan-review；worktree 保留）。
  执行期顺带记录：① ai-config wire.at 补齐 preferred_provider 既存缺口；
  ② rust-ref live_run.rs 基线缺 thinking_level 编译断裂修复；③ os-config
  collection browser 单元格编辑 dirty 缺口修复；④ auto-ai-cli 两例 ash 测
  试缺 skip 守卫修复（均已在对应提交注明）。

- 2026-09-22 draft handoff（[agent]）：
  `stage: new`，PLAN-034，revision 1。
  `outcome: pass`——契约完整（目标/设计/AC/T 覆盖全部 SD 与 AC），路径
  均已对照仓库核实，可在已记录授权（§4.1）范围内移交 work。
  `next: work`（auto-plan-work；worktree 按§8 建立）。变更的任务/验收
  ID：T-01…T-07 / AC-01…AC-07 / SD-01、SD-02。

## 10. 待澄清事项

1. **auto-musk roles-config-page 适配**（tier 下拉 → 增加 models 展示/
   编辑）：不破坏但会看不到新字段。建议后续独立小计划处理；owner：用户
   决定优先级。
2. **os-config `model` 列行级联动下拉**（按行 provider 过滤其模型，需
   编辑器新增行级上下文机制 + 全模型枚举端点）：T-05 的 v1 不含，是否
   要求本计划内完成由用户定；默认后续增强。
3. **CHAIN_SHORT_WAIT=1s 的取值**：设计默认 1s（§5.4），已按常量实现
   （daemon server 模块 `CHAIN_SHORT_WAIT`，permit_wait 纯函数选择）；是
   否需要更短（0s 立切）或做成 daemon 配置项——复审时确认，默认维持常量。
4. **tier 体系退役时间表**：本计划明确不退役；若后续退役，涉及
   builtin 角色、tier_routing、/api/enums/tiers 与 musk UI 的清理，另立
   计划。
