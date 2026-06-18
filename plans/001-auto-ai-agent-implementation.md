# Plan 001: auto-ai-agent 实施（Agent 引擎 + Profession 库 + Workflow）

> **Status**: Draft
> **设计文档**: [docs/auto-ai-agent-design.md](../docs/auto-ai-agent-design.md)
> **仓库**: `auto-ai`
> **依赖**: `auto-ai-client`(Layer 2,已完成)
> **影响**: AutoForge(移植)、Ash(F3 升级)、未来所有 AutoOS App

---

## 1. 目标

在 `auto-ai` 仓库新建 `crates/auto-ai-agent`,实现 Layer 3+4:

- **Agent 引擎**:ReAct 循环 + Memory + Tool 调度。
- **内置 Profession 库**:预调校的 Coder/Architect/Tester/Reviewer 等,App 直接用或继承。
- **Workflow 引擎**:多 Agent 编排(步骤/依赖/条件),资源调度委托 aaid。
- **.at 配置**:Profession 和 Workflow 的声明式定义 + `inherit` 继承。

---

## 2. 实施阶段

### Phase 1: Crate 骨架 + 核心 Trait

**交付**:可编译的 crate,核心 trait 定义,Agent 基本结构。

1. 创建 `crates/auto-ai-agent/`,添加到 workspace。
2. `profession.rs`:`Profession` trait(name/system_prompt/model/temperature/max_turns/allowed_tools/memory_limit)。
3. `tool.rs`:`Tool` trait(name/description/parameters/execute)+ `ToolRegistry`。
4. `memory.rs`:`Memory` struct(add/trim/to_messages)。
5. `error.rs`:`AgentError` / `ToolError` 统一错误。
6. `agent.rs`:`Agent` struct + 基本结构(new/register_tool),**暂不实现 ReAct 循环**。
7. 单测:trait 定义编译、ToolRegistry 注册/查找、Memory add/trim。

### Phase 2: ReAct 循环

**交付**:Agent 能跑完整的 ReAct 循环(思考→工具调用→观察→完成)。

1. `agent.rs::run()`:
   - 构建 messages(系统提示词 + 任务 + 历史)。
   - 调用 `AiClient::complete()`(auto-ai-client)。
   - 解析 LLM 返回:是否有 tool_use?有→执行工具→结果加入 history→下一轮;无→完成。
2. Tool 调用:LLM 返回 tool_use 请求(JSON)→ 从 ToolRegistry 查找 → execute → 结果文本加入 history。
3. 终止条件:LLM 不再请求工具 / 达到 max_turns / LLM 明确标记完成。
4. AgentResult:output(最终文本)+ turns(执行轮次)+ tool_calls(调用记录)。
5. 单测:mock AiClient(返回预设的 tool_use → tool_result → final)验证循环。
6. 集成测试(需 API key):简单 Coder profession + mock tool,跑一轮真实 LLM。

### Phase 3: 内置 Profession 库

**交付**:4+ 预调校的通用 Profession,从 AutoForge 提取。

1. `professions/` 模块:mod.rs + 每个角色一个文件。
2. 从 AutoForge `backend/src/forge/ai.rs` 提取系统提示词:
   - `professions/coder.rs` — 代码编写(Forge 的核心 prompt)。
   - `professions/architect.rs` — 系统设计(Forge relay architect)。
   - `professions/tester.rs` — 测试编写。
   - `professions/reviewer.rs` — 代码审查。
3. 新建(无 Forge 来源):
   - `professions/translator.rs` — NL→命令翻译(Ash F3 用)。
   - `professions/runner.rs` — 执行命令/查找信息。
4. 每个 Profession 用 `ProfessionConfig`(支持 `model`/`temperature`/`max_turns` 可配)。
5. `resources/professions/*.at`:对应 `.at` 配置文件(系统提示词以 .at 形式存储,便于修改)。
6. 单测:每个内置 Profession 的 `system_prompt()` 非空、`model()` 返回有效值。

### Phase 4: .at 配置 + `inherit` 继承

**交付**:Profession 支持配置文件定义 + 继承覆盖。

1. `profession.rs::ProfessionConfig`:从 .at 文件解析的配置结构。
2. `parse_at_profession(content) -> ProfessionConfig`:解析 .at(复用 auto_config 风格扫描器)。
3. `inherit` 逻辑:加载内置模板 → 合并覆盖字段(system_prompt_append 追加,tools `+` 前缀追加,model/temperature 覆盖)。
4. `load(path) -> Box<dyn Profession>`:从文件加载(支持 inherit)。
5. `load_builtin(name) -> Box<dyn Profession>`:加载内置 Profession。
6. 单测:纯配置定义、inherit + 覆盖、inherit + append prompt、inherit + 追加工具。

### Phase 5: Workflow 引擎

**交付**:多 Agent 编排,步骤/依赖/条件。

1. `workflow.rs`:`Workflow` struct + `WorkflowStep`。
2. `.at` 解析:`parse_at_workflow(content) -> WorkflowConfig`(steps/depends_on/condition)。
3. 拓扑排序:根据 depends_on 排序步骤。
4. 执行引擎:`run(tools, initial_input)`:
   - 维护 `context: HashMap<String, String>`(变量存储,$output_var)。
   - 模板替换($user_request → context 中的值)。
   - 每步创建 Agent(加载 Profession + 注册 tools)→ Agent.run(input)。
   - 条件检查(condition 表达式求值,决定是否跳过)。
5. 结果汇总:每步的 output + 全流程 token 用量。
6. 单测:线性步骤、条件跳过、上下文传递、拓扑排序。

### Phase 6: AutoForge 移植

**交付**:AutoForge 从自己的 Agent 引擎切换到 auto-ai-agent。

1. AutoForge `backend/Cargo.toml` 添加 `auto-ai-agent` 依赖。
2. Forge 的工具(ReadFile/WriteFile/RunTests 等)实现 `Tool` trait。
3. Forge 的 Agent 调用从 `forge/ai.rs::ClaudeProvider::chat_turn()` → `auto_ai_agent::Agent::run()`。
4. Forge 的 relay run 从 `relay/run.rs` → `auto_ai_agent::Workflow::run()`。
5. Forge 的 Profession 提示词从代码 → `inherit` 内置 + `system_prompt_append` 定制。
6. 验证:Forge 用 auto-ai-agent 后,Agent 行为与 v0.1 tag 一致。
7. 删除 Forge 的 `forge/ai.rs`(被 auto-ai-agent 替代)。

### Phase 7: Ash F3 升级

**交付**:Ash F3 从直接调 AiClient → 用 Translator profession + Agent。

1. `auto-shell` 添加 `auto-ai-agent` 依赖。
2. F3 的 `ask_ai()` → `Agent::new(Translator::default(), client).run(question)`。
3. Translator profession 的系统提示词更专业(知道 ash 语法、管道 DSL、.at 配置)。
4. 验证:F3 返回的命令质量提升。

---

## 3. 依赖关系

```
Phase 1 (骨架)
  ↓
Phase 2 (ReAct 循环)
  ↓
Phase 3 (内置 Profession 库) ← 从 Forge 提取
  ↓
Phase 4 (.at 配置 + inherit)
  ↓
Phase 5 (Workflow 引擎)
  ↓
Phase 6 (Forge 移植) ← 需要 Phase 1-4
  ↓
Phase 7 (Ash F3 升级) ← 需要 Phase 1-3
```

Phase 6 和 7 可以并行(互不依赖)。

---

## 4. 关键文件

| 文件 | Phase | 说明 |
|---|---|---|
| `crates/auto-ai-agent/Cargo.toml` | 1 | crate 定义,依赖 auto-ai-client |
| `src/lib.rs` | 1 | 公共 API 导出 |
| `src/profession.rs` | 1+4 | Profession trait + ConfigProfession + load/inherit |
| `src/tool.rs` | 1 | Tool trait + ToolRegistry |
| `src/memory.rs` | 1 | Memory struct |
| `src/agent.rs` | 1+2 | Agent struct + ReAct loop |
| `src/workflow.rs` | 5 | Workflow 引擎 |
| `src/relay.rs` | 5 | RelayTarget trait |
| `src/error.rs` | 1 | 统一错误 |
| `professions/coder.rs` | 3 | 从 Forge 提取 |
| `professions/architect.rs` | 3 | 从 Forge 提取 |
| `professions/tester.rs` | 3 | 从 Forge 提取 |
| `professions/reviewer.rs` | 3 | 从 Forge 提取 |
| `professions/translator.rs` | 3 | 新建(Ash 用) |
| `professions/runner.rs` | 3 | 新建 |
| `resources/professions/*.at` | 3 | 调校参数(.at 格式) |

---

## 5. 验证

- **Phase 1**: trait 编译 + ToolRegistry/Memory 单测。
- **Phase 2**: mock LLM 的 ReAct 循环单测(3 轮:tool_use→result→final);集成测试(真实 API key)。
- **Phase 3**: 每个 Profession 的 `system_prompt()` 非空 + 覆盖 Forge 调校内容。
- **Phase 4**: inherit + 覆盖 + append 的配置解析单测。
- **Phase 5**: 线性/条件/依赖 Workflow 单测。
- **Phase 6**: Forge 行为对比 v0.1(同一任务,Agent 输出质量不退化)。
- **Phase 7**: Ash F3 翻译质量对比(同一问题,Agent > 直连 complete)。
