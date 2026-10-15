//! `dirhop index show`: what the folder index holds, where the folders are, and a key to rebuild it.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::config::Config;
use crate::history;
use crate::index::{self, Index};
use crate::paths;
use crate::pins;
use crate::rank;
use crate::tui::{self, ACCENT, DIM, MATCH, SPINNER, Term, thousands};

struct Stats {
    folders: usize,
    repos: usize,
    in_priority: usize,
    age: Duration,
    took: Option<Duration>,
    current: bool,
    bytes: u64,
    /// (top-level folder, folder count), biggest first.
    areas: Vec<(String, usize)>,
    /// (root, is priority, folder count) in config order.
    roots: Vec<(String, bool, usize)>,
}

struct View {
    stats: Option<Stats>,
    pins: usize,
    visited: usize,
    rebuilding: Option<(Receiver<io::Result<()>>, Arc<AtomicUsize>)>,
    message: Option<(String, bool)>,
    frame: usize,
}

pub fn run() -> u8 {
    let cfg = Config::load();
    tui::install_panic_hook();
    let result = tui::enter().and_then(|mut term| event_loop(&mut term, &cfg));
    tui::leave();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("dirhop: {e}");
            2
        }
    }
}

fn event_loop(term: &mut Term, cfg: &Config) -> io::Result<()> {
    let mut v = View {
        stats: index::read(cfg).map(|i| stats(cfg, i)),
        pins: pins::load().len(),
        visited: history::scores().len(),
        rebuilding: None,
        message: None,
        frame: 0,
    };
    loop {
        if let Some((rx, _)) = &v.rebuilding
            && let Ok(done) = rx.try_recv()
        {
            v.rebuilding = None;
            v.message = Some(match done {
                Ok(()) => ("Index rebuilt.".into(), false),
                Err(e) => (format!("Rebuild failed: {e}"), true),
            });
            v.stats = index::read(cfg).map(|i| stats(cfg, i));
        }
        term.draw(|f| draw(f, &v))?;
        if !event::poll(Duration::from_millis(if v.rebuilding.is_some() { 60 } else { 500 }))? {
            v.frame += 1;
            continue;
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind == KeyEventKind::Release {
            continue;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return Ok(()),
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('r' | 'R') if v.rebuilding.is_none() => {
                let (tx, rx) = mpsc::channel();
                let count = Arc::new(AtomicUsize::new(0));
                let seen = count.clone();
                let cfg = cfg.clone();
                thread::spawn(move || {
                    let res = index::rebuild(&cfg, |n| seen.store(n, Ordering::Relaxed)).map(|_| ());
                    let _ = tx.send(res);
                });
                v.rebuilding = Some((rx, count));
                v.message = None;
            }
            _ => {}
        }
    }
}

fn stats(cfg: &Config, idx: Index) -> Stats {
    let mut areas: HashMap<String, usize> = HashMap::new();
    for e in &idx.entries {
        let mut parts = e.display.split('/');
        let first = parts.next().unwrap_or_default();
        let area = match (first, parts.next()) {
            (_, None) => continue,
            ("~", Some(top)) => format!("~/{top}"),
            (root, Some(top)) => format!("{root}/{top}"),
        };
        *areas.entry(area).or_default() += 1;
    }
    let mut areas: Vec<_> = areas.into_iter().collect();
    areas.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let count_under = |root: &PathBuf| idx.entries.iter().filter(|e| e.path.starts_with(root)).count();
    let mut roots: Vec<(String, bool, usize)> = Vec::new();
    for (raw, priority) in
        cfg.priority_roots.iter().map(|r| (r, true)).chain(cfg.search_roots.iter().map(|r| (r, false)))
    {
        if !roots.iter().any(|(r, _, _)| r == raw) {
            roots.push((raw.clone(), priority, count_under(&paths::expand(raw))));
        }
    }

    Stats {
        folders: idx.entries.len(),
        repos: idx.entries.iter().filter(|e| e.repo).count(),
        in_priority: idx.entries.iter().filter(|e| e.priority).count(),
        age: idx.age,
        took: idx.took,
        current: idx.current,
        bytes: fs::metadata(index::file()).map(|m| m.len()).unwrap_or(0),
        areas,
        roots,
    }
}

fn draw(f: &mut Frame, v: &View) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(Line::from(Span::styled(" dirhop index ", Style::new().fg(ACCENT).bold())))
        .title_bottom(
            Line::from(vec![
                Span::styled(" r", Style::new().fg(ACCENT)),
                Span::styled(" rebuild  ", Style::new().fg(DIM)),
                Span::styled("esc", Style::new().fg(ACCENT)),
                Span::styled(" close ", Style::new().fg(DIM)),
            ])
            .right_aligned(),
        );
    let inner = block.inner(f.area()).inner(Margin::new(2, 1));
    f.render_widget(block, f.area());

    let [summary, _, status, _, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    let Some(s) = &v.stats else {
        f.render_widget(
            Paragraph::new(Span::styled("No index yet. Press r to build it.", Style::new().fg(DIM))),
            summary,
        );
        draw_status(f, status, v);
        return;
    };

    let cell = |label: &str, value: String, note: String| {
        vec![
            Span::styled(format!("{label:<10}"), Style::new().fg(DIM)),
            Span::styled(value, Style::new().bold()),
            Span::styled(note, Style::new().fg(DIM)),
        ]
    };
    let took = s.took.map(|t| format!(" · scan took {} ms", t.as_millis())).unwrap_or_default();
    let left = vec![
        Line::from(cell(
            "Folders",
            thousands(s.folders),
            format!(" · {} in priority folders", thousands(s.in_priority)),
        )),
        Line::from(cell("Git repos", thousands(s.repos), String::new())),
        Line::from(cell("Updated", ago(s.age.as_secs()), took)),
    ];
    let right = vec![
        Line::from(cell("Pinned", thousands(v.pins), String::new())),
        Line::from(cell("Visited", thousands(v.visited), " folders in history".into())),
        Line::from(cell("File", paths::contract(&index::file()), format!(" · {}", size(s.bytes)))),
    ];
    let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(summary);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), r);
    draw_status(f, status, v);

    let wide = body.width >= 100;
    let [areas_rect, roots_rect] = if wide {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body)
    } else {
        let roots_h = s.roots.len() as u16 + 2;
        Layout::vertical([Constraint::Min(4), Constraint::Length(roots_h)]).areas(body)
    };
    draw_areas(f, areas_rect, s);
    draw_roots(f, roots_rect, s);
}

fn draw_status(f: &mut Frame, area: Rect, v: &View) {
    let line = if let Some((_, count)) = &v.rebuilding {
        Line::from(vec![
            Span::styled(format!("{} ", SPINNER[v.frame % SPINNER.len()]), Style::new().fg(ACCENT)),
            Span::raw(format!("scanning… {} folders", thousands(count.load(Ordering::Relaxed)))),
        ])
    } else if let Some((msg, err)) = &v.message {
        Line::from(Span::styled(msg.clone(), Style::new().fg(if *err { Color::Red } else { Color::Green })))
    } else if v.stats.as_ref().is_some_and(|s| !s.current) {
        Line::from(Span::styled(
            "Built with different search settings. The picker rescans anyway; r rebuilds it now.",
            Style::new().fg(MATCH),
        ))
    } else {
        let dim = |t: &'static str| Span::styled(t, Style::new().fg(DIM));
        let cmd = |t: &'static str| Span::styled(t, Style::new().fg(ACCENT));
        Line::from(vec![
            dim("The picker refreshes this on every scan; "),
            cmd("hop name"),
            dim(" and "),
            cmd("dirhop query"),
            dim(" read it directly."),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_areas(f: &mut Frame, area: Rect, s: &Stats) {
    let mut lines = vec![heading("Where the folders are"), Line::default()];
    let room = (area.height as usize).saturating_sub(2);
    // A blank row between bars when there is space, so they read as separate bars.
    let spaced = s.areas.len() * 2 <= room;
    let rows = if spaced { room.div_ceil(2) } else { room };
    let label_w = s.areas.iter().take(rows).map(|(a, _)| a.chars().count()).max().unwrap_or(0).min(28);
    let max = s.areas.first().map_or(1, |a| a.1).max(1);
    let count_w = thousands(max).len();
    let bar_w = (area.width as usize).saturating_sub(label_w + count_w + 6);
    for (i, (name, n)) in s.areas.iter().enumerate() {
        if i + 1 == rows && s.areas.len() > rows {
            let rest: usize = s.areas[i..].iter().map(|a| a.1).sum();
            lines.push(Line::from(Span::styled(
                format!("{:<label_w$}  {} more areas, {} folders", "…", s.areas.len() - i, thousands(rest)),
                Style::new().fg(DIM),
            )));
            break;
        }
        if spaced && i > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![
            Span::raw(format!("{:<label_w$}  ", truncate(name, label_w))),
            Span::styled(bar(*n, max, bar_w), Style::new().fg(ACCENT)),
            Span::styled(format!(" {:>count_w$}", thousands(*n)), Style::new().fg(DIM)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_roots(f: &mut Frame, area: Rect, s: &Stats) {
    let mut lines = vec![heading("Roots"), Line::default()];
    for (i, (root, priority, n)) in s.roots.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        let (mark, kind) = if *priority { ("◆ ", "priority") } else { ("· ", "search") };
        let missing = !paths::expand(root).is_dir();
        lines.push(Line::from(vec![
            Span::styled(mark, Style::new().fg(ACCENT)),
            Span::styled(format!("{root:<18} "), Style::new().bold()),
            Span::styled(format!("{kind:<9}"), Style::new().fg(DIM)),
            if missing {
                Span::styled("missing", Style::new().fg(Color::Red))
            } else {
                Span::styled(format!("{:>7}", thousands(*n)), Style::new().fg(DIM))
            },
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn ago(secs: u64) -> String {
    match rank::age_label(secs) {
        l if l == "now" => "just now".into(),
        l => format!("{l} ago"),
    }
}

fn heading(t: &'static str) -> Line<'static> {
    Line::from(Span::styled(t, Style::new().fg(DIM).bold()))
}

/// A bar with eighth-block precision, so small differences still show.
fn bar(n: usize, max: usize, width: usize) -> String {
    const PARTS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (n * width * 8).div_ceil(max.max(1));
    let mut out = "█".repeat(eighths / 8);
    out.push_str(PARTS[eighths % 8]);
    out
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b / 1024),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_scale_to_width() {
        assert_eq!(bar(10, 10, 4), "████");
        assert_eq!(bar(5, 10, 4), "██");
        assert_eq!(bar(1, 10, 4), "▌");
        assert_eq!(bar(0, 10, 4), "");
    }
}
