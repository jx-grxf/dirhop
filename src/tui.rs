//! The UI draws to stderr, so stdout stays free for the chosen path
//! (`cd "$(dirhop)"` keeps working).

use std::io::{self, BufWriter, Stderr};
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor, execute};
use ratatui::style::Color;
use ratatui::{Terminal, TerminalOptions, Viewport};

// Stderr is unbuffered: without the BufWriter every cursor move and colour change of a
// frame is its own write. That's cheap on a Unix pty, but each one is a round trip
// through ConPTY on Windows, so frames trickle onto the screen line by line.
pub type Term = Terminal<CrosstermBackend<BufWriter<Stderr>>>;

#[derive(Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub accent: Color,
    pub matched: Color,
    pub dim: Color,
    pub selected_bg: Color,
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "cyan",
        accent: Color::Cyan,
        matched: Color::Yellow,
        dim: Color::DarkGray,
        selected_bg: Color::Indexed(236),
    },
    Theme {
        name: "violet",
        accent: Color::Indexed(141),
        matched: Color::Indexed(215),
        dim: Color::DarkGray,
        selected_bg: Color::Indexed(236),
    },
    Theme {
        name: "green",
        accent: Color::Indexed(114),
        matched: Color::Indexed(222),
        dim: Color::DarkGray,
        selected_bg: Color::Indexed(236),
    },
    Theme {
        name: "amber",
        accent: Color::Indexed(214),
        matched: Color::Indexed(117),
        dim: Color::DarkGray,
        selected_bg: Color::Indexed(236),
    },
    // For light terminal backgrounds: darker accents and a pale selection bar.
    Theme {
        name: "light",
        accent: Color::Indexed(25),
        matched: Color::Indexed(166),
        dim: Color::Indexed(244),
        selected_bg: Color::Indexed(254),
    },
    Theme {
        name: "mono",
        accent: Color::Reset,
        matched: Color::Reset,
        dim: Color::DarkGray,
        selected_bg: Color::Indexed(237),
    },
];

static THEME: AtomicUsize = AtomicUsize::new(0);

pub fn set_theme(name: &str) {
    THEME.store(THEMES.iter().position(|t| t.name == name).unwrap_or(0), Ordering::Relaxed);
}

pub fn theme() -> Theme {
    THEMES[THEME.load(Ordering::Relaxed)]
}

pub fn accent() -> Color {
    theme().accent
}

pub fn matched() -> Color {
    theme().matched
}

pub fn dim() -> Color {
    theme().dim
}

pub fn selected_bg() -> Color {
    theme().selected_bg
}

/// Rows the compact picker takes below the prompt.
pub const COMPACT_HEIGHT: u16 = 16;

pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 12345 -> "12,345"
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn backend() -> CrosstermBackend<BufWriter<Stderr>> {
    CrosstermBackend::new(BufWriter::with_capacity(64 * 1024, io::stderr()))
}

pub fn enter() -> io::Result<Term> {
    enable_raw_mode()?;
    execute!(io::stderr(), EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(backend())
}

/// The picker in the rows below the prompt instead of the whole screen, like `fzf --height`.
/// Falls back to full screen when the terminal is too short for it to be useful.
pub fn enter_compact() -> io::Result<Term> {
    let rows = ratatui::crossterm::terminal::size().map(|(_, h)| h).unwrap_or(0);
    if rows < COMPACT_HEIGHT + 4 {
        return enter();
    }
    enable_raw_mode()?;
    execute!(io::stderr(), EnableMouseCapture)?;
    // The inline viewport asks the terminal where the cursor is; one that doesn't answer
    // gets the full-screen picker instead of an error.
    match Terminal::with_options(backend(), TerminalOptions { viewport: Viewport::Inline(COMPACT_HEIGHT) }) {
        Ok(term) => Ok(term),
        Err(_) => enter(),
    }
}

/// A full-screen terminal on top of a compact one, for screens that need the room
/// (settings). Leaving it returns to the compact picker.
pub fn with_fullscreen<T>(f: impl FnOnce(&mut Term) -> io::Result<T>) -> io::Result<T> {
    execute!(io::stderr(), EnterAlternateScreen)?;
    let result = Terminal::new(backend()).and_then(|mut t| f(&mut t));
    execute!(io::stderr(), LeaveAlternateScreen)?;
    result
}

/// Wipes the rows the compact picker used, so the prompt comes back clean. Done by hand:
/// `Terminal::clear` asks the terminal for the cursor position first, a round trip whose
/// answer can arrive after raw mode is off and end up typed into the prompt.
pub fn finish(term: &mut Term, compact: bool) {
    if !compact {
        return;
    }
    let area = term.get_frame().area();
    let _ = execute!(io::stderr(), cursor::MoveTo(area.x, area.y), Clear(ClearType::FromCursorDown));
}

pub fn leave() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), DisableMouseCapture, LeaveAlternateScreen, cursor::Show);
}

/// Restores the terminal before a panic message is printed.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn thousands_separators() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(4881), "4,881");
        assert_eq!(super::thousands(1234567), "1,234,567");
    }
}
