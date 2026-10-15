use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::actions;
use crate::config::Config;
use crate::history;
use crate::index;
use crate::pins;
use crate::preview::{self, GitInfo, GitWorker, Preview};
use crate::rank::{self, Ranker, Signals};
use crate::scan::{self, Entry};
use crate::settings;
use crate::tui::{self, ACCENT, DIM, MATCH, SELECTED_BG, Term};

pub enum Outcome {
    Picked(PathBuf),
    Cancelled,
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TERMINAL_EDITORS: &[&str] = &["vi", "vim", "nvim", "nano", "hx", "helix", "micro", "emacs", "kak", "pico"];
const STATUS_TTL: Duration = Duration::from_secs(4);

struct Picker {
    entries: Vec<Entry>,
    by_path: HashMap<PathBuf, usize>,
    /// Which entries the running scan has confirmed; the rest are dropped when it finishes.
    seen: Vec<bool>,
    /// (score, entry index), best first once sorted. Always holds every matching entry.
    results: Vec<(i64, usize)>,
    unsorted: bool,
    ranker: Ranker,
    query: String,
    /// The query `results` was computed for, to tell whether the next one only narrows it.
    ranked_query: String,
    selected: usize,
    scroll: usize,
    list_height: usize,
    scanning: bool,
    rx: Receiver<scan::Msg>,
    signals: Signals,
    status: Option<(String, Instant)>,
    updates: Option<Receiver<String>>,
    preview: Option<(PathBuf, Preview)>,
    git: GitWorker,
    git_info: HashMap<PathBuf, Option<GitInfo>>,
    git_pending: Option<PathBuf>,
    show_help: bool,
    frame: usize,
    now: u64,
}

pub fn run(term: &mut Term, cfg: &mut Config, query: &str, updates: Option<Receiver<String>>) -> io::Result<Outcome> {
    let (_, rx) = mpsc::channel();
    let mut p = Picker {
        entries: Vec::new(),
        by_path: HashMap::new(),
        seen: Vec::new(),
        results: Vec::new(),
        unsorted: false,
        ranker: Ranker::new(),
        query: query.to_string(),
        ranked_query: query.to_string(),
        selected: 0,
        scroll: 0,
        list_height: 10,
        scanning: false,
        rx,
        signals: Signals::load(),
        status: None,
        updates,
        preview: None,
        git: GitWorker::spawn(),
        git_info: HashMap::new(),
        git_pending: None,
        show_help: false,
        frame: 0,
        now: history::now(),
    };
    p.ranker.set_query(&p.query);
    if let Some(idx) = index::load(cfg) {
        for e in idx.entries {
            p.insert(e, false);
        }
        p.sort();
    }
    p.rescan(cfg, false);

    loop {
        p.drain(cfg);
        if p.unsorted {
            p.sort();
        }
        term.draw(|f| p.draw(f, cfg))?;

        let busy = p.scanning || p.git_pending.is_some();
        if !event::poll(Duration::from_millis(if busy { 40 } else { 250 }))? {
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
    fn insert(&mut self, mut e: Entry, seen: bool) {
        rank::prepare(&mut e, &self.signals);
        let idx = self.entries.len();
        if let Some(s) = self.ranker.score(&e) {
            self.results.push((s, idx));
            self.unsorted = true;
        }
        self.by_path.insert(e.path.clone(), idx);
        self.seen.push(seen);
        self.entries.push(e);
    }

    /// `fresh` starts from an empty list (scan settings changed); otherwise the current list
    /// stays on screen and the scan only corrects it.
    fn rescan(&mut self, cfg: &Config, fresh: bool) {
        let (tx, rx) = mpsc::channel();
        scan::spawn(cfg, tx);
        self.rx = rx;
        if fresh {
            self.entries.clear();
            self.by_path.clear();
            self.seen.clear();
            self.results.clear();
            self.preview = None;
            self.selected = 0;
            self.scroll = 0;
        } else {
            self.seen.fill(false);
        }
        self.scanning = true;
    }

    fn drain(&mut self, cfg: &Config) {
        let mut changed = false;
        let mut done = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                scan::Msg::Batch(batch) => {
                    for e in batch {
                        let Some(&i) = self.by_path.get(&e.path) else {
                            self.insert(e, true);
                            continue;
                        };
                        self.seen[i] = true;
                        let old = &mut self.entries[i];
                        if old.repo != e.repo || old.priority != e.priority {
                            old.repo = e.repo;
                            old.priority = e.priority;
                            rank::prepare(old, &self.signals);
                            changed = true;
                        }
                    }
                }
                scan::Msg::Done => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.scanning = false;
            changed |= self.prune();
            let _ = index::save(cfg, &self.entries);
        }
        if changed {
            self.refresh();
        }

        while let Some((path, info)) = self.git.try_recv() {
            if self.git_pending.as_ref() == Some(&path) {
                self.git_pending = None;
            }
            self.git_info.insert(path, info);
        }
        if let Some(rx) = &self.updates
            && let Ok(msg) = rx.try_recv()
        {
            self.set_status(msg);
        }
    }

    /// Drops entries the scan didn't find again. Returns whether anything went away.
    fn prune(&mut self) -> bool {
        if self.seen.iter().all(|s| *s) {
            return false;
        }
        let entries = std::mem::take(&mut self.entries);
        let seen = std::mem::take(&mut self.seen);
        self.entries = entries.into_iter().zip(seen).filter_map(|(e, s)| s.then_some(e)).collect();
        self.seen = vec![true; self.entries.len()];
        self.by_path = self.entries.iter().enumerate().map(|(i, e)| (e.path.clone(), i)).collect();
        true
    }

    fn remove(&mut self, path: &Path) {
        let Some(i) = self.by_path.remove(path) else { return };
        self.entries.remove(i);
        self.seen.remove(i);
        self.by_path = self.entries.iter().enumerate().map(|(i, e)| (e.path.clone(), i)).collect();
        self.refresh();
    }

    fn rescore_all(&mut self) {
        self.results.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(s) = self.ranker.score(e) {
                self.results.push((s, i));
            }
        }
    }

    /// New query typed: rank again and jump back to the top.
    fn requery(&mut self) {
        let narrow = rank::narrows(&self.ranked_query, &self.query);
        self.ranker.set_query(&self.query);
        if narrow {
            let (ranker, entries) = (&mut self.ranker, &self.entries);
            self.results.retain_mut(|r| match ranker.score(&entries[r.1]) {
                Some(s) => {
                    r.0 = s;
                    true
                }
                None => false,
            });
        } else {
            self.rescore_all();
        }
        self.ranked_query = self.query.clone();
        self.selected = 0;
        self.scroll = 0;
        self.sort();
    }

    /// Scores changed under the same query (pins, rescan): rank again, keep the highlighted folder.
    fn refresh(&mut self) {
        let keep = (self.selected > 0).then(|| self.current().map(|e| e.path.clone())).flatten();
        self.rescore_all();
        self.results.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.unsorted = false;
        self.selected = keep
            .and_then(|p| self.results.iter().position(|r| self.entries[r.1].path == p))
            .unwrap_or(0)
            .min(self.results.len().saturating_sub(1));
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

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    fn set_query(&mut self, q: String) {
        self.query = q;
        self.requery();
    }

    fn key(&mut self, k: KeyEvent, term: &mut Term, cfg: &mut Config) -> io::Result<Option<Outcome>> {
        if self.show_help {
            self.show_help = false;
            return Ok(None);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.list_height.max(1) as isize;
        match k.code {
            KeyCode::Esc => return Ok(Some(Outcome::Cancelled)),
            KeyCode::Char('c' | 'g' | 'q') if ctrl => return Ok(Some(Outcome::Cancelled)),
            KeyCode::Enter => {
                let Some(path) = self.current().map(|e| e.path.clone()) else { return Ok(None) };
                if path.is_dir() {
                    return Ok(Some(Outcome::Picked(path)));
                }
                self.set_status(format!("{} no longer exists", crate::paths::contract(&path)));
                self.remove(&path);
            }
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.results.len().saturating_sub(1),
            KeyCode::F(1) => self.show_help = true,
            KeyCode::Char('?') if self.query.is_empty() => self.show_help = true,
            KeyCode::Char('u') if ctrl => self.set_query(String::new()),
            KeyCode::Char('w') if ctrl => {
                let trimmed = self.query.trim_end_matches([' ', '/']);
                let cut = trimmed.rfind([' ', '/']).map(|i| i + 1).unwrap_or(0);
                let q = self.query[..cut].to_string();
                self.set_query(q);
            }
            KeyCode::Tab => {
                if let Some(e) = self.current() {
                    let q = format!("{}/", e.display);
                    self.set_query(q);
                }
            }
            KeyCode::BackTab => {
                // Up one level: from a Tab scope to its parent, otherwise to the highlighted
                // folder's siblings.
                let base = if self.query.ends_with('/') {
                    self.query.trim_end_matches('/').to_string()
                } else {
                    self.current().map(|e| e.display.clone()).unwrap_or_default()
                };
                let q = base.rfind('/').map(|i| base[..=i].to_string()).unwrap_or_default();
                self.set_query(q);
            }
            KeyCode::Char('b') if ctrl => self.toggle_pin(),
            KeyCode::Char('y') if ctrl => {
                if let Some(path) = self.current().map(|e| e.path.clone()) {
                    let shown = crate::paths::contract(&path);
                    match actions::copy_path(&path) {
                        Ok(()) => self.set_status(format!("copied {shown}")),
                        Err(e) => self.set_status(format!("copy failed: {e}")),
                    }
                }
            }
            KeyCode::Char('t') if ctrl => {
                if let Some(path) = self.current().map(|e| e.path.clone()) {
                    match actions::new_tab(&path) {
                        Ok(what) => self.set_status(format!("opened a {what}")),
                        Err(e) => self.set_status(e),
                    }
                }
            }
            KeyCode::Char('o') if ctrl => {
                if let Some(e) = self.current() {
                    let msg = match actions::open_in_file_manager(&e.path) {
                        Ok(()) => format!("opened {}", e.name()),
                        Err(err) => err,
                    };
                    self.set_status(msg);
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
                    self.rescan(cfg, true);
                }
            }
            KeyCode::Char('r') if ctrl => {
                self.signals = Signals::load();
                self.git_info.clear();
                self.preview = None;
                self.refresh();
                self.rescan(cfg, false);
                self.set_status("rescanning");
            }
            KeyCode::Backspace => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(q);
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                let mut q = self.query.clone();
                q.push(c);
                self.set_query(q);
            }
            _ => {}
        }
        Ok(None)
    }

    fn toggle_pin(&mut self) {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return };
        let name = crate::paths::contract(&path);
        match pins::toggle(&mut self.signals.pins, &path) {
            Ok(pinned) => {
                if let Some(&i) = self.by_path.get(&path) {
                    rank::prepare(&mut self.entries[i], &self.signals);
                }
                // Stay on the folder that was just pinned, wherever it moves to.
                self.rescore_all();
                self.results.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                self.unsorted = false;
                if let Some(pos) = self.results.iter().position(|r| self.entries[r.1].path == path) {
                    self.selected = pos;
                }
                self.set_status(if pinned { format!("pinned {name}") } else { format!("unpinned {name}") });
            }
            Err(e) => self.set_status(format!("could not save pins: {e}")),
        }
    }

    fn open_editor(&mut self, cfg: &Config) -> io::Result<Option<Outcome>> {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return Ok(None) };
        let mut parts = cfg.editor.split_whitespace();
        let Some(program) = parts.next() else {
            self.set_status("no editor set, ctrl-s to pick one");
            return Ok(None);
        };
        let args: Vec<&str> = parts.collect();
        let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
        let base = base.strip_suffix(".exe").unwrap_or(base);
        if TERMINAL_EDITORS.contains(&base) {
            // Hand the terminal over, then cd into the folder once the editor quits.
            tui::leave();
            let (input, output) = actions::console()?;
            let _ = Command::new(program)
                .args(&args)
                .arg(".")
                .current_dir(&path)
                .stdin(input)
                .stdout(output.try_clone()?)
                .stderr(output)
                .status();
            return Ok(Some(Outcome::Picked(path)));
        }
        let msg = match actions::spawn_detached(program, &args, &path) {
            Ok(()) => format!("opened in {base}"),
            Err(err) => format!("{base}: {err}"),
        };
        self.set_status(msg);
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame, cfg: &Config) {
        let [top, body, foot] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
        self.draw_input(f, top);

        let show_preview = body.width >= 90;
        let [list, side] = if show_preview {
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body)
        } else {
            [body, Rect::default()]
        };
        self.draw_list(f, list);
        if show_preview {
            self.draw_preview(f, side);
        }
        self.draw_footer(f, foot, cfg);
        if self.show_help {
            draw_help(f, cfg);
        }
    }

    fn draw_input(&self, f: &mut Frame, area: Rect) {
        let counter = if self.scanning {
            format!(" {} {}/{} ", SPINNER[self.frame % SPINNER.len()], self.results.len(), self.entries.len())
        } else {
            format!(" {}/{} ", self.results.len(), self.entries.len())
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(DIM))
            .title(Line::from(Span::styled(" dirhop ", Style::new().fg(ACCENT).bold())))
            .title(Line::from(Span::styled(counter, Style::new().fg(DIM))).right_aligned());
        let inner = block.inner(area);
        let mut spans = vec![Span::styled("› ", Style::new().fg(ACCENT).bold())];
        if self.query.is_empty() {
            spans.push(Span::styled("type to search · pinned and recent folders first", Style::new().fg(DIM)));
        } else {
            spans.push(Span::raw(self.query.as_str()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
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
            let msg = if self.scanning {
                "  scanning…"
            } else if self.entries.is_empty() {
                "  no folders found, check the search folders with ctrl-s"
            } else {
                "  no matching folders"
            };
            f.render_widget(Paragraph::new(Span::styled(msg, Style::new().fg(DIM))), area);
            return;
        }

        let overflow = self.results.len() > h;
        // Leave a gap column so the scrollbar doesn't merge with the preview's divider.
        let width = (area.width as usize).saturating_sub(if overflow { 2 } else { 0 });
        let mut lines = Vec::with_capacity(h);
        for row in self.scroll..(self.scroll + h).min(self.results.len()) {
            let e = &self.entries[self.results[row].1];
            let hl = self.ranker.highlights(e);
            lines.push(row_line(e, &hl, row == self.selected, width, self.now));
        }
        f.render_widget(Paragraph::new(lines), area);

        if overflow {
            let mut state = ScrollbarState::new(self.results.len().saturating_sub(h)).position(self.scroll);
            let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_symbol("▐")
                .thumb_style(Style::new().fg(DIM));
            let bar_area = Rect { width: area.width.saturating_sub(1), ..area };
            f.render_stateful_widget(bar, bar_area, &mut state);
        }
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::new().borders(Borders::LEFT).border_style(Style::new().fg(DIM));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(idx) = self.results.get(self.selected).map(|r| r.1) else { return };
        let path = self.entries[idx].path.clone();
        if self.preview.as_ref().map(|p| &p.0) != Some(&path) {
            self.preview = Some((path.clone(), preview::load(&path)));
        }
        let (_, pv) = self.preview.as_ref().unwrap();
        if pv.branch.is_some() && !self.git_info.contains_key(&path) && self.git_pending.as_ref() != Some(&path) {
            self.git.request(&path);
            self.git_pending = Some(path.clone());
        }
        let e = &self.entries[idx];
        let pad = inner.inner(Margin::new(1, 0));
        let width = pad.width as usize;

        let mut title = vec![Span::styled(e.name().to_string(), Style::new().bold())];
        if e.pinned {
            title.push(Span::styled("  ★ pinned", Style::new().fg(MATCH)));
        }
        let mut lines = vec![Line::from(title)];

        if let Some(b) = &pv.branch {
            let mut spans = vec![
                Span::styled("git ", Style::new().fg(DIM)),
                Span::styled(b.clone(), Style::new().fg(Color::Magenta)),
            ];
            match self.git_info.get(&path) {
                Some(Some(g)) => {
                    if g.ahead > 0 {
                        spans.push(Span::styled(format!("  ↑{}", g.ahead), Style::new().fg(ACCENT)));
                    }
                    if g.behind > 0 {
                        spans.push(Span::styled(format!("  ↓{}", g.behind), Style::new().fg(ACCENT)));
                    }
                    spans.push(if g.changed == 0 {
                        Span::styled("  ✓ clean", Style::new().fg(Color::Green))
                    } else {
                        Span::styled(format!("  ● {} changed", g.changed), Style::new().fg(MATCH))
                    });
                }
                Some(None) => {}
                None => spans.push(Span::styled("  …", Style::new().fg(DIM))),
            }
            lines.push(Line::from(spans));
            if let Some(Some(GitInfo { last_commit: Some((hash, subject, time)), .. })) = self.git_info.get(&path) {
                let age = rank::age_label(self.now.saturating_sub(*time));
                let text = format!("{hash} {subject}");
                let room = width.saturating_sub(age.len() + 3);
                lines.push(Line::from(vec![
                    Span::styled(truncate(&text, room), Style::new().fg(DIM)),
                    Span::styled(format!(" · {age}"), Style::new().fg(DIM)),
                ]));
            }
        }
        let mut meta = Vec::new();
        if !pv.languages.is_empty() {
            meta.push(Span::styled(pv.languages.join(", "), Style::new().fg(Color::Green)));
            meta.push(Span::styled("  ·  ", Style::new().fg(DIM)));
        }
        meta.push(Span::styled(
            format!("{} item{}", pv.total, if pv.total == 1 { "" } else { "s" }),
            Style::new().fg(DIM),
        ));
        lines.push(Line::from(meta));

        if let Some(text) = &pv.readme {
            lines.push(Line::default());
            let budget = (inner.height as usize / 4).clamp(1, 4);
            for l in wrap(text, width, budget) {
                lines.push(Line::from(Span::styled(l, Style::new().fg(DIM).add_modifier(Modifier::ITALIC))));
            }
        }
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
        f.render_widget(Paragraph::new(lines), pad);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect, cfg: &Config) {
        if let Some((msg, at)) = &self.status
            && at.elapsed() < STATUS_TTL
        {
            f.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), Style::new().fg(MATCH))), area);
            return;
        }
        let mut hints = vec![
            ("enter", "cd"),
            ("tab", "inside"),
            ("⇧tab", "up"),
            ("^b", "pin"),
            ("^y", "copy"),
            ("^t", "new tab"),
            ("^o", "open"),
        ];
        if !cfg.editor.is_empty() {
            hints.push(("^e", "editor"));
        }
        hints.push(("^s", "settings"));
        let help = if self.query.is_empty() { ("?", "help") } else { ("f1", "help") };
        let len = |(k, t): (&str, &str)| k.chars().count() + t.chars().count() + 3;

        let mut budget = (area.width as usize).saturating_sub(len(help) + 1);
        let mut spans = vec![Span::raw(" ")];
        for h in hints {
            if len(h) > budget {
                break;
            }
            budget -= len(h);
            spans.push(Span::styled(h.0, Style::new().fg(ACCENT)));
            spans.push(Span::styled(format!(" {}  ", h.1), Style::new().fg(DIM)));
        }
        spans.push(Span::styled(help.0, Style::new().fg(ACCENT)));
        spans.push(Span::styled(format!(" {}", help.1), Style::new().fg(DIM)));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn draw_help(f: &mut Frame, cfg: &Config) {
    let keys: &[(&str, &str)] = &[
        ("enter", "cd into the folder"),
        ("↑ ↓  ^p ^n", "move"),
        ("tab", "search inside the highlighted folder"),
        ("shift-tab", "go up a level"),
        ("^b", "pin or unpin (pinned folders stay on top)"),
        ("^y", "copy the path"),
        ("^t", "open in a new terminal tab"),
        ("^o", "open in the file manager"),
        ("^e", "open in your editor"),
        ("^s", "settings"),
        ("^r", "rescan"),
        ("^u  ^w", "clear the search / delete a word"),
        ("esc", "cancel"),
    ];
    let search: &[(&str, &str)] =
        &[("'word", "exact match"), ("^word", "starts with"), ("word$", "ends with"), ("!word", "exclude")];
    let cmd = &cfg.command;
    let shell: &[(String, &str)] = &[
        (format!("{cmd} name"), "jump straight there if one folder clearly wins"),
        (format!("{cmd} -"), "back to the previous folder"),
        (format!("{cmd} .."), "paths work like cd"),
    ];

    let mut lines = vec![Line::default()];
    let row = |k: &str, t: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<12}"), Style::new().fg(ACCENT)),
            Span::styled(t.to_string(), Style::new()),
        ])
    };
    let heading = |t: &'static str| Line::from(Span::styled(format!("  {t}"), Style::new().fg(DIM).bold()));
    lines.push(heading("keys"));
    lines.extend(keys.iter().map(|(k, t)| row(k, t)));
    lines.push(Line::default());
    lines.push(heading("search"));
    lines.extend(search.iter().map(|(k, t)| row(k, t)));
    lines.push(Line::default());
    lines.push(heading("shell"));
    lines.extend(shell.iter().map(|(k, t)| row(k, t)));
    lines.push(Line::default());

    let area = f.area();
    let h = (lines.len() as u16 + 2).min(area.height);
    let w = 66.min(area.width);
    let [popup] = Layout::vertical([Constraint::Length(h)]).flex(Flex::Center).areas(area);
    let [popup] = Layout::horizontal([Constraint::Length(w)]).flex(Flex::Center).areas(popup);
    f.buffer_mut().set_style(area, Style::new().fg(DIM).remove_modifier(Modifier::BOLD));
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(Line::from(Span::styled(" help ", Style::new().fg(ACCENT).bold())))
        .title_bottom(Line::from(Span::styled(" any key closes ", Style::new().fg(DIM))).right_aligned());
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn row_line(e: &Entry, hl: &[usize], selected: bool, width: usize, now: u64) -> Line<'static> {
    let base = if selected { Style::new().bg(SELECTED_BG) } else { Style::new() };
    let mut spans = vec![
        Span::styled(if selected { "▌" } else { " " }, base.fg(ACCENT)),
        if e.pinned {
            Span::styled("★ ", base.fg(MATCH))
        } else if e.repo {
            Span::styled("◆ ", base.fg(ACCENT))
        } else if e.priority {
            Span::styled("· ", base.fg(DIM))
        } else {
            Span::styled("  ", base)
        },
    ];

    let age = e.last_visit.filter(|_| width >= 48).map(|t| format!(" {} ", rank::age_label(now.saturating_sub(t))));
    let right = age.as_ref().map_or(0, |a| a.chars().count());

    let chars: Vec<char> = e.display.chars().collect();
    let avail = width.saturating_sub(3 + right);
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
    if let Some(age) = age {
        if used + right <= width {
            spans.push(Span::styled(" ".repeat(width - used - right), base));
        }
        spans.push(Span::styled(age, base.fg(DIM)));
    } else if selected && used < width {
        spans.push(Span::styled(" ".repeat(width - used), base));
    }
    Line::from(spans)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Greedy word wrap into at most `max_lines` lines; the last one ends in `…` if text is left.
fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let need = cur.chars().count() + usize::from(!cur.is_empty()) + word.chars().count();
        if need > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                let last = lines.last_mut().unwrap();
                *last = truncate(&format!("{last} …"), width);
                return lines;
            }
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&truncate(word, width));
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_respects_width_and_line_budget() {
        let lines = wrap("one two three four five six seven", 10, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.chars().count() <= 10));
        assert!(lines[1].ends_with('…'));
        assert_eq!(wrap("short", 20, 3), vec!["short"]);
    }
}
