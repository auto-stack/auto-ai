# Design：ash 优先的命令执行层（auto-ai-cli）

- 日期：2026-09-11
- 状态：草案（随 PLAN-033 评审）
- 关联仓库：`auto-ai`（本仓库，改造方）、`auto-shell`（ash 提供方，只读依赖）
- 关联计划：`docs/plans/033-ash-first-shell-execution.md`

---

## 1. 背景与动机

auto-ai-cli 的 `run_command` 工具目前直接把命令交给系统 shell（Windows 上是
`cmd /C`，Unix 上是 `sh -c`，见 `crates/auto-ai-cli/src/tools.rs` 的 `RunCommand`），
仅有应用层的命令名白名单与危险模式字符串检查。安全边界薄、跨平台行为不一致
（工具描述里硬编码 "This is a Windows cmd.exe environment"）。

姊妹仓库 auto-shell 提供的 **ash（AutoShell）** 是面向 AI Agent 的跨平台 shell：

- 内置安全策略层（`ash-core::security::SecurityPolicy`，auto-shell Plan 008/009）：
  `--sandbox <dir>` / `--read-only` / `--no-network` / `--allow` / `--deny` /
  `--no-exec` / `--dry-run` / `--audit <file>`；
- 结构化 pipeline（`ls | filter .size > 10.mb | sort .name`）与 AutoLang 脚本
  （`.ash` 文件，完整编程能力），跨平台同一行为；
- 实测可用（v0.1.0，见 §3.2）。

目标：**命令执行优先走 ash（带沙箱），ash 不存在或"未能执行"时回退系统 shell；
同时提供让 Agent 编写/运行 ash（AutoLang）脚本的工具。**

## 2. 结论先行（可行性判定）

**方案可行，推荐采用"子进程调用 ash.exe"的架构**，但有三个与用户字面需求
有偏差的关键取舍，需要评审确认（详见 §6 回退矩阵与 PLAN-033 §10）：

1. **策略拒绝（sandbox 拦截）不回退系统 shell**。用户原话是"ash 不存在或执行
   失败时回退 bash/powershell"，但若把"沙箱拒绝了"也当成"失败"去回退，等于
   用回退机制绕过沙箱，安全性反而低于现状。拒绝必须上浮为提示（改路径或
   显式 `force:true`），而不是静默换壳重跑。
2. **命令自身失败（exit ≠ 0）不回退**。`cargo build` 失败重跑一遍 cmd.exe 只会
   再失败一次，还可能重复副作用（写文件、发请求）。回退只发生在"ash 未能
   开始执行这条命令"（命令不存在、解析错误等，此时命令零副作用）的场景。
3. **auto-shell 文档里的 `ash agent run/check/describe-tools` 子命令在当前二进制
   中不存在**（`docs/for-agents.md` 描述的是 auto-shell 侧 Plan 028 的规划）。
   本设计不能依赖结构化 JSON 信封，只能以退出码 + stderr 文本契约为准，
   并为将来迁移到信封留出接口（§7.4）。

另有一个硬约束决定了架构形态：**auto-ai 不能以库形式依赖 auto-shell**（§3.4
循环依赖），因此"内嵌 ash-core"方案被否决，spawn 子进程是唯一干净路径。

## 3. 调研结论（2026-09-11 实测）

### 3.1 auto-ai 侧现状

| 事实 | 位置/证据 |
|---|---|
| `run_command` 是唯一命令执行工具：白名单 `ALLOWED_PREFIXES` + 危险模式 + `force` 旁路；Windows `cmd /C`，Unix `sh -c`；无超时；无交互审批（"⏸ PAUSED" 只是返回给模型的文本提示，靠模型重试带 `force:true`） | `crates/auto-ai-cli/src/tools.rs`（`RunCommand`） |
| `search` 工具也 shell out（Windows `cmd /C findstr`，Unix `grep`） | 同上（`Search`） |
| 工具注册点集中在一处 | `crates/auto-ai-cli/src/main.rs` `build_agent()`（约 271–276 行） |
| `ToolOutput` 契约 = `content`（进 LLM）+ `details`（只进 UI/日志，约定 `run_command → {truncation, full_output_path}`） | `crates/auto-ai-agent/src/tool.at`；CLI 目前未填 details |
| auto-ai-cli 是**原生 Rust crate，无 `.at` 轨**（agent/daemon/ai-config/client 四 crate 才有 `.at → rust/` 双轨转译） | 仓库结构；PLAN-064 的 CLI follow-up（d294f5d）直接改 `.rs` 的先例 |
| CLI 已依赖 tokio（macros / rt-multi-thread / sync） | `crates/auto-ai-cli/Cargo.toml` |
| 角色 `.at` 提示词不含 shell 习惯文案（转译产物 `rust/` 中的 powershell 字样是陈旧漂移） | `grep builtin_roles/*.at` |

注：用户描述的"系统提供的 bash 或 powershell"与实际代码有出入——现状是
Windows 上 `cmd.exe`。本设计按实际代码（cmd/sh）描述回退目标，行为不变。

### 3.2 ash 侧实测（v0.1.0，`auto-shell/ash/target/debug/ash.exe`）

| 验证项 | 结果 |
|---|---|
| `ash -c "echo a && echo b"`、`git status --short \| wc -l` 管道 | ✅ 可用，exit 0 |
| 外部命令（`cargo --version`） | ✅ 可执行（Windows 上未知/外部命令委托 PowerShell 执行） |
| `--sandbox /tmp -c "cat <沙箱外文件>"` | ✅ 拦截，exit 1 |
| `--read-only -c "rm <文件>"` | ✅ 拦截，exit 1，stderr `Error: security: write command 'rm' blocked by --read-only`，文件未删 |
| `--no-network -c "curl ..."` | ✅ 拦截，exit 1 |
| `--deny cargo -c "cargo --version"` | ✅ 拦截，exit 1 |
| 未知命令 `nonexistent_cmd_xyz` | exit 1，stderr 为 PowerShell 风格 "The term ... is not recognized ..."（Windows） |
| `.ash` 脚本显式 `exit(3)` | ✅ exit code 3 正确传播 |
| `.ash` 脚本**运行时错误**（调用未定义函数） | ⚠️ 打印 `Error: Undefined function: ...` 但 **exit 0**（ash 侧缺口） |
| `ash -c "..." --json` | 输出为结果值的 JSON 字符串（`"hello\n"`），**无** for-agents.md 所述完整信封 |
| `ash agent ...` | ❌ 子命令不存在（"ash: agent: No such file"） |
| 安装形态 | `install.ps1`/`install.sh` 走 `cargo install` → `~/.cargo/bin`；开发环境当前只有 debug 构建，且已在 PATH 上 |

### 3.3 安全模型的诚实表述

ash 的沙箱是 **SecurityPolicy 在命令分发/spawn 之前做的策略级拦截**（危险模式、
allow/deny 名单、能力开关、路径限制），不是 OS 级隔离（无 job object/seccomp）：

- 对 ash 内建命令（ls/cat/rm/mv/…的 80 命令族）拦截是完整的；
- 对委托给 PowerShell 的外部命令，拦截发生在**命令名/模式层**（如 `--no-network`
  按 `NETWORK_EXTERNALS` 名单拦 curl/wget/ssh…），无法约束被放行的外部进程
  在 OS 层面的文件/网络行为。

因此正确的定位是：**ash = "比裸 cmd.exe 安全得多的默认执行面"**（默认拒绝
沙箱外路径写、拦截已知危险模式、可审计），而不是硬安全边界。硬隔离需叠加
`--allow` 白名单或 `--no-exec`，属于后续可选档位（§7.3）。

### 3.4 依赖关系与架构否决项

```
auto-shell (lib) ──path dep──▶ auto-ai-agent, auto-ai-client   （auto-shell/ash/auto-shell/Cargo.toml）
ash-core ──▶ auto-val（不依赖 auto-ai）
```

- **内嵌 auto-shell/ash 库**：形成 cargo path 依赖环，❌ 否决。
- **内嵌 ash-core**：无环，但 ash-core 只有 parser/pipeline/security，**没有 80
  个内建命令实现**（命令在 auto-shell lib 里），不足以"执行命令"，且把两个仓库
  的构建耦合死，❌ 否决（留作 §7.4 演进备选）。
- **spawn `ash(.exe)` 子进程**：进程边界天然解耦、ash 升级独立、失败可观测，✅ 采用。

## 4. 方案总览

```
                       ┌────────────────────────────────────────────┐
 Agent (LLM)           │  auto-ai-cli                               │
   │ tool_call         │                                            │
   ▼                   │  run_command / run_ash_script              │
┌──────────┐   cmd     │   ┌──────────────────────────────────┐     │
│ RunCommand├──────────┼──▶│ shell_exec 模块（新）              │     │
└──────────┘           │   │  AshLocator   发现+探测+缓存       │     │
                       │   │  ShellExec     组装 ash 调用+超时   │     │
                       │   │  FailureClassifier  退出码/stderr  │     │
                       │   └───────┬──────────────┬─────────────┘     │
                       │           │ ash 可用      │ ash 不可用/probe 失败
                       │           ▼              ▼                   │
                       │   ash --sandbox <cwd>    cmd /C  |  sh -c    │
                       │   [-c <cmd> | script.ash]（现状回退轨）       │
                       │           │                                  │
                       │           ▼ 分类：Denied / PreExec / Ran*    │
                       │   Denied→提示不回退；PreExec→回退系统 shell    │
                       └────────────────────────────────────────────┘
```

改动集中在 auto-ai-cli 一个 crate（原生 Rust，无 `.at` 轨负担）：

| 组件 | 性质 | 职责 |
|---|---|---|
| `src/shell_exec.rs`（新） | 模块 | ash 发现/探测/缓存、调用组装、超时、失败分类 |
| `src/tools.rs` `RunCommand` | 改造 | 走 shell_exec；动态工具描述；输出标注执行器；填 `details` |
| `src/tools.rs` `RunAshScript`（新） | 新工具 | 写临时 `.ash` → `ash --sandbox <cwd> script.ash`；仅 ash 可用时注册 |
| `src/main.rs` `build_agent()` | 一行级 | 条件注册 `RunAshScript` |
| `src/tools.rs` `Search`（可选 P2） | 改造 | ash 可用时改走 `ash -c "grep ..."` 统一跨平台行为 |

## 5. 详细设计

### 5.1 AshLocator（发现与探测）

发现链（按序，首个命中即用；结果以 `OnceLock` 进程级缓存，含"未找到"负缓存）：

1. 环境变量 `AUTO_AI_ASH_BIN`（显式覆盖，测试也靠它注入）；
2. `PATH` 上找 `ash` / `ash.exe`；
3. 兄弟仓库启发：分别从**可执行文件目录**和**当前工作目录**向上最多 3 层，找
   `auto-shell/ash/target/release/ash[.exe]`，其次 `target/debug/ash[.exe]`
   （覆盖 `D:\autostack\auto-ai` 与 `D:\autostack\auto-shell` 并列的开发布局；
   release 优先于 debug）。

命中后做一次性 probe：`ash --version` 退出码为 0 且 `ash --help` 输出包含
`--sandbox`，才判定"ash 可用"。probe 失败视为不存在（负缓存），本次会话不再
尝试——避免每条命令都付探测成本，也避免半残的 ash 反复制造回退抖动。

### 5.2 ShellExec（ash 轨调用组装）

```
ash [--sandbox <cwd 绝对路径>] [--no-network] [--audit <file>] (-c <cmd> | <script.ash>) [--json]
```

- 沙箱根 = 当前工作目录（canonicalize 后的绝对路径）。`run_command` 与脚本
  共用；后续若引入 cd 类工具再随动。
- `--no-network`：默认**不开**（`cargo`/`npm` 等白名单命令依赖网络，默认开会造成
  大量无谓回退）；工具参数 `no_network: true` 可按次启用。
- `--audit <file>`：默认不开；环境变量 `AUTO_AI_ASH_AUDIT` 指定路径则透传。
- 超时：默认 120s，工具参数 `timeout_ms`（1..=600_000）可调。用
  `tokio::time::timeout` 包裹子进程等待（实现上可 `spawn_blocking` + std Command
  或 tokio::process，以最小改动为准），超时 kill 进程树并返回明确的超时错误。
  **超时不回退**（命令可能已产生部分副作用）。
- stdout/stderr 分开捕获，UTF-8 lossy 解码（与现状一致）。

### 5.3 FailureClassifier（失败分类——本设计的核心）

输入：`(exit_code, stderr_full)`；输出四类：

| 分类 | 判定（契约） | 含义 | 处置 |
|---|---|---|---|
| `RanOk` | exit == 0 | 命令成功 | 直接返回输出 |
| `Denied` | exit != 0 且 stderr 含 `Error: security:` | **策略拒绝**（sandbox/read-only/no-network/deny） | **不回退**；返回 PAUSED 风格提示：被哪条策略拦、建议改路径/收窄命令，或 `force:true` 走系统 shell（保持现语义：force 是显式人工授权口） |
| `PreExecFailure` | exit != 0 且 stderr 命中预执行失败特征：`is not recognized`（Windows 外部命令未找到）/ `command not found`（Unix）/ `Undefined function:` / `Undefined command:` / `Parse error` | ash 根本没能开始执行这条命令，**零副作用** | **回退系统 shell** 重跑，输出标注 `[ash 未执行成功：<原因摘要>，已改用 <shell>]` |
| `RanFailed` | exit != 0，其余一切（含无法识别的 stderr） | 命令确实跑了但失败 | **不回退**（避免重复副作用）；原样返回 stdout+stderr+exit code，和现状一致 |

保守默认：**任何拿不准的情况归 `RanFailed`（不回退）**。回退是优化项（兼容性），
不是正确性项；宁可多一次模型重试，不可多一次副作用。

契约的脆弱性（stderr 文本匹配）明确记录于测试 fixture 与 §7.4 演进路径；
auto-shell 将来实现 `ash agent run` 信封后，分类器换成读顶层 `status`
（`success/failed/denied/partial`），接口不变。

### 5.4 RunCommand 工具改造

参数 schema 变化（保持向后兼容，均为可选新增）：

```json
{ "cmd": "...",                       // 必填，不变
  "force": false,                     // 现有，语义不变：直接走系统 shell（跳过白名单/危险模式检查）
  "timeout_ms": 120000,               // 新增可选，1..=600_000
  "no_network": false }               // 新增可选，仅 ash 轨生效（透传 --no-network）
```

执行流：

1. `force:true` → 系统 shell（现状路径，含现白名单跳过语义），输出标注 `[exec: cmd.exe (forced)]`。
2. 白名单/危险模式检查（现状逻辑不变，非 force 时）。
3. ash 不可用 → 系统 shell，标注 `[exec: cmd.exe (ash unavailable)]`。
4. ash 可用 → `ShellExec::run`，按 §5.3 分类处置；回退时输出标注
   `[exec: cmd.exe (fallback: ash <原因>)]`，成功则 `[exec: ash]`。
5. `details` 填 `{ executor, classification, ash_error?, exit_code?, truncation }`
   （只进 UI/日志，不进 LLM 上下文——遵守 tool.at 的 ToolOutput 契约）。

工具描述**按执行器动态生成**（注册时探测一次）：

- ash 轨：说明这是 ash/AutoShell 环境——跨平台一致、支持结构化管道
  （`ls | filter .size > 1.mb | sort .name`）、无 heredoc、文件操作被沙箱限制在
  工作目录内、被拦截时会收到 PAUSED 提示；
- cmd 轨：保持现有 "Windows cmd.exe environment" 文案。

### 5.5 RunAshScript 工具（"用 ash 脚本来做事情"）

```json
{ "content": "fn main() { ... }",     // 二选一：脚本正文
  "path": "scripts/foo.ash",          // 二选一：已有脚本路径
  "args": ["a", "b"],                 // 可选，位置参数（脚本内 system("echo $1") 取用）
  "timeout_ms": 120000 }              // 可选
```

执行：content 写入 `%TEMP%/auto-ai-ash-<n>.ash`（用后删除）→ 
`ash --sandbox <cwd> script.ash`（`path` 形态在沙箱内直接运行）→ 失败分类同 §5.3
（脚本级：`Denied` 不回退、`PreExecFailure` 如脚本文件无法读取/解析错误**不回退**
——脚本是 ash 专属能力，系统 shell 跑不了 AutoLang，回退无意义，直接把 ash 的
错误返回给模型改脚本）。

仅在 ash probe 通过时注册（`build_agent()` 条件注册，模式同 SkillTool 的
非空才注册）。工具描述给出最小 AutoLang 速查（`fn main(){}` / `var` / `print()` /
`system("...")` / `exit(1)`，并指向 auto-shell/examples/ 的 35+ 范例风格）。

已知缺口必须在描述里告知模型：**脚本运行时错误（如调用未定义函数）当前
exit 0**（§3.2），约定脚本结尾显式 `exit(<code>)` 表达成败，模型据此判读。

### 5.6 审计与可观测

- 每次执行（ash 轨与回退轨）在 `details` 记录执行器、分类、退出码；
- `AUTO_AI_ASH_AUDIT=<file>` 时透传 `--audit`，由 ash 落 JSONL；
- UI 侧（linear/tui/print 三轨）渲染工具结果时，标注行随 content 自然可见，
  不需要改渲染代码（标注是 content 首行前缀）。

## 6. 回退矩阵（总表）

| 场景 | ash 状态/分类 | 动作 | 输出标注 |
|---|---|---|---|
| 显式授权 | `force:true` | 系统 shell（跳过白名单，现状语义） | `[exec: cmd.exe (forced)]` |
| ash 未安装/probe 失败 | — | 系统 shell（现状行为） | `[exec: cmd.exe (ash unavailable)]` |
| 沙箱/策略拒绝 | `Denied` | **不回退**，PAUSED 提示 + 改进建议 + force 指引 | `[exec: ash (denied)]` |
| ash 未能开始执行（not found / parse / undefined） | `PreExecFailure` | **回退**系统 shell 重跑 | `[exec: cmd.exe (fallback: <原因>)]` |
| 命令执行了但失败 | `RanFailed` | **不回退**，返回真实输出 | `[exec: ash]` + exit code |
| 超时 / ash 进程崩溃 | timeout/crash | **不回退**，返回超时/崩溃错误 | `[exec: ash (timeout/crash)]` |
| ash 脚本工具 | 任意失败 | **不回退**（AutoLang 无法换壳执行） | `[exec: ash script]` |

## 7. 风险与缓解

### 7.1 stderr 文本契约脆弱（中）
ash 升级改错误文案 → `PreExecFailure` 误判为 `RanFailed`（方向安全：只是少了
回退，不会误回退）。缓解：fixture 单测钉死已知文案；分类器对未知保守；
`AUTO_AI_ASH_BIN` 支持测试注入固定版本。

### 7.2 双重执行副作用（高，已由矩阵消除）
唯一允许回退的 `PreExecFailure` 定义为"命令零副作用"。评审重点确认该判定
的stderr 特征集合是否足够保守（宁缺毋滥）。

### 7.3 沙箱非 OS 级（中，预期管理）
文档与工具描述如实表述"策略级拦截"；不宣称硬隔离。后续可加 `strict` 档位
（`--allow` 白名单 + `--no-network` + `--read-only` 组合）作为配置项。

### 7.4 ash 侧缺口（外部依赖，记录不阻塞）
- 脚本运行时错误 exit 0 → 工具描述约定显式 exit；建议 auto-shell 立项修复
  （其计划编号见 auto-shell `docs/plans/NEXT.md`，当前下一号 081）。
- `ash agent run` 信封未实现（auto-shell Plan 028）→ 实现后迁移分类器。
- 开发机只有 debug 构建 → 部署/性能场合需 release 构建或 `cargo install`。

### 7.5 首次运行副作用（低）
ash 首次启动会创建 `~/.ashrc`（帮助文案自述）。无害，记录在案。

## 8. 测试策略

1. **单元**（`shell_exec.rs` 内 `#[cfg(test)]`）：发现链顺序（env > PATH > 兄弟
   启发；负缓存）、probe 判定、分类器 fixture 表（每类至少 3 条真实 stderr 样本，
   含 §3.2 实测原文）、超时参数边界。
2. **集成**（`crates/auto-ai-cli/tests/ash_exec.rs`，新目录）：用
   `AUTO_AI_ASH_BIN` 注入真 ash 时跑通 AC 场景矩阵（沙箱外写被拒、not-found
   回退、exit≠0 不回退、脚本 exit 传播）；ash 缺失（指向不存在路径 + 清 PATH）
   时回退轨正确。CI 无 ash 环境自动 skip 并打印原因。
3. **回归**：`cargo test --workspace`（现有 254 测试不回归）；`run_command`
   对模型可见的 schema/description 快照断言（防提示词漂移）。
4. **手工 e2e**：真机 `auto-ai-cli chat` 跑一轮含命令执行的对话，肉眼核对标注行
   与 details（清单写入 PLAN-033 T-06）。

## 9. 未来演进（非本设计范围）

- 迁移到 `ash agent run` 结构化信封（分类器换数据源，接口不变）；
- 长驻 ash 子进程/批量会话（当前每命令一进程，简单可靠优先）；
- `strict` 安全档位配置化；`search` 工具统一走 ash；
- **多路径可写白名单 + 项目/会话级策略关联**（用户 2026-09-11 提出）：当前
  `SecurityPolicy.sandbox_dir` 仅支持单根；演进方向为"项目配置可写目录/文件集
  + 会话级审批追加 + 其余路径写需用户确认"，需 auto-shell 侧（多根沙箱 +
  策略文件 + 结构化拒绝原因）与 auto-ai 侧（策略配置 + 审批 UX + Denied
  提示带路径级 remediation）各立一计划；本设计的 Denied-不回退契约与其前向
  兼容；
- 若 auto-shell 未来解除了对 auto-ai 的依赖，可重评估 ash-core 嵌入。
