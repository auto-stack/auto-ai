//! TUI rendering and event loop using ratatui + crossterm.
//!
//! Architecture (streaming-first):
//! - A **resident background task** owns the `Agent` (so its memory survives
//!   across turns). The main loop talks to it over two channels:
//!     • `input_tx`  (String)            — main → task: a user message to run.
//!     • `stream_tx` (StreamEvent)       — task → main: streaming events.
//! - The main loop runs every ~50ms: drain stream events → advance the
//!   spinner → render → poll keyboard. Because rendering is *not* blocked on
//!   `agent.run_stream`, text deltas show up on screen as they arrive.
//! - An assistant turn is rendered as an ordered list of content blocks
//!   (● 回答 / 💭 思考 / 工具(细分) / ⏳ 运行中), each with a uniform shape and
//!   a blank line between blocks.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, ListState, Padding, Paragraph};
// NOTE: ratatui's `Block` is referenced via the fully-qualified path
// `ratatui::widgets::Block` because this module also uses `chat_model::Block`
// (a content block in the chat model). Importing both unqualified collides.
use ratatui::Terminal;
use tui_textarea::TextArea;
use tokio::sync::mpsc;

use auto_ai_agent::{Agent, StreamEvent};

use crate::chat_model::{Block, ChatLine, ChatLog, ToolBlock};

/// Spinner frames cycled while the assistant is "thinking" (no text yet).
const SPINNER_FRAMES: [&str; 3] = [".", "..", "..."];
const SPINNER_INTERVAL: Duration = Duration::from_millis(400);

/// App state for the TUI.
pub struct App {
    pub chat: ChatLog,
    pub input: TextArea<'static>,
    pub role: String,
    pub turn: u32,
    pub tool_count: usize,
    pub total_tokens: u64,
    pub should_quit: bool,
    pub is_streaming: bool,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub auto_scroll: bool,
    /// Current spinner animation frame (advanced while streaming).
    pub spinner_frame: usize,
    /// When the spinner last advanced.
    pub last_spinner_tick: Instant,
    /// Cancel handle for the turn currently executing (set when a message is
    /// sent, cleared when the turn ends). Esc sets the flag to true.
    pub current_cancel: Option<Arc<AtomicBool>>,
    /// Manual scroll offset (top line index into the chat). Used when
    /// [`Self::auto_scroll`] is false (i.e. the user is reviewing history).
    pub scroll_offset: usize,
}

impl App {
    pub fn new(role: &str) -> Self {
        let now = Instant::now();
        Self {
            chat: ChatLog::new(),
            input: new_input_textarea(),
            role: role.into(),
            turn: 0,
            tool_count: 0,
            total_tokens: 0,
            should_quit: false,
            is_streaming: false,
            history: Vec::new(),
            history_idx: None,
            auto_scroll: true,
            spinner_frame: 0,
            last_spinner_tick: now,
            current_cancel: None,
            scroll_offset: 0,
        }
    }

    fn take_input(&mut self) -> String {
        let text: String = self.input.lines().join("\n");
        self.input = new_input_textarea();
        text
    }

    /// Advance the spinner if enough time elapsed. Called every render cycle.
    fn tick_spinner(&mut self) {
        if !self.is_streaming {
            return;
        }
        if self.last_spinner_tick.elapsed() >= SPINNER_INTERVAL {
            self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
            self.last_spinner_tick = Instant::now();
        }
    }

    /// The current spinner dots string (e.g. "...").
    fn spinner_str(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()]
    }
}

fn new_input_textarea() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text("Type a message... (Enter=send, Shift+Enter=newline)");
    input.set_block(input_block());
    input
}

/// Build the input box's block. Uses 1-column left padding so typed text (and
/// the placeholder) doesn't sit flush against the border.
fn input_block() -> ratatui::widgets::Block<'static> {
    ratatui::widgets::Block::default()
        .borders(Borders::ALL)
        .title(" Input ")
        .border_style(Style::default().fg(Color::DarkGray))
        .padding(Padding::left(1))
}

/// Run the TUI chat loop with a real agent.
pub async fn run_tui_chat(role: &str, continue_last: bool) -> Result<(), String> {
    enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| format!("alt screen: {e}"))?;
    // Enable mouse capture so the scroll wheel pans the chat log (the
    // terminal's own scrollback isn't available under the alt screen).
    execute!(stdout, event::EnableMouseCapture).map_err(|e| format!("mouse capture: {e}"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| format!("terminal: {e}"))?;

    let mut app = App::new(role);
    // Startup banner as the first chat line: Session/Dir/Model/Daemon/Version
    // (visible throughout the session — scroll up to review).
    app.chat.add_system(&format!(
        "{}\nWelcome to auto-ai-cli TUI. Type a message and press Enter.",
        crate::build_banner("assistant")
    ));

    let client = crate::build_client().await;
    let mut agent = crate::build_agent("assistant", client, true)
        .map_err(|e| format!("build agent: {e}"))?;

    // Two channels bridging the main loop and the resident agent task:
    //   input_tx  — main → task: (user message, cancel handle for this turn).
    //   stream_tx — task → main: StreamEvents from run_stream.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<(String, Arc<AtomicBool>)>();
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();

    // Resident agent task: owns the agent (memory persists across turns),
    // runs one `run_stream` per incoming (message, cancel) pair.
    // Session persistence: on startup, optionally loads the last session's
    // messages into the agent's memory; after each turn, saves the updated
    // memory back to disk (review: -c/--continue feature).
    let cwd_for_session = std::env::current_dir().unwrap_or_default();
    let session_loaded = if continue_last {
        match crate::session::load(&cwd_for_session) {
            Some(record) => {
                let n = record.messages.len();
                agent.preload_messages(record.messages);
                // Notify the TUI that a session was restored.
                let _ = stream_tx.send(StreamEvent::Delta {
                    text: format!("📖 恢复了上一次会话（{} 条消息）。继续对话即可。\n\n", n),
                });
                true
            }
            None => false,
        }
    } else {
        false
    };
    let _ = session_loaded; // (loaded flag available for future use)

    let join_handle = tokio::spawn(async move {
        let mut agent = agent;
        while let Some((text, cancel)) = input_rx.recv().await {
            let tx = stream_tx.clone();
            let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> =
                Arc::new(move |ev| { let _ = tx.send(ev); });
            // run_stream errors surface as StreamEvent::Error already; a hard
            // failure just ends this turn (the task stays alive for the next).
            let _ = agent.run_stream(&text, on_event, cancel).await;
            // Persist the updated conversation after each turn.
            crate::session::save(&cwd_for_session, "session", agent.memory_messages());
        }
    });

    let mut list_state = ListState::default();

    loop {
        // ── Drain all streaming events. ──
        while let Ok(ev) = stream_rx.try_recv() {
            handle_stream_event(&mut app, ev);
        }

        // ── Render. ──
        app.tick_spinner();
        update_list_state(&mut list_state, &app);
        terminal.draw(|f| render_app(f, &app, &mut list_state)).ok();

        if app.should_quit {
            break;
        }

        // ── Poll input (50ms): keyboard + mouse wheel. ──
        if event::poll(std::time::Duration::from_millis(50)).map_err(|e| format!("poll: {e}"))? {
            match event::read().map_err(|e| format!("read: {e}"))? {
                Event::Key(key) => handle_key(&mut app, key, &input_tx),
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
                _ => {}
            }
        }
    }

    // Dropping input_tx makes the resident task's recv() return None → it exits.
    drop(input_tx);
    // Don't await the join indefinitely; the task exits once input_rx closes.
    let _ = tokio::time::timeout(Duration::from_millis(500), join_handle).await;

    disable_raw_mode().map_err(|e| format!("disable raw: {e}"))?;
    execute!(io::stdout(), event::DisableMouseCapture).map_err(|e| format!("disable mouse: {e}"))?;
    execute!(io::stdout(), LeaveAlternateScreen).map_err(|e| format!("leave alt: {e}"))?;
    Ok(())
}

/// Update list state: auto-scroll to bottom or respect manual scroll.
fn update_list_state(state: &mut ListState, app: &App) {
    let total_items: usize = app.chat.lines.iter().map(|l| line_height(l)).sum();
    if app.auto_scroll && total_items > 0 {
        state.select(Some(total_items.saturating_sub(1)));
    }
}

/// Approximate rendered height of a chat line (for auto-scroll accounting).
/// Counts the top rule + dialog header + bottom rule for Assistant/User dialogs.
fn line_height(line: &ChatLine) -> usize {
    match line {
        ChatLine::User { text, .. } => {
            // blank + top rule + header + body lines + bottom rule
            1 + 1 + 1 + text.lines().count().max(1) + 1
        }
        ChatLine::System(text) | ChatLine::Error(text) | ChatLine::Divider(text) => {
            text.lines().count().max(1)
        }
        ChatLine::Assistant(turn) => {
            // blank + top rule + header + blocks + bottom rule
            let mut h = 1 + 1 + 1;
            for (i, block) in turn.blocks.iter().enumerate() {
                if i > 0 {
                    h += 1; // blank line between blocks
                }
                h += 1; // title line
                h += block_body_height(block);
            }
            h += 1; // bottom rule
            h.max(1)
        }
    }
}

/// Rendered height of a block's body (below its title line).
fn block_body_height(block: &Block) -> usize {
    match block {
        Block::Answer { text } => text.lines().count().max(1),
        Block::Thinking { text } => text.lines().count().max(1),
        Block::Tool(t) => {
            if t.collapsed || !t.done {
                0
            } else {
                t.result.lines().count().min(11) + 1
            }
        }
        _ => 0,
    }
}

/// Handle a keyboard event.
fn handle_key(
    app: &mut App,
    key: KeyEvent,
    input_tx: &mpsc::UnboundedSender<(String, Arc<AtomicBool>)>,
) {
    // Ctrl-C → quit.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    // On Windows, crossterm emits both Press and Release for each physical key.
    // IME commit (confirming a candidate) replays an Enter *Release* ("ghost
    // Enter"), which must not trigger actions. Only handle key presses — this
    // also prevents every action from firing twice on Windows. See crossterm
    // #752, ratatui #347.
    if key.kind != KeyEventKind::Press {
        return;
    }

    if app.is_streaming {
        match key.code {
            // Esc → request cancellation. Sets the flag; the turn actually ends
            // when StreamEvent::Cancelled arrives (keeps memory consistent).
            KeyCode::Esc => {
                if let Some(c) = &app.current_cancel {
                    c.store(true, Ordering::SeqCst);
                }
            }
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Tab => app.chat.toggle_last_tool(),
            // PageUp/PageDown pan the chat log even while streaming.
            KeyCode::PageUp => scroll_chat(app, ScrollDir::Up, 10),
            KeyCode::PageDown => jump_to_bottom(app),
            _ => {}
        }
        return;
    }

    // Enter → submit.
    if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
        let text = app.take_input();
        let text = text.trim().to_string();
        if text.is_empty() { return; }
        if text == "q" || text == "quit" || text == "exit" {
            app.should_quit = true;
            return;
        }
        if text.starts_with('/') {
            handle_slash_command(app, &text);
            return;
        }
        app.chat.add_user(&text);
        app.history.push(text.clone());
        app.history_idx = None;
        // Start a fresh assistant turn and send the message to the agent task.
        // Create a fresh cancel handle for this turn (Esc sets it to true).
        app.chat.start_assistant();
        app.is_streaming = true;
        app.turn += 1;
        app.auto_scroll = true;
        app.last_spinner_tick = Instant::now();
        let cancel = Arc::new(AtomicBool::new(false));
        app.current_cancel = Some(cancel.clone());
        let _ = input_tx.send((text, cancel));
        return;
    }

    // History recall (only when single-line input).
    if key.code == KeyCode::Up && app.input.lines().len() == 1 && !app.history.is_empty() {
        let idx = app.history_idx
            .map(|i| if i > 0 { i - 1 } else { 0 })
            .unwrap_or(app.history.len() - 1);
        app.history_idx = Some(idx);
        app.input = new_input_textarea();
        app.input.insert_str(&app.history[idx]);
        return;
    }
    if key.code == KeyCode::Down {
        if let Some(idx) = app.history_idx {
            if idx + 1 < app.history.len() {
                app.history_idx = Some(idx + 1);
                app.input = new_input_textarea();
                app.input.insert_str(&app.history[idx + 1]);
            } else {
                app.history_idx = None;
                app.input = new_input_textarea();
            }
        }
        return;
    }

    // PageUp/PageDown: pan the chat log (review history). ↑/↓ stay with the
    // input box (cursor move / history recall).
    if key.code == KeyCode::PageUp {
        scroll_chat(app, ScrollDir::Up, 10);
        return;
    }
    if key.code == KeyCode::PageDown {
        jump_to_bottom(app);
        return;
    }

    // Tab: toggle tool collapse.
    if key.code == KeyCode::Tab {
        app.chat.toggle_last_tool();
        return;
    }

    // Ctrl-L: jump to newest (keep history).
    if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
        jump_to_bottom(app);
        return;
    }

    app.input.input(key);
}

/// Handle a mouse event — the scroll wheel pans the chat log. Each notch moves
/// 3 lines (comfortable granularity). Scrolling leaves "follow newest" mode;
/// scrolling all the way back down re-engages it.
fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => scroll_chat(app, ScrollDir::Up, 3),
        MouseEventKind::ScrollDown => scroll_chat(app, ScrollDir::Down, 3),
        _ => {}
    }
}

#[derive(Clone, Copy)]
enum ScrollDir { Up, Down }

/// Pan the chat log by `lines` in the given direction, entering manual scroll
/// mode. Moving back to (or past) the bottom re-engages "follow newest".
fn scroll_chat(app: &mut App, dir: ScrollDir, lines: usize) {
    app.auto_scroll = false;
    // Compute the current max offset to clamp against. We don't know the
    // visible height here precisely (it depends on the rendered area), so we
    // allow the offset to grow; render_app clamps it to max_offset on draw.
    match dir {
        ScrollDir::Up => {
            app.scroll_offset = app.scroll_offset.saturating_add(lines);
        }
        ScrollDir::Down => {
            app.scroll_offset = app.scroll_offset.saturating_sub(lines);
            if app.scroll_offset == 0 {
                app.auto_scroll = true;
            }
        }
    }
}

/// Jump back to following the newest output (bottom of the log).
fn jump_to_bottom(app: &mut App) {
    app.scroll_offset = 0;
    app.auto_scroll = true;
}

fn handle_slash_command(app: &mut App, cmd: &str) {
    match cmd {
        "/help" => {
            app.chat.add_system(
                "Commands:\n  /help    Show this help\n  /roles   List available roles\n  /config  Open AutoOS Settings\n  /clear   Clear chat history\n  q        Quit\n  Tab      Toggle tool block\n  Up/Down  History recall"
            );
        }
        "/roles" => {
            let registry = auto_ai_agent::RoleRegistry::load();
            let mut lines = vec!["Available roles:".to_string()];
            for s in registry.list() {
                let kind = if s.is_builtin { "builtin" } else { "user" };
                lines.push(format!(
                    "  {name:<14} tier={tier:<4} [{kind}]",
                    name = s.name,
                    tier = format!("{:?}", s.tier).to_lowercase(),
                    kind = kind,
                ));
            }
            app.chat.add_system(&lines.join("\n"));
        }
        "/clear" => {
            app.chat = ChatLog::new();
            app.turn = 0;
            app.tool_count = 0;
            app.total_tokens = 0;
            app.chat.add_system("Chat cleared.");
        }
        "/config" => {
            app.chat.add_system("Opening AutoOS Settings in browser...");
            let daemon_url = std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into());
            tokio::spawn(async move {
                let http = reqwest::Client::new();
                let _ = http.post(format!("{}/v1/services/os-config/ensure", daemon_url))
                    .timeout(std::time::Duration::from_secs(20))
                    .send().await;
                #[cfg(target_os = "windows")]
                { let _ = std::process::Command::new("cmd").args(["/C", "start", "", "http://localhost:17700"]).spawn(); }
                #[cfg(target_os = "macos")]
                { let _ = std::process::Command::new("open").arg("http://localhost:17700").spawn(); }
            });
        }
        _ => app.chat.add_system(&format!("Unknown: {cmd}. Type /help.")),
    }
}

/// Map a stream event onto chat model mutations.
fn handle_stream_event(app: &mut App, ev: StreamEvent) {
    // Agent output grows the chat — re-engage "follow newest" so the user
    // sees fresh content even if they were reviewing history. (If they want
    // to stay on history they must stop the stream first.)
    app.auto_scroll = true;
    app.scroll_offset = 0;
    match ev {
        StreamEvent::Delta { text } => {
            app.chat.append_delta(&text);
        }
        StreamEvent::Warning { text } => {
            // Advisory (e.g. near-turn-cap) — show as a dimmed system note in
            // the current turn so it's not mistaken for the model's answer.
            app.chat.add_system(&format!("⚠️ {text}"));
        }
        StreamEvent::ToolStart { tool, args } => {
            app.chat.start_tool(&tool, &args);
            app.tool_count += 1;
        }
        StreamEvent::Tool { tool, args, result, .. } => {
            app.chat.finish_tool(&tool, &args, &result);
        }
        StreamEvent::Done { result } => {
            app.total_tokens += result.total_tokens;
            app.chat.finish_assistant();
            app.chat.add_divider(&format!(
                "──── turn {} │ {} tool call{} │ {} tokens ────",
                result.turns,
                result.tool_calls.len(),
                if result.tool_calls.len() == 1 { "" } else { "s" },
                app.total_tokens,
            ));
            app.is_streaming = false;
            app.current_cancel = None;
        }
        StreamEvent::Cancelled { result } => {
            // Soft cancel: keep partial output, note it was cancelled.
            app.total_tokens += result.total_tokens;
            app.chat.finish_assistant();
            app.chat.add_system("⊘ 已取消");
            app.is_streaming = false;
            app.current_cancel = None;
        }
        StreamEvent::Error { message } => {
            app.chat.add_error(&format_agent_error(&message));
            app.is_streaming = false;
            app.current_cancel = None;
        }
    }
}

/// Format agent errors with helpful hints.
fn format_agent_error(msg: &str) -> String {
    if msg.contains("429") || msg.contains("rate limit") {
        format!("⚠️ Rate limited: {}\n  The provider's API quota is exhausted. Wait and retry.", msg)
    } else if msg.contains("401") || msg.contains("unauthorized") {
        format!("⚠️ Auth error: {}\n  Check API key in AI Daemon settings.", msg)
    } else if msg.contains("Connection refused") || msg.contains("daemon") {
        format!("⚠️ Daemon unreachable: {}\n  Start it: cargo run -p auto-ai-daemon", msg)
    } else {
        format!("[error] {msg}")
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn render_app(f: &mut ratatui::Frame, app: &App, list_state: &mut ListState) {
    // Inset the whole app by 1 column left/right so borders/status/help text
    // have breathing room instead of touching the terminal edge.
    let area = f.area().inner(Margin { horizontal: 1, vertical: 0 });
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // status bar
            Constraint::Min(5),      // chat log
            Constraint::Length(3),   // input
            Constraint::Length(1),   // help bar
        ])
        .split(area);

    // Status bar.
    let mut status = format!(
        "auto-ai-cli │ role: {} │ turn: {} │ tools: {} │ {} tokens │ {}",
        app.role, app.turn, app.tool_count, app.total_tokens,
        if app.is_streaming { "● streaming" } else { "○ ready" },
    );
    // Surface "reviewing history" so the user knows why new content isn't shown.
    if !app.auto_scroll {
        status.push_str(" │ ← 回滚中 (PageDown/滚轮向下=回到底部)");
    }
    let status_style = if app.is_streaming {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    };
    f.render_widget(Paragraph::new(status).style(status_style), chunks[0]);

    // Chat log — build all lines with styling.
    let all_lines = build_chat_lines(app);
    let chat_area = chunks[1];

    // Use Paragraph with scroll for the chat log (simpler than List for
    // variable-height items + auto-scroll).
    let total = all_lines.len();
    let visible_height = chat_area.height as usize;
    // auto_scroll → stick to bottom; otherwise honor the manual offset,
    // clamped to [0, max_offset] so it can't scroll past the bottom.
    let max_offset = total.saturating_sub(visible_height);
    let scroll_offset = if app.auto_scroll {
        max_offset
    } else {
        app.scroll_offset.min(max_offset)
    };

    let chat_para = Paragraph::new(all_lines)
        .scroll((scroll_offset as u16, 0))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .block(
            ratatui::widgets::Block::default()
                .borders(Borders::TOP)
                .title(format!(" Chat ({} lines) ", total)),
        );
    f.render_widget(chat_para, chat_area);

    // Input.
    f.render_widget(&app.input, chunks[2]);

    // Help bar.
    let help = if app.is_streaming {
        "Esc=取消 │ Tab=toggle tool │ 滚轮/PageUp=回滚 │ PageDown=回底 │ q=quit"
    } else {
        "Enter=send │ Shift+Enter=newline │ Tab=toggle │ 滚轮/PageUp=回滚 │ PageDown=回底 │ Up/Down=history │ /help │ q=quit"
    };
    f.render_widget(Paragraph::new(help).style(Style::default().fg(Color::DarkGray)), chunks[3]);
}

/// Build styled Lines for the entire chat log.
///
/// Layout (dialog > block hierarchy):
///   Each Assistant/User message is a *dialog*: a colored header line
///   (role · tier · timestamp), indented content blocks, and a thin bottom
///   rule. System/error/divider lines render standalone.
fn build_chat_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let last_idx = app.chat.lines.len().saturating_sub(1);
    for (line_idx, line) in app.chat.lines.iter().enumerate() {
        let is_last_line = line_idx == last_idx;
        match line {
            ChatLine::User { text, created_at } => {
                lines.push(blank_line());
                lines.push(top_rule());
                // Dialog header: role · time (no tier for the user).
                lines.push(Line::from(vec![
                    Span::styled("你", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(" · ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{}", created_at.format("%H:%M:%S")),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                // Indented body.
                for tl in text.lines() {
                    lines.push(Line::styled(
                        format!("  {}", tl),
                        Style::default().fg(Color::Cyan),
                    ));
                }
                lines.push(bottom_rule());
            }
            ChatLine::Assistant(turn) => {
                lines.push(blank_line());
                lines.push(top_rule());
                // Dialog header: assistant · tier · time.
                let tier = role_tier_label(&app.role);
                lines.push(Line::from(vec![
                    Span::styled(app.role.clone(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::styled(" · ", Style::default().fg(Color::DarkGray)),
                    Span::styled(tier, Style::default().fg(Color::Blue)),
                    Span::styled(" · ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{}", turn.created_at.format("%H:%M:%S")),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                // Content blocks, indented 2 spaces.
                let spinner = app.spinner_str();
                let streaming = app.is_streaming;
                for (i, block) in turn.blocks.iter().enumerate() {
                    if i > 0 {
                        lines.push(blank_line());
                    }
                    let is_last_block = i + 1 == turn.blocks.len();
                    render_block(
                        &mut lines,
                        block,
                        spinner,
                        streaming && is_last_line && is_last_block,
                    );
                }
                // Trailing "thinking" spinner when nothing has streamed yet.
                if turn.blocks.is_empty() && streaming && is_last_line {
                    lines.push(Line::styled(
                        format!("● {} 思考中", spinner),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                lines.push(bottom_rule());
            }
            ChatLine::System(text) => {
                lines.push(blank_line());
                for tl in text.lines() {
                    lines.push(Line::styled(
                        format!("  {}", tl),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            ChatLine::Error(text) => {
                lines.push(blank_line());
                for tl in text.lines() {
                    lines.push(Line::styled(
                        format!("  {}", tl),
                        Style::default().fg(Color::Red),
                    ));
                }
            }
            ChatLine::Divider(text) => {
                lines.push(Line::styled(
                    text.clone(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }
    lines
}

fn blank_line() -> Line<'static> {
    Line::raw(String::new())
}

/// A thin top rule opening a dialog.
fn top_rule() -> Line<'static> {
    Line::styled(
        "─".repeat(48),
        Style::default().fg(Color::DarkGray),
    )
}

/// A thin bottom rule closing a dialog.
fn bottom_rule() -> Line<'static> {
    Line::styled(
        "─".repeat(48),
        Style::default().fg(Color::DarkGray),
    )
}

/// Look up the role's tier label (e.g. "mid") for the dialog header.
fn role_tier_label(role: &str) -> String {
    let registry = auto_ai_agent::RoleRegistry::load();
    match registry.resolve_role(role) {
        Some(r) => format!("{:?}", r.model_tier()).to_lowercase(),
        None => "?".into(),
    }
}

/// Render a single block's lines (title + body) into `lines`. All blocks use
/// the same `●` marker for vertical alignment; the type is encoded by color +
/// a text label (思考/回答/读取/写入/…).
/// `trailing_spinner` is shown when this is the final block of the actively
/// streaming turn and the block is empty/running (a "thinking" hint).
fn render_block(
    lines: &mut Vec<Line<'static>>,
    block: &Block,
    spinner: &str,
    trailing_spinner: bool,
) {
    match block {
        Block::Answer { text } => {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Green)),
                Span::styled("回答", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            ]));
            if text.is_empty() && trailing_spinner {
                lines.push(Line::styled(
                    format!("  {}思考中", spinner),
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                // Markdown rendering (headings, lists, bold/italic, code…).
                // pulldown-cmark is tolerant of partial input, so re-parsing
                // each frame during streaming is safe.
                let mut md_lines = crate::markdown::render_lines(text);
                lines.append(&mut md_lines);
            }
        }
        Block::Thinking { text } => {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::DarkGray)),
                Span::styled("思考", Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)),
            ]));
            push_text_body(lines, text, Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM));
        }
        Block::Tool(t) => render_tool_block(lines, t, spinner),
        Block::User { text } => {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Cyan)),
                Span::styled("你", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(format!(" {}", text)),
            ]));
        }
        Block::System { text } => {
            for tl in text.lines() {
                lines.push(Line::styled(
                    format!("  {}", tl),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
        Block::Error { text } => {
            for tl in text.lines() {
                lines.push(Line::styled(
                    format!("  {}", tl),
                    Style::default().fg(Color::Red),
                ));
            }
        }
        Block::Divider { text } => {
            lines.push(Line::styled(
                text.clone(),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
}

/// Push the body lines of a text block with basic code-fence highlighting.
fn push_text_body(lines: &mut Vec<Line<'static>>, text: &str, base_style: Style) {
    let mut in_code = false;
    for text_line in text.lines() {
        if text_line.trim_start().starts_with("```") {
            in_code = !in_code;
            lines.push(Line::styled(
                format!("  {}", text_line),
                Style::default().fg(Color::Yellow),
            ));
            continue;
        }
        if in_code {
            lines.push(Line::styled(
                format!("  {}", text_line),
                Style::default().fg(Color::Gray),
            ));
        } else {
            lines.push(Line::styled(format!("  {}", text_line), base_style));
        }
    }
}

/// Per-tool type → (verb label, accent color). All blocks share the same `●`
/// marker; the type is encoded by color + this verb label, keeping every row
/// vertically aligned without per-type emoji widths.
fn tool_style(tool: &str) -> (&'static str, Color) {
    match tool {
        "read_file" => ("读取", Color::Blue),
        "write_file" => ("写入", Color::Red),
        "edit_file" => ("编辑", Color::Yellow),
        "list_dir" => ("列目录", Color::Blue),
        "search" => ("搜索", Color::Cyan),
        "run_command" => ("命令", Color::Yellow),
        "spawn_pipeline" => ("流水线", Color::Magenta),
        "skill" => ("技能", Color::Magenta),
        _ => ("工具", Color::Blue),
    }
}

fn render_tool_block(lines: &mut Vec<Line<'static>>, t: &ToolBlock, spinner: &str) {
    let (verb, color) = tool_style(&t.tool);
    if !t.done {
        // Running state — accent in yellow with a spinner, no result yet.
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Yellow)),
            Span::styled(verb.to_string(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", t.args_summary), Style::default().fg(Color::Gray)),
            Span::styled(format!(" {}", spinner), Style::default().fg(Color::DarkGray)),
        ]));
        return;
    }
    // Done — `● verb  summary [Tab]`, color-coded by tool kind.
    let mut spans = vec![
        Span::styled("● ", Style::default().fg(color)),
        Span::styled(verb.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ];
    if !t.args_summary.is_empty() {
        spans.push(Span::styled(format!("  {}", t.args_summary), Style::default().fg(Color::Gray)));
    }
    spans.push(Span::styled(" [Tab]", Style::default().fg(Color::DarkGray)));
    lines.push(Line::from(spans));
    // Result body (collapsed by default; Tab expands).
    if !t.collapsed {
        for result_line in t.result.lines().take(15) {
            lines.push(Line::styled(
                format!("  │ {}", result_line),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let extra = t.result.lines().count().saturating_sub(15);
        if extra > 0 {
            lines.push(Line::styled(
                format!("  │ … ({} more)", extra),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
}
