# Auto-AI-Agent 设计文档

> **Status**: Draft — 待实施
> **位置**: `auto-ai` 仓库 `crates/auto-ai-agent/`
> **依赖**: `auto-ai-client`(Layer 2 LLM 调用)
> **上层**: AutoForge / Ash / 未来所有 AutoOS App
> **关系**: [设计文档 §15](https://github.com/auto-stack/auto-lang/blob/master/docs/design/15-ai-daemon-infrastructure.md) 的 Layer 3+4。

---

## 1. 定位

`auto-ai-agent` 是 AutoOS AI 基础设施的 **Layer 3+4**,在 `auto-ai-client`(LLM 调用)之上,App 代码之下:

```
Layer 4: Workflow    — 多 Agent 编排(Relay/步骤/条件/资源调度)
Layer 3: Agent       — 单 Agent ReAct 循环 + Profession + Tool + Memory
Layer 2: Client      — LLM API 调用 + daemon discovery (auto-ai-client ✅)
Layer 1: Daemon      — 并发仲裁 + key vault + usage (aaid ✅)
```

**核心价值**:App 不再自己实现 Agent 引擎、Relay 协议、工具调度——只需注册工具 + 选择/定制 Profession。

---

## 2. 设计原则

1. **调校成果可复用**——Prompt 调校是 Agent 开发成本最高的部分。内置预调校的通用 Profession 库(Coder/Reviewer/Tester/Architect...),App 可直接用或继承微调,不从零调校。
2. **配置驱动**——Profession 和 Workflow 支持 `.at` 配置文件,非程序员也能调整 Agent 行为。
3. **继承 + 覆盖**——`inherit : "coder"` 加载内置模板的全部调校成果,只改差异部分。
4. **资源调度委托 aaid**——Workflow 不自己管并发,所有 LLM 请求走 aaid 的全局 Semaphore。
5. **MCP 互通**——Agent 可暴露为 MCP server,供其他 Agent/App 调用(Agent 即服务)。

---

## 3. 核心抽象

### 3.1 Profession(角色)

定义一个 Agent **是什么**:它的专长、系统提示词、可用工具集、模型偏好、行为约束。

```rust
/// 角色:定义 Agent 的身份和能力边界。
pub trait Profession: Send + Sync {
    /// 角色名("coder"、"reviewer")。
    fn name(&self) -> &str;

    /// 完整系统提示词(调校精华)。
    fn system_prompt(&self) -> &str;

    /// 推荐模型。
    fn model(&self) -> &str { "glm-4.5" }

    /// 生成温度(创造力 vs 确定性)。
    fn temperature(&self) -> f64 { 0.3 }

    /// 最大 ReAct 轮次(防无限循环)。
    fn max_turns(&self) -> usize { 10 }

    /// 这个角色允许使用的工具名列表(App 注册的工具中,只有这些可用)。
    fn allowed_tools(&self) -> Vec<String> { vec![] }  // 空 = 全部

    /// 可选:对 Memory 的约束(如:只保留最近 N 轮对话)。
    fn memory_limit(&self) -> Option<usize> { Some(20) }
}
```

### 3.2 Tool(工具)

定义一个 Agent **能做什么**:可调用的函数。

```rust
/// 工具:Agent 可调用的函数。
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// 工具名(必须唯一)。
    fn name(&self) -> &str;

    /// 给 LLM 的描述(LLM 据此决定是否调用)。
    fn description(&self) -> &str;

    /// 参数 schema(JSON Schema 格式,给 LLM 看)。
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    /// 执行工具,返回结果文本(给 LLM 看)。
    async fn execute(&self, args: &serde_json::Value) -> Result<String, ToolError>;
}
```

App 注册自己的工具:

```rust
// auto-forge 注册文件操作工具
struct WriteFileTool;
#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn description(&self) -> &str { "Write content to a file" }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "file path"},
            "content": {"type": "string", "description": "file content"}
        }, "required": ["path", "content"]})
    }
    async fn execute(&self, args: &serde_json::Value) -> Result<String, ToolError> {
        let path = args["path"].as_str().unwrap();
        let content = args["content"].as_str().unwrap();
        std::fs::write(path, content).map_err(|e| ToolError::Exec(e.to_string()))?;
        Ok(format!("wrote {} bytes to {}", content.len(), path))
    }
}
```

### 3.3 Agent(代理)

一个运行中的 Agent = Profession + Tools + Memory + LLM Client。

```rust
/// 单 Agent:跑 ReAct 循环(思考→工具调用→观察→重复)。
pub struct Agent {
    profession: Box<dyn Profession>,
    tools: ToolRegistry,
    memory: Memory,
    client: Arc<AiClient>,
}

impl Agent {
    pub fn new(profession: Box<dyn Profession>, client: AiClient) -> Self;

    /// 注册工具(App 专属工具)。
    pub fn register_tool(&mut self, tool: Box<dyn Tool>);

    /// 运行一个任务(ReAct 循环直到完成或 max_turns)。
    pub async fn run(&mut self, task: &str) -> Result<AgentResult, AgentError>;

    /// 获取对话历史。
    pub fn history(&self) -> &[Message];
}
```

ReAct 循环内部:

```
User task: "write a binary search function"
  Turn 1:
    LLM(thinking): I need to write a binary search function.
    LLM(action): call write_file(path="src/binary_search.at", content="...")
    Tool result: "wrote 245 bytes to src/binary_search.at"
  Turn 2:
    LLM(thinking): The file is written. Let me verify by reading it.
    LLM(action): call read_file(path="src/binary_search.at")
    Tool result: "fn binary_search(...) { ... }"
  Turn 3:
    LLM(final): Binary search function written to src/binary_search.at.
    Done.
```

### 3.4 Memory(记忆)

```rust
/// 对话历史 + context window 管理。
pub struct Memory {
    messages: Vec<Message>,
    limit: usize,  // 最大保留轮次(由 Profession.memory_limit() 控制)
}

impl Memory {
    pub fn add(&mut self, role: &str, content: &str);
    pub fn to_messages(&self) -> &[Message];
    /// 当超过 limit 时,压缩旧消息(保留 system + 最近 N 轮)。
    pub fn trim(&mut self);
}
```

---

## 4. 内置 Profession 库(预调校模板)

**这是 auto-ai-agent 的核心价值**:经过实战调校的通用角色,App 直接用或继承。

### 4.1 Rust 内置(代码定义)

```rust
// auto-ai-agent/src/professions/coder.rs
pub struct Coder { prompt: String, model: String, ... }

impl Profession for Coder {
    fn name(&self) -> &str { "coder" }
    fn system_prompt(&self) -> &str { &self.prompt }
    fn model(&self) -> &str { &self.model }
    fn temperature(&self) -> f64 { 0.2 }
    fn max_turns(&self) -> usize { 15 }
    // ...
}
```

### 4.2 初始内置清单(从 AutoForge 提取)

| Profession | 来源 | 状态 | 调校轮次 |
|---|---|---|---|
| **Coder** | Forge `forge/ai.rs` | ✅ 可提取 | ~30+ 轮 |
| **Architect** | Forge relay | ✅ 可提取 | ~20 轮 |
| **Tester** | Forge relay | ✅ 可提取 | ~15 轮 |
| **Reviewer** | Forge relay | ⚠️ 部分 | ~10 轮 |
| **Documenter** | 新建 | ⬜ | — |
| **Runner** | 新建 | ⬜ | — |
| **Translator** | Ash F3 雏形 | ⬜ | — |

### 4.3 .at 配置格式(声明式定义)

```auto
// crates/auto-ai-agent/resources/professions/coder.at
profession {
    name : "coder"
    desc : "通用代码编写 Agent"
    model : "glm-4.5"
    temperature : 0.2
    max_turns : 15

    system_prompt : "
你是一个专业的代码编写 Agent。你的职责是根据任务描述写出高质量代码。

规则:
1. 先理解任务,再动手。
2. 使用提供的工具读写文件。
3. 写完后验证(读回来检查)。
4. 用简洁、符合语言习惯的代码。
5. 如果任务不明确,先提问。
"
    tools : [read_file, write_file, list_dir, run_command]
}
```

### 4.4 继承 + 覆盖

App 定制时不用从零写——`inherit` 加载内置模板,只改差异:

```auto
// ~/.config/autoos/agents/auto-coder.at (用户/App 定制)
profession {
    inherit : "coder"                // 加载内置 Coder 的全部调校

    name : "auto-coder"              // 改名
    model : "glm-4.5"               // 覆盖模型
    system_prompt_append : "
        重要:本项目使用 Auto 语言(.at),不是 Rust。
        参考 CLAUDE.md 中的语法规则。
        常见错误:let mut → var, -> → 无返回箭头。
    "                               // 追加提示词(不替换原有)
    tools : [+run_auto_tests]       // 追加工具(+ 前缀)
    temperature : 0.1               // 更保守
}
```

加载逻辑:

```rust
// profession.rs
pub fn load(path: &Path) -> Result<Box<dyn Profession>, ...> {
    let config = parse_at_file(path)?;
    if let Some(base_name) = &config.inherit {
        let mut base = load_builtin(base_name)?;  // 加载内置模板
        base.override_from(&config);               // 覆盖差异
        Ok(base)
    } else {
        Ok(ProfessionConfig::from(config).into_profession())
    }
}
```

---

## 5. Workflow(多 Agent 编排)

### 5.1 概念

Workflow = 多个 Agent 按步骤协作完成一个复杂任务。每个步骤由一个 Profession 的 Agent 执行,步骤间传递上下文。

### 5.2 .at 配置

```auto
// ~/.config/autoos/workflows/feature-dev.at
workflow {
    name : "feature-development"
    desc : "需求 → 设计 → 编码 → 测试 → 审查"

    steps : [
        relay {
            id : "architect"
            profession : "architect"
            input : "$user_request"
            output : "$design_doc"
        }
        relay {
            id : "coder"
            profession : "coder"
            input : "基于以下设计文档实现代码:\n$design_doc"
            output : "$code_result"
            depends_on : ["architect"]
        }
        relay {
            id : "tester"
            profession : "tester"
            input : "测试以下代码:\n$code_result"
            output : "$test_result"
            depends_on : ["coder"]
        }
        relay {
            id : "reviewer"
            profession : "reviewer"
            input : "审查:\n$code_result\n测试结果:\n$test_result"
            output : "$review"
            depends_on : ["tester"]
            condition : "$test_result.contains(fail)"  // 仅测试失败时审查
        }
    ]

    on_failure : "retry_once"
    max_total_tokens : 100000
}
```

### 5.3 执行引擎

```rust
pub struct Workflow {
    name: String,
    steps: Vec<WorkflowStep>,
    tools: ToolRegistry,   // 共享给所有 Agent
    client: Arc<AiClient>,
}

pub struct WorkflowStep {
    id: String,
    profession: String,       // Profession 名
    input_template: String,   // 模板($var 替换)
    output_var: String,       // 输出存入的变量名
    depends_on: Vec<String>,  // 前置步骤
    condition: Option<String>,// 条件表达式
}

impl Workflow {
    pub fn load(path: &Path) -> Result<Self, ...>;
    pub async fn run(&self, tools: Vec<Box<dyn Tool>>, initial_input: &str) -> Result<WorkflowResult, ...>;
}
```

执行流程:

```
1. 解析步骤的依赖关系 → 拓扑排序
2. 按顺序执行:
   a. 创建 Agent(从内置/配置加载 Profession + 注册 tools)
   b. 模板替换($user_request → 实际输入)
   c. Agent.run(input)
   d. 输出存入 context[$output_var]
   e. 条件检查(下一步是否执行)
3. 所有步骤完成 → 汇总结果
```

### 5.4 与 aaid 的资源调度集成

Workflow 内的每个 Agent 的 LLM 请求都走 aaid:

```
Workflow engine
  ├─ Architect Agent → auto-ai-client → aaid → LLM API (占用 1 槽)
  ├─ Coder Agent    → auto-ai-client → aaid → LLM API (占用 1 槽)
  └─ Tester Agent   → auto-ai-client → aaid → LLM API (占用 1 槽)
                                          ↑
                                  aaid 全局仲裁(4 槽上限)
```

Workflow 引擎不管并发——aaid 管。

---

## 6. Relay 协议(Agent 间委托)

### 6.1 概念

Agent 在执行过程中可以**委托**子任务给其他 Agent(不是预定义的 Workflow 步骤,而是运行时动态委托)。

```
Coder Agent 正在写代码:
  → 遇到一个测试需求
  → 动态委托 Tester Agent:"测试我刚写的 binary_search.at"
  → Tester 执行,返回结果
  → Coder 继续工作
```

### 6.2 接口

```rust
pub trait RelayTarget: Send + Sync {
    /// 接收委托任务,返回结果。
    async fn delegate(&self, task: &str, context: &WorkflowContext) -> Result<String, AgentError>;
}

impl RelayTarget for Agent {
    async fn delegate(&self, task: &str, context: &WorkflowContext) -> Result<String, AgentError> {
        self.run(task).await.map(|r| r.output)
    }
}
```

Agent 可注册其他 Agent 作为 relay target:

```rust
let mut coder = Agent::new(Box::new(Coder::default()), client);
coder.register_relay("tester", Box::new(tester_agent));
coder.register_relay("reviewer", Box::new(reviewer_agent));
coder.run("implement feature X and test it").await?;
// Coder 在 ReAct 循环中可以动态委托 tester。
```

---

## 7. App 使用模式

### 7.1 最简:单个 Agent

```rust
// Ash F3:简单翻译,不需要 Workflow
use auto_ai_agent::{Agent, professions::Translator};

let client = AiClient::new()?;
let mut agent = Agent::new(Box::new(Translator::default()), client);
let result = agent.run("list all rust files").await?;
println!("{}", result.output);  // "find . -name '*.rs'"
```

### 7.2 标准:Agent + 自定义工具

```rust
// UI Editor:代码生成 Agent
use auto_ai_agent::{Agent, professions::Coder};

let mut agent = Agent::new(Box::new(Coder::default()), client);
agent.register_tool(Box::new(ReadAuraTool));     // 读 AURA 组件
agent.register_tool(Box::new(WriteAuraTool));    // 写 AURA 组件
agent.register_tool(Box::new(PreviewTool));      // 预览渲染
agent.run("create a login form widget").await?;
```

### 7.3 完整:Workflow + 多 Agent

```rust
// AutoForge:完整 relay 流程
use auto_ai_agent::Workflow;

let workflow = Workflow::load("workflows/feature-dev.at")?;
let tools = vec![
    Box::new(ReadFileTool),
    Box::new(WriteFileTool),
    Box::new(RunTestsTool),
    Box::new(RunCompilerTool),
];
let result = workflow.run(tools, "implement binary search").await?;
```

### 7.4 配置驱动(无代码)

```bash
# 用户不改代码,只改配置文件
auto-agent run --workflow feature-dev --input "implement binary search"
```

---

## 8. Crate 结构

```
crates/auto-ai-agent/
├── Cargo.toml
├── src/
│   ├── lib.rs               公共 API
│   ├── agent.rs             Agent struct + ReAct loop
│   ├── profession.rs        Profession trait + ConfigProfession + load()
│   ├── tool.rs              Tool trait + ToolRegistry
│   ├── memory.rs            对话历史管理
│   ├── workflow.rs          Workflow 引擎(拓扑排序 + 步骤执行)
│   ├── relay.rs             RelayTarget trait + 动态委托
│   └── error.rs             统一错误
│
├── professions/             内置预调校 Profession 库
│   ├── mod.rs
│   ├── coder.rs             代码编写
│   ├── architect.rs         系统设计
│   ├── tester.rs            测试编写
│   ├── reviewer.rs          代码审查
│   ├── documenter.rs        文档编写
│   ├── runner.rs            命令执行/信息查找
│   └── translator.rs        NL→命令 翻译
│
└── resources/
    └── professions/         .at 格式的调校参数
        ├── coder.at
        ├── architect.at
        ├── tester.at
        └── ...
```

---

## 9. 从 AutoForge 迁移

### 9.1 迁移策略

```
Phase 1: 创建 auto-ai-agent 框架(Agent + Profession + Tool + ReAct loop)
Phase 2: 从 Forge 提取预调校 Profession(Coder/Architect/Tester/Reviewer)
Phase 3: Forge 切换到 auto-ai-agent(删自己的 forge/ai.rs,用 auto-ai-agent)
Phase 4: 实现 Workflow 引擎(Forge 的 relay run → 通用 Workflow)
Phase 5: Forge 的 relay 配置迁移到 .at workflow 文件
```

### 9.2 Forge 贡献清单

| Forge 现有 | 迁移到 auto-ai-agent | 备注 |
|---|---|---|
| `forge/ai.rs` Coder prompt | `professions/coder.at` | 数十轮调校成果 |
| `relay/turn.rs` ReAct loop | `agent.rs` ReAct engine | 通用化 |
| `relay/run.rs` relay steps | `workflow.rs` engine | 通用化 |
| Tool 定义(read/write/exec) | Forge 保留(作为 Tool impl) | App 专属 |
| `provider/claude.rs` | 已迁移到 `auto-ai-client` | ✅ |

---

## 10. 验证计划

- `Agent::run()` 单测:mock LLM + mock tool,验证 ReAct 循环。
- `Profession::load()` 单测:继承/覆盖/纯配置。
- `Workflow::run()` 单测:步骤依赖、条件、上下文传递。
- 集成测试:用真实 API key 跑 Coder profession,验证写出可用代码。
- Forge 迁移验证:Forge 用 auto-ai-agent 后,行为与 v0.1 一致。
