//! TUI rendering and event loop using ratatui + crossterm.
//!
//! (Plan 010 — TUI)
//!
//! Architecture:
//! - Main thread: ratatui render loop (draw + poll keyboard)
//! - Background task: agent.run_stream (sends StreamEvents via mpsc channel)
//! - `on_event` callback pushes events into the channel; render loop drains them.

use std::io;
use std::sync::Arc;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use tui_textarea::TextArea;
use tokio::sync::mpsc;

use auto_ai_agent::StreamEvent;

use crate::chat_model::{ChatLine, ChatLog};

/// App state for the TUI.
pub struct App {
    pub chat: ChatLog,
    pub input: TextArea<'static>,
    pub role: String,
    pub turn: u32,
    pub tool_count: usize,
    pub scroll: u16,
    pub should_quit: bool,
    pub is_streaming: bool,
    /// History of user inputs (for up/down arrow recall).
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
}

impl App {
    pub fn new(role: &str) -> Self {
        let mut input = TextArea::default();
        input.set_placeholder_text("Type your message... (Enter to send, Shift+Enter for newline)");
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
            scroll: 0,
            should_quit: false,
            is_streaming: false,
            history: Vec::new(),
            history_idx: None,
        }
    }

    /// Take the current input text and clear the input box.
    fn take_input(&mut self) -> String {
        let text: String = self.input.lines().join("\n");
        self.input = TextArea::default();
        self.input.set_placeholder_text("Type your message... (Enter to send, Shift+Enter for newline)");
        self.input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Input ")
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        text
    }
}

/// Run the TUI chat loop.
pub async fn run_tui_chat(
    _agent_factory: impl FnOnce() -> Result<auto_ai_agent::Agent, String>,
    role: &str,
) -> Result<(), String> {
    // Setup terminal.
    enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| format!("alt screen: {e}"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| format!("terminal: {e}"))?;

    let mut app = App::new(role);
    app.chat.add_system("Welcome to auto-ai-cli TUI. Type a message and press Enter. Press q or Ctrl-C to quit.");

    // Channel for streaming events from agent → TUI.
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();

    // The agent will be created on first user message.
    let mut agent: Option<auto_ai_agent::Agent> = None;

    // Main event loop.
    loop {
        // Render.
        terminal.draw(|f| render_app(f, &app)).map_err(|e| format!("draw: {e}"))?;

        // Poll for events: keyboard or stream.
        if event::poll(std::time::Duration::from_millis(50)).map_err(|e| format!("poll: {e}"))? {
            if let Event::Key(key) = event::read().map_err(|e| format!("read: {e}"))? {
                handle_key(&mut app, key, &stream_tx, &mut agent, role);
            }
        }

        // Drain streaming events.
        while let Ok(ev) = stream_rx.try_recv() {
            handle_stream_event(&mut app, ev);
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal.
    disable_raw_mode().map_err(|e| format!("disable raw: {e}"))?;
    execute!(io::stdout(), LeaveAlternateScreen).map_err(|e| format!("leave alt: {e}"))?;

    Ok(())
}

/// Handle a keyboard event.
fn handle_key(
    app: &mut App,
    key: KeyEvent,
    stream_tx: &mpsc::UnboundedSender<StreamEvent>,
    _agent: &mut Option<auto_ai_agent::Agent>,
    _role: &str,
) {
    // Ctrl-C or q (when not typing) → quit.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    // If streaming, only allow quit/tab.
    if app.is_streaming {
        match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Tab => app.chat.toggle_last_tool(),
            _ => {} // ignore other keys while streaming
        }
        return;
    }

    // Enter → submit input.
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

        // Handle slash commands.
        if text.starts_with('/') {
            handle_slash_command(app, &text);
            return;
        }

        // Add user message to chat.
        app.chat.add_user(&text);
        app.history.push(text.clone());
        app.history_idx = None;

        // Start assistant streaming.
        app.chat.start_assistant();
        app.is_streaming = true;
        app.turn += 1;

        // Spawn the agent run in a background task.
        // For now, we need the agent. Create on first message if needed.
        // This is a simplified placeholder — the actual agent run is handled
        // by the caller who passes in an agent factory.
        // TODO: wire up the actual agent.run_stream call here in Phase 2/3.
        // For Phase 1, just simulate a quick response.
        let tx = stream_tx.clone();
        let t = text.clone();
        tokio::spawn(async move {
            // Placeholder: send a simulated delta + done.
            tx.send(StreamEvent::Delta {
                text: format!("[Phase 1 TUI placeholder] You said: {t}\nAgent streaming will be wired in Phase 2."),
            }).ok();
            tx.send(StreamEvent::Done {
                result: auto_ai_agent::AgentResult {
                    output: format!("Placeholder response to: {t}"),
                    turns: 1,
                    tool_calls: vec![],
                    total_tokens: 0,
                },
            }).ok();
        });
        return;
    }

    // Up arrow: history recall (when input is empty or at first line).
    if key.code == KeyCode::Up && app.input.lines().len() == 1 {
        if !app.history.is_empty() {
            let idx = app.history_idx.map(|i| if i > 0 { i - 1 } else { 0 }).unwrap_or(app.history.len() - 1);
            app.history_idx = Some(idx);
            app.input = TextArea::default();
            app.input.insert_str(&app.history[idx]);
        }
        return;
    }

    // Down arrow: history forward.
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

    // Tab: toggle tool block collapse.
    if key.code == KeyCode::Tab {
        app.chat.toggle_last_tool();
        return;
    }

    // All other keys: forward to textarea.
    app.input.input(key);
}

/// Handle a slash command (in-TUI, no LLM call).
fn handle_slash_command(app: &mut App, cmd: &str) {
    match cmd {
        "/help" => {
            app.chat.add_system("Commands: /help, /roles, /config, q/quit/exit, Tab=toggle tool, Up/Down=history");
        }
        "/roles" => {
            let mut lines = vec!["Available roles:".to_string()];
            for name in auto_ai_agent::builtin_names() {
                if let Some(r) = auto_ai_agent::load_builtin(name) {
                    lines.push(format!(
                        "  {name:<14} tier={:<4} max_turns={}",
                        format!("{:?}", r.model_tier()).to_lowercase(),
                        r.max_turns()
                    ));
                }
            }
            app.chat.add_system(&lines.join("\n"));
        }
        "/config" => {
            app.chat.add_system("Opening AutoOS Settings... (use /config outside TUI for browser launch)");
        }
        _ => {
            app.chat.add_system(&format!("Unknown command: {cmd}. Type /help."));
        }
    }
}

/// Handle a streaming event from the agent.
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
            app.chat.finish_assistant();
            app.chat.add_divider(&format!(
                "──── turn {}, {} tool call{} ────",
                result.turns,
                result.tool_calls.len(),
                if result.tool_calls.len() == 1 { "" } else { "s" }
            ));
            app.is_streaming = false;
        }
        StreamEvent::Error { message } => {
            app.chat.add_error(&message);
            app.is_streaming = false;
        }
    }
}

/// Render the app to the terminal.
fn render_app(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // status bar
            Constraint::Min(5),     // chat log
            Constraint::Length(3),  // input
            Constraint::Length(1),  // help bar
        ])
        .split(f.area());

    // Status bar.
    let status = format!(
        " auto-ai-cli │ role: {} │ turn: {} │ tools: {} │ {}",
        app.role,
        app.turn,
        app.tool_count,
        if app.is_streaming { "● streaming..." } else { "○ ready" },
    );
    let status_style = if app.is_streaming {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    };
    f.render_widget(Paragraph::new(status).style(status_style), chunks[0]);

    // Chat log.
    let items: Vec<ListItem> = app
        .chat
        .lines
        .iter()
        .flat_map(|line| render_chat_line(line))
        .collect();
    let chat_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(" Chat "),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(chat_list, chunks[1]);

    // Input box.
    f.render_widget(&app.input, chunks[2]);

    // Help bar.
    let help = " Enter=send │ Shift+Enter=newline │ Tab=toggle tool │ ↑↓=history │ q=quit ";
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}

/// Convert a ChatLine into renderable ListItem lines.
fn render_chat_line(line: &ChatLine) -> Vec<ListItem<'static>> {
    match line {
        ChatLine::User(text) => {
            vec![ListItem::new(vec![Line::from(vec![
                Span::styled("你> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(text.clone()),
            ])])]
        }
        ChatLine::Assistant(msg) => {
            let mut lines = vec![Line::from(vec![
                Span::styled("assistant ───", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            ])];

            // Assistant text.
            if !msg.text.is_empty() {
                for text_line in msg.text.lines() {
                    lines.push(Line::raw(format!("  {}", text_line)));
                }
            }

            // Tool blocks.
            for tool in &msg.tools {
                let icon = if tool.collapsed { "▶" } else { "▼" };
                let summary = &tool.args_summary;
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{icon} "), Style::default().fg(Color::Yellow)),
                    Span::styled(tool.tool.clone(), Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                    if !summary.is_empty() {
                        Span::styled(format!(" {summary}"), Style::default().fg(Color::Gray))
                    } else {
                        Span::raw("")
                    },
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
        ChatLine::System(text) => {
            vec![ListItem::new(Line::from(vec![
                Span::styled(text.clone(), Style::default().fg(Color::DarkGray)),
            ]))]
        }
        ChatLine::Error(text) => {
            vec![ListItem::new(Line::from(vec![
                Span::styled(format!("[error] {text}"), Style::default().fg(Color::Red)),
            ]))]
        }
        ChatLine::Divider(text) => {
            vec![ListItem::new(Line::from(vec![
                Span::styled(text.clone(), Style::default().fg(Color::DarkGray)),
            ]))]
        }
    }
}
