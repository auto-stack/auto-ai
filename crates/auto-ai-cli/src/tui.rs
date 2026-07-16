//! TUI rendering and event loop using ratatui + crossterm.
//!
//! (Plan 010 — TUI)
//!
//! Architecture:
//! - Main loop uses tokio::select! to interleave keyboard events (crossterm)
//!   with streaming events (agent.run_stream via mpsc channel).
//! - Agent runs inline (not spawned) so its &mut is valid.

use std::io;
use std::sync::Arc;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
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
    pub should_quit: bool,
    pub is_streaming: bool,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
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
            should_quit: false,
            is_streaming: false,
            history: Vec::new(),
            history_idx: None,
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

    // Pending user text to send to agent (set when Enter is pressed).
    let mut pending_input: Option<String> = None;

    loop {
        // Render.
        terminal.draw(|f| render_app(f, &app)).map_err(|e| format!("draw: {e}"))?;

        // If we have pending input and not streaming, start the agent.
        if let Some(text) = pending_input.take() {
            if !app.is_streaming {
                app.chat.start_assistant();
                app.is_streaming = true;
                app.turn += 1;

                // Run agent inline. The on_event callback sends through the channel.
                let tx = stream_tx.clone();
                let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |ev| {
                    let _ = tx.send(ev);
                });

                match agent.run_stream(&text, on_event).await {
                    Ok(_) => {}
                    Err(e) => {
                        app.chat.add_error(&format!("{e}"));
                        app.is_streaming = false;
                    }
                }
                // run_stream is done — flush any remaining events.
                while let Ok(ev) = stream_rx.try_recv() {
                    handle_stream_event(&mut app, ev);
                }
            }
        }

        // Drain streaming events (from run_stream's callback).
        while let Ok(ev) = stream_rx.try_recv() {
            handle_stream_event(&mut app, ev);
        }

        if app.should_quit {
            break;
        }

        // Poll keyboard (non-blocking, 50ms timeout).
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

/// Handle a keyboard event.
fn handle_key(app: &mut App, key: KeyEvent, pending_input: &mut Option<String>) {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    if app.is_streaming {
        // While streaming: only allow quit + tool toggle.
        match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Tab => app.chat.toggle_last_tool(),
            _ => {}
        }
        return;
    }

    // Enter → submit.
    if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
        let text = app.take_input();
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
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

    // History recall.
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

    // Forward to textarea.
    app.input.input(key);
}

fn handle_slash_command(app: &mut App, cmd: &str) {
    match cmd {
        "/help" => {
            app.chat.add_system("Commands: /help, /roles, q=quit, Tab=toggle tool, Up/Down=history");
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
        _ => app.chat.add_system(&format!("Unknown: {cmd}. /help for commands.")),
    }
}

fn handle_stream_event(app: &mut App, ev: StreamEvent) {
    match ev {
        StreamEvent::Delta { text } => app.chat.append_delta(&text),
        StreamEvent::Tool { tool, args, result, .. } => {
            app.chat.add_tool(&tool, &args, &result);
            app.tool_count += 1;
        }
        StreamEvent::Done { result } => {
            app.chat.finish_assistant();
            app.chat.add_divider(&format!(
                "──── turn {}, {} tool call{} ────",
                result.turns, result.tool_calls.len(),
                if result.tool_calls.len() == 1 { "" } else { "s" },
            ));
            app.is_streaming = false;
        }
        StreamEvent::Error { message } => {
            app.chat.add_error(&message);
            app.is_streaming = false;
        }
    }
}

fn render_app(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Status bar.
    let status = format!(
        " auto-ai-cli │ role: {} │ turn: {} │ tools: {} │ {}",
        app.role, app.turn, app.tool_count,
        if app.is_streaming { "● streaming..." } else { "○ ready" },
    );
    let status_style = if app.is_streaming {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    };
    f.render_widget(Paragraph::new(status).style(status_style), chunks[0]);

    // Chat log.
    let items: Vec<ListItem> = app.chat.lines.iter().flat_map(render_chat_line).collect();
    let chat_list = List::new(items)
        .block(Block::default().borders(Borders::TOP).title(" Chat "));
    f.render_widget(chat_list, chunks[1]);

    // Input.
    f.render_widget(&app.input, chunks[2]);

    // Help bar.
    let help = " Enter=send │ Shift+Enter=newline │ Tab=toggle tool │ Up/Down=history │ q=quit ";
    f.render_widget(Paragraph::new(help).style(Style::default().fg(Color::DarkGray)), chunks[3]);
}

fn render_chat_line(line: &ChatLine) -> Vec<ListItem<'static>> {
    match line {
        ChatLine::User(text) => vec![ListItem::new(Line::from(vec![
            Span::styled("你> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(text.clone()),
        ]))],
        ChatLine::Assistant(msg) => {
            let mut lines = vec![Line::from(vec![
                Span::styled("assistant ───", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            ])];
            for text_line in msg.text.lines() {
                lines.push(Line::raw(format!("  {}", text_line)));
            }
            for tool in &msg.tools {
                let icon = if tool.collapsed { "▶" } else { "▼" };
                let summary = &tool.args_summary;
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{icon} "), Style::default().fg(Color::Yellow)),
                    Span::styled(tool.tool.clone(), Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                    if !summary.is_empty() {
                        Span::styled(format!(" {summary}"), Style::default().fg(Color::Gray))
                    } else { Span::raw("") },
                ]));
                if !tool.collapsed {
                    for result_line in tool.result.lines().take(10) {
                        lines.push(Line::raw(format!("    │ {}", result_line)));
                    }
                    if tool.result.lines().count() > 10 {
                        lines.push(Line::raw("    │ ..."));
                    }
                }
            }
            vec![ListItem::new(lines)]
        }
        ChatLine::System(text) => vec![ListItem::new(Line::from(vec![
            Span::styled(text.clone(), Style::default().fg(Color::DarkGray)),
        ]))],
        ChatLine::Error(text) => vec![ListItem::new(Line::from(vec![
            Span::styled(format!("[error] {text}"), Style::default().fg(Color::Red)),
        ]))],
        ChatLine::Divider(text) => vec![ListItem::new(Line::from(vec![
            Span::styled(text.clone(), Style::default().fg(Color::DarkGray)),
        ]))],
    }
}
