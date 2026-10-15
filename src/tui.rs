//! The UI draws to stderr, so stdout stays free for the chosen path
//! (`cd "$(dirhop)"` keeps working).

use std::io::{self, Stderr};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::crossterm::{cursor, execute};
use ratatui::style::Color;

pub type Term = Terminal<CrosstermBackend<Stderr>>;

pub const ACCENT: Color = Color::Cyan;
pub const MATCH: Color = Color::Yellow;
pub const DIM: Color = Color::DarkGray;
pub const SELECTED_BG: Color = Color::Indexed(236);

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

pub fn enter() -> io::Result<Term> {
    enable_raw_mode()?;
    execute!(io::stderr(), EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(io::stderr()))
}

pub fn leave() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), LeaveAlternateScreen, cursor::Show);
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
