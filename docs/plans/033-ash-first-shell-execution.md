---
plan_id: PLAN-033
status: reviewed
feature_name: ash 优先的命令执行层（auto-ai-cli）
author: [agent]
created_at: 2026-09-11T00:00:00Z
updated_at: 2026-09-11T15:00:00Z
plan_revision: 1
current_step: 6
total_steps: 6
supersedes_spec_components: []
new_spec_components:
  - docs/specs/auto-ai-cli/shell-execution.md
touched_goals: []
worktree: D:/autostack/.wt/ai-033/auto-ai
worktree_branch: plan-033-dev
worktree_base_commit: 373cd50
worktree_head_commit: 46d169a
dependency_snapshots:
  - repo: auto-lang, commit: f26ba9a41, worktree: D:/autostack/.wt/ai-033/auto-lang (detached, read-only)
---

# PLAN-033：ash 优先的命令执行层（auto-ai-cli）

## 0. 变更摘要

auto-ai-cli 的 `run_command`（及新增的 `run_ash_script`）改为优先经姊妹仓库
auto-shell 提供的 `ash` 子进程执行命令与 AutoLang 脚本，默认携带
`--sandbox <cwd>` 策略沙箱；ash 不存在（发现/探测失败）或"未能开始执行命令"
（命令不存在/解析错误，零副作用）时回退现有系统 shell 轨道（Windows
`cmd /C` / Unix `sh -c`）。**策略拒绝与命令自身失败不回退**（防绕过沙箱、防
双重副作用），以 PAUSED 提示或真实输出上浮给模型。配套：执行器动态工具描述、
超时、`details` 执行元数据、失败分类器及其测试。

设计全文：`docs/designs/2026-09-11-ash-first-shell-execution-design.md`（本计划
的契约基础，评审时一并审）。

## 1. 目标

1. 命令执行默认面从裸系统 shell 迁移为带策略沙箱的 ash（subprocess）。
2. ash 不可用/预执行失败时无感回退系统 shell，输出可辨识实际执行器。
3. 提供 `run_ash_script` 工具让模型编写并运行 AutoLang 脚本（仅 ash 可用时注册）。
4. 失败分类契约（退出码 + stderr 特征）可测试、可演进（未来换 `ash agent` 信封）。
5. 补齐命令超时（现状缺失），ash 轨与回退轨一致生效。

### 非目标

- 不做 ash-core / auto-shell 库内嵌（cargo path 依赖环，见设计 §3.4）。
- 不改 agent/daemon/ai-config/client 四个 `.at` 双轨 crate（CLI 为原生 Rust，
  PLAN-064 先例 d294f5d 直接改 `.rs`）。
- 不实现 OS 级硬隔离；不实现 auto-shell 侧的 `ash agent` 子命令或其信封。
- 不改角色提示词（`.at` 轨无 shell 习惯文案，`rust/` 内字样为陈旧漂移）。
- 不动白名单/危险模式/force 的既有安全语义。

## 2. 架构方案

新增 `crates/auto-ai-cli/src/shell_exec.rs`（AshLocator 发现链 + 一次性 probe +
OnceLock 缓存；ShellExec 组装 `ash --sandbox <cwd> [-c <cmd> | script.ash]` +
tokio 超时；FailureClassifier 四分类 `RanOk/Denied/PreExecFailure/RanFailed`，
保守未知归 `RanFailed`）。`tools.rs` 的 `RunCommand` 接线该模块并动态生成工具
描述；新增 `RunAshScript`（临时 `.ash` 或给定 path，失败不回退）；
`main.rs build_agent()` 条件注册。回退矩阵与调用组装细节见设计文档 §5–§6。

## 3. 技术栈

Rust（auto-ai-cli，tokio 已在依赖中）；子进程 `std::process`/`tokio::process`
（实现取最小改动）；外部二进制 `ash`（auto-shell v0.1.0+，要求 `--sandbox` 支持，
以 probe 验证）。无新第三方依赖。

## 4. 需求分析与背景调查

### 现状证据（2026-09-11 调研）

- `crates/auto-ai-cli/src/tools.rs`：`RunCommand`（`cmd /C` / `sh -c`、白名单
  `ALLOWED_PREFIXES`、危险模式、`force`、无超时、无 details）；`Search`
  （findstr/grep shell out）。
- `crates/auto-ai-cli/src/main.rs` `build_agent()` 约 271–276 行注册全部工具；
  SkillTool 已有"条件注册"先例（非空才注册）。
- `crates/auto-ai-agent/src/tool.at`：ToolOutput = content + details 契约，
  `run_command → {truncation, full_output_path}` 约定（CLI 未用 details）。
- auto-shell 实测（v0.1.0 debug 构建，已在开发机 PATH）：
  `-c`/管道/`&&`/`.ash` 脚本可用；`--sandbox/--read-only/--no-network/--deny`
  拦截 exit 1；未知命令 exit 1（Windows stderr 为 PowerShell 风格 not-recognized）；
  策略拒绝 stderr 前缀 `Error: security:`；脚本显式 `exit(3)` 传播；
  **脚本运行时错误 exit 0（ash 侧缺口）**；`ash agent` 子命令不存在。
- 依赖环：`auto-shell/ash/auto-shell/Cargo.toml` path 依赖
  `auto-ai-agent`/`auto-ai-client` → auto-ai 不可反向库依赖；ash-core 缺内建命令。
- 本仓库 `docs/specs/` 不存在（skill 要求记录：Specs 缺位，以代码为准，本计划
  以 SD-01 建立首个 spec）。

### 授权记录

用户（本会话 2026-09-11）授权：调研 auto-ai 与 ../auto-shell、形成解决方案
（含设计文档）、制定实施计划——首轮完成。同日追加授权：**回退矩阵获批
（拒绝/命令失败不回退），并授权开始执行本计划**。允许修改范围：auto-ai 仓库
（按任务表 T-01…T-06）；auto-shell 仅读。

### 计划编号依据

`docs/plans/` 现无编号文件；`archive/` 最大号 032。git 全历史（含
diff-filter=A/D）证实 033–064 号计划文件从未在仓库存在——提交信息与
KNOWN-DEBT 引用的 PLAN-064 为**未落盘的会话级计划**，不占本仓库编号。按
"两目录最大有效数字前缀 +1"规则取 **033**。（初稿曾防御性取 065，同日勘误，
见 §9。）

### 关键取舍（需评审确认，倾向已写入设计 §2/§6）

- 沙箱策略拒绝 **不回退**（回退即绕过沙箱）；命令自身失败 **不回退**（双重
  副作用）；仅预执行失败（零副作用）回退。与用户字面"失败即回退"有偏差，
  属安全取舍，列入 §10 待确认。

## 5. 详细设计

见设计文档 `docs/designs/2026-09-11-ash-first-shell-execution-design.md`
§5（AshLocator/ShellExec/FailureClassifier/RunCommand/RunAshScript 逐项）、
§6（回退矩阵总表）、§8（测试策略）。任务级落点：

| 任务 | 文件/符号 | 说明 |
|---|---|---|
| T-01 | `src/shell_exec.rs` 新建：`AshLocator` | env `AUTO_AI_ASH_BIN` → PATH → 兄弟启发（exe/cwd 向上 ≤3 层找 `auto-shell/ash/target/{release,debug}/ash[.exe]`）；probe `--version`+`--help` 含 `--sandbox`；OnceLock 缓存含负缓存 |
| T-02 | `src/shell_exec.rs`：`FailureClassifier` | 输入 `(exit_code, stderr)`；`Error: security:` → Denied；`is not recognized`/`command not found`/`Undefined function:`/`Undefined command:`/`Parse error` → PreExecFailure；0 → RanOk；其余 → RanFailed |
| T-03 | `src/tools.rs`：`RunCommand` 改造 | 接线 shell_exec；动态 description（ash 轨/cmd 轨）；参数新增 `timeout_ms`(1..=600_000, 默认 120_000)、`no_network`(默认 false)；content 首行执行器标注；details 填 `{executor, classification, ash_error?, exit_code?}`；超时 kill |
| T-04 | `src/tools.rs`：`RunAshScript` 新增 + `main.rs` 注册 | `{content|path, args?, timeout_ms?}`；临时文件 `%TEMP%/auto-ai-ash-<n>.ash` 用后清理；失败不回退；描述含 AutoLang 速查与"运行时错误 exit 0 需显式 exit()"警示 |
| T-05 | `crates/auto-ai-cli/tests/ash_exec.rs` 新建 | 集成场景矩阵（AC 对应）；无 ash 自动 skip；`#[cfg(test)]` 单测随模块 |
| T-06 | 验证与文档 | workspace 测试、手工 e2e 清单执行、README 命令执行层一段说明 |

### 规范增量

| delta_id | add/modify/retire | docs/specs/... target | before/after rule | rationale | acceptance IDs |
|---|---|---|---|---|---|
| SD-01 | add | docs/specs/auto-ai-cli/shell-execution.md | before：无此 spec（docs/specs/ 目录本身不存在，工具执行行为仅存在于 tools.rs 代码）。after：规范化 ash 优先执行契约——发现链与 probe、ash 调用组装（沙箱根=cwd、可选 no_network/audit）、失败四分类判定特征、回退矩阵（Denied/RanFailed/timeout/脚本不回退；PreExec/ash 缺失回退；force 直连）、输出标注与 details 字段、run_ash_script 注册门控与脚本约定（显式 exit） | 该契约被 CLI 工具层与未来演进（ash agent 信封迁移）共同依赖，需可引用的规范锚点 | AC-01…AC-10 |

（本仓库 Specs 缺位系首次记录；SD-01 目标路径为新建目录，work 阶段创建。）

## 6. 测试设计

- 单元：发现链顺序/负缓存、probe 判定、分类器 fixture 表（每类 ≥3 条真实
  stderr 样本，含实测原文）、参数边界（timeout_ms 上下限、schema 往返）。
- 集成：`AUTO_AI_ASH_BIN` 注入真 ash 跑 AC 矩阵；注入不存在路径且清 PATH 验证
  回退轨；环境无 ash 自动 skip 并打印原因。
- 回归：`cargo test --workspace` 全绿（现有 254 测试不回归）；工具
  schema/description 快照断言防漂移。
- 手工：e2e 清单（T-06）：`auto-ai-cli chat` 实测沙箱外写被拒提示、普通命令
  标注 `[exec: ash]`、`AUTO_AI_ASH_BIN=/nonexistent` 下回退标注。

## 7. 验收标准

| ID | 标准 | 验证方法（预期） |
|---|---|---|
| AC-01 | ash 可用时 `run_command` 经 `ash --sandbox <cwd>` 执行 | 单测断言调用组装；集成：注入真 ash 跑 `echo ok`，输出含 `[exec: ash]`，exit 0 |
| AC-02 | ash 不可用（env 指向不存在且 PATH 无）时回退系统 shell，无 panic | 集成：输出含 `[exec: cmd.exe (ash unavailable)]`（Windows）/`sh`（Unix），命令成功 |
| AC-03 | 策略拒绝不回退：sandbox 外写/危险命令被 ash 拦截时返回 PAUSED 风格提示与 force 指引 | 集成：`ash` 轨跑沙箱外写命令 → 输出含 denied 提示、无系统 shell 重跑痕迹（无第二条命令执行的输出） |
| AC-04 | 预执行失败回退：ash 报命令不存在时改由系统 shell 执行 | 集成：构造仅在 cmd 下存在的命令（或 not-found stderr 注入）→ 输出含 `fallback` 标注且命令成功 |
| AC-05 | 命令自身失败（exit≠0）不回退，返回真实 stdout/stderr/exit | 集成：`ash` 轨跑必败命令（如 `exit 2` 脚本或 `cargo --bogus-flag`）→ 输出含错误与退出码、无 fallback 标注 |
| AC-06 | `force:true` 直接走系统 shell（现语义不变） | 单测/集成：force 时无 ash 进程调用（可用审计 env 断言未透传） |
| AC-07 | `run_ash_script` 仅 ash 可用时注册；脚本显式 `exit(3)` 传播 exit 3；运行时错误返回 ash 报错文本 | 单测（注册门控）+ 集成（exit 传播、未定义函数错误上浮） |
| AC-08 | 超时生效：默认 120s，超时 kill 并返回明确超时错误，不回退 | 单测（小超时 + sleep 脚本，<5s 用时） |
| AC-09 | 测试全绿：`cargo test -p auto-ai-cli` 与 `cargo test --workspace` | 命令退出码 0；无 ash 环境集成 skip 并说明 |
| AC-10 | 工具描述按执行器动态生成（ash 轨含 ash 用法与沙箱说明；cmd 轨保持现状文案） | 快照单测：两种 probe 状态下 description 断言 |

## 8. 执行步骤

依赖：T-01 → T-02 → T-03 →（T-04 可与 T-03 并行）→ T-05 → T-06。

- **T-01** `shell_exec.rs`：AshLocator（发现链 + probe + 缓存） `[x]`
  - 新文件 `crates/auto-ai-cli/src/shell_exec.rs`；`main.rs` 挂 `mod shell_exec;`
  - 验证：`cargo test -p auto-ai-cli ash_locator` 绿；含 env 注入与负缓存用例
    - [✅ 已完成] worktree `.wt/ai-033/auto-ai` @ plan-033-dev 547db95
      （基线 373cd50；依赖 `.wt/ai-033/auto-lang` @ f26ba9a41 detached 只读快照，
      解 crates→`../../auto-lang` 相对路径）。`cargo test -p auto-ai-cli shell_exec`
      = 7 passed / 0 failed（发现链 5 项：env 权威/PATH/兄弟启发 release 优先/
      深度限界/全缺；真机 probe 1 项命中 PATH 上 ash v0.1.0）。
  - 关联：AC-01、AC-02
- **T-02** `shell_exec.rs`：FailureClassifier + fixture 单测 `[x]`
  - 验证：`cargo test -p auto-ai-cli failure_classifier` 绿（4 类 × ≥3 样本）
    - [✅ 已完成] plan-033-dev 8e17ff3。真实样本钉契约（实测发现 **Denied 有两个
      前缀**：`Error: security:` 与 `Error: sandbox:`，含 "security: security:"
      双前缀怪癖；PreExec 增实测 `Undefined variable:`）。5 个分类测试含
      "Denied 优先于 PreExec"。shell_exec 模块 12/12 绿。
  - 关联：AC-03、AC-04、AC-05
- **T-03** `tools.rs`：RunCommand 接线（动态描述、超时、标注、details、回退矩阵） `[x]`（R1 修复后复勾，46d169a）
  - 验证：`cargo test -p auto-ai-cli run_command` 绿 + `cargo clippy -p auto-ai-cli` 无新告警
    - [✅ 已完成] plan-033-dev ed422d3。执行层 `run_with_timeout`（读线程防死锁
      + deadline kill）+ `ash_invocation` 纯函数组装（单测钉参数序）；工具层双轨
      动态描述、schema 增 `timeout_ms`/`no_network`、`[exec: …]` 标注 + 非零
      exit 行、details 四类齐。**第二注册点发现**：`spawn_pipeline.rs:77` 也
      注册 RunCommand，同步改 `new()`。`cargo test -p auto-ai-cli` = 38/0
      （含真机：ash 轨 echo、沙箱外 cat 拒绝不回退且文件未被读、cargo 伪旗标
      失败不重试、ping 超时 kill）。clippy 告警均预存（rust-ref/main.rs 旧段），
      新代码零告警。未引入 tokio process/time feature——std Command +
      spawn_blocking 即够，依赖面更小。
    - [⚠️ R1 失效部分] `ExecOutcome::classification()` 未守卫 `timed_out`（F-02，
      见 §9 R1）；R1 复审另发现 shell_exec.rs:24 一条 redundant_closure 告警
      （F-03）。其余证据仍有效。
    - [✅ R1 修复] 46d169a：classification 开头 `timed_out → RanFailed` 守卫
      （含注释说明超时命令已执行、stderr 撞标记不得回退）+ 双向回归测试
      `timed_out_wins_over_pre_exec_markers`（同 stderr：ExecOutcome 层
      RanFailed / 纯分类器层仍 PreExec）；闭包改传函数本体，clippy 新代码
      零告警复验。
  - 关联：AC-01…AC-06、AC-08、AC-10
- **T-04** `tools.rs` + `main.rs`：RunAshScript 与条件注册 `[x]`
  - 验证：`cargo test -p auto-ai-cli run_ash_script` 绿（注册门控 + exit 传播）
    - [✅ 已完成] plan-033-dev dfce25a。content（%TEMP% 暂存用后清理）/path
      二选一 + args + timeout；永不回退；描述含 AutoLang 速查与"v0.1.0 运行
      时错误 exit 0 需显式 exit()"警示；`build_agent()` 仅 probe 通过时注册
      （先例同 SkillTool）。7 项测试含真机 exit(3) 传播、运行时错误文本
      上浮、path+args 直跑。附带发现：ash 对脚本 exit(N≠0) 附一行 stderr
      栈回溯（无害）。**注册门控本身无独立单测**（build_agent 需 Client），
      以代码检视 + 条件编译路径保证。
  - 关联：AC-07
- **T-05** 场景矩阵测试（AC 场景矩阵、无 ash skip） `[x]`（R1 修复后复勾，46d169a）
  - 验证：本机（ash 在 PATH）`cargo test -p auto-ai-cli --test ash_exec` 全绿
    - [✅ 已完成（路径适配）] plan-033-dev 27614d8。**偏差记录**：auto-ai-cli
      为 bin-only crate（无 lib target），`tests/` 集成测试无法导入模块内部
      ——场景矩阵以模块内测试等效落地（T-03/T-04 已含），AC-02 补"子进程
      自再执行"模式：父测试以 `AUTO_AI_ASH_BIN=X:/...` 重跑测试二进制，
      子测试断言 probe=None 且 run_command 走系统壳（标注 +
      details.executor）。最终 `cargo test -p auto-ai-cli` = 46/46。
    - [⚠️ R1 失效部分] AC-04 正向路径（PreExecFailure→系统壳重试）无 live
      测试（F-01，见 §9 R1）；计划 §7 预告的"not-found stderr 注入"未兑现。
      其余矩阵证据仍有效。
    - [✅ R1 修复] 46d169a：`live_pre_exec_failure_falls_back_to_system_shell`
      ——伪造 ash（Windows .cmd / Unix .sh，任意调用打 not-recognized stderr
      +exit 1）经手工 AshInfo 直喂 run_ash_track（不动进程缓存），断言
      `(fallback: ash` 标注 + 命令经系统壳实际执行 + details.executor；不
      依赖真 ash，任何环境可跑。CLI 全量 48/48。
  - 关联：AC-01…AC-08
- **T-06** 验证与文档收口 `[x]`（R1 修复后复勾，终验重跑 287/0）
  - `cargo test --workspace` 全绿；README 增"命令执行层"一节；KNOWN-DEBT 不动
    （ash 侧缺口由 auto-shell 立项，见 §10）
  - 手工 e2e 清单执行并记录证据
    - [✅ 已完成（自动化等效 + 交互项移交 review）] plan-033-dev 914b2f0。
      `cargo test --workspace` = **285 passed / 0 failed**（含 CLI 46、agent
      115、daemon 62 等，零回归）。SD-01 spec 落于 worktree
      `docs/specs/auto-ai-cli/shell-execution.md`（merge 时发布，本仓库首个
      module Spec）。README 增 Command execution (ash-first) 一节。**手工
      交互式 LLM e2e（需 aaid + API key + 真人肉眼核对 UI 标注）移交
      review 门**：其可程序化断言的部分（标注格式、details、拒绝文案）
      已由 46 项测试覆盖。
    - [⚠️ R1 失效部分] 285/0 对应 914b2f0；F-01/F-02/F-03 修复后须重跑全量
      作为终验。文档/spec 部分不受影响。
    - [✅ R1 修复后终验] 46d169a：`cargo test --workspace` = **287 passed /
      0 failed**（CLI 48 含新增 2 项；agent 115、daemon 62 等零回归）；
      clippy 新代码零告警。
  - 关联：AC-09、全部复核

## 9. 复审记录

- 2026-09-11（draft handoff）：调研两仓库后建稿 r1；设计文档同日产出。
  stage: new / outcome: pass（在已记录授权=调研+设计+计划内）/ next: work。
- 2026-09-11（编号勘误 + 授权落定）：初稿误用 065（防御性避开提交信息中的
  PLAN-064 身份）。经查 git 全历史，docs/plans/ 从未存在 033–064 号文件
  （添加/删除记录均无），PLAN-064 为未落盘的会话级计划，仅存于提交信息与
  KNOWN-DEBT 引用。按本仓库权威编号（docs/plans/ + archive/ 最大 032）改号
  **033**，plan_id 与文件名同步更新，契约内容不变（revision 仍为 1，编号勘误
  非语义变更）。同日用户拍板：**回退矩阵（拒绝/命令失败不回退）获批**，
  §10.1 关闭；§10.2 按设计推荐默认采纳。用户授权开始执行（work）。
- 2026-09-11（work handoff）：`stage: work | PLAN-033 | r1 | outcome: pass |
  code_commit: plan-033-dev 547db95→914b2f0（T-01…T-06 共 6 提交，基线
  373cd50）| task_ids: T-01..T-06 全勾 | evidence: cargo test -p auto-ai-cli
  46/46（含真机 ash 轨/拒绝不回退/失败不重试/超时 kill/子进程回退轨/
  脚本 exit 传播）、cargo test --workspace 285/0、clippy 新代码零告警 |
  blockers: 无 | next: review`。worktree `.wt/ai-033/auto-ai`（依赖快照
  `.wt/ai-033/auto-lang` @ f26ba9a41 只读）保留待 review/merge。执行期
  适配 3 项已记录于任务条目：①worktree 组布局（`.worktrees/` 内相对路径
  `../../auto-lang` 解析不到 → `.wt/ai-033/` 组 + 依赖快照，符合 wt 惯例）；
  ②tests/ash_exec.rs → 模块内测试（bin-only crate）；③Denied 前缀实测为
  两类（security/sandbox）已入分类器与 spec。交互式 LLM e2e（需 aaid +
  API key）移交 review 门。
- 2026-09-11（**R1 review**）：`stage: review | PLAN-033 | r1 |
  outcome: **needs_fix** | reviewed_commit: 914b2f0（worktree
  .wt/ai-033/auto-ai，干净无未提交改动）| base_commit: 373cd50 |
  dependency_revisions: auto-lang f26ba9a41（.wt/ai-033/auto-lang 只读）|
  spec_inputs: docs/specs/auto-ai-cli/shell-execution.md（SD-01 草案，
  worktree 内）| acceptance_results: AC-01/02/03/05/06/07/09/10 pass，
  **AC-04 partial（F-01）、AC-08 partial（F-02）** | findings: F-01/F-02/F-03
  （见下）| evidence: cargo test -p auto-ai-cli 复跑 46/46（9.12s）；
  cargo test --workspace 285/0 复用（同 914b2f0 无改动，本会话当日跑）；
  clippy 新代码 1 告警（F-03）；diff 范围核对仅 auto-ai-cli+文档 |
  next: work（修复 F-01/F-02/F-03 后重验）`。
  **独立性声明**：review 与执行同会话，结论自工件重建（重跑测试、重读
  diff 与代码），非采信执行摘要。

  **R1 发现（稳定 ID）**：

  - **F-01（P2·覆盖缺口·AC-04/T-05）**：PreExecFailure→系统壳回退的正向
    路径无 live 测试。现有 fallback 断言均为"不回退"方向（AC-03/05）或
    "ash 缺失"方向（AC-02 子进程测试）；计划 §7 AC-04 预告的"not-found
    stderr 注入"未兑现。**修正**：补一测——以伪造 ash（Windows .bat/
    Unix .sh：`--version`/`--help` 过 probe，`-c` 打印 not-recognized 到
    stderr 并 exit 1）直接喂 `run_ash_track`（AshInfo 可手工构造，无需
    动缓存），断言输出含 `(fallback: ash` 标注且命令经系统壳成功。
  - **F-02（P1·正确性·AC-08/T-03）**：`ExecOutcome::classification()`
    （shell_exec.rs:199）未守卫 `timed_out`。超时被 kill 的命令若部分
    stderr 碰巧含预执行标记（如命令自身输出的 "command not found" 文本），
    会误判 PreExecFailure→回退→**重跑可能已有副作用的命令**，违反
    spec §7.3 不变量"回退仅零副作用"。**修正**：classification 开头加
    `if self.timed_out { return RanFailed }`（单点，两类调用方同享），
    并补 timed_out×marker 交互单测。
  - **F-03（P3·整洁·T-03）**：shell_exec.rs:24 `|| probe_ash()` 冗余闭包
    （clippy redundant_closure）。**修正**：改传 `probe_ash` 本体。

  处置：T-03/T-05/T-06 重开（保留未失效证据并标注失效部分），T-01/T-02/
  T-04 维持，current_step=3。SD-01 spec 文本与实现相符，无需改动；
  F-02 修复后 spec §4 表中 RanFailed 判定行已涵盖（"其余一切（含…超时）"），
  实现将回归契约。
- 2026-09-11（R1 修复 work handoff）：`stage: work | PLAN-033 | r1 |
  outcome: pass | code_commit: 46d169a（单提交修复 F-01/F-02/F-03，承
  914b2f0）| task_ids: T-03/T-05/T-06 复勾，current_step=6 | evidence:
  cargo test -p auto-ai-cli 48/48（新增 timed_out×marker 双向回归 +
  假 ash 回退 live 测试）、cargo test --workspace 287/0、clippy 新代码
  零告警 | blockers: 无 | next: review（R2，用户已授权）`。
- 2026-09-11（**R2 review**）：`stage: review | PLAN-033 | r1 |
  outcome: **pass** | reviewed_commit: 46d169a（worktree 干净）|
  base_commit: 373cd50 | dependency_revisions: auto-lang f26ba9a41（不变）|
  spec_inputs: docs/specs/auto-ai-cli/shell-execution.md（SD-01，内容与
  实现相符——F-02 修复使代码回归 spec §4 既有 RanFailed 判定行，spec 零改）|
  acceptance_results: AC-01…AC-10 全 pass（AC-04 经
  live_pre_exec_failure_falls_back_to_system_shell 补全；AC-08 经
  timed_out_wins_over_pre_exec_markers 双向回归 + 守卫补全）|
  findings: R1 的 F-01/F-02/F-03 全部核销——F-02 守卫置于 classification
  入口先于一切 stderr 判定（单点，两类调用方同享）且注释明确不变量；
  F-01 假 ash 测试断言真实行为（标注+系统壳实际执行+details.executor），
  非镜像实现；F-03 clippy 复验零告警 | evidence: cargo test -p auto-ai-cli
  fresh 复跑 48/48（9.14s）；workspace 287/0 于同 commit 46d169a 当日跑
  （复用理由：reviewed commit 未变）；git show 46d169a diff 审查确认无
  范围外改动 | next: merge`。独立性同 R1 声明（同会话、自工件重建）。
  **状态置 reviewed。**

## 10. 待澄清事项

1. ~~**回退边界确认**~~ **已关闭（2026-09-11 用户确认）**：采纳设计推荐的
   回退矩阵——策略拒绝与命令自身失败不回退，仅预执行失败/ash 缺失回退。
2. **默认安全档位（默认已采纳，可随时否决）**：默认仅 `--sandbox <cwd>`，
   `--no-network` 按次参数开启、`--audit` 走 env（设计 §5.2）。若需更严默认
   （如 always `--no-network`）在 T-03 前提出即可，不阻塞开工。
3. **ash 侧缺口的外部立项**（不阻塞本计划）：脚本运行时错误 exit 0、
   `ash agent` 信封未实现、仅有 debug 构建、**沙箱多路径白名单与项目/会话
   级策略关联**（用户 2026-09-11 提出，分析已随本轮交付，待立 auto-shell
   侧计划）。建议在 auto-shell 侧立项（其 NEXT.md 下一号 081）。
   **负责人：用户/auto-shell 维护者。**
4. **ash 部署形态约定**：生产/分发场景是否以 `cargo install`（~/.cargo/bin）
   为受支持前提，决定兄弟启发式是否仅为开发便利。**负责人：用户；默认：
   PATH/安装位为准，兄弟启发仅兜底（不阻塞执行）。**
