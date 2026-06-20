# 003 — Workflow 扩展:循环 / 验证 / 门控(C,优先级中)

> **状态**:设计 + 实施计划。
> **仓库**:auto-ai(`crates/auto-ai-agent/src/workflow.rs` + 配置解析)。
> **优先级**:3️⃣ 中 —— 给 `.at` Workflow 补 auto-forge Flow 的核心能力,作为 Skill 驱动的可靠性兜底。
> **触发时机**:当 Skill 驱动的三步曲跑多了、发现"模型不够可靠遵守流程"时,做这个。

## 目标

把 auto-ai 现有的轻量 `.at` Workflow(拓扑排序 + 条件跳过)**扩展到接近 auto-forge Flow 的能力**:加**循环(失败重试)、验证器(产出质量门)、工具守卫(每步工具策略)、人工门控(暂停审批)**。让"预设编排"也能可靠跑(不像 Skill 那样靠模型自觉)。

## 现状 vs 目标

| 能力 | 现状 | 目标 |
|---|---|---|
| 步骤顺序 | ✅ 拓扑排序 | ✅ |
| 条件跳过 | ✅ `$var.contains(x)` | ✅ |
| **循环** | ❌ | ✅ `on_fail: retry(step, max=N)` |
| **验证器** | ❌ | ✅ `validate: [{type, ...}]` |
| **工具守卫** | ❌(全步共用) | ✅ `tools: {allow, forbid}` 每步 |
| **门控** | ❌ | ✅ `gate: auto/human` |

## 设计:扩展 `.at` 格式

扩展 `relay` 块,新增 4 个可选字段(向后兼容 —— 老的 `.at` 不带这些字段照常跑):

```text
relay {
    id : "reviewer"
    profession : "reviewer"
    input : "review:\n$code"
    output : "$review"
    depends_on : ["coder"]

    # 新增:验证器(产出质量门)
    validate : [
        { type : "output_contains", pattern : "STATUS:" },
        { type : "output_not_contains", pattern : "INCOMPLETE" }
    ]

    # 新增:循环(验证失败 → 回 coder 重做,最多 3 次)
    on_fail : { retry : "coder", max : 3 }

    # 新增:工具守卫(此步只允许这些工具)
    tools : { allow : [read_file, search], forbid : [write_file] }

    # 新增:门控(等人工审批才继续)
    gate : "human"
}
```

### 验证器类型(MVP 4 种)
- `output_contains { pattern }` —— 步骤输出含某字符串(如 "STATUS:")
- `output_not_contains { pattern }` —— 输出不含某字符串(如 "INCOMPLETE")
- `output_min_length { min }` —— 输出至少 N 字符(非空产出)
- `all/any { validators: [...] }` —— 组合(auto-forge 的 all/any)

### 循环
- `on_fail : { retry : "<step_id>", max : N }` —— 验证失败时跳回指定步骤重跑,最多 N 次;超 N 则整个 workflow 失败。

### 工具守卫
- `tools : { allow : [...], forbid : [...] }` —— 该步的 agent 只看到 allow 列表里的工具(forbid 黑名单)。空 allow = 继承全部。这让 reviewer 不能改代码、coder 能改。

### 门控
- `gate : "auto"`(默认,验证通过即继续)/ `gate : "human"`(暂停,等外部信号)。
- MVP:`human` 门控在 `Workflow::run` 里返回一个 `Paused { at_step }` 状态,调用方(API/CLI)负责等用户确认后调 `resume()`。**先做 auto,human 留接口**。

## 文件结构

```
crates/auto-ai-agent/src/
├── workflow.rs          ← 扩展 WorkflowStep + run 逻辑
├── workflow_validator.rs ← 新:验证器类型 + check()
└── config/...           ← (可选).at workflow 解析加新字段
```

## Tasks

### Task 1: WorkflowStep 扩展 + 解析
- [ ] `WorkflowStep` 加字段:`validators: Vec<Validator>`,`on_fail: Option<RetrySpec>`,`tools: Option<ToolGuard>`,`gate: Gate`
- [ ] `parse_at_workflow` 解析新字段(向后兼容:老 .at 不带则默认空)
- [ ] 类型定义 + 解析测试

### Task 2: 验证器引擎(`workflow_validator.rs`)
- [ ] `Validator` enum + `check(output: &str) -> Result<(), String>`
- [ ] 4 种类型:output_contains/not_contains/min_length、all/any 组合
- [ ] 单元测试(每种)

### Task 3: 循环逻辑
- [ ] `Workflow::run` 检测验证失败 → 按 `on_fail.retry` 回退步骤、计数、超 max 报错
- [ ] 需要把"已执行步骤 + 循环计数"作为运行状态追踪
- [ ] 测试:reviewer 失败 → 回 coder → 第 3 次仍失败 → workflow 失败

### Task 4: 工具守卫
- [ ] 每步 agent 构造时,按 `tools.allow/forbid` 过滤 ToolRegistry
- [ ] 测试:reviewer 的 agent 看不到 write_file

### Task 5: 门控接口(auto 实现,human 留接口)
- [ ] `Gate` enum(auto/human)
- [ ] `human` → `Workflow::run` 返回 `Paused`,加 `resume()` 方法
- [ ] auto 端到端跑通;human 写接口 + 文档,不强制实现 UI

### Task 6: 验证 + 文档 + 提交
- [ ] 端到端测试(feature-dev.at 加 reviewer 循环)
- [ ] 更新设计文档
- [ ] push

## 验收
- `.at` workflow 能表达"review 不过 → 回 coder 重做(最多 N 次)"。
- 验证器能门控产出质量。
- 每步能限制工具。
- 老 `.at` 文件不受影响(向后兼容)。

## 范围排除
- checkpoint/diff(auto-forge 的)—— 后续。
- `human` 门控的 UI/API —— 留接口,不实现。
- 嵌套 TaskPlan(auto-forge 的多 Flow 宏编排)—— 不做。
