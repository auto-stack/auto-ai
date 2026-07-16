//! TUI rendering and event loop using ratatui + crossterm.
//!
//! (Plan 010 — TUI Phase 3-4)
//!
//! Architecture:
//! - Main loop: render + poll keyboard + drain stream channel
//! - Agent.run_stream runs inline but yields between turns via the poll loop.
//!   During streaming, the on_event callback pushes deltas through mpsc,
//!   and the main loop renders them on the next poll cycle (50ms).

use std::io;
use std::sync::Arc;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;
use tui_textarea::TextArea;
use tokio::sync::mpsc;

use auto_ai_agent::{Agent, StreamEvent};

use crate::chat_model::{ChatLine, ChatLog};

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
}

impl App {
    pub fn new(role: &str) -> Self {
        let mut input = TextArea::default();
        input.set_placeholder_text("Type a message... (Enter=send, Shift+Enter=newline)");
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Input ")
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        Self {
            chat: ChatLog::new(),
            input,
            role: role.into(),
            turn: 0,
            tool_count: 0,
            total_tokens: 0,
            should_quit: false,
            is_streaming: false,
            history: Vec::new(),
            history_idx: None,
            auto_scroll: true,
        }
    }

    fn take_input(&mut self) -> String {
        let text: String = self.input.lines().join("\n");
        self.input = TextArea::default();
        self.input.set_placeholder_text("Type a message... (Enter=send, Shift+Enter=newline)");
        self.input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Input ")
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        text
    }
}

/// Run the TUI chat loop with a real agent.
pub async fn run_tui_chat(role: &str) -> Result<(), String> {
    enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| format!("alt screen: {e}"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| format!("terminal: {e}"))?;

    let mut app = App::new(role);
    app.chat.add_system("Welcome to auto-ai-cli TUI. Type a message and press Enter.");

    let client = crate::build_client();
    let mut agent = crate::build_agent("assistant", client, true)
        .map_err(|e| format!("build agent: {e}"))?;

    // Channel: agent's on_event callback → main loop.
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();
    let mut list_state = ListState::default();

    // Pending user text to send to agent.
    let mut pending_input: Option<String> = None;

    loop {
        // ── Drain streaming events first (so we render fresh data). ──
        while let Ok(ev) = stream_rx.try_recv() {
            handle_stream_event(&mut app, ev);
        }

        // ── If pending input, run the agent turn (blocking until done). ──
        if let Some(text) = pending_input.take() {
            if !app.is_streaming {
                app.chat.start_assistant();
                app.is_streaming = true;
                app.turn += 1;
                app.auto_scroll = true;

                // Render once to show "assistant ───" before blocking.
                update_list_state(&mut list_state, &app);
                terminal.draw(|f| render_app(f, &app, &mut list_state)).ok();

                let tx = stream_tx.clone();
                let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |ev| {
                    let _ = tx.send(ev);
                });

                match agent.run_stream(&text, on_event).await {
                    Ok(_) => {}
                    Err(e) => {
                        let msg = format!("{e}");
                        app.chat.add_error(&format_agent_error(&msg));
                        app.is_streaming = false;
                    }
                }
                // Flush remaining events.
                while let Ok(ev) = stream_rx.try_recv() {
                    handle_stream_event(&mut app, ev);
                }
            }
        }

        // ── Render. ──
        update_list_state(&mut list_state, &app);
        terminal.draw(|f| render_app(f, &app, &mut list_state)).ok();

        if app.should_quit {
            break;
        }

        // ── Poll keyboard (50ms). ──
        if event::poll(std::time::Duration::from_millis(50)).map_err(|e| format!("poll: {e}"))? {
            if let Event::Key(key) = event::read().map_err(|e| format!("read: {e}"))? {
                handle_key(&mut app, key, &mut pending_input);
            }
        }
    }

    disable_raw_mode().map_err(|e| format!("disable raw: {e}"))?;
    execute!(io::stdout(), LeaveAlternateScreen).map_err(|e| format!("leave alt: {e}"))?;
    Ok(())
}

/// Update list state: auto-scroll to bottom or respect manual scroll.
fn update_list_state(state: &mut ListState, app: &App) {
    let total_items: usize = app.chat.lines.iter().map(|l| match l {
        ChatLine::User(_) => 1,
        ChatLine::System(_) => 1,
        ChatLine::Error(_) => 1,
        ChatLine::Divider(_) => 1,
        ChatLine::Assistant(msg) => {
            let tool_lines: usize = msg.tools.iter().map(|t| {
                if t.collapsed { 1 } else { 1 + t.result.lines().count().min(11) }
            }).sum();
            1 + msg.text.lines().count() + tool_lines
        }
    }).sum();

    if app.auto_scroll {
        if total_items > 0 {
            state.select(Some(total_items.saturating_sub(1)));
        }
    }
}

/// Handle a keyboard event.
fn handle_key(app: &mut App, key: KeyEvent, pending_input: &mut Option<String>) {
    // Ctrl-C → quit.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    if app.is_streaming {
        match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Tab => app.chat.toggle_last_tool(),
            KeyCode::PageDown => app.auto_scroll = true,
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
        *pending_input = Some(text);
        return;
    }

    // History recall (only when single-line input).
    if key.code == KeyCode::Up && app.input.lines().len() == 1 && !app.history.is_empty() {
        let idx = app.history_idx.map(|i| if i > 0 { i - 1 } else { 0 }).unwrap_or(app.history.len() - 1);
        app.history_idx = Some(idx);
        app.input = TextArea::default();
        app.input.insert_str(&app.history[idx]);
        return;
    }
    if key.code == KeyCode::Down {
        if let Some(idx) = app.history_idx {
            if idx + 1 < app.history.len() {
                app.history_idx = Some(idx + 1);
                app.input = TextArea::default();
                app.input.insert_str(&app.history[idx + 1]);
            } else {
                app.history_idx = None;
                app.input = TextArea::default();
            }
        }
        return;
    }

    // Tab: toggle tool collapse.
    if key.code == KeyCode::Tab {
        app.chat.toggle_last_tool();
        return;
    }

    // Ctrl-L: clear screen (keep history).
    if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.auto_scroll = true;
        return;
    }

    app.input.input(key);
}

fn handle_slash_command(app: &mut App, cmd: &str) {
    match cmd {
        "/help" => {
            app.chat.add_system(
                "Commands:\n  /help    Show this help\n  /roles   List available roles\n  /config  Open AutoOS Settings\n  /clear   Clear chat history\n  q        Quit\n  Tab      Toggle tool block\n  Up/Down  History recall"
            );
        }
        "/roles" => {
            let mut lines = vec!["Available roles:".to_string()];
            for name in auto_ai_agent::builtin_names() {
                if let Some(r) = auto_ai_agent::load_builtin(name) {
                    lines.push(format!("  {name:<14} tier={:<4} max_turns={}",
                        format!("{:?}", r.model_tier()).to_lowercase(), r.max_turns()));
                }
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
            // Spawn browser open (non-blocking, don't stall TUI).
            let daemon_url = std::env::var("AAID_URL").unwrap_or_else(|_| "http://127.0.0.1:17654".into());
            tokio::spawn(async move {
                let http = reqwest::Client::new();
                // Ensure os-config via aaid.
                let _ = http.post(format!("{}/v1/services/os-config/ensure", daemon_url))
                    .timeout(std::time::Duration::from_secs(20))
                    .send().await;
                // Open browser.
                #[cfg(target_os = "windows")]
                { let _ = std::process::Command::new("cmd").args(["/C", "start", "", "http://localhost:17700"]).spawn(); }
                #[cfg(target_os = "macos")]
                { let _ = std::process::Command::new("open").arg("http://localhost:17700").spawn(); }
            });
        }
        _ => app.chat.add_system(&format!("Unknown: {cmd}. Type /help.")),
    }
}

fn handle_stream_event(app: &mut App, ev: StreamEvent) {
    match ev {
        StreamEvent::Delta { text } => {
            app.chat.append_delta(&text);
        }
        StreamEvent::Tool { tool, args, result, .. } => {
            app.chat.add_tool(&tool, &args, &result);
            app.tool_count += 1;
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
        }
        StreamEvent::Error { message } => {
            app.chat.add_error(&format_agent_error(&message));
            app.is_streaming = false;
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // status bar
            Constraint::Min(5),      // chat log
            Constraint::Length(3),   // input
            Constraint::Length(1),   // help bar
        ])
        .split(f.area());

    // Status bar.
    let status = format!(
        " auto-ai-cli │ role: {} │ turn: {} │ tools: {} │ {} tokens │ {}",
        app.role, app.turn, app.tool_count, app.total_tokens,
        if app.is_streaming { "● streaming" } else { "○ ready" },
    );
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
    let scroll_offset = if app.auto_scroll {
        total.saturating_sub(visible_height)
    } else {
        0 // manual scroll mode (future: track offset)
    };

    let chat_para = Paragraph::new(all_lines)
        .scroll((scroll_offset as u16, 0))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(format!(" Chat ({} lines) ", total)),
        );
    f.render_widget(chat_para, chat_area);

    // Input.
    f.render_widget(&app.input, chunks[2]);

    // Help bar.
    let help = if app.is_streaming {
        " Tab=toggle tool │ PageDown=scroll to bottom │ q=quit "
    } else {
        " Enter=send │ Shift+Enter=newline │ Tab=toggle │ Up/Down=history │ /help │ q=quit "
    };
    f.render_widget(Paragraph::new(help).style(Style::default().fg(Color::DarkGray)), chunks[3]);
}

/// Build styled Lines for the entire chat log.
fn build_chat_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for line in &app.chat.lines {
        match line {
            ChatLine::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("你> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw(text.clone()),
                ]));
            }
            ChatLine::Assistant(msg) => {
                lines.push(Line::from(vec![
                    Span::styled("assistant ───", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                ]));
                // Render text with basic code-block highlighting.
                let mut in_code = false;
                for text_line in msg.text.lines() {
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
                        lines.push(Line::raw(format!("  {}", text_line)));
                    }
                }
                // Tool blocks.
                for tool in &msg.tools {
                    let icon = if tool.collapsed { "▶" } else { "▼" };
                    let summary = &tool.args_summary;
                    let mut spans = vec![
                        Span::raw("  "),
                        Span::styled(format!("{icon} "), Style::default().fg(Color::Yellow)),
                        Span::styled(tool.tool.clone(), Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                    ];
                    if !summary.is_empty() {
                        spans.push(Span::styled(format!(" {summary}"), Style::default().fg(Color::Gray)));
                    }
                    spans.push(Span::styled(" [Tab]", Style::default().fg(Color::DarkGray)));
                    lines.push(Line::from(spans));
                    if !tool.collapsed {
                        for result_line in tool.result.lines().take(15) {
                            lines.push(Line::styled(
                                format!("    │ {}", result_line),
                                Style::default().fg(Color::DarkGray),
                            ));
                        }
                        if tool.result.lines().count() > 15 {
                            lines.push(Line::styled(String::from("    │ ..."), Style::default().fg(Color::DarkGray)));
                        }
                    }
                }
            }
            ChatLine::System(text) => {
                for tl in text.lines() {
                    lines.push(Line::styled(
                        format!("  {}", tl),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            ChatLine::Error(text) => {
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
