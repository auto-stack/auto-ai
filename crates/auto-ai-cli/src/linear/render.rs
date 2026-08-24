//! Linear-UI rendering: the dynamic tail frame, commit-block builders, and
//! the width-aware line wrapper (Plan 029 §2.3–2.4).
//!
//! Two hard rules drive this module:
//! 1. Committed lines must never exceed the terminal width — `insert_before`
//!    clips overflow, so `wrap_lines` pre-wraps (CJK-aware, word-preferred)
//!    and the exact wrapped height is what gets printed.
//! 2. Per-frame cost is bounded by the tail (TAIL_HEIGHT rows) — the preview
//!    only looks at the last few lines of the active streaming text, never
//!    the whole transcript.

use std::io;
use std::time::{Duration, Instant};

use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crossterm::{queue, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Fixed height of the inline viewport:
/// tip(1) + status(1) + preview(≤8) + editor(3) + help(1) = 14.
pub const TAIL_HEIGHT: u16 = 14;

/// Input poll interval — also the render throttle ceiling (Plan 029 §2.4).
pub const POLL_MS: Duration = Duration::from_millis(33);

const SPINNER_FRAMES: [&str; 3] = [".", "..", "..."];
const SPINNER_INTERVAL: Duration = Duration::from_millis(400);
/// Max result-tail lines shown under a committed tool summary before
/// folding kicks in (real-terminal feedback: 2 was too aggressive — a
/// routine `ls` got folded; 41 keeps typical listings fully visible).
const TOOL_RESULT_TAIL: usize = 41;

pub type LinearTerminal = Terminal<CrosstermBackend<io::Stdout>>;

// ── shared styles ─────────────────────────────────────────────────────────────

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn dim_italic() -> Style {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
}

/// Per-tool (verb, accent color) — mirrors the fullscreen TUI's tool_style.
pub fn tool_style(tool: &str) -> (&'static str, Color) {
    match tool {
        "read_file" => ("读取", Color::Blue),
        "write_file" => ("写入", Color::Red),
        "edit_file" => ("编辑", Color::Yellow),
        "list_dir" => ("目录", Color::Blue),
        "search" => ("搜索", Color::Cyan),
        "run_command" => ("命令", Color::Yellow),
        "spawn_pipeline" => ("流水线", Color::Magenta),
        "skill" => ("技能", Color::Magenta),
        _ => ("工具", Color::Blue),
    }
}

// ── commit-block builders (archived into scrollback once, never redrawn) ─────

/// A user message: `> text` (cyan). The marker distinguishes plain sends
/// (`>`), in-run steering (`»`), and queued follow-ups (`»»`) — all narrow
/// glyphs so the text column stays aligned (Plan 030 R1).
pub fn user_lines(text: &str, marker: &str) -> Vec<Line<'static>> {
    let mut out = vec![Line::raw(String::new())];
    let marker_w = marker.width();
    for (i, l) in text.lines().enumerate() {
        let p = if i == 0 { marker } else { &" ".repeat(marker_w) };
        out.push(Line::styled(
            format!("{p}{l}"),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    }
    out
}

/// Thinking text: dim italic (pi-style — visible but visually subordinate).
pub fn thinking_lines(text: &str) -> Vec<Line<'static>> {
    let mut out = vec![Line::raw(String::new())];
    out.push(Line::from(vec![
        Span::styled("~ ", dim()),
        Span::styled("思考", dim_italic()),
    ]));
    out.push(Line::raw(String::new()));
    for l in text.lines() {
        out.push(Line::styled(format!("  {l}"), dim()));
    }
    out
}

/// The final answer: markdown rendered exactly once at commit time
/// (Plan 029 §2.3 — no per-frame re-parsing, unlike the old TUI). Body is
/// indented 2 columns under the header (Plan 030 R2).
pub fn answer_lines(text: &str) -> Vec<Line<'static>> {
    let mut out = vec![Line::raw(String::new())];
    out.push(Line::from(vec![
        Span::styled("* ", Style::default().fg(Color::Green)),
        Span::styled(
            "回答",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
    ]));
    out.push(Line::raw(String::new()));
    for l in crate::markdown::render_lines(text) {
        let mut line = l;
        line.spans.insert(0, Span::raw("  "));
        out.push(line);
    }
    out
}

/// A completed tool call: colored summary line (with its `/expand` id) plus
/// a short result tail (Plan 030 R4: 2 lines by default).
pub fn tool_lines(tool: &str, args_summary: &str, result: &str, id: u64) -> Vec<Line<'static>> {
    let (verb, color) = tool_style(tool);
    let mut out = vec![Line::raw(String::new())];
    out.push(Line::from(vec![
        Span::styled("+ ", Style::default().fg(color)),
        Span::styled(verb.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {args_summary}"), Style::default().fg(Color::Gray)),
        Span::styled(
            format!(" · ✓ {} 行 · #{id}", result.lines().count().max(1)),
            dim(),
        ),
    ]));
    out.push(Line::raw(String::new()));
    for l in result.lines().take(TOOL_RESULT_TAIL) {
        out.push(Line::styled(format!("  │ {l}"), dim()));
    }
    let extra = result.lines().count().saturating_sub(TOOL_RESULT_TAIL);
    if extra > 0 {
        out.push(Line::styled(format!("  … +{extra} 行 · /expand {id}"), dim()));
    }
    out
}

/// An advisory (near-turn-cap etc.) — dimmed so it can't be mistaken for the
/// model's answer. Committed (kept in history) rather than transient.
pub fn warning_lines(text: &str) -> Vec<Line<'static>> {
    vec![
        Line::raw(String::new()),
        Line::styled(format!("! {text}"), Style::default().fg(Color::Yellow)),
    ]
}

pub fn error_lines(text: &str) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::raw(String::new()),
        Line::from(vec![Span::styled(
            "× 错误",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]),
    ];
    for l in text.lines() {
        out.push(Line::styled(format!("  {l}"), Style::default().fg(Color::Red)));
    }
    out
}

/// A turn-end divider (thin rule with usage). Leading blank separates it
/// from the previous block's content; the next block brings its own.
pub fn divider_lines(text: &str) -> Vec<Line<'static>> {
    vec![
        Line::raw(String::new()),
        Line::styled(text.to_string(), dim()),
    ]
}

/// `/expand <id>`: re-commit a tool call's full result, replicating the
/// original summary header plus a "完整结果" suffix (the expanded block
/// must name which tool it was).
pub fn expand_lines(tool: &str, args_summary: &str, result: &str, id: u64) -> Vec<Line<'static>> {
    const MAX_LINES: usize = 300;
    let (verb, color) = tool_style(tool);
    let mut out = vec![
        Line::raw(String::new()),
        Line::from(vec![
            Span::styled("+ ", Style::default().fg(color)),
            Span::styled(verb.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {args_summary}"), Style::default().fg(Color::Gray)),
            Span::styled(
                format!(" · ✓ {} 行 · #{id} · 完整结果", result.lines().count().max(1)),
                dim(),
            ),
        ]),
        Line::raw(String::new()),
    ];
    for l in result.lines().take(MAX_LINES) {
        out.push(Line::styled(format!("  │ {l}"), dim()));
    }
    let extra = result.lines().count().saturating_sub(MAX_LINES);
    if extra > 0 {
        out.push(Line::styled(format!("  │ … (+{extra} 行，超出展示上限)"), dim()));
    }
    out
}

/// Plain dim text (banner, help, notes).
pub fn system_lines(text: &str) -> Vec<Line<'static>> {
    text.lines().map(|l| Line::styled(l.to_string(), dim())).collect()
}


// ── tail frame ────────────────────────────────────────────────────────────────

/// Last `n` lines of `text` (at least one, possibly empty, line).
fn tail_lines(text: &str, n: usize) -> Vec<&str> {
    let mut v: Vec<&str> = text.lines().collect();
    if v.len() > n {
        v = v.split_off(v.len() - n);
    }
    if v.is_empty() {
        v.push("");
    }
    v
}

/// Build the active-block preview: running-tool line, thinking tail (dim),
/// answer tail (plain) — truncated to the preview height with an indicator.
fn build_preview(s: &super::LinearState, height: usize) -> Vec<Line<'static>> {
    if height == 0 {
        return Vec::new();
    }
    let mut v: Vec<Line<'static>> = Vec::new();
    if let Some((tool, args)) = &s.running_tool {
        let (verb, color) = tool_style(tool);
        v.push(Line::from(vec![
            Span::styled("+ ", Style::default().fg(Color::Yellow)),
            Span::styled(verb.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {args} {}…", s.spinner_str()), Style::default().fg(Color::Gray)),
        ]));
    }
    if !s.active_thinking.trim().is_empty() {
        for l in tail_lines(&s.active_thinking, 30) {
            v.push(Line::styled(format!("~ {l}"), dim_italic()));
        }
    }
    if !s.active_answer.trim().is_empty() {
        for l in tail_lines(&s.active_answer, 30) {
            v.push(Line::styled(l.to_string(), Style::default()));
        }
    }
    if v.is_empty() {
        if s.is_streaming {
            v.push(Line::styled(
                format!("~ 思考中 {}", s.spinner_str()),
                dim(),
            ));
        }
        return v;
    }
    let total = v.len();
    if total > height {
        v = v.split_off(total - height);
        let skipped = total - height + 1;
        v[0] = Line::styled(format!("  … ↑ 已接收 {skipped} 行"), dim());
    }
    v
}

/// Draw one tail frame (≤ TAIL_HEIGHT rows) and park the hardware cursor on
/// the editor's caret so IME candidate windows anchor correctly (Plan 029
/// §2.4 — the manual equivalent of pi's CURSOR_MARKER mechanism).
///
/// While a selector is open (pi-style in-place editor swap), the preview +
/// editor region is replaced by the selector list and the hardware cursor
/// stays hidden.
pub fn draw(term: &mut LinearTerminal, s: &super::LinearState) {
    let mut editor_area = Rect::default();
    let _ = queue!(io::stdout(), BeginSynchronizedUpdate);
    let _ = term.draw(|f| {
        if let Some(sel) = &s.selector {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // tip
                    Constraint::Length(1), // status
                    Constraint::Min(0),    // selector (preview + editor slot)
                    Constraint::Length(1), // help
                ])
                .split(f.area());
            let tip_style =
                if s.tip.is_empty() { dim() } else { Style::default().fg(Color::Yellow) };
            f.render_widget(Paragraph::new(s.tip.clone()).style(tip_style), chunks[0]);
            f.render_widget(
                Paragraph::new(format!("auto-ai-cli │ {0}", s.role)).style(Style::default().fg(Color::Green)),
                chunks[1],
            );
            render_selector(f, chunks[2], sel);
            f.render_widget(
                Paragraph::new("↑↓=选择 · Enter=确认 · Esc=取消").style(dim()),
                chunks[3],
            );
            return;
        }

        // Multi-line editor mode: once the input has line breaks, the editor
        // grows (up to 8 rows) at the cost of the streaming preview — the
        // user is composing, not watching the stream. Single-line keeps the
        // compact 3-row box with the full preview.
        let editor_rows: u16 = match s.input.lines().len() {
            0 | 1 => 3,
            n => (2 + n as u16).min(8),
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),          // tip
                Constraint::Length(1),          // status
                Constraint::Min(1),             // active preview (may shrink)
                Constraint::Length(editor_rows), // editor
                Constraint::Length(1),          // help
            ])
            .split(f.area());

        // Tip line: transient notices (steer queued, quit-armed…).
        let tip_style = if s.tip.is_empty() { dim() } else { Style::default().fg(Color::Yellow) };
        f.render_widget(Paragraph::new(s.tip.clone()).style(tip_style), chunks[0]);

        // Status line: only meaningful while streaming (turn/token/progress);
        // hidden when idle to keep the quiet tail minimal.
        if s.is_streaming {
            let state_str = format!("*{} streaming", s.spinner_str());
            let status = format!(
                "auto-ai-cli │ {} │ turn {} · {} tok │ 共 {} tok │ {}",
                s.role, s.turn, s.turn_tokens, s.total_tokens, state_str,
            );
            f.render_widget(
                Paragraph::new(status).style(Style::default().fg(Color::Yellow)),
                chunks[1],
            );
        }

        // Active-block preview.
        let preview = build_preview(s, chunks[2].height as usize);
        if !preview.is_empty() {
            f.render_widget(Paragraph::new(preview), chunks[2]);
        }

        // Editor.
        f.render_widget(&s.input, chunks[3]);
        editor_area = chunks[3];

        // Help line.
        let help = if s.is_streaming {
            "Esc=取消 │ 流式中 Enter=插话(工具批后注入) │ Ctrl-C=再按退出"
        } else {
            "Enter=发送 │ Shift/Ctrl+Enter=换行 │ ↑↓=历史(单行)/行移动(多行) │ /help │ q=退出"
        };
        f.render_widget(Paragraph::new(help).style(dim()), chunks[4]);
    });
    let _ = execute!(io::stdout(), EndSynchronizedUpdate);

    // Hardware cursor policy (pi default): keep it hidden — tui-textarea
    // already renders its own caret, and showing the hardware cursor next to
    // it draws a second (underline) cursor. IME anchoring opts back in via
    // AUTOAI_HARDWARE_CURSOR=1. A selector also owns the tail (no text caret).
    if !hardware_cursor_enabled() || s.selector.is_some() {
        let _ = execute!(io::stdout(), crossterm::cursor::Hide);
        return;
    }
    let (row, col) = s.input.cursor();
    let line = s.input.lines().get(row).map(|l| l.as_str()).unwrap_or("");
    let disp_col: usize = line.chars().take(col).map(|c| c.width().unwrap_or(0)).sum();
    let x = (editor_area.x + 2 + disp_col as u16).min(editor_area.right().saturating_sub(1));
    let y = editor_area.y + 1 + row as u16;
    let _ = term.set_cursor_position(Position::new(x, y));
    let _ = term.show_cursor();
}

/// Whether the hardware (terminal) cursor should be shown at the editor
/// caret. Off by default (double-cursor artifact); on = IME candidate
/// windows anchor to the caret (pi's PI_HARDWARE_CURSOR equivalent).
fn hardware_cursor_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("AUTOAI_HARDWARE_CURSOR").map(|v| v == "1").unwrap_or(false)
    })
}

/// Render the in-place selector list: a bold title, then a scrolling window
/// of items with the cursor row highlighted.
fn render_selector(
    f: &mut ratatui::Frame,
    area: Rect,
    sel: &super::SelectorState,
) {
    let rows = area.height as usize;
    if rows == 0 {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows);
    lines.push(Line::styled(
        sel.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let list_rows = rows.saturating_sub(1);
    let (start, end) = selector_window(sel.selected, sel.items.len(), list_rows);
    for (i, (label, desc)) in sel.items[start..end].iter().enumerate() {
        let idx = start + i;
        let marker = if idx == sel.selected { "▸ " } else { "  " };
        let line = if desc.is_empty() {
            format!("{marker}{label}")
        } else {
            format!("{marker}{label:<14} {desc}")
        };
        let style = if idx == sel.selected {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        lines.push(Line::styled(line, style));
    }
    let extra = sel.items.len().saturating_sub(list_rows);
    if extra > 0 {
        lines.push(Line::styled(format!("  … 共 {extra} 项未显示"), dim()));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Visible window [start, end) for a list of `len` items with `selected`
/// highlighted inside `rows` visible rows (follow-the-cursor scrolling).
fn selector_window(selected: usize, len: usize, rows: usize) -> (usize, usize) {
    if rows == 0 || len == 0 {
        return (0, 0);
    }
    let rows = rows.min(len);
    let start = selected.min(len - 1).saturating_sub(rows - 1).min(len - rows);
    // Keep the selected row inside [start, start+rows).
    let start = if selected < start { selected } else { start };
    (start, start + rows)
}

// ── width-aware wrapper ───────────────────────────────────────────────────────

/// Wrap styled lines to `width` display columns. Each input line becomes at
/// least one output line; output lines never exceed `width` cells, so the
/// printed height of a commit can be computed exactly.
pub fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }
    lines.into_iter().flat_map(|l| wrap_line(l, width)).collect()
}

/// Greedy wrap of one line, preferring break points after spaces and before
/// CJK characters (both are safe in terminals); falls back to a hard break
/// for unbreakable runs (e.g. long URLs).
fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    // Flatten to (char, style) so spans can be regrouped after breaking.
    let flat: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|sp| {
            let st = sp.style;
            sp.content.chars().map(move |c| (c, st))
        })
        .collect();
    if flat.is_empty() {
        return vec![line];
    }

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    let mut cur_w = 0usize;
    // Break opportunity: index into `cur` where a new line may start.
    let mut brk: Option<usize> = None;

    let flush = |out: &mut Vec<Line<'static>>, cur: Vec<(char, Style)>, base: Style| {
        out.push(rebuild_line(cur, base));
    };

    for (c, st) in flat {
        let w = c.width().unwrap_or(0);
        if cur_w + w > width && !cur.is_empty() {
            if c == ' ' {
                // The overflowing char is a space: end the line here and drop
                // it — classic word-wrap behavior (no trailing-space line).
                flush(&mut out, std::mem::take(&mut cur), line.style);
                cur_w = 0;
                brk = None;
                continue;
            }
            if w >= 2 {
                // A CJK char may start its own line — prefer that over an
                // earlier break point so lines are filled as much as possible.
                flush(&mut out, std::mem::take(&mut cur), line.style);
                cur_w = 0;
                brk = None;
            } else {
                // Break at the last opportunity if one exists in the middle
                // of the line; otherwise hard-break here.
                match brk.filter(|b| *b > 0 && *b < cur.len()) {
                    Some(b) => {
                        let rest = cur.split_off(b);
                        flush(&mut out, cur, line.style);
                        cur = rest;
                    }
                    _ => {
                        flush(&mut out, std::mem::take(&mut cur), line.style);
                    }
                }
                cur_w = cur.iter().map(|(ch, _)| ch.width().unwrap_or(0)).sum();
                brk = None;
            }
        }
        // A CJK char is itself a valid break point (break *before* it).
        if w >= 2 {
            brk = Some(cur.len());
        }
        cur_w += w;
        cur.push((c, st));
        // A space ends a word — the next char may start a new line.
        if c == ' ' {
            brk = Some(cur.len());
        }
    }
    if !cur.is_empty() {
        flush(&mut out, cur, line.style);
    }
    if out.is_empty() {
        out.push(rebuild_line(Vec::new(), line.style));
    }
    out
}

/// Regroup (char, style) cells into a Line, merging consecutive same-style
/// runs into spans, preserving the line's base style.
fn rebuild_line(cells: Vec<(char, Style)>, base: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur_style: Option<Style> = None;
    for (c, st) in cells {
        match cur_style {
            Some(s) if s == st => buf.push(c),
            _ => {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), cur_style.unwrap()));
                }
                cur_style = Some(st);
                buf.push(c);
            }
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, cur_style.unwrap()));
    }
    let mut l = Line::from(spans);
    l.style = base;
    l
}

/// Spinner helper shared with the state module: advance and report change.
pub fn tick_spinner(is_streaming: bool, frame: &mut usize, last: &mut Instant) -> bool {
    if !is_streaming {
        return false;
    }
    if last.elapsed() >= SPINNER_INTERVAL {
        *frame = (*frame + 1) % SPINNER_FRAMES.len();
        *last = Instant::now();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Line<'static> {
        Line::raw(text.to_string())
    }

    fn display_width(l: &Line<'static>) -> usize {
        l.spans
            .iter()
            .flat_map(|s| s.content.chars())
            .map(|c| c.width().unwrap_or(0))
            .sum()
    }

    fn chars_of(lines: &[Line<'static>]) -> usize {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().flat_map(|s| s.content.chars()))
            .count()
    }

    /// Committed lines must never exceed the terminal width — `insert_before`
    /// clips overflow, so this invariant is what makes commit heights exact.
    #[test]
    fn wrap_never_exceeds_width_and_loses_no_chars() {
        let cases = [
            "aaaa bbbb cccc dddd",
            "你好世界你好世界测试",
            "mixed 中文and english words in one line",
            "unbreakable-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "",
            "short",
        ];
        for text in cases {
            let out = wrap_lines(vec![plain(text)], 10);
            assert!(!out.is_empty(), "at least one line: {text:?}");
            for l in &out {
                assert!(display_width(l) <= 10, "{text:?} → line too wide: {l:?}");
            }
            assert_eq!(chars_of(&out), text.chars().count(), "char loss: {text:?}");
        }
    }

    /// CJK chars are valid break points: a pure-CJK run wraps per character
    /// pair at width 4 (each CJK char is 2 cells).
    #[test]
    fn wrap_cjk_breaks_per_char() {
        let out = wrap_lines(vec![plain("你好世界")], 4);
        assert_eq!(out.len(), 2);
        assert_eq!(display_width(&out[0]), 4);
        assert_eq!(display_width(&out[1]), 4);
    }

    /// Word boundaries win over hard breaks when available.
    #[test]
    fn wrap_prefers_space_breaks() {
        let out = wrap_lines(vec![plain("alpha beta")], 10);
        assert_eq!(out.len(), 1, "fits exactly, no wrap: {out:?}");
        let out = wrap_lines(vec![plain("alpha beta gamma")], 10);
        // "alpha beta" fits; "gamma" moves down whole (no mid-word cut).
        let last = out.last().unwrap();
        let text: String = last
            .spans
            .iter()
            .flat_map(|s| s.content.chars())
            .collect::<String>();
        assert_eq!(text.trim(), "gamma");
    }

    /// Styles survive wrapping: a two-tone line wrapped apart keeps each side's
    /// span style.
    #[test]
    fn wrap_preserves_span_styles() {
        let line = Line::from(vec![
            Span::styled("red part ".to_string(), Style::default().fg(Color::Red)),
            Span::styled("green part".to_string(), Style::default().fg(Color::Green)),
        ]);
        let out = wrap_lines(vec![line], 9);
        assert!(out.len() >= 2);
        let first: String = out[0].spans.iter().flat_map(|s| s.content.chars()).collect();
        assert_eq!(first.trim(), "red part");
        assert_eq!(out[0].spans[0].style.fg, Some(Color::Red));
        let last = out.last().unwrap();
        assert_eq!(last.spans[0].style.fg, Some(Color::Green));
    }

    /// Preview truncation keeps exactly `height` lines with an indicator on top.
    #[test]
    fn preview_truncates_to_height() {
        let mut s = crate::linear::LinearState::new("assistant");
        s.active_answer = (1..=20).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let p = build_preview(&s, 4);
        assert_eq!(p.len(), 4);
        let first: String = p[0].spans.iter().flat_map(|sp| sp.content.chars()).collect();
        assert!(first.contains('↑'), "indicator expected: {first}");
        // The newest content is always visible.
        let last: String = p[3].spans.iter().flat_map(|sp| sp.content.chars()).collect();
        assert_eq!(last, "line20");
    }

    /// Idle with nothing active renders an empty preview (no spinner ghost).
    #[test]
    fn preview_empty_when_idle() {
        let s = crate::linear::LinearState::new("assistant");
        assert!(build_preview(&s, 8).is_empty());
    }
}

#[cfg(test)]
mod selector_tests {
    use super::selector_window;

    /// The selector window keeps the highlighted row visible (follow-cursor).
    #[test]
    fn selector_window_follows_cursor() {
        // 10 items, 3 visible rows: selected=7 → window [5,8).
        assert_eq!(selector_window(7, 10, 3), (5, 8));
        // Top of list clamps to [0,3).
        assert_eq!(selector_window(0, 10, 3), (0, 3));
        // Bottom of list keeps the last rows visible.
        assert_eq!(selector_window(9, 10, 3), (7, 10));
        // More rows than items → whole list.
        assert_eq!(selector_window(2, 3, 10), (0, 3));
    }
}
