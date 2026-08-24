//! Linear chat UI: "linear output + dynamic tail" (Plan 029).
//!
//! Three layers:
//! 1. **Archive** — completed blocks (user messages, tool summaries, the
//!    final markdown answer…) are committed once into the terminal's native
//!    scrollback via `insert_before` and never redrawn. Native copy, search,
//!    and scrollback all just work because we never capture the mouse or
//!    enter the alternate screen.
//! 2. **Tail** — a fixed 14-row inline viewport (tip / status / active-block
//!    preview / editor / help) redrawn at most every ~33ms; per-frame cost is
//!    bounded by the tail, not the transcript length.
//! 3. **Modal** — heavy interactions stay in the fullscreen TUI
//!    (`--mode fullscreen`); none are wired into the linear UI yet.
//!
//! Input works while streaming: Enter sends a steering message (Plan 026)
//! that the agent injects after the current tool batch.

pub mod render;
pub mod term;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use auto_ai_agent::StreamEvent;

use crate::agent_task::AgentCommand;
use crate::chat_model::format_args_summary;

use render::*;
use term::LinearTerm;

/// In-place selector state (pi-style editor swap): the preview+editor region
/// of the tail is replaced by this list while open. `kind` decides what
/// confirming does (currently only "role").
pub struct SelectorState {
    pub kind: &'static str,
    pub title: String,
    /// (label, description) — label is the value confirmed.
    pub items: Vec<(String, String)>,
    pub selected: usize,
}

/// UI state for the linear chat. Unlike the fullscreen TUI there is no chat
/// model to keep — committed content lives in the terminal scrollback; only
/// the *active* (uncommitted) streaming text is held here.
pub struct LinearState {
    pub role: String,
    pub input: TextArea<'static>,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub is_streaming: bool,
    pub current_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Last TurnStart number (status display).
    pub turn: u32,
    /// Tokens reported by TurnEnd events for the in-flight run.
    pub turn_tokens: u64,
    /// Session-cumulative tokens from Done/Cancelled results.
    pub total_tokens: u64,
    pub tool_count: usize,
    /// Tool results this session, keyed by display id (`/expand N`).
    pub tool_log: Vec<(u64, String)>,
    pub tool_seq: u64,
    /// Uncommitted streaming text. `active_answer` may be reclassified as
    /// thinking when a ToolStart arrives (chat_model semantics: text before a
    /// tool call is reasoning).
    pub active_thinking: String,
    pub active_answer: String,
    /// (tool, args summary) of the currently executing tool call.
    pub running_tool: Option<(String, String)>,
    /// Transient tip-line message (steer queued, quit-armed…).
    pub tip: String,
    pub spinner_frame: usize,
    pub last_spinner_tick: Instant,
    /// Ctrl-C while streaming arms a second-press confirmation.
    pub quit_armed: bool,
    pub should_quit: bool,
    pub dirty: bool,
    /// Open in-place selector (replaces preview+editor in the tail).
    pub selector: Option<SelectorState>,
}

impl LinearState {
    fn new(role: &str) -> Self {
        Self {
            role: role.into(),
            input: new_input_textarea(),
            history: Vec::new(),
            history_idx: None,
            is_streaming: false,
            current_cancel: None,
            turn: 0,
            turn_tokens: 0,
            total_tokens: 0,
            tool_count: 0,
            tool_log: Vec::new(),
            tool_seq: 0,
            active_thinking: String::new(),
            active_answer: String::new(),
            running_tool: None,
            tip: String::new(),
            spinner_frame: 0,
            last_spinner_tick: Instant::now(),
            quit_armed: false,
            should_quit: false,
            dirty: true,
            selector: None,
        }
    }

    fn spinner_str(&self) -> &'static str {
        SPINNER_STRS[self.spinner_frame % SPINNER_STRS.len()]
    }

    fn take_input(&mut self) -> String {
        let text: String = self.input.lines().join("\n");
        self.input = new_input_textarea();
        text
    }

    fn reset_streaming(&mut self) {
        self.is_streaming = false;
        self.current_cancel = None;
        self.running_tool = None;
        self.active_thinking.clear();
        self.active_answer.clear();
        self.tip.clear();
        self.turn_tokens = 0;
        self.quit_armed = false;
    }
}

const SPINNER_STRS: [&str; 3] = [".", "..", "..."];

fn new_input_textarea() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text("Enter=发送 · Shift/Ctrl+Enter=换行 · Esc=取消运行");
    // tui-textarea underlines the entire cursor line by default — that's the
    // stray underline under typed input. The rendered block caret (reverse
    // video) is enough; disable the line highlight.
    input.set_cursor_line_style(ratatui::style::Style::default());
    input.set_block(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_set(ratatui::symbols::border::ROUNDED)
            .title(" 输入 ")
            .border_style(ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray))
            .padding(ratatui::widgets::Padding::left(1)),
    );
    input
}

/// Run the linear chat loop.
pub async fn run_linear_chat(role: &str, continue_last: bool) -> Result<(), String> {
    let mut term = LinearTerm::new()?;
    term::install_panic_hook();
    let mut s = LinearState::new(role);

    // First frame so the inline viewport is established before any commit.
    render::draw(&mut term.terminal, &s);

    // Startup banner + onboarding hint — first scrollback entries.
    term.commit(system_lines(&crate::build_banner(role)))?;
    term.commit(system_lines(
        "输入问题开始对话 · /help 查看命令 · /roles 切换角色 · q 退出",
    ))?;

    let client = crate::build_client().await;
    let mut agent = crate::build_agent(role, client.clone(), true)
        .map_err(|e| format!("build agent: {e}"))?;

    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();

    // Session restore: preload memory + replay the recent tail (Plan 029
    // §2.3 — only the last few rounds are reprinted, not the whole history).
    let cwd = std::env::current_dir().unwrap_or_default();
    if continue_last {
        if let Some(record) = crate::session::load(&cwd) {
            let n = record.messages.len();
            let tail_start = record.messages.len().saturating_sub(6);
            let tail = &record.messages[tail_start..];
            agent.preload_messages(record.messages.clone());
            term.commit(divider_lines(&format!(
                "── 恢复会话 · {n} 条消息已载入（回放最近 {} 条）──",
                tail.len()
            )))?;
            term.commit(replay_lines(tail))?;
        }
    }

    let cmd_tx = crate::agent_task::spawn(agent, role.to_string(), client, cwd, stream_tx);

    let mut result = Ok(());
    'outer: loop {
        // ── Drain streaming events (commit directly into scrollback). ──
        while let Ok(ev) = stream_rx.try_recv() {
            if let Err(e) = handle_stream_event(&mut s, &mut term, ev) {
                result = Err(e);
                break 'outer;
            }
            s.dirty = true;
        }

        // ── Render (throttled by the poll interval below). ──
        if tick_spinner(s.is_streaming, &mut s.spinner_frame, &mut s.last_spinner_tick) {
            s.dirty = true;
        }
        if s.dirty {
            render::draw(&mut term.terminal, &s);
            s.dirty = false;
        }

        if s.should_quit {
            break;
        }

        // ── Poll input. ──
        match event::poll(POLL_MS) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => handle_key(&mut s, &mut term, &cmd_tx, key),
                Ok(Event::Resize(..)) => s.dirty = true, // tail redraws; archive is immutable
                Ok(_) => {}
                Err(e) => {
                    result = Err(format!("read: {e}"));
                    break;
                }
            },
            Ok(false) => {}
            Err(e) => {
                result = Err(format!("poll: {e}"));
                break;
            }
        }
    }

    drop(cmd_tx);
    term.restore();
    // Shell prompt lands below the (frozen) tail.
    println!();
    result
}

// ── event → state/archive mapping (Plan 029 §2.5) ─────────────────────────────

fn handle_stream_event(s: &mut LinearState, term: &mut LinearTerm, ev: StreamEvent) -> Result<(), String> {
    match ev {
        StreamEvent::TurnStart { turn } => {
            s.turn = turn;
        }
        StreamEvent::Thinking { text } => {
            s.active_thinking.push_str(&text);
        }
        StreamEvent::Delta { text } => {
            s.active_answer.push_str(&text);
        }
        StreamEvent::Warning { text } => {
            // Advisory (e.g. near-turn-cap) — committed dim-yellow so it stays
            // in history without being mistaken for the answer.
            term.commit(warning_lines(&text))?;
        }
        StreamEvent::TurnEnd { usage, .. } => {
            s.turn_tokens += usage.map(|u| (u.input_tokens + u.output_tokens) as u64).unwrap_or(0);
        }
        StreamEvent::ToolStart { tool, args } => {
            // Text streamed before a tool call is reasoning — archive it as a
            // dim thinking block, then show the running tool in the preview.
            flush_pending_thinking(s, term)?;
            s.running_tool = Some((tool.clone(), format_args_summary(&tool, &args)));
        }
        StreamEvent::Tool { tool, args, result, .. } => {
            s.running_tool = None;
            let summary = format_args_summary(&tool, &args);
            s.tool_seq += 1;
            term.commit(tool_lines(&tool, &summary, &result, s.tool_seq))?;
            s.tool_log.push((s.tool_seq, result));
            s.tool_count += 1;
        }
        StreamEvent::Done { result } => {
            s.total_tokens += result.total_tokens;
            finish_turn(s, term, false, result.tool_calls.len())?;
        }
        StreamEvent::Cancelled { result } => {
            s.total_tokens += result.total_tokens;
            finish_turn(s, term, true, result.tool_calls.len())?;
        }
        StreamEvent::Error { message } => {
            term.commit(error_lines(&crate::format_agent_error(&message)))?;
            s.reset_streaming();
        }
    }
    Ok(())
}

/// Archive any uncommitted thinking/answer text as a dim thinking block
/// (called at tool-call boundaries).
fn flush_pending_thinking(s: &mut LinearState, term: &mut LinearTerm) -> Result<(), String> {
    let mut text = std::mem::take(&mut s.active_thinking);
    let ans = std::mem::take(&mut s.active_answer);
    if !ans.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&ans);
    }
    if !text.trim().is_empty() {
        term.commit(thinking_lines(&text))?;
    }
    Ok(())
}

/// Commit the turn's final output and reset streaming state.
fn finish_turn(
    s: &mut LinearState,
    term: &mut LinearTerm,
    cancelled: bool,
    tools_in_run: usize,
) -> Result<(), String> {
    let think = std::mem::take(&mut s.active_thinking);
    if !think.trim().is_empty() {
        term.commit(thinking_lines(&think))?;
    }
    let ans = std::mem::take(&mut s.active_answer);
    if !ans.trim().is_empty() {
        // The one and only markdown parse for this answer (Plan 029 §2.3).
        term.commit(answer_lines(&ans))?;
    }
    if cancelled {
        term.commit(divider_lines(&format!(
            "──── ⊘ 已取消 · {} 次工具 · 累计 {} tokens ────",
            tools_in_run, s.total_tokens
        )))?;
    } else {
        term.commit(divider_lines(&format!(
            "──── 回合结束 · {} 次工具 · 累计 {} tokens ────",
            tools_in_run, s.total_tokens
        )))?;
    }
    s.reset_streaming();
    Ok(())
}

/// Replay a few restored session messages (user / assistant text / tool
/// names) as simple committed lines.
fn replay_lines(msgs: &[auto_ai_client::Message]) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Line;
    use auto_ai_client::ContentBlock;

    let mut out = Vec::new();
    for m in msgs {
        let mut text = String::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text: t } => text.push_str(t),
                ContentBlock::ToolUse { name, .. } => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("⚙ {name}"));
                }
                ContentBlock::ToolResult { .. } => {}
            }
        }
        match m.role.as_str() {
            "user" => out.push(Line::styled(
                format!("❯ {text}"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            _ => {
                for l in text.lines() {
                    out.push(Line::styled(l.to_string(), Style::default()));
                }
            }
        }
    }
    out
}

// ── input ─────────────────────────────────────────────────────────────────────

fn handle_key(
    s: &mut LinearState,
    term: &mut LinearTerm,
    cmd_tx: &mpsc::UnboundedSender<AgentCommand>,
    key: KeyEvent,
) {
    // Ctrl-C → quit (armed double-press while streaming).
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if s.is_streaming && !s.quit_armed {
            s.quit_armed = true;
            s.tip = "再按一次 Ctrl-C 退出 · Esc 仅取消本轮".into();
        } else {
            s.should_quit = true;
        }
        return;
    }

    // Windows ghost-Enter guard (crossterm #752): only handle presses.
    if key.kind != KeyEventKind::Press {
        return;
    }

    // In-place selector owns the tail while open.
    if s.selector.is_some() {
        handle_selector_key(s, term, cmd_tx, key);
        return;
    }

    // Esc while streaming → request cancellation (Plan 026 soft cancel).
    if key.code == KeyCode::Esc && s.is_streaming {
        if let Some(c) = &s.current_cancel {
            c.store(true, Ordering::SeqCst);
        }
        return;
    }

    // Enter → submit (steer / follow-up while streaming). Only a *bare*
    // Enter submits; Shift/Ctrl/Alt+Enter are line breaks (they fall through
    // to the textarea, which inserts the newline).
    if key.code == KeyCode::Enter
        && !key.modifiers.intersects(
            KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
        )
    {
        let text = s.take_input();
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if text == "q" || text == "quit" || text == "exit" {
            s.should_quit = true;
            return;
        }
        if text.starts_with('/') {
            handle_slash_command(s, term, cmd_tx, &text);
            return;
        }
        s.history.push(text.clone());
        s.history_idx = None;
        if s.is_streaming {
            // Queued interjection. If the final answer is already streaming
            // (no tool running), a steering message could be dropped when the
            // run ends naturally — a follow-up instead revives the run with
            // it (Plan 026 semantics; Plan 029 Phase 6.1).
            if use_follow_up(s) {
                term.commit(user_lines(&text, "↪ ")).ok();
                let _ = cmd_tx.send(AgentCommand::FollowUp(text));
                s.tip = "↪ 已排队：本回合结束后继续".into();
            } else {
                term.commit(user_lines(&text, "⇢ ")).ok();
                let _ = cmd_tx.send(AgentCommand::Steer(text));
                s.tip = "⇢ 已排队：当前工具批结束后注入".into();
            }
        } else {
            term.commit(user_lines(&text, "❯ ")).ok();
            s.is_streaming = true;
            s.last_spinner_tick = Instant::now();
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            s.current_cancel = Some(cancel.clone());
            let _ = cmd_tx.send(AgentCommand::Run { text, cancel });
        }
        s.dirty = true;
        return;
    }

    // History recall — single-line input only. In multi-line mode ↑/↓ belong
    // to the editor (cursor moves between lines) and fall through below.
    if key.code == KeyCode::Up && s.input.lines().len() == 1 && !s.history.is_empty() {
        let idx = s.history_idx
            .map(|i| if i > 0 { i - 1 } else { 0 })
            .unwrap_or(s.history.len() - 1);
        s.history_idx = Some(idx);
        s.input = new_input_textarea();
        s.input.insert_str(&s.history[idx]);
        s.dirty = true;
        return;
    }
    if key.code == KeyCode::Down && s.input.lines().len() == 1 {
        if let Some(idx) = s.history_idx {
            if idx + 1 < s.history.len() {
                s.history_idx = Some(idx + 1);
                s.input = new_input_textarea();
                s.input.insert_str(&s.history[idx + 1]);
            } else {
                s.history_idx = None;
                s.input = new_input_textarea();
            }
            s.dirty = true;
        }
        return;
    }

    let before = s.input.lines().join("\n");
    s.input.input(key);
    if s.input.lines().join("\n") != before {
        s.dirty = true;
    }
}

/// follow_up vs steer decision (Plan 029 Phase 6.1): when the final answer
/// is actively streaming (no tool running, answer text accumulating), a
/// steering message risks being dropped at natural run end — queue it as a
/// follow-up instead, which revives the run.
fn use_follow_up(s: &LinearState) -> bool {
    s.is_streaming && s.running_tool.is_none() && !s.active_answer.trim().is_empty()
}

/// Keyboard handling while the in-place selector is open.
fn handle_selector_key(
    s: &mut LinearState,
    term: &mut LinearTerm,
    cmd_tx: &mpsc::UnboundedSender<AgentCommand>,
    key: KeyEvent,
) {
    let len = s.selector.as_ref().map(|sel| sel.items.len()).unwrap_or(0);
    if len == 0 {
        s.selector = None;
        return;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(sel) = &mut s.selector {
                sel.selected = if sel.selected == 0 { len - 1 } else { sel.selected - 1 };
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(sel) = &mut s.selector {
                sel.selected = (sel.selected + 1) % len;
            }
        }
        KeyCode::Esc => {
            s.selector = None;
        }
        KeyCode::Enter => {
            let Some(sel) = s.selector.take() else { return };
            if sel.kind == "role" {
                let Some((name, _)) = sel.items.get(sel.selected) else { return };
                let name = name.clone();
                // SetRole saves the old memory first, then rebuilds with
                // the new role — `-c` history survives the switch.
                let _ = cmd_tx.send(AgentCommand::SetRole(name.clone()));
                s.role = name.clone();
                term.commit(divider_lines(&format!(
                    "──── 已切换角色 → {name}（记忆已重置，旧会话已保存）────"
                )))
                .ok();
            }
        }
        _ => {}
    }
    s.dirty = true;
}

fn handle_slash_command(
    s: &mut LinearState,
    term: &mut LinearTerm,
    cmd_tx: &mpsc::UnboundedSender<AgentCommand>,
    cmd: &str,
) {
    match cmd {
        "/help" => {
            term.commit(system_lines(
                "命令:\n  /help          显示本帮助\n  /roles         选择角色（切换后重建 agent）\n  /expand <id>   展开某次工具调用的完整结果（id 见 ⚙ 行尾 #N）\n  /config        打开 AutoOS 设置\n  /clear         清空会话与终端回滚区\n  q              退出\n  ↑/↓            历史回溯\n  流式中 Enter   插话：工具批后注入（⇢）或回合结束后继续（↪）",
            ))
            .ok();
        }
        "/roles" => {
            let registry = auto_ai_agent::RoleRegistry::load();
            let items: Vec<(String, String)> = registry
                .list()
                .iter()
                .map(|r| {
                    let kind = if r.is_builtin { "builtin" } else { "user" };
                    (
                        r.name.clone(),
                        format!("tier={:<4} [{kind}]", format!("{:?}", r.tier).to_lowercase()),
                    )
                })
                .collect();
            if items.is_empty() {
                term.commit(system_lines("没有可用角色")).ok();
                return;
            }
            let current = items.iter().position(|(n, _)| *n == s.role).unwrap_or(0);
            s.selector = Some(SelectorState {
                kind: "role",
                title: "选择角色（Enter 确认，Esc 取消）".into(),
                items,
                selected: current,
            });
        }
        "/clear" => {
            if s.is_streaming {
                s.tip = "运行中无法清空，请先 Esc 取消".into();
                return;
            }
            let _ = cmd_tx.send(AgentCommand::Reset);
            s.turn = 0;
            s.tool_count = 0;
            s.total_tokens = 0;
            s.tool_log.clear();
            s.tool_seq = 0;
            // The one operation allowed to wipe the archive: purge the
            // scrollback, clear the screen, and re-anchor the inline
            // viewport on the empty screen (Plan 029 Phase 6.4).
            if term.clear_screen().is_ok() {
                render::draw(&mut term.terminal, s);
                term.commit(divider_lines("──── 新会话 ────")).ok();
            } else {
                term.commit(divider_lines("──── 会话已清空（agent 记忆已重置）────")).ok();
            }
        }
        "/config" => {
            term.commit(system_lines("正在打开 AutoOS 设置…")).ok();
            let daemon_url =
                std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into());
            tokio::spawn(async move {
                let http = reqwest::Client::new();
                let _ = http
                    .post(format!("{daemon_url}/v1/services/os-config/ensure"))
                    .timeout(std::time::Duration::from_secs(20))
                    .send()
                    .await;
                #[cfg(target_os = "windows")]
                {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", "http://localhost:17700"])
                        .spawn();
                }
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open")
                        .arg("http://localhost:17700")
                        .spawn();
                }
            });
        }
        other if other.starts_with("/expand") => {
            let arg = other.trim_start_matches("/expand").trim();
            let Ok(id) = arg.parse::<u64>() else {
                s.tip = "用法: /expand <id>（id 见工具摘要行尾的 #N）".into();
                return;
            };
            match s.tool_log.iter().rev().find(|(tid, _)| *tid == id) {
                Some((tid, result)) => {
                    // Reference-based append (Plan 029 Phase 6.2): the full
                    // result is committed into the linear flow on demand.
                    term.commit(expand_lines(result, *tid)).ok();
                }
                None => {
                    s.tip = format!("#{id} 不存在（本会话工具调用 #1-#{}）", s.tool_seq);
                }
            }
        }
        other => {
            term.commit(system_lines(&format!("未知命令: {other}，/help 查看"))).ok();
        }
    }
    s.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 6.1 decision table: follow-up only when the final answer is
    /// actively streaming; steering everywhere else in a run.
    #[test]
    fn follow_up_vs_steer_decision() {
        let mut s = LinearState::new("assistant");
        assert!(!use_follow_up(&s), "idle: not a queueing case at all");

        s.is_streaming = true;
        assert!(!use_follow_up(&s), "no answer yet → steer");

        s.active_answer = "正在写终答".into();
        assert!(use_follow_up(&s), "final answer streaming → follow-up");

        s.running_tool = Some(("read_file".into(), "x.rs".into()));
        assert!(!use_follow_up(&s), "tool running → steer");

        s.running_tool = None;
        s.active_answer.clear();
        s.active_thinking = "推理中".into();
        assert!(!use_follow_up(&s), "thinking only → steer");
    }
}
