# Plan 007: auto-ai-cli — 交互式 Agent Demo

> **状态**：实施计划，待执行
> **仓库**：`auto-ai`（新 crate `auto-ai-cli`）
> **前置**：Plan 005（Assistant role）、Plan 006（streaming tool_calls）
> **定位**：auto-ai 的"hello world agent"——clone 后 `cargo run` 即可体验 agent 能力，不需要 auto-musk 或任何前端。

---

## 0. 目标

在 auto-ai 仓库内建一个独立的 CLI 程序 `auto-ai-cli`，展示 auto-ai-agent 的核心能力：
- 交互式多轮 chat（REPL）
- 内置 Role 切换（assistant / coder / reviewer）
- 流式输出（token by token）
- 工具调用展示（read_file / search / list_dir 等）
- Skill 调用（如果已安装）

**不做**：Web UI、会话持久化、workflow 编排、relay/handoff。

## 1. crate 结构

```
crates/auto-ai-cli/
  Cargo.toml
  src/
    main.rs       — CLI 入口 + REPL 循环
    tools.rs      — 内置工具集（复用 musk 的工具实现模式）
```

### 依赖
```toml
[dependencies]
auto-ai-agent = { path = "../auto-ai-agent" }
auto-ai-client = { path = "../auto-ai-client" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
async-trait = "0.1"
serde_json = "1"
```

**不依赖** axum/tower-http/reqwest（不需要 HTTP server）。

## 2. CLI 接口

```sh
# 交互式 chat（默认 role = assistant）
auto-ai-cli chat
auto-ai-cli chat --role coder

# 单次任务
auto-ai-cli run "list files in current directory"
auto-ai-cli run --role reviewer "review src/main.rs"

# 列出可用 role
auto-ai-cli roles
```

用 `clap` derive。

## 3. 内置工具集

精简版（只读 + 执行，不含 write/edit——demo 不改文件）：
- `read_file` — 读文件
- `list_dir` — 列目录
- `search` — 正则搜索
- `run_command` — 执行命令（复用 musk 的安全分级）

这些工具直接在 `tools.rs` 实现（与 musk 的 tools.rs 结构相同，但不含 write/edit/batch_replace/glob/list_symbols）。约 200 行。

## 4. REPL 循环（main.rs）

仿 musk 的 `chat_loop`，但更精简：

```
auto-ai-cli chat — role: assistant (tier: mid)
> list the files in this directory
[tool] list_dir → Cargo.toml
                   src/
                   ...
assistant ────
The current directory contains...
──── turn 1, 1 tool call ────

> what does main.rs do?
...
> exit
```

关键实现：
- 用 `AiClient::with_url` 构造 client（避免 `AiClient::new()` 的 nested runtime 问题）
- `Agent::with_history` 支持多轮
- `run_stream` 的 `on_event` 回调：Delta → print token，Tool → print inline，Done → summary
- `--role` 参数 → `load_builtin(role)` 获取 Role 实例

## 5. 路径约束

复用 musk 的 `tool_safety` 思路（Design 004）：CWD = project root，工具只能操作项目内文件。但因为 demo 不含 write/edit，风险较低——`run_command` 仍需白名单/PAUSE。

简化：demo 的 `run_command` 直接复用 musk 的 `classify_command` 白名单逻辑。

## 6. 实施步骤

| 步骤 | 内容 | 验证 |
|---|---|---|
| 1 | 创建 crate 骨架（Cargo.toml + workspace 注册） | `cargo build -p auto-ai-cli` |
| 2 | tools.rs：4 个工具 + run_command 安全分级 | 编译通过 |
| 3 | main.rs：clap CLI + `roles` 子命令 + `run` 单次任务 | `cargo run -p auto-ai-cli roles` 列出 8 个 |
| 4 | main.rs：`chat` 交互式 REPL + 流式输出 | 手动跑一轮多轮对话 |
| 5 | workspace 注册 + 文档 | `cargo run -p auto-ai-cli chat` |

## 7. 与 musk 的关系

| | auto-ai-cli | auto-musk |
|---|---|---|
| 定位 | auto-ai 的 demo + 架构参考 | 完整的 AI 编码 agent app |
| UI | CLI REPL | Web app（chats/flows/wiki） |
| 持久化 | 无（进程内 memory） | JSON 持久化（chats store） |
| 工具 | 4 个（只读 + run_command） | 9 个（含 write/edit/batch） |
| Role | 全部内置（assistant/coder/...） | 同 + 用户自定义 .at |
| HTTP server | 无 | musk serve :8080 |
| 复杂度 | ~400 行 | ~5000+ 行 |

## 8. 作为架构参考的价值

新开发者 clone auto-ai 后：
1. `cargo run -p auto-ai-daemon` — 启动 daemon
2. `cargo run -p auto-ai-cli chat` — 和 agent 对话
3. 看 `auto-ai-cli/src/` 就能理解 "Role + Tool + Client + Daemon" 四层如何配合

这是最低门槛的 "跑起来看看" 路径。
