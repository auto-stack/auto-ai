//! Markdown rendering for the TUI.
//!
//! Wraps the [`tui_markdown`] crate (pulldown-cmark → Ratatui `Text`). That
//! crate targets `ratatui-core 0.1`, whose `Text`/`Line`/`Span`/`Style` types
//! differ from this project's `ratatui 0.29`. This module bridges the two by
//! rebuilding ratatui-0.29 lines from the parsed result, so the rest of the
//! TUI only ever sees native `ratatui::text::Line`.
//!
//! Supported by tui_markdown (pulldown-cmark): headings, paragraphs, bold/
//! italic/strike, ordered + unordered + task lists, block quotes, thematic
//! rules, and fenced code blocks (with optional syntax highlighting).
//! Tables are *not* supported by tui_markdown (rendered as plain text).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Render a markdown string into styled ratatui lines (2-space indent, matching
/// the surrounding answer/thinking block body indentation).
pub fn render_lines(md: &str) -> Vec<Line<'static>> {
    let text = tui_markdown::from_str(md);
    text.lines
        .iter()
        .map(|l| convert_line(l))
        .collect()
}

/// Convert a `ratatui_core::text::Line` into a native `ratatui::text::Line`.
fn convert_line(line: &ratatui_core::text::Line) -> Line<'static> {
    let spans: Vec<Span<'static>> = line.spans.iter().map(convert_span).collect();
    let mut out = Line::from(spans);
    out.style = convert_style(&line.style);
    out
}

/// Convert a `ratatui_core::text::Span` into a native `ratatui::text::Span`.
fn convert_span(span: &ratatui_core::text::Span) -> Span<'static> {
    Span::styled(span.content.to_string(), convert_style(&span.style))
}

/// Convert a `ratatui_core::style::Style` into a native `ratatui::style::Style`.
fn convert_style(src: &ratatui_core::style::Style) -> Style {
    let mut out = Style::default();
    if let Some(fg) = src.fg {
        out = out.fg(convert_color(fg));
    }
    if let Some(bg) = src.bg {
        out = out.bg(convert_color(bg));
    }
    if !src.add_modifier.is_empty() {
        out = out.add_modifier(convert_modifier(src.add_modifier));
    }
    out
}

/// Convert a `ratatui_core::style::Color` into a native `ratatui::style::Color`.
/// The variant sets are identical across the two crates.
fn convert_color(c: ratatui_core::style::Color) -> Color {
    use ratatui_core::style::Color as C;
    match c {
        C::Reset => Color::Reset,
        C::Black => Color::Black,
        C::Red => Color::Red,
        C::Green => Color::Green,
        C::Yellow => Color::Yellow,
        C::Blue => Color::Blue,
        C::Magenta => Color::Magenta,
        C::Cyan => Color::Cyan,
        C::Gray => Color::Gray,
        C::DarkGray => Color::DarkGray,
        C::LightRed => Color::LightRed,
        C::LightGreen => Color::LightGreen,
        C::LightYellow => Color::LightYellow,
        C::LightBlue => Color::LightBlue,
        C::LightMagenta => Color::LightMagenta,
        C::LightCyan => Color::LightCyan,
        C::White => Color::White,
        C::Rgb(r, g, b) => Color::Rgb(r, g, b),
        C::Indexed(i) => Color::Indexed(i),
    }
}

/// Convert a `ratatui_core::style::Modifier` (bitflags) into the native one.
fn convert_modifier(m: ratatui_core::style::Modifier) -> Modifier {
    let mut out = Modifier::empty();
    // The bit values are identical across both crates; rebuild from the named
    // flags so we don't depend on the raw integer representation.
    use ratatui_core::style::Modifier as M;
    if m.contains(M::BOLD) { out |= Modifier::BOLD; }
    if m.contains(M::DIM) { out |= Modifier::DIM; }
    if m.contains(M::ITALIC) { out |= Modifier::ITALIC; }
    if m.contains(M::UNDERLINED) { out |= Modifier::UNDERLINED; }
    if m.contains(M::SLOW_BLINK) { out |= Modifier::SLOW_BLINK; }
    if m.contains(M::RAPID_BLINK) { out |= Modifier::RAPID_BLINK; }
    if m.contains(M::REVERSED) { out |= Modifier::REVERSED; }
    if m.contains(M::HIDDEN) { out |= Modifier::HIDDEN; }
    if m.contains(M::CROSSED_OUT) { out |= Modifier::CROSSED_OUT; }
    out
}
