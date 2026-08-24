# Plan 030: CLI UI 视觉改善——对齐、留白与降噪（对标 codex-cli 的排版规则）

> **状态**：✅ 已实施（2026-08-24，Phase 1/2/3/4 落地；同日真机二轮反馈修正）
> **二轮反馈修正（2026-08-24）**：① 块头与块体之间加 1 空行（思考/回答/工具三个 builder）；② divider（回合结束）补前导空行——块尾到分隔线之间留白，下一 block 自带前导空行补齐分隔线之后的留白；③ `/expand` 标题复刻原工具摘要头（`+ 目录 args · ✓ N 行 · #id · 完整结果`），tool_log 扩为 (id, tool, args, result)。
> **仓库**：auto-ai（仅 auto-ai-cli；Plan 029 线性 UI 的视觉打磨，不动事件模型/agent 层）
> **背景**：真机使用反馈——界面"太紧凑、不够整齐"。对比 codex-cli：其干净感来自**规则的统一**（单宽前缀、固定缩进、固定空行节奏、克制的图标使用），而非单纯留白多。

## 1. 问题诊断（现状 vs codex-cli）

| # | 问题 | 根因 |
|---|------|------|
| 1 | block 头起始列参差 | `❯ 💭 ● ⚙` emoji 宽度在终端渲染不一（1~2 列且光标推进不稳定），"图标+空格"的对齐是假的 |
| 2 | 正文与 block 头同级、无层次 | 回答正文（markdown 渲染）从第 0 列开始，工具结果 `  │ ` 却缩进 2 列，规则不一致 |
| 3 | 空行节奏不统一 | thinking/answer/tool 有前导空行，warning/divider 没有；块内头-体间密度无约定 |
| 4 | 工具结果尾预览太密 | `TOOL_RESULT_TAIL = 10` 行，连续工具调用把屏幕塞满 |
| 5 | banner 框线噪音 | `┌─┐│└` 装饰框本身是视觉负担，codex 用平铺文本 |

## 2. 设计规则（本计划定死的排版契约）

- **R1 单宽前缀**：所有 block 头第一个字符必须是窄字符（ASCII 或 Latin-1 Narrow，**禁用 East-Asian Ambiguous 宽度字符与 emoji**——CJK 终端会把 ambiguous 画成 2 列）：用户 `>`（steer `»`、follow-up `»»`）、思考 `~`、回答 `*`、工具 `+`、警告 `!`、错误 `×`。
- **R2 统一头格式**：`<前缀> <标签>  <元信息>`（前缀+1空格，标签+2空格），正文一律缩进 2 列（含 markdown 回答体）。
- **R3 空行节奏**：任意两个 block 之间恰好 1 空行；块内（头与体）0 空行；所有 builder 自带前导空行（warning/divider 补齐）。
- **R4 结果折叠**：工具结果默认显示 2 行 + `… (+N 行 · /expand #id 查看)`；完整内容走 `/expand`。
- **R5 banner 平铺**：三行平铺文本（标题行亮色、directory/model/session 暗色），无框线。

## 3. Phase 划分

### Phase 1: 前缀与对齐（R1+R2）
- [x] `render.rs`：user_lines `❯/⇢/↪` → `>/»/»»`（窄字符；续行缩进随 marker 宽度对齐）；thinking `💭 ` → `~ `；answer `● ` → `• `；tool `⚙  ` → `+ `。
- [x] `answer_lines`：markdown 每行加 2 列缩进（`Span::raw("  ")` 前缀或 render_lines 后统一 prefix）。
- [x] thinking 体、error 体维持 `  ` 缩进（已是）。

### Phase 2: 空行与折叠（R3+R4）
- [x] `warning_lines` 补前导空行 + `!` 前缀；`error_lines` 补 `× 错误` 块头；`divider_lines` 保持无空行（自身就是分隔）。
- [x] `TOOL_RESULT_TAIL` 10 → 2 → **41**（真机三轮反馈：2 折叠太激进，常规 `ls` 都被折；41 行内全量展示，超出走 `/expand`）；`#id` 保留在摘要行尾（/expand 可发现性）；折叠行改为 `… (+N 行 · /expand #id)`。
- [x] 尾部 preview 的思考尾 `♪ ` → `~ `（与提交态一致）。

### Phase 3: banner 平铺（R5）
- [x] `main.rs build_banner`：~~无框线平铺~~ **真机反馈回滚**：恢复框线（平铺被反馈"变难看"），直角框 `┌┐└┘` → 圆角框 `╭╮╰╯`。**真机反馈框内错位，根因不是圆角字符宽度**，而是脚本改写时把字面量的 `
\` 续行转义丢成裸换行——Rust 字符串允许裸换行且换行后缩进成为内容，导致第 2 行起多 9 个前导空格；已用 Edit 工具恢复续行转义，圆角框最终保留。输入框圆角（viewport 渲染路径）正常。

### Phase 4: 验证与文档
- [x] 单测全绿（15 passed）+ clippy 无新增（builder 相关断言）+ `cargo test -p auto-ai-cli` + clippy。
- [x] 本文档状态回写；KNOWN-DEBT 无新增项预期。

## 4. 明确不做
- 输入框样式重构（已有多行自适应逻辑，真机反馈未抱怨边框本身）。
- help 行内容精简（等真机反馈）。
- fullscreen TUI 同步（逃生门，低频路径）。
