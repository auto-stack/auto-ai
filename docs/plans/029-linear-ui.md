# Plan 029: auto-ai-cli 交互形态重构——线性输出 + 尾部动态（主屏 inline viewport + 按需模态）

> **状态**：✅ 已实施（2026-08-24，Phase 1/2/3/5/6 落地；Phase 6.5 /tree 挂起（前置条件缺失）、Phase 6.6 人工验证清单待做）
> **finish-plan 复审（2026-08-25）**：代码逐项核验通过（15 单测 + clippy 基线一致，本会话重跑）；Phase 6.5 延期项已登记 KNOWN-DEBT。
> **Phase 6.6 冒烟（2026-08-25，agent 驱动 Windows Terminal 真机）**：✅ CJK 提交渲染（完整回合 `~ 思考`→`+ 目录 #1`→`+ 读取 #2`→`* 回答`→`──── 回合结束`，中文正文无"字 字 字"错乱——insert_before 补丁生效）；✅ banner 圆角框右侧对齐；✅ `/clear` 清屏 + 回滚区 Purge（滚轮回看无残留）+ viewport 重锚；✅ resize（Win+左/右贴靠宽度减半再恢复，尾部重绘正常、无崩溃）；✅ 退出收尾干净（提示符正常落位、无 UI 残留）。**仍需人工**：鼠标框选复制（含跨块多行）、Windows IME 候选窗锚定（AUTOAI_HARDWARE_CURSOR=1）、VSCode 终端 / tmux 环境矩阵、流式性能体感。
> **仓库**：auto-ai（auto-ai-cli 主改；agent / ai-config / daemon / musk 零改动——本计划是 PLAN-026 事件模型的消费端）
> **实施记录与计划偏差**（finish-plan 复审 2026-08-24，复审后以 Phase 6 补全）：
> 1. ~~follow_up 未接线~~ → **Phase 6.1 已补**：终答流式期（无运行工具且 active_answer 非空）Enter → `follow_up()`（回合自然结束后复活），其余流式期 → `steer()`（工具批后注入）；标记 `⇢`/`↪` 区分。
> 2. **Warning 改为提交历史**（计划表格写"尾部提示区"）：near-turn-cap 类告警值得留在转录里，提交为暗黄色行。
> 3. ~~~/clear 不清终端回滚区~~ → **Phase 6.4 已补**：`Clear(Purge)` 清回滚区 + 清屏 + 重建 Terminal 重锚 viewport + Reset 时同步清空 session 文件；重锚路径待真终端手测。
> 4. ~~Phase 4 模态层未实施~~ → **Phase 6.2/6.3 部分补全**：`/expand <id>` 引用式追加（tool_log 登记 + 摘要行尾 `#N`）、`/roles` 原地选择器（尾部换渲染，↑↓/Enter/Esc，确认后 `SetRole` 重建 agent 且先保存旧记忆）。**/tree 挂起**：CLI 会话无分支树可展示（pi 的 /tree 导航分支会话），待会话分支功能落地；`--mode fullscreen` 为全屏逃生门。
> 5. **chat_model 小改未做**（"UI 状态移出模型 + 块稳定 ID"）：线性 UI 不再使用 ChatLog（提交即归档，内存只留 agent 侧会话），该重构只对 fullscreen TUI 有意义，降级为后续可选。
> 6. 死代码清理完成：tui.rs 的 update_list_state/line_height/block_body_height（vestigial ListState 路径）已删除。
> 7. **人工验证待做**（Phase 6.6 清单）：Windows Terminal / tmux / VSCode 三环境的原生复制（含 CJK）、resize、IME、/clear 重锚、流式性能体感（自动化覆盖不到）。
> 8. **首跑回归修复（2026-08-24 真机反馈）**：① ratatui 0.29 `insert_before` 宽字符 bug——提交路径 `draw_lines` 不过 `Buffer::diff`，CJK 续接 cell（空格）被打印覆盖字形右半，中文输出全部 "字 字 字" 错乱；本地补丁 `sanitize_commit_buffer`（提交前清空续接 cell；行尾 padding 清空曾致滚动残迹透出——`Print("")` 不覆盖旧屏内容，2026-08-24 二轮回归后撤销，仅保留续接 cell 清空），上游修复后删除（已登记 KNOWN-DEBT）。② 输入框双光标（硬件光标下划线 + textarea 块光标）——硬件光标默认隐藏，`AUTOAI_HARDWARE_CURSOR=1` 显式开启（pi 同款默认）。③ 块间缺空行——thinking/answer/tool 提交块补前置空行分隔。 ④ 空闲时尾部状态行（turn/token/就绪）不渲染，仅流式运行中显示（黄色），减少常驻噪音。
> **目标**：CLI 交互形态从"全屏接管 TUI"重构为"线性输出 + 尾部动态"：主屏渲染（不进 alternate screen、不捕获鼠标）+ ratatui `Viewport::Inline` 承载动态尾部 + `Terminal::insert_before` 把完成块提交进终端原生回滚区 + 重交互按需进入模态。修复两类问题：**性能**（每帧全屏重绘 + 全量 markdown 重解析）与**可用性**（原生复制/搜索/链接失效）。
> **参考实现**：pi-mono 本地克隆 `D:\github\pi`（main @ a1f955e9f），`packages/tui/`（TuiMainScreen 为主）+ `packages/coding-agent/src/modes/interactive/interactive-mode.ts`。
> **前置**：PLAN-026 已实施（StreamEvent 含 turn 层，`steer()/follow_up()/` 取消已暴露但 TUI 未消费——本计划补上）。
> **关联计划**：PLAN-027/028 互不影响；auto-musk 不受影响。
> **事实核验**（2026-08-24，本机依赖源码）：
> 1. ratatui 0.29.0 `Terminal::insert_before(height, draw_fn)` 存在且与 `Viewport::Inline` 配套（`terminal.rs:571`）；默认特性走 no-scrolling-regions 实现（逐块重打，一次性开销 O(提交行数)），`scrolling-regions` 为可选特性。
> 2. crossterm 0.28.1 原生提供 `BeginSynchronizedUpdate`/`EndSynchronizedUpdate`（CSI 2026，`terminal.rs:435/488`）。
> 3. auto-ai-cli **不在 .at 转译轨**（crate 内无 .at 文件），本计划无双轨同步负担。
> 4. **认知修正**：pi 当前实现**没有**"shift 打印进回滚区"技巧（早期 pi-tui 原型的机制，现行代码全历史零命中）。`TuiMainScreen` 的真实机制是"全文档常驻组件树 + 行级 diff 增量重绘"，被顶出视口的行自然落入原生回滚区。详见 §1。

---

## 0. 问题与动机

现状（`crates/auto-ai-cli/src/tui.rs`，916 行）：

1. **全屏接管三件套**：`enable_raw_mode` + `EnterAlternateScreen` + `EnableMouseCapture`（tui.rs:132-137）——终端原生回滚、鼠标框选复制、搜索、OSC 8 链接点击全部失效。这是"无法复制"的根源。
2. **性能模型错误**：固定 ~50ms 循环全屏重绘（tui.rs:199-222），每帧从零重建**全部**历史样式行（`build_chat_lines` → `render_block` → `render_tool_block`），且 Answer 块**每帧重新解析 markdown**（tui.rs:791-796，注释自认"靠 pulldown-cmark 容错"）。会话越长帧成本线性上涨。
3. **自维护伪滚动**：`auto_scroll`/`scroll_offset`/`ListState`/`line_height` 估算（tui.rs:55-65, 236-284, 416-443）——在 ratatui Paragraph offset 上手搓滚动，注定长尾 bug，且完全多余：线性形态下历史归终端管。
4. **PLAN-026 能力闲置**：agent 已暴露 `steer()`/`follow_up()`/队列查询（agent.rs:346-366），当前 TUI 流式中**直接丢弃键盘输入**，既不能插话也没 pending 反馈。
5. **无 IME 处理**（仅 Windows ghost-Enter 规避）；**无 non-TTY 降级**（裸 `auto-ai-cli` 必进全屏，管道/CI 场景必须知道 `run` 存在）。

## 1. pi 参考实现索引（移植蓝本）

pi 仓库前缀 `D:\github\pi\packages\`：

| 关注点 | pi 位置 | 移植要点 |
|---|---|---|
| 主屏渲染器 `TuiMainScreen`：无 alt screen / 无鼠标上报 / 无 scroll region，全文档挂载 + 行级 diff | `tui/src/tui-main-screen.ts:57, 180-547` | **机制参考，不逐字移植**——行级 diff + 底部锚定由 ratatui `Viewport::Inline` 的 Buffer diff 等价覆盖；"提交进回滚区"由 `insert_before` 等价覆盖 |
| diff 三策略：首帧全量 / 宽度变化或视口上方变化全量重绘 / 热路径光标上移+逐行擦写 | 同上 `:210-529` | resize 与宽度变化语义直接借鉴（我们：尾部全量重绘，归档不动，见 §2.3） |
| synchronized output（CSI 2026）防闪烁 | 同上 + `terminal.ts` | crossterm 0.28.1 原生封装，每帧 flush 包裹 |
| 尾部构成：document → pendingMessages → status → widgetsAbove → editor → widgetsBelow → footer 按序渲染，diff 天然钉底 | `coding-agent/src/modes/interactive/interactive-mode.ts:938-946` | 我们的尾部四区布局直接对标（pending 区 V1 简化为状态行提示，见 §2.6） |
| 流式渲染：`message_update` 重建当前消息组件，Markdown 组件 (text,width) 缓存 + 未闭合代码围栏裁剪防闪烁；16ms 渲染节流 | `tui/src/components/markdown.ts:146-169, 277-281`；`tui.ts:343, 764-824` | 我们更简单：**流式期间尾部只显原文预览，块完成时渲染一次 markdown 并提交**——解析成本从每帧全量降为每块一次 |
| `/tree` 等选择器：**编辑器区原地换组件**，非真弹窗，无需屏幕保存/恢复 | `interactive-mode.ts:4502-4525`（`showSelector`） | 轻量模态的原型（§2.7） |
| 双渲染器架构：TuiMainScreen（默认）+ TuiAltScreen（按需全屏），pi 自己也补了 alt 模式——主屏模式下终端拥有滚动权，应用做不到 sticky dock / 嵌套滚动 / 可靠命中测试 | `tui/src/tui-alt-screen.ts`；根目录 `tui-plan.md`（alt 布局系统设计文档） | **直接支持本计划的三层划分**：日常线性 + 重交互走 alt-screen 模态；旧 tui.rs 降级复用为模态层（§2.7） |
| IME 锚定：编辑器假光标 + `CURSOR_MARKER`（APC 零宽序列）→ TUI 把**硬件光标**移到该处 | `tui/src/components/editor.ts:537-570`；`tui-main-screen.ts:554-585` | ratatui 方案的手工等价：draw 后按编辑器光标位置 `MoveTo` + `Show`（§2.4） |
| 消息首尾 OSC 133 标记（终端提示符跳转） | `coding-agent/src/components/assistant-message.ts:7-9, 84-86` | **V1 放弃**：ratatui Buffer 是 cell 网格装不下零宽序列，需绕过 `insert_before` 自写 raw 提交通道才可实现，留 V2 评估（§5） |
| 每行尾 `\x1b[0m\x1b]8;;\x07` 复位防样式泄漏 | `tui/src/tui.ts:1160-1169` | ratatui Buffer 每格独立 style + backend 正确发 SGR，**天然免疫**，无需移植 |
| 非 TTY 自动降级 print 模式 | `coding-agent/src/main.ts:109-119` | 我们补 isatty 检测（§2.8） |
| 崩溃兜底：uncaughtException 强制恢复终端；退出时光标落文档末尾 + 换行 | `interactive-mode.ts:3987-4004`；`tui-main-screen.ts:101-109` | panic hook + Drop guard 双保险（§2.8） |
| 桌面级进度 OSC 9;4 | `tui/src/terminal.ts:531-545` | 可选增强，Phase 5 顺手 |

**刻意不移植**：pi 的自研 diff 渲染器（ratatui 已提供等价物）、2363 行自研编辑器（保留 tui-textarea）、kitty keyboard 协议协商（crossterm 基础键位够用）。

## 2. 方案

### 2.1 核心决策

- **D1 用 ratatui 内建形态，不重造渲染器**：`Viewport::Inline(N)` 主屏锚定底部 + Buffer diff 只作用于尾部 N 行；`insert_before` 完成提交。pi 自研是因为 TS 生态没有 ratatui。
- **D2 与 pi 的关键分歧——归档层不可变**：pi 全文档常驻、任意行可变重绘（所以能重生成/树导航重排）；我们提交进回滚区的块**永不重绘**。收益：复制/搜索原生可用、零重绘成本、内存只留数据模型（chat_model）；代价：改历史的操作只能"追加新记录"表达，浏览/重排交给模态层（与 pi 自己补 alt-screen 的动机一致）。此分歧是**有意决策**，非实现妥协。
- **D3 三层模型**：线性归档层（不可变、零成本）/ 动态尾部（inline viewport，≤14 行）/ 模态层（按需、短命，重型走 alt screen）。
- **D4 旧 tui.rs 不删**：降级为 `--mode fullscreen` 与重型模态的渲染设施；自维护滚动代码随观察期结束删除。

### 2.2 架构与模块（auto-ai-cli 内）

```
src/
├── main.rs            # +--mode linear 接线（Phase 5 默认切换）；isatty 分流
├── agent_task.rs      # 新：从 tui.rs:156-195 抽出的常驻 agent 任务 + 通道架构（UI 无关，两 UI 共用）
├── linear/            # 新：线性 UI
│   ├── mod.rs         # run_linear_chat 入口 + 事件循环（select 流事件/按键/tick）
│   ├── term.rs        # 终端生命周期（raw/Inline(14)/CSI 2026/panic hook/Drop guard）+ commit()
│   ├── tail.rs        # 尾部四区布局与渲染（状态行/活动块预览/编辑器/提示行）
│   └── events.rs      # StreamEvent → LinearState 翻译（自 tui.rs:492-551 平移扩展）
├── chat_model.rs      # 小改：UI 状态（streaming_idx 作用域、collapsed）移出模型；块加稳定 ID（为 /expand 预留）
├── markdown.rs        # 复用：提交时的 markdown→Buffer 渲染桥
├── session.rs         # 不动
└── tui.rs             # 降级为 fullscreen 模态（Phase 4 重构为可进入/退出的模态会话）
```

`App` 上的杂散状态（spinner/cancel/历史，tui.rs:44-66）归入 `LinearState`；错误格式统一用 main.rs:483-556 的 rich formatter（废弃 tui.rs:554-564 简版）。

### 2.3 线性归档层（提交语义）

- **提交动作**：`commit(height, render_fn)` → 渲染进一次性 Buffer → `terminal.insert_before` → 内容永久进入原生回滚区，inline 区自动下移。提交后永不重绘；**宽度变化不重排历史**（归档不可变的既定语义，与讨论结论一致）。
- **markdown 每块只解析一次**：流式期间尾部只显纯文本预览（§2.4）；块完成（TurnEnd 后的最终答案 / Cancelled 的部分产出）时解析一次、渲染、提交。根治每帧重解析。
- **提交内容**：
  - 用户消息：发送即提交（右对齐/前缀样式标记）；
  - 最终答案：markdown 渲染 + 前后留空行；
  - thinking：全文暗色提交（与 pi 展示 thinking 一致，颜色区分）；
  - 工具调用：摘要行 1-2 行（`⚙ read src/main.rs · ✓ 214 行`，成功绿/失败红/取消黄）+ 结果尾部 ≤10 行预览；全文由模态 `/expand <id>` 查看（Phase 4）；
  - 错误：统一 rich 格式；Cancelled：部分产出照常提交 + `[已取消]` 标记行。
- **会话恢复**（`-c`）：打印分隔行 + 最近 3 轮重放，其余静默载入 chat_model/memory；全量历史由模态查看。避免重启时把上百轮灌进回滚区。
- **`/clear` 语义重定义**：清空 chat_model + 发送 `\x1b[2J\x1b[3J` 清屏清回滚区——唯一允许动回滚区的操作，用户显式要求。

### 2.4 动态尾部（Viewport::Inline(14)，固定高度）

```
[插话/告警提示   0–1 行]   ← 流式中 Enter 后 "已排队，当前工具批后注入"；Warning
[状态行           1 行 ]   ← spinner、turn N、tokens(TurnEnd.usage 累计)、耗时、模式
[活动块预览      ≤8 行 ]   ← 流式中 Delta/Thinking/ToolStart 的纯文本尾部窗口，
                              超限显示尾部 8 行 + "… ↑ 已接收 N 行"
[编辑器           3 行 ]   ← tui-textarea（多行）
[快捷键提示       1 行 ]
```

- **高度固定 14**：ratatui Inline 高度创建时定死，变高需重建 Terminal——不折腾，预览区内部滚动（pi 编辑器 max-height 同思路）。
- **渲染节流**：事件到达标记脏，循环合并渲染，下限 30-50ms（对齐 pi 16ms~我们保守取 33ms）；每帧 flush 以 `Begin/EndSynchronizedUpdate` 包裹（`insert_before` 调用同样包裹）。
- **硬件光标定位（IME 关键）**：每帧 `terminal.draw` 后，取 tui-textarea `cursor_position()` + 编辑器区域偏移计算屏幕坐标，`MoveTo` + `Show`；非编辑焦点（纯流式查看）`Hide`。这是 pi `CURSOR_MARKER` 机制的手工等价，IME 候选窗才能锚定在输入位置。
- **按键（V1）**：Enter 发送 / 流式中=插话；Shift+Enter 换行；Esc 取消流式（沿用 `Arc<AtomicBool>` 5 检查点）；Ctrl-C 退出（流式中首按提示再按确认，对齐 pi 双击退出）；Up/Down 编辑器内移动，空行首行 Up 进历史（沿用 tui.rs:356-377 规则）；**PageUp/Down 与鼠标滚动删除**——历史交给终端。

### 2.5 事件映射（StreamEvent → 动作）

| 事件 | 归档层 | 尾部 |
|---|---|---|
| TurnStart | — | 状态行 turn N，spinner 起 |
| Thinking | — | 预览追加（暗色） |
| Delta | — | 预览追加 |
| ToolStart | — | 预览切工具进行中样式 |
| Tool | 工具摘要行 + 尾预览 | 预览清空 |
| TurnEnd | — | 状态行更新 usage 累计 |
| Warning | — | 提示区黄字 |
| Done | 最终答案 markdown 提交 | 预览清空、状态复位、编辑器聚焦 |
| Cancelled | 部分产出 + `[已取消]` 行 | 复位 |
| Error | 统一格式错误块 | 复位 |

### 2.6 输入与流式中插话（消费 PLAN-026）

- 流式中 Enter：立即提交显示该用户消息（带 `⇢` 插话标记）+ 状态行提示排队，调用 `agent.steer()`——注入时机（当前工具批后、下轮 LLM 前）由 agent 保证，转录展示顺序与记忆注入顺序的轻微错位以标记消化，**V1 不做独立 pending 区状态机**；实测混乱再加 pi 式 pendingMessages 区。
- run 结束瞬间的输入自动归类 `follow_up()`（循环里按 agent 状态分派）。
- 斜杠命令 `/help /roles /clear /config` 平移（/clear 见 §2.3）。

### 2.7 模态层（Phase 4）

- **轻量**（y/n 确认、`/roles`、`/config` 列表选择）：pi 式**编辑器区原地换组件**，复用尾部区域，结束恢复。
- **重型**（`/tree` 会话树、`/expand <id>` 工具全文、会话恢复选择器）：进入 alternate screen 的全屏 ratatui TUI——**复用旧 tui.rs 的渲染设施**；退出 `LeaveAlternateScreen` 后尾部全量重绘，线性流无损。

### 2.8 终端生命周期与降级

- 进入：`enable_raw_mode` + `EnableBracketedPaste`（crossterm 0.28 支持）；**不进 alt screen、不捕获鼠标**——这是"能复制"的全部秘密。
- 退出：`Show` 光标、禁 raw/paste，光标落转录末尾 `\r\n`，shell 提示符干净落在下方（pi 同款收尾）。
- 兜底：panic hook + Drop guard 双保险恢复终端；退出码区分用户退出/崩溃。
- **非 TTY 降级**：stdin 或 stdout 任一非 tty（或显式 `--mode print`）→ 线性打印模式（复用 chat_loop 的流式打印骨架），`run`/`pipeline`/superpowers|relay REPL 不动。
- **入口变化**：裸 `auto-ai-cli`（TTY）默认进线性模式（原全屏 TUI 改 `--mode fullscreen`）；Phase 5 切换默认。

## 3. 任务分解

**Phase 1 骨架（`--mode linear` 可试用，旧 TUI 默认不变）**
1. 抽取 `agent_task.rs`（tui.rs:156-195 的通道/常驻任务，两 UI 共用）；
2. `linear/term.rs`：生命周期 + `commit()` + CSI 2026 包裹 + panic hook/Drop guard；
3. `linear/mod.rs` 事件循环 + `tail.rs` 四区渲染（预览纯文本）+ tui-textarea + 硬件光标定位；
4. `events.rs` 平移扩展 handle_stream_event；纯文本块提交；
5. 输入：Enter/Esc/Ctrl-C/历史；流式中 Enter → steer()；
6. `main.rs` 接 `--mode linear` 与 `-c` 恢复（分隔行 + 最近 3 轮）。

**Phase 2 质感**
1. markdown 提交渲染（markdown.rs 桥 → 一次性 Buffer → insert_before）；
2. 工具摘要行 + 尾预览 + 状态着色；thinking 暗色提交；
3. 错误块统一 rich 格式；Cancelled 部分产出 + 标记；
4. 渲染节流 33ms + 脏标记。

**Phase 3 交互完善**
1. follow_up 自动归类；usage 累计入状态行；
2. 斜杠命令平移 + /clear 重定义；
3. Warning 提示区。

**Phase 4 模态层**
1. 轻量选择器原地换编辑器区；
2. 重型 alt-screen 模态（/tree、/expand）复用 tui.rs 设施；进出后线性流完整性验证。

**Phase 5 收尾与切换**
1. isatty 降级 + 裸命令默认线性（全屏挂 `--mode fullscreen`）；
2. resize 专项手测：Windows Terminal / tmux / VSCode 终端 / ConPTY（宽度变化尾部重绘、归档不重排）；
3. 删自维护滚动死代码（tui.rs:236-284 的 ListState/line_height 等 vestigial 部分）；
4. ARCHITECTURE.md 补 auto-ai-cli 章节；可选 OSC 9;4。

**Phase 6 补全（2026-08-24 finish-plan 复审后追加——吸收原偏差 1/3/4 与手测项）**
1. **follow_up 语义分流**：流式中 Enter 的分派规则——工具运行中或尚无终答 → `steer()`（工具批后注入）；终答正在流式（无运行工具且 active_answer 非空）→ `follow_up()`（本回合自然结束后以该消息复活 run）；
2. **/expand 引用式追加**：Tool 事件登记 tool_log（自增 id），摘要行显示 `#N`；`/expand N` 把全文以暗色块提交追加进线性流（引用式追加语义，替代重型模态）；
3. **/roles 原地选择器**：尾部换渲染（预览+编辑器区合并为列表，无需重建 Terminal/变高），↑↓ 导航、Enter 确认、Esc 取消；确认后 `SetRole` → 先保存旧记忆再按新角色重建 agent（`-c` 历史不丢）；
4. **/clear 清屏**：`Clear(Purge)` 清回滚区 + 重建 Terminal 重锚 inline viewport + 清空 session 文件；
5. **/tree 重型模态：挂起**——前置条件缺失：CLI 会话是线性的，没有分支树可展示（pi 的 /tree 导航的是分支会话）。待会话分支功能落地后再评估；过渡期 `--mode fullscreen` 是全屏逃生门（2026-08-25 finish-plan 复审：已登记 KNOWN-DEBT 延期项）；
6. **人工验证清单（不可自动化）**：三终端（Windows Terminal / tmux / VSCode）鼠标原生复制（含 CJK）、resize 行为、Windows IME、/clear 清屏后 viewport 重锚、流式性能体感。

## 4. 验收标准

- 主屏模式：退出后完整转录留在终端回滚区；Windows Terminal / tmux / VSCode 三环境鼠标原生框选复制得到正确文本（含 CJK 不乱码）；全程无鼠标上报（模态期间除外）。
- 性能：5K token 流式期间每帧 diff 仅覆盖尾部 ≤14 行；提交块零重绘；markdown 每块解析次数=1（日志断言）；100+ 轮会话空闲 CPU≈0、内存不随帧累积。
- 功能：流式中 Enter → 消息立即显示 + steer 注入（ScriptedClient 断言注入时机，复用 PLAN-026 基建）；Esc 取消保留部分产出；`-c` 恢复后可继续对话且 memory 完整。
- 回归：`--mode fullscreen`、`run`、`pipeline`、superpowers/relay REPL 行为不变；现有测试全过。
- 健壮性：panic 后终端状态完好（raw mode 无泄漏）；管道下运行自动 print 模式。

## 5. 风险与边界

1. **Inline 高度固定**：变高需重建 Terminal；以固定 14 行 + 预览内部滚动规避。若未来尾部需求超预算，评估重建 Terminal 或模态化。
2. **insert_before 实现路径**：默认特性逐块重打（一次性 O(行数) 开销，可接受）；可选启用 ratatui `scrolling-regions` 特性优化；tmux 复制模式等场景手测。
3. **resize 边缘行为**：ratatui inline viewport 有历史 bug 记录；resize 只重绘尾部（归档不动是既定语义），Phase 5 手测矩阵兜底。
4. **OSC 133 放弃（V1）**：Buffer cell 网格装不下零宽序列；若要支持需自写绕过 ratatui 的 raw 提交通道，V2 评估。
5. **IME（Windows 重点）**：raw mode 下 IME 组合事件经 crossterm 的传递需实测；硬件光标定位是 pi 验证过的锚定路径，bracketed paste 兜底长中文输入。
6. **归档不可变的语义边界**：重生成/编辑历史不可行，只能追加新记录 + 模态浏览；若未来产品语义强依赖历史重排，需回到 pi 式全文档挂载（届时本方案的模态层仍成立）。
7. **双轨**：auto-ai-cli 无 .at 轨（已核验），无同步负担。

## 6. 对其他模块的影响

零：agent / ai-config / daemon / auto-musk 均不改。唯一跨文件清理是 CLI 内部（agent_task 抽取、错误格式统一、滚动死代码删除）与 ARCHITECTURE.md 补章节。
