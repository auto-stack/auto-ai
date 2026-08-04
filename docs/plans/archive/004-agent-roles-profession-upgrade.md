# Plan 004: Agent Roles（Profession 升级为可配置的 Role）

> **Status**: Approved
> **设计参考**: auto-forge 的 Profession/Soul/AgentConfig 三段式；本仓库 `docs/`（若有）
> **仓库**: `auto-ai`（核心）+ `auto-musk`（接通 + API + UI）+ `auto-os-config`（注册模块）
> **依赖**: 现有 `Profession` trait、`ProfessionConfig` .at 解析器、`ModelTier`、`ModeRegistry`
> **影响**: musk agent 解析（skills 过滤 / tier 校验 / soul 定制）、auto-os-config 第四模块

---

## 0. 背景与动机

当前 `Profession` 是 **Rust trait + 7 个编译期硬编码实现**，无法在运行时配置。auto-forge
证明了把"角色身份"做成**可编辑的配置实体**的价值：tier 范围、skills 白名单、token budget、
人设（Soul）。本计划把 AutoOS 的 Profession 升级为可配置、可 CRUD 的 **Role**。

「Profession」更名为 **「Role」**（更贴近"角色"语义，与 auto-forge 的 Profession 概念对应但更直观）。

### 用户需求（本次范围）
每个 Role 可定义：
1. **专有的 Skill 列表**（白名单，约束 agent 实际能用的 skills）
2. **Soul**（人设 markdown，个体 Agent 经由 Mode 的 `extra_system_prompt` 可定制）
3. **可选的 Tier**（多选，mode 的 tier 越界时钳制）
4. **Token Budget**（**仅存盘，暂不生效**——为将来 BudgetTracker 留字段）

### 关键技术约束（已调研确认）
- **`.at` 不存多段 markdown**：现有 `Value::Display` 不转义引号/换行，且 grammar 是行式的。
  生态已用 **sidecar `.md`** 解决（`professions/coder.rs` 即 `include_str!("resources/souls/coder.md")`）。
  → **Role 的 Soul 存为 `<name>.soul.md`，`.at` 里只存 `soul_file` 引用**。
- 现有 `.at` profession 解析器（`ProfessionConfig`）已支持 inherit / 工具覆盖，**只需扩展新字段**。
- **Auto 的编译期执行能力无法替代 proc-macro**（已确认：Auto 只能反射 Auto 代码，不能反射
  Rust struct）。因此本次**不**做 `#[derive(ToAtom)]`，改为补一个 `emit.rs` 发射器 + 手写 writer。
  derive proc-macro 留作 `auto-lang` 的独立计划（见末尾"后续计划"）。

---

## 1. 目标 / 验收标准

**功能验收（端到端）**
- [ ] auto-os-config 侧栏出现第 4 个模块 **🎭 AI Roles**
- [ ] Roles 页列出全部 Role（内置 7 + 用户自定义），内置带 🔒 标记
- [ ] 可新建/编辑 Role：填 Soul（markdown 大文本框）、勾选 Skills、多选 Tiers、设 token_budget
- [ ] 保存后 `~/.config/autoos/roles/<name>.at` + `<name>.soul.md` 落盘，重载可见
- [ ] **musk 实际生效**：一个 Role 的 skills 白名单会限制 `musk chat` 实际暴露的 skills；
      mode 的 profession 若 tier 越界，启动时 warn 并钳制；mode 的 `extra_system_prompt`
      被 append 到 Soul（修复死字段）
- [ ] token_budget 字段可填可存，但**不影响** agent 行为（明确标注"暂不生效"）

**非目标（本次不做）**
- Token Budget 强制执行（仅存盘）
- 内置 7 个 Role 改为只读数据（保持编译 trait 不变，零回归；要改就"复制为用户 Role"）
- `#[derive(ToAtom)]` proc-macro（独立计划，见 §7）
- Roles 之间不能互相 inherit 用户 Role（只能 inherit 内置）—— 首期简化

---

## 2. 数据模型

### Role 文件：`~/.config/autoos/roles/<name>.at`

```at
role {
    name : "precise-coder"
    description : "编码角色，强调 TDD"
    inherit : "coder"                       # 可选：继承内置
    model_tier : "max"                      # 默认 tier（向下兼容）
    allowed_tiers : [mid, pro, max]         # ← 新：可多选（空=不限制）
    skills : [test-driven-development, brainstorming]  # ← 新：skill 白名单
    token_budget : 2000000                  # ← 新：仅存盘
    temperature : 0.3
    max_turns : 40
    tools : [read_file, write_file, ...]
    soul_file : "precise-coder.soul.md"     # ← 新：指向同目录 sidecar md
}
```

### Soul 文件：`~/.config/autoos/roles/<name>.soul.md`
纯 markdown，UI 里就是个大文本框。可包含 `# Soul of the X`、`## Personality` 等约定段。

### 与现有数据的对应
| 概念 | 来源 | 格式 |
|---|---|---|
| **Role**（含 Soul/tiers/skills/budget） | `~/.config/autoos/roles/<name>.at` + 同名 `.soul.md` | .at + md |
| Mode（绑定 Role + 定制 soul） | `~/.config/autoos/modes/*.at`（已存在） | .at |
| 内置 Role 兜底 | 编译期 7 个 trait 实现 | Rust |

---

## 3. 实施阶段

### Phase A — auto-lang：补 .at 发射器（生态修复，本计划前置）

**交付**：`Value`/`Node` → 正确转义的 .at 源码字符串；round-trip 测试通过。

> 这是本计划的副产品：当前 `.at` 只有解析器，发射器（`Display`）不转义、会产出无法读回的
> 坏字符串。补齐它，Role writer 才有可靠的底层。**该能力补在 `auto-lang`**，供全生态复用。

1. **`auto-val/src/emit.rs`**（新文件）
   - `pub fn escape_string(s: &str) -> String`：镜像解析器转义
     （`parser.rs:408-418` 为契约：`\n \t \r \\ \"`，未知 `\x`→原样）。
   - `pub fn format_value(v: &Value, indent: usize) -> String`：按 indent 打印
     （`Str`→`"...";`、`Bool/Int/...`→字面量、`Array`→`[a, b]`、`Obj`→`{ k : v; ... }`）。
   - `pub fn format_node(n: &Node, indent: usize) -> String`：打印
     `name {\n  k : v;\n}`，正确区分 args/props/kids（镜像 `node.rs:829-934`）。
   - `pub trait AtomSource { fn to_at_source(&self) -> String }`，给 `Value`/`Node`/`Atom` 实现。
2. **`auto-val/src/lib.rs`**：`mod emit; pub use emit::*;`
3. **round-trip 测试**（`emit.rs` 内 `#[cfg(test)]`）：
   构造含特殊字符的字符串（`"a\"b\\c\nd"`）、数组、嵌套节点 → `to_at_source()`
   → `AtomParser::parse()` → 断言等值。这是整个生态的回归保险。

**验证**：`cargo test -p auto-val emit`。

---

### Phase B — auto-ai：Role 核心数据层

**交付**：扩展的 `ProfessionConfig`、扩展的 `Profession` trait、`RoleRegistry`、`serialize_at_role`。

1. **扩展 `ProfessionConfig`**（`config/profession_config.rs`）
   - 新增字段：`allowed_tiers: Option<Vec<ModelTier>>`、`skills: Option<Vec<String>>`、
     `token_budget: Option<u64>`、`soul_file: Option<String>`。
   - `parse_at_profession` 读取这 4 个新字段（复用现有 `opt_string_list`/`opt_uint`；
     allowed_tiers 用新的 `opt_string_list` + tier 解析）。
2. **扩展 `Profession` trait**（`profession.rs`）+ `ConfigProfession`
   - trait 新增**带默认实现**的方法（保证 7 个编译内置零改动即兼容）：
     `allowed_tiers() -> Vec<ModelTier> { vec![] }`（空=不限）、
     `token_budget() -> Option<u64> { None }`、`skills() -> Vec<String> { vec![] }`（空=不约束）。
   - `ConfigProfession` 从 `ProfessionConfig` 的新字段实现这些方法。
3. **`serialize_at_role(cfg: &ProfessionConfig) -> String`**（新函数，`config/profession_config.rs`）
   - 用 Phase A 的 `auto-val::emit` 构造 `Node { name: "role", props: {...} }`
     （只序列化 `Some` 字段）→ `node.to_at_source()`。
   - 约 40 行，遍历字段 set_prop，None 跳过。
   - round-trip 测试：`serialize_at_role` → `parse_at_profession` → 字段等值。
4. **`RoleRegistry`**（新文件 `src/roles.rs`，镜像 `musk/src/mode.rs` 的 ModeRegistry）
   - `load()`：内置（`builtin_names()` + `load_builtin()`）+ 扫描
     `~/.config/autoos/roles/*.at`（用户覆盖同名内置）。
   - `list() -> Vec<RoleSummary>`：含
     name/description/tier/allowed_tiers/skills/token_budget/is_builtin。
   - `get(name) -> Option<RoleDetail>`：含 soul markdown 全文（读 sidecar `.soul.md` 或内联 `system_prompt`）。
   - `save(name, cfg, soul_md)`：写 `.at`（`serialize_at_role`）+ `.soul.md`（仅当 soul 非空且来自编辑）。
   - `delete(name)`：删 `.at` + `.soul.md`（内置返回错误）。
   - 加载 sidecar soul：若 `soul_file` 存在，读同目录 md 文件作为 `system_prompt`。
5. **公开 API**：在 `auto-ai-agent/src/lib.rs` 导出 `RoleRegistry`、`serialize_at_role`、扩展的 trait 方法。

**验证**：`cargo test -p auto-ai-agent`（含新 round-trip 测试）。

---

### Phase C — auto-musk：接通 + API + UI

**交付**：musk agent 解析读 RoleRegistry、Roles CRUD API、roles-config-page bundle。

1. **`build_agent_from_mode` 接通**（`lib.rs`）
   - `resolve_profession`：先查 **RoleRegistry**（用户 .at）→ 再 `load_builtin` 兜底。
   - **Skills 过滤**：若 role 的 `skills()` 非空，`SkillRegistry::scan` 后只保留这些；
     空=维持现状（mode 的 `skills: true` 时全开）。
   - **Tier 校验**：若 role 声明了 `allowed_tiers` 且 mode 解析出的 tier 不在其中，
     `tracing::warn!` 并钳制到范围内**最高** tier（不 panic）。
   - **`extra_system_prompt` 接通**（修复死字段）：`build_agent_from_mode` 把
     `mode.extra_system_prompt` append 到 `profession.system_prompt()`（若非空）——
     这就是"个体 Agent 定制自己的 SOUL"。
2. **Roles API**（`server.rs`，复用现有 `ServeDir` + CORS）
   - `GET /api/roles` → `[{name, description, tier, allowed_tiers, skills, token_budget, is_builtin}]`
   - `GET /api/roles/{name}` → 单个 Role 详情（含 soul markdown 全文）
   - `PUT /api/roles/{name}` → 写 .at + .soul.md（body: `{...cfg, soul_md?}`）
   - `DELETE /api/roles/{name}` → 删（内置返回 403）
3. **roles-config-page.vue**（`auto-musk/frontend/src/`，vite.config.ts 加第 4 个 entry）
   - 列表区：Role 卡片（名称、tier 徽章、allowed_tiers 色点、skill 数、🔒 表示内置）
   - 编辑面板：名称 / 描述 / **Soul（大文本框）** / **Skills（复选框，从 `/api/skills` 取全量）**
     / **Allowed Tiers（多选 toggle，5 个 tier）** / token_budget（数字，标注"暂不生效"）
     / temperature / max_turns / inherit
   - 保存调 `PUT /api/roles/{name}`；内置 Role 编辑按钮禁用（提示"复制为新 Role"）
   - 全部用主题变量（`var(--accent)` 等），跟现有三页风格一致
4. **vite.config.ts**：entry 加 `'roles-config-page': './src/roles-config-page.vue'`

**验证**：
- `cargo build -p musk` 通过
- `curl http://127.0.0.1:8080/api/roles` 返回内置 7 个
- 手动 PUT 一个测试 role，确认 .at + .soul.md 落盘

---

### Phase D — auto-os-config：注册模块 + 清理

**交付**：第 4 模块上线；AI Agents 页去掉 professions 表。

1. **`useModules.ts`** 注册第 4 个模块：
   ```ts
   { id: 'ai-roles', name: 'AI Roles', icon: '🎭',
     description: 'Agent roles: soul, skills, tiers',
     remote: 'http://127.0.0.1:8080/roles-config-page.js' }
   ```
2. **AI Agents 页**（`agents-config-page.vue`）删除 Professions 表（Roles 取而代之，独立模块）。
3. **测试更新**（`test-both-modules.mjs`）：nav 断言 4 项；新增 Roles 块（点击 → role 卡片渲染 + 无错误）；
   `test-theme-switch.mjs` 加 Roles 页主题色生效断言。

**验证**：`node test-both-modules.mjs` 全 4 模块 PASS。

---

## 4. 阶段与验证检查点

| 阶段 | 仓库 | 验证命令 | 预期 |
|---|---|---|---|
| A | auto-lang | `cargo test -p auto-val emit` | round-trip 测试通过 |
| B | auto-ai | `cargo test -p auto-ai-agent` | 新字段解析+序列化 round-trip 通过 |
| C | auto-musk | `cargo build -p musk` + `curl /api/roles` | 编译通过，返回 7 内置 role |
| D | auto-os-config | `node test-both-modules.mjs` | 4 模块全 PASS |

每阶段一个提交（Phase A 提交到 auto-lang，B 到 auto-ai，C 到 auto-musk，D 到 auto-os-config）。

---

## 5. 范围与边界（明确告知）

- ✅ **Skills 白名单生效**（role 指定的 skills 才注册给 agent）
- ✅ **Tier 校验生效**（越界 warn + 钳制到范围最高）
- ✅ **Soul 继承 + Mode 定制**生效（extra_system_prompt 修复）
- ⏸ **Token Budget 仅存盘不强制**（用户明确要求"暂时不生效"，保留字段为将来 BudgetTracker）
- 内置 7 role 只读（要改就"复制为新 Role"，基于 `inherit`）；不删除编译 trait（零回归）
- 用户 Role 只能 inherit 内置，不能 inherit 另一个用户 Role（首期简化）

---

## 6. 风险与缓解

| 风险 | 缓解 |
|---|---|
| `emit.rs` 转义不完全，产出坏 .at | Phase A 强制 round-trip 测试（特殊字符全覆盖）；转义规则严格镜像 parser |
| trait 加方法破坏外部实现者 | 全部用**默认实现**；内部 7 个内置 + `ConfigProfession` 实现新方法，其余靠默认 |
| RoleRegistry 扫描 .at 解析失败导致 musk 启动崩 | 单个 role 解析失败时 `tracing::warn!` 跳过，不 panic（镜像 ModeRegistry 容错） |
| 4 个 bundle 的共享 CSS 注入 | 已有 `cssInjectedByJs` 插件，多 entry 自动覆盖，无需额外处理 |
| Soul sidecar md 丢失 | `get()` 时若 soul_file 指向的 md 不存在，回退到内联 system_prompt（若有）并 warn |

---

## 7. 后续计划（不在本次范围）

- **`#[derive(ToAtom)]` / `#[derive(FromAtom)]` proc-macro**：当第 3、4 个结构需要双向 .at
  序列化、手写 writer 开始重复时，在 **`auto-lang/plans/`** 新建计划（如
  `NNN-derive-to-atom-proc-macro.md`）。届时底层复用本计划 Phase A 的 `emit.rs`，
  标注设计参考 Serde（`#[atom(node="role")]`、`#[atom(skip)]`、`#[atom(rename="model_tier")]`、
  `Option<T>` → None 省略字段）。**注意**：本次调研已确认 Auto 的编译期能力无法替代
  proc-macro（Auto 只反射 Auto 代码，不反射 Rust struct），故必须用 Rust proc-macro。
- **Token Budget 强制执行**：引入 BudgetTracker（参考 auto-forge `budget.rs`），
  把 token_budget 接入 agent run 循环。
- **用户 Role 互相 inherit**：当前仅 inherit 内置；未来支持 DAG 解析。

---

## 实施状态复核（2026-07-20，见 docs/reviews/002）

- **Phase A/B/C/D 主体**：已完整实现。auto-lang 的 escape/format、RoleConfig 字段、Role trait 扩展、musk CRUD API + 前端、os-config 模块注册全部落地。
- **🔴 F1（功能缺陷，跨仓库）— Tier 钳制只 warn 不应用**：`auto-musk/backend/crates/musk/src/lib.rs:118-140` 计算了 `clamped`（越界 tier 钳制到允许范围最高），并 `tracing::warn!`，但 `:143` `OwnedRole::new(role)` 用的是**原始 role**，`clamped` 从未赋回。结果：声明了 `allowed_tiers` 的 Role 越界时只打日志，实际请求仍带越界 tier。§5 把此项标 ✅ 与代码不符。**修复需在 auto-musk 仓库**：给 `OwnedRole` 加 `override_tier` 字段。
- **Token Budget**：`role_def.rs:71-76` 注释明确"stored only, not yet enforced"。BudgetTracker 已在 `orchestration/budget.rs` 存在但未接通 agent run 循环。符合计划 §7 "后续计划"的预期。
- **§5 的 `#[derive(ToAtom)]` proc-macro**：仍为后续计划（如计划所述）。

