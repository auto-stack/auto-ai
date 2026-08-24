//! Linear-UI terminal lifecycle (Plan 029).
//!
//! Main-screen rendering: no alternate screen, no mouse capture, no scroll
//! region. History lives in the terminal's native scrollback; a fixed-height
//! inline viewport (ratatui `Viewport::Inline`) is the dynamic tail, and
//! `Terminal::insert_before` permanently commits completed blocks into the
//! scrollback above it. Every write is wrapped in synchronized output
//! (CSI 2026) so partial frames never flicker.

use std::io;

use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
};
use crossterm::{execute, queue};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::render::{wrap_lines, TAIL_HEIGHT};
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use unicode_width::UnicodeWidthStr;

/// Fix up a commit buffer before `insert_before` prints it.
///
/// ratatui 0.29's inline `insert_before` path (`draw_lines`) feeds every
/// buffer cell straight to `CrosstermBackend::draw` **without** going through
/// `Buffer::diff` — unlike the normal `Terminal::draw` path, where diff's
/// `to_skip` logic drops the continuation cells of wide glyphs. Unpatched,
/// each CJK character gets a literal space printed over its right half
/// (every wide char renders as "字 "), which scrambles CJK output.
///
/// Fix: continuation cells of multi-column glyphs → empty symbol, so the
/// backend prints nothing there and the terminal's own cursor advance
/// handles the width. (The wide glyph itself covers both columns, so this
/// is safe even when insert_before draws over scrolled-up stale content.)
///
/// Trailing padding cells must NOT be blanked the same way: insert_before
/// draws into rows that may still hold stale pre-scroll content, and a
/// `Print("")` prints nothing, letting that stale content bleed through.
///
/// Upstream issue; drop this patch when ratatui fixes insert_before for
/// multi-column graphemes (tracked in KNOWN-DEBT).
fn sanitize_commit_buffer(buf: &mut Buffer) {
    for y in 0..buf.area.height {
        let mut x: u16 = 0;
        while x < buf.area.width {
            let w = buf
                .cell(Position::new(x, y))
                .map(|c| c.symbol().width())
                .unwrap_or(1)
                .max(1) as u16;
            for k in 1..w {
                if let Some(c) = buf.cell_mut(Position::new(x + k, y)) {
                    c.set_symbol("");
                }
            }
            x += w;
        }
    }
}

/// The linear UI's ratatui terminal (inline viewport on the main screen).
pub struct LinearTerm {
    pub terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl LinearTerm {
    /// Enter linear mode: raw mode + inline viewport. Everything above the
    /// viewport (the shell's own output, prior sessions) is left untouched.
    pub fn new() -> Result<Self, String> {
        enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(TAIL_HEIGHT),
            },
        )
        .map_err(|e| format!("terminal: {e}"))?;
        Ok(Self { terminal })
    }

    /// Permanently commit styled lines into the scrollback above the inline
    /// viewport. Lines are pre-wrapped to the terminal width first —
    /// `insert_before` clips whatever exceeds `height`, so the printed height
    /// must be computed exactly (never rely on Paragraph wrapping here).
    pub fn commit(&mut self, lines: Vec<Line<'static>>) -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }
        let (cols, _) = crossterm::terminal::size().map_err(|e| e.to_string())?;
        let text = Text::from(wrap_lines(lines, cols as usize));
        let height = text.lines.len() as u16;
        let mut stdout = io::stdout();
        queue!(stdout, BeginSynchronizedUpdate).map_err(|e| e.to_string())?;
        self.terminal
            .insert_before(height, move |buf| {
                let area = Rect::new(0, 0, buf.area.width, height);
                Paragraph::new(text).render(area, buf);
                sanitize_commit_buffer(buf);
            })
            .map_err(|e| format!("commit: {e}"))?;
        execute!(stdout, EndSynchronizedUpdate).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Wipe the screen *and* the scrollback, then re-anchor the inline
    /// viewport on the cleared screen (`/clear` — the one operation allowed
    /// to touch the archive, because the user explicitly asked).
    pub fn clear_screen(&mut self) -> Result<(), String> {
        execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::Purge),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
        )
        .map_err(|e| e.to_string())?;
        // The old terminal's viewport bookkeeping is stale after the purge —
        // a fresh inline terminal re-anchors at the bottom of the empty
        // screen. (Stdout is a shared handle, so a new backend is fine.)
        let backend = CrosstermBackend::new(io::stdout());
        self.terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(TAIL_HEIGHT),
            },
        )
        .map_err(|e| format!("re-anchor: {e}"))?;
        Ok(())
    }

    /// Leave linear mode: restore cooked mode, show the cursor. The committed
    /// transcript stays in the scrollback; the caller prints one newline so
    /// the shell prompt lands below the tail.
    pub fn restore(self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
    }
}

/// Install a panic hook that undoes raw mode before the default hook prints
/// — otherwise a panic leaves the terminal unusable (Plan 029 §2.8).
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::widgets::{Paragraph, Widget};

    /// The commit-buffer sanitizer must blank the continuation cells of wide
    /// glyphs (ratatui insert_before prints raw cells — an unblanked
    /// continuation space overwrites the right half of every CJK char) and
    /// trim trailing padding.
    #[test]
    fn sanitize_blanks_wide_continuations_keeps_padding() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        Paragraph::new("你好a").style(Style::default()).render(area, &mut buf);
        // Pre-sanitize: cells 1 and 3 hold the reset " " continuation cells,
        // cells 4..10 are padding spaces.
        assert_eq!(buf.cell(Position::new(0, 0)).unwrap().symbol(), "你");
        assert_eq!(buf.cell(Position::new(1, 0)).unwrap().symbol(), " ");
        sanitize_commit_buffer(&mut buf);
        assert_eq!(buf.cell(Position::new(0, 0)).unwrap().symbol(), "你");
        assert_eq!(buf.cell(Position::new(1, 0)).unwrap().symbol(), "");
        assert_eq!(buf.cell(Position::new(2, 0)).unwrap().symbol(), "好");
        assert_eq!(buf.cell(Position::new(3, 0)).unwrap().symbol(), "");
        assert_eq!(buf.cell(Position::new(4, 0)).unwrap().symbol(), "a");
        // Trailing padding stays as real spaces: insert_before may draw over
        // scrolled-up stale rows, and a "" cell prints nothing there.
        for x in 5..10 {
            assert_eq!(buf.cell(Position::new(x, 0)).unwrap().symbol(), " ", "padding cell {x}");
        }
    }
}
