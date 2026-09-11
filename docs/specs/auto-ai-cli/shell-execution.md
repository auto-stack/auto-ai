# Spec：auto-ai-cli 命令执行层（ash 优先）

- 规范 ID：`docs/specs/auto-ai-cli/shell-execution.md`（SD-01，PLAN-033 建立；本仓库首个 module Spec）
- 实现：`crates/auto-ai-cli/src/shell_exec.rs`、`crates/auto-ai-cli/src/tools.rs`（RunCommand / RunAshScript）
- 设计依据：`docs/designs/2026-09-11-ash-first-shell-execution-design.md`
- 状态：随 PLAN-033 进入评审

## 1. 范围

auto-ai-cli 内建命令执行工具（`run_command`、`run_ash_script`）的执行器选择、
安全策略、失败分类与回退契约。适用于任何把本 CLI 当作 Agent 前端的场景；
不约束 daemon / agent 层（它们不执行 shell）。

## 2. ash 发现与探测（AshLocator）

发现链按序，首个命中即用，进程级缓存（含"未找到"负缓存）：

1. 环境变量 `AUTO_AI_ASH_BIN`——**权威**：指向的文件不存在时直接判定
   "ash 不可用"，不再走后续链（该变量同时是测试注入点）；
2. `PATH` 上的 `ash` / `ash.exe`；
3. 兄弟仓启发：从可执行文件目录与当前工作目录各自向上最多 3 层，找
   `auto-shell/ash/target/{release,debug}/ash[.exe]`（release 优先）。

命中后必须通过 probe 才算可用：`ash --version` 退出码 0 **且** `ash --help`
输出包含 `--sandbox`。probe 失败按"不可用"负缓存。

## 3. ash 轨调用组装

```
ash [--sandbox <cwd 绝对路径>] [--no-network] [--audit <file>] (-c <cmd> | <script.ash> [args...])
```

- 沙箱根 = 规范化后的当前工作目录；
- `--no-network`：工具参数 `no_network:true` 时按次附加（默认不加）；
- `--audit`：环境变量 `AUTO_AI_ASH_AUDIT` 非空时透传（默认关）；
- 硬超时：默认 120 000 ms，工具参数 `timeout_ms`（1..=600 000）调整；超时
  kill 直属子进程（Windows 下孙进程可能存活，为已知限制）；
- stdout/stderr 分开捕获，UTF-8 lossy 解码。

## 4. 失败分类（FailureClassifier）

输入 `(exit_code, stderr)`，输出四类：

| 分类 | 判定 | 后续 |
|---|---|---|
| `RanOk` | exit == 0 | 正常返回 |
| `Denied` | exit ≠ 0 且 stderr 含 `Error: security:` 或 `Error: sandbox:` | **不回退**，返回 PAUSED 提示 |
| `PreExecFailure` | exit ≠ 0 且 stderr 含 `is not recognized` / `command not found` / `Undefined function:` / `Undefined variable:` / `Undefined command:` / `Parse error` | **回退系统 shell** |
| `RanFailed` | 其余一切（含 exit=None 进程崩溃、未知 stderr 形态、超时） | **不回退**，返回真实输出 |

优先级：`Denied` > `PreExecFailure` > `RanFailed`。stderr 特征以真实样本
fixture 单测钉死（ash v0.1.0，2026-09-11 实测；ash 改文案时测试即报警）。
将来 ash 提供 `agent run` 结构化信封后，分类器数据源替换、接口不变。

## 5. 回退矩阵（强制行为）

| 场景 | 动作 | 输出标注（content 首行） |
|---|---|---|
| `force:true` | 直接系统 shell（跳过白名单，现行 force 语义） | `[exec: <sh> (forced)]` |
| ash 不可用 | 系统 shell | `[exec: <sh> (ash unavailable)]` |
| 策略拒绝 | 不回退；PAUSED + 指引（改命令/限路径，或 force） | `[exec: ash (denied)]` |
| 预执行失败 | 回退系统 shell（零副作用，安全） | `[exec: <sh> (fallback: ash <原因>)]` |
| 命令失败 / 超时 / 崩溃 | 不回退；返回输出与退出码 | `[exec: ash]` / `[exec: ash (timeout…)]` |
| 脚本工具任意失败 | 不回退（AutoLang 无法换壳执行） | `[exec: ash script …]` |

`<sh>` = Windows `cmd.exe` / Unix `sh`。非零退出码在 content 尾行以
`[exit: N]` 上浮。`details`（仅 UI/日志，不进 LLM 上下文）至少含
`executor`、`exit_code`、`timed_out`、`timeout_ms`，失败时另含
`classification`、`ash_error`。

## 6. run_ash_script 约定

- 仅在 ash probe 通过时注册（系统 shell 不能执行 AutoLang，永不回退）；
- 参数：`content`（临时文件 `%TEMP%/auto-ai-ash-<pid>-<n>.ash`，用后删除）
  与 `path` 二选一，另有 `args`（位置参数）、`timeout_ms`；
- 模型可见约定：脚本必须显式 `exit(code)`——ash v0.1.0 的运行时错误
  （如未定义函数）exit 0，显式 exit 是唯一可靠成败信号；工具描述中
  必须持续携带该警示与 AutoLang 速查。

## 7. 不变量

1. 白名单与危险模式检查先于任何执行器选择（force 除外）；
2. 任何"拿不准"的失败一律不回退（回退是兼容优化，不是正确性依赖）；
3. 回退仅发生在命令**零副作用**被证实的前提下（PreExecFailure / ash 缺失）；
4. 工具描述必须与实际执行器一致（双轨文案，注册时按 probe 结果选定）。
