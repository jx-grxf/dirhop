use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::config::Config;
use crate::preview::{self, Preview};
use crate::rank::{self, Ranker};
use crate::scan::{self, Entry};
use crate::settings;
use crate::tui::{self, ACCENT, DIM, MATCH, SELECTED_BG, Term};

pub enum Outcome {
    Picked(PathBuf),
    Cancelled,
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TERMINAL_EDITORS: &[&str] = &["vi", "vim", "nvim", "nano", "hx", "helix", "micro", "emacs", "kak", "pico"];

struct Picker {
    entries: Vec<Entry>,
    /// (score, entry index), best first once sorted.
    results: Vec<(i64, usize)>,
    unsorted: bool,
    ranker: Ranker,
    query: String,
    selected: usize,
    scroll: usize,
    list_height: usize,
    scanning: bool,
    rx: Receiver<scan::Msg>,
    history: HashMap<PathBuf, f64>,
    status: Option<String>,
    updates: Option<Receiver<String>>,
    preview: Option<(usize, Preview)>,
    frame: usize,
}

pub fn run(term: &mut Term, cfg: &mut Config, query: &str, updates: Option<Receiver<String>>) -> io::Result<Outcome> {
    let (_, rx) = mpsc::channel();
    let mut p = Picker {
        entries: Vec::new(),
        results: Vec::new(),
        unsorted: false,
        ranker: Ranker::new(),
        query: query.to_string(),
        selected: 0,
        scroll: 0,
        list_height: 10,
        scanning: false,
        rx,
        history: crate::history::scores(),
        status: None,
        updates,
        preview: None,
        frame: 0,
    };
    p.ranker.set_query(&p.query);
    p.rescan(cfg);

    loop {
        p.drain();
        if p.unsorted {
            p.sort();
        }
        term.draw(|f| p.draw(f, cfg))?;

        let timeout = Duration::from_millis(if p.scanning { 40 } else { 400 });
        if !event::poll(timeout)? {
            p.frame += 1;
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Some(out) = p.key(k, term, cfg)? {
                    return Ok(out);
                }
            }
            _ => {}
        }
    }
}

impl Picker {
    fn rescan(&mut self, cfg: &Config) {
        let (tx, rx) = mpsc::channel();
        scan::spawn(cfg, tx);
        self.rx = rx;
        self.entries.clear();
        self.results.clear();
        self.preview = None;
        self.selected = 0;
        self.scroll = 0;
        self.scanning = true;
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                scan::Msg::Batch(batch) => {
                    for mut e in batch {
                        e.bonus = rank::bonus(&e, &self.history);
                        let idx = self.entries.len();
                        if let Some(s) = self.ranker.score(&e) {
                            self.results.push((s, idx));
                            self.unsorted = true;
                        }
                        self.entries.push(e);
                    }
                }
                scan::Msg::Done => self.scanning = false,
            }
        }
        if let Some(rx) = &self.updates
            && let Ok(msg) = rx.try_recv()
        {
            self.status = Some(msg);
        }
    }

    fn requery(&mut self) {
        self.ranker.set_query(&self.query);
        self.results.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(s) = self.ranker.score(e) {
                self.results.push((s, i));
            }
        }
        self.selected = 0;
        self.scroll = 0;
        self.sort();
    }

    /// Re-sorts while keeping the highlighted row put if the user moved off the top.
    fn sort(&mut self) {
        let keep = (self.selected > 0).then(|| self.results.get(self.selected).map(|r| r.1)).flatten();
        self.results.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        if let Some(id) = keep {
            self.selected = self.results.iter().position(|r| r.1 == id).unwrap_or(0);
        }
        self.unsorted = false;
    }

    fn current(&self) -> Option<&Entry> {
        self.results.get(self.selected).map(|r| &self.entries[r.1])
    }

    fn move_by(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let max = self.results.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    fn key(&mut self, k: KeyEvent, term: &mut Term, cfg: &mut Config) -> io::Result<Option<Outcome>> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.list_height.max(1) as isize;
        match k.code {
            KeyCode::Esc => return Ok(Some(Outcome::Cancelled)),
            KeyCode::Char('c' | 'g' | 'q') if ctrl => return Ok(Some(Outcome::Cancelled)),
            KeyCode::Enter => return Ok(self.current().map(|e| Outcome::Picked(e.path.clone()))),
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.results.len().saturating_sub(1),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.requery();
            }
            KeyCode::Char('w') if ctrl => {
                let trimmed = self.query.trim_end_matches([' ', '/']);
                let cut = trimmed.rfind([' ', '/']).map(|i| i + 1).unwrap_or(0);
                self.query.truncate(cut);
                self.requery();
            }
            KeyCode::Char('o') if ctrl => {
                if let Some(e) = self.current() {
                    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                    let path = e.path.clone();
                    self.status = Some(match spawn_detached(opener, &[], &path) {
                        Ok(()) => format!("opened {}", e.name()),
                        Err(err) => format!("{opener}: {err}"),
                    });
                }
            }
            KeyCode::Char('e') if ctrl => {
                if let Some(out) = self.open_editor(cfg)? {
                    return Ok(Some(out));
                }
            }
            KeyCode::Char('s') if ctrl => {
                let before = cfg.clone();
                settings::run(term, cfg, false)?;
                let rescan = before.priority_roots != cfg.priority_roots
                    || before.search_roots != cfg.search_roots
                    || before.exclude != cfg.exclude
                    || before.show_hidden != cfg.show_hidden
                    || before.respect_gitignore != cfg.respect_gitignore
                    || before.max_depth != cfg.max_depth;
                if rescan {
                    self.rescan(cfg);
                }
            }
            KeyCode::Char('r') if ctrl => {
                self.history = crate::history::scores();
                self.rescan(cfg);
                self.status = Some("rescanning".into());
            }
            KeyCode::Tab => {
                // Drill down: search inside the highlighted folder.
                if let Some(e) = self.current() {
                    self.query = format!("{}/", e.display);
                    self.requery();
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.requery();
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.query.push(c);
                self.requery();
            }
            _ => {}
        }
        Ok(None)
    }

    fn open_editor(&mut self, cfg: &Config) -> io::Result<Option<Outcome>> {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return Ok(None) };
        let mut parts = cfg.editor.split_whitespace();
        let Some(program) = parts.next() else {
            self.status = Some("no editor set — ctrl-s to pick one".into());
            return Ok(None);
        };
        let args: Vec<&str> = parts.collect();
        let base = program.rsplit('/').next().unwrap_or(program);
        if TERMINAL_EDITORS.contains(&base) {
            // Hand the terminal over, then cd into the folder once the editor quits.
            tui::leave();
            let tty = File::options().read(true).write(true).open("/dev/tty")?;
            let _ = Command::new(program)
                .args(&args)
                .arg(".")
                .current_dir(&path)
                .stdin(tty.try_clone()?)
                .stdout(tty.try_clone()?)
                .stderr(tty)
                .status();
            return Ok(Some(Outcome::Picked(path)));
        }
        self.status = Some(match spawn_detached(program, &args, &path) {
            Ok(()) => format!("opened in {base}"),
            Err(err) => format!("{base}: {err}"),
        });
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame, cfg: &Config) {
        let [top, body, foot] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
        self.draw_input(f, top);

        let show_preview = body.width >= 90;
        let [list, side] = if show_preview {
            Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(body)
        } else {
            [body, Rect::default()]
        };
        self.draw_list(f, list);
        if show_preview {
            self.draw_preview(f, side);
        }
        self.draw_footer(f, foot, cfg);
    }

    fn draw_input(&self, f: &mut Frame, area: Rect) {
        let counter = if self.scanning {
            format!(" {} {}/{} ", SPINNER[self.frame % SPINNER.len()], self.results.len(), self.entries.len())
        } else {
            format!(" {}/{} ", self.results.len(), self.entries.len())
        };
        let block = Block::bordered()
            .border_style(Style::new().fg(DIM))
            .title(Line::from(Span::styled(" dirhop ", Style::new().fg(ACCENT).bold())))
            .title(Line::from(Span::styled(counter, Style::new().fg(DIM))).right_aligned());
        let inner = block.inner(area);
        let line = Line::from(vec![Span::styled("› ", Style::new().fg(ACCENT).bold()), Span::raw(self.query.as_str())]);
        f.render_widget(Paragraph::new(line).block(block), area);
        let x = inner.x + 2 + self.query.chars().count() as u16;
        f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let h = area.height as usize;
        self.list_height = h;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + h {
            self.scroll = self.selected + 1 - h;
        }

        if self.results.is_empty() {
            let msg = if self.scanning { "  scanning…" } else { "  no matching folders" };
            f.render_widget(Paragraph::new(Span::styled(msg, Style::new().fg(DIM))), area);
            return;
        }

        let width = area.width as usize;
        let mut lines = Vec::with_capacity(h);
        for row in self.scroll..(self.scroll + h).min(self.results.len()) {
            let e = &self.entries[self.results[row].1];
            let hl = self.ranker.highlights(e);
            lines.push(row_line(e, &hl, row == self.selected, width));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::new().borders(Borders::LEFT).border_style(Style::new().fg(DIM));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(idx) = self.results.get(self.selected).map(|r| r.1) else { return };
        if self.preview.as_ref().map(|p| p.0) != Some(idx) {
            self.preview = Some((idx, preview::load(&self.entries[idx].path)));
        }
        let (_, pv) = self.preview.as_ref().unwrap();
        let e = &self.entries[idx];

        let mut lines = vec![Line::from(Span::styled(e.name().to_string(), Style::new().bold()))];
        let mut meta = Vec::new();
        if let Some(b) = &pv.branch {
            meta.push(Span::styled(format!("⎇ {b}"), Style::new().fg(Color::Magenta)));
        }
        if !pv.languages.is_empty() {
            if !meta.is_empty() {
                meta.push(Span::styled("  ·  ", Style::new().fg(DIM)));
            }
            meta.push(Span::styled(pv.languages.join(", "), Style::new().fg(Color::Green)));
        }
        if !meta.is_empty() {
            lines.push(Line::from(meta));
        }
        lines.push(Line::from(Span::styled(
            format!("{} item{}", pv.total, if pv.total == 1 { "" } else { "s" }),
            Style::new().fg(DIM),
        )));
        lines.push(Line::default());
        if let Some(err) = &pv.error {
            lines.push(Line::from(Span::styled(err.clone(), Style::new().fg(Color::Red))));
        }
        let room = (inner.height as usize).saturating_sub(lines.len());
        for (i, (name, is_dir)) in pv.children.iter().enumerate() {
            if i + 1 == room && pv.children.len() > room {
                lines.push(Line::from(Span::styled(format!("  … {} more", pv.total - i), Style::new().fg(DIM))));
                break;
            }
            let hidden = name.starts_with('.');
            let line = if *is_dir {
                let style = if hidden { Style::new().fg(DIM) } else { Style::new().fg(Color::Blue) };
                Line::from(vec![Span::styled("▸ ", Style::new().fg(DIM)), Span::styled(format!("{name}/"), style)])
            } else {
                let style = if hidden { Style::new().fg(DIM) } else { Style::new() };
                Line::from(vec![Span::raw("  "), Span::styled(name.clone(), style)])
            };
            lines.push(line);
        }
        let pad = Rect { x: inner.x + 1, width: inner.width.saturating_sub(1), ..inner };
        f.render_widget(Paragraph::new(lines), pad);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect, cfg: &Config) {
        let key = |k: &'static str| Span::styled(k, Style::new().fg(ACCENT));
        let txt = |t: &'static str| Span::styled(t, Style::new().fg(DIM));
        let mut spans = vec![key(" enter"), txt(" cd  "), key("tab"), txt(" inside  "), key("^o"), txt(" open  ")];
        if !cfg.editor.is_empty() {
            spans.extend([key("^e"), txt(" editor  ")]);
        }
        spans.extend([key("^s"), txt(" settings  "), key("esc"), txt(" quit")]);
        if let Some(s) = &self.status {
            spans.push(Span::styled(format!("   {s}"), Style::new().fg(MATCH)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn row_line(e: &Entry, hl: &[usize], selected: bool, width: usize) -> Line<'static> {
    let base = if selected { Style::new().bg(SELECTED_BG) } else { Style::new() };
    let mut spans = vec![
        Span::styled(if selected { "▌" } else { " " }, base.fg(ACCENT)),
        if e.repo {
            Span::styled("◆ ", base.fg(ACCENT))
        } else if e.priority {
            Span::styled("· ", base.fg(DIM))
        } else {
            Span::styled("  ", base)
        },
    ];

    let chars: Vec<char> = e.display.chars().collect();
    let avail = width.saturating_sub(3);
    let skip = if chars.len() > avail { chars.len() - avail + 1 } else { 0 };
    if skip > 0 {
        spans.push(Span::styled("…", base.fg(DIM)));
    }

    // Group runs of equally styled chars into one span.
    let mut run = String::new();
    let mut run_style = base;
    for (i, c) in chars.iter().enumerate().skip(skip) {
        let mut style = if i >= e.name_start { base.add_modifier(Modifier::BOLD) } else { base.fg(DIM) };
        if hl.binary_search(&i).is_ok() {
            style = base.fg(MATCH).add_modifier(Modifier::BOLD);
        }
        if style != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = style;
        run.push(*c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if selected && used < width {
        spans.push(Span::styled(" ".repeat(width - used), base));
    }
    Line::from(spans)
}

fn spawn_detached(program: &str, args: &[&str], path: &std::path::Path) -> io::Result<()> {
    Command::new(program)
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}
