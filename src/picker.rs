use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::actions;
use crate::config::Config;
use crate::history;
use crate::index;
use crate::paths;
use crate::pins;
use crate::preview::{GitInfo, GitWorker, Preview, PreviewWorker, RowGitWorker};
use crate::rank::{self, Ranker, Signals};
use crate::scan::{self, Entry};
use crate::settings;
use crate::tui::{self, SPINNER, Term, accent, dim, matched, selected_bg};

pub enum Outcome {
    Picked(PathBuf),
    Cancelled,
}

const TERMINAL_EDITORS: &[&str] = &["vi", "vim", "nvim", "nano", "hx", "helix", "micro", "emacs", "kak", "pico"];
const STATUS_TTL: Duration = Duration::from_secs(4);
const PREVIEW_WAIT: Duration = Duration::from_millis(12);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    Repos,
    Pinned,
    Recent,
    Files,
}

const SCOPES: &[(Scope, &str)] = &[
    (Scope::All, "all"),
    (Scope::Repos, "repos"),
    (Scope::Pinned, "pinned"),
    (Scope::Recent, "recent"),
    (Scope::Files, "files"),
];

/// The tab filter on top of the search. Files live in their own list, so every folder
/// passes for them.
fn in_scope(scope: Scope, e: &Entry) -> bool {
    match scope {
        Scope::All | Scope::Files => true,
        Scope::Repos => e.repo,
        Scope::Pinned => e.pinned,
        Scope::Recent => e.visited,
    }
}

fn score(ranker: &mut Ranker, scope: Scope, empty_query: bool, e: &Entry) -> Option<i64> {
    if !in_scope(scope, e) {
        return None;
    }
    let s = ranker.score(e)?;
    // The recent tab reads like a history: newest jump first.
    if scope == Scope::Recent && empty_query {
        return Some(e.last_visit.map_or(e.idle, |t| t as i64));
    }
    Some(s)
}

struct Picker {
    entries: Vec<Entry>,
    by_path: HashMap<PathBuf, usize>,
    /// Which entries the running scan has confirmed; the rest are dropped when it finishes.
    seen: Vec<bool>,
    /// Listed on the first visit to the files tab, never saved.
    files: Vec<Entry>,
    files_rx: Option<Receiver<scan::Msg>>,
    scope: Scope,
    /// (score, index into the current tab's list), best first once sorted. Always holds
    /// every matching entry.
    results: Vec<(i64, usize)>,
    unsorted: bool,
    ranker: Ranker,
    query: String,
    /// The query `results` was computed for, to tell whether the next one only narrows it.
    ranked_query: String,
    /// Naming the highlighted folder for `hop @name`: (folder, name typed so far).
    alias_prompt: Option<(PathBuf, String)>,
    selected: usize,
    scroll: usize,
    list_height: usize,
    list_area: Rect,
    /// Where each tab was drawn: (row, first column, last column).
    tab_hits: Vec<(u16, u16, u16, Scope)>,
    last_click: Option<(Instant, usize)>,
    scanning: bool,
    scan_started: Instant,
    rx: Receiver<scan::Msg>,
    signals: Signals,
    zoxide: Option<Receiver<HashMap<PathBuf, f64>>>,
    status: Option<(String, Instant)>,
    updates: Option<Receiver<String>>,
    /// Folder listings already loaded; scrolling back over them costs nothing.
    previews: HashMap<PathBuf, Preview>,
    previewer: PreviewWorker,
    preview_pending: Option<PathBuf>,
    git: GitWorker,
    rows_git: RowGitWorker,
    rows_requested: Vec<PathBuf>,
    git_info: HashMap<PathBuf, Option<GitInfo>>,
    git_pending: Option<PathBuf>,
    git_enabled: bool,
    show_help: bool,
    traced_first_frame: bool,
    frame: usize,
    now: u64,
}

pub fn run(term: &mut Term, cfg: &mut Config, query: &str, updates: Option<Receiver<String>>) -> io::Result<Outcome> {
    let (_, rx) = mpsc::channel();
    let mut p = Picker {
        entries: Vec::new(),
        by_path: HashMap::new(),
        seen: Vec::new(),
        files: Vec::new(),
        files_rx: None,
        scope: Scope::All,
        results: Vec::new(),
        unsorted: false,
        ranker: Ranker::new(),
        query: query.to_string(),
        ranked_query: query.to_string(),
        alias_prompt: None,
        selected: 0,
        scroll: 0,
        list_height: 10,
        list_area: Rect::default(),
        tab_hits: Vec::new(),
        last_click: None,
        scanning: false,
        scan_started: Instant::now(),
        rx,
        signals: Signals::load_own().with_aliases(cfg),
        zoxide: Some(spawn_zoxide()),
        status: None,
        updates,
        previews: HashMap::new(),
        previewer: PreviewWorker::preview(),
        preview_pending: None,
        git: GitWorker::git(),
        rows_git: RowGitWorker::spawn(),
        rows_requested: Vec::new(),
        git_info: HashMap::new(),
        git_pending: None,
        git_enabled: cfg.git_status,
        show_help: false,
        traced_first_frame: false,
        frame: 0,
        now: history::now(),
    };
    p.ranker.set_query(&p.query);
    // A recent index is trusted as is: no scan competing with the UI for disk and CPU.
    // Ctrl-R still rescans on demand.
    let mut fresh = false;
    if let Some(idx) = index::load(cfg) {
        fresh = idx.age < index::STALE_AFTER;
        for e in idx.entries {
            p.insert(e, false);
        }
        p.sort();
    }
    crate::trace::mark(|| format!("index loaded, {} folders, fresh: {fresh}", p.entries.len()));
    if !fresh {
        p.rescan(cfg, false);
    }

    loop {
        p.drain(cfg);
        if p.unsorted {
            p.sort();
        }
        term.draw(|f| p.draw(f, cfg))?;
        if !p.traced_first_frame {
            p.traced_first_frame = true;
            crate::trace::mark(|| "first frame drawn".into());
        }

        if !event::poll(Duration::from_millis(if p.busy() { 40 } else { 250 }))? {
            p.frame += 1;
            continue;
        }
        // Handle every event already waiting before drawing again, so held arrows, fast
        // typing and the mouse wheel don't queue up one full redraw each.
        loop {
            let out = match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => p.key(k, term, cfg)?,
                Event::Mouse(m) => p.mouse(m, cfg),
                _ => None,
            };
            if let Some(out) = out {
                return Ok(out);
            }
            if !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}

impl Picker {
    fn busy(&self) -> bool {
        self.scanning
            || self.files_rx.is_some()
            || self.git_pending.is_some()
            || self.preview_pending.is_some()
            || self.zoxide.is_some()
            || self.rows_requested.iter().any(|p| !self.git_info.contains_key(p))
    }

    fn list(&self) -> &[Entry] {
        if self.scope == Scope::Files { &self.files } else { &self.entries }
    }

    fn empty_query(&self) -> bool {
        self.query.trim().is_empty()
    }

    fn insert(&mut self, mut e: Entry, seen: bool) {
        rank::prepare(&mut e, &self.signals);
        let idx = self.entries.len();
        if self.scope != Scope::Files
            && let Some(s) = score(&mut self.ranker, self.scope, self.query.trim().is_empty(), &e)
        {
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
        self.scan_started = Instant::now();
        if fresh {
            self.entries.clear();
            self.by_path.clear();
            self.seen.clear();
            self.files.clear();
            self.files_rx = None;
            self.results.clear();
            self.previews.clear();
            self.selected = 0;
            self.scroll = 0;
            if self.scope == Scope::Files {
                self.list_files(cfg);
            }
        } else {
            self.seen.fill(false);
        }
        self.scanning = true;
    }

    fn list_files(&mut self, cfg: &Config) {
        let (tx, rx) = mpsc::channel();
        scan::spawn_files(cfg, tx);
        self.files.clear();
        self.files_rx = Some(rx);
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
                        if old.priority != e.priority {
                            old.priority = e.priority;
                            rank::prepare(old, &self.signals);
                            changed = true;
                        }
                    }
                }
                scan::Msg::Done(repos) => {
                    for e in &mut self.entries {
                        let repo = repos.contains(&e.path);
                        if e.repo != repo {
                            e.repo = repo;
                            rank::prepare(e, &self.signals);
                            changed = true;
                        }
                    }
                    done = true;
                    break;
                }
            }
        }
        if done {
            crate::trace::mark(|| format!("scan done, {} folders", self.entries.len()));
            self.scanning = false;
            changed |= self.prune();
            let _ = index::save(cfg, &self.entries, self.scan_started.elapsed());
        }
        if let Some(rx) = &self.files_rx {
            let empty_query = self.empty_query();
            loop {
                match rx.try_recv() {
                    Ok(scan::Msg::Batch(batch)) => {
                        for mut e in batch {
                            rank::prepare(&mut e, &self.signals);
                            let idx = self.files.len();
                            if self.scope == Scope::Files
                                && let Some(s) = score(&mut self.ranker, Scope::Files, empty_query, &e)
                            {
                                self.results.push((s, idx));
                                self.unsorted = true;
                            }
                            self.files.push(e);
                        }
                    }
                    Ok(scan::Msg::Done(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                        crate::trace::mark(|| format!("files listed, {}", self.files.len()));
                        self.files_rx = None;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if let Some(rx) = &self.zoxide {
            match rx.try_recv() {
                Ok(scores) => {
                    crate::trace::mark(|| format!("zoxide answered, {} folders", scores.len()));
                    self.zoxide = None;
                    if !scores.is_empty() {
                        history::merge(&mut self.signals.history, scores);
                        for e in &mut self.entries {
                            rank::prepare(e, &self.signals);
                        }
                        changed = true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.zoxide = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if changed && self.scope != Scope::Files {
            self.refresh();
        }
        while let Some((path, pv)) = self.previewer.try_recv() {
            self.store_preview(path, pv);
        }
        while let Some((path, info)) = self.git.try_recv() {
            if self.git_pending.as_ref() == Some(&path) {
                self.git_pending = None;
            }
            self.git_info.insert(path, info);
        }
        while let Some((path, info)) = self.rows_git.try_recv() {
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
        if self.scope == Scope::Files {
            self.files.retain(|e| e.path != path);
        } else {
            let Some(i) = self.by_path.remove(path) else { return };
            self.entries.remove(i);
            self.seen.remove(i);
            self.by_path = self.entries.iter().enumerate().map(|(i, e)| (e.path.clone(), i)).collect();
        }
        self.refresh();
    }

    fn rescore_all(&mut self) {
        self.results.clear();
        let empty_query = self.empty_query();
        let list = if self.scope == Scope::Files { &self.files } else { &self.entries };
        for (i, e) in list.iter().enumerate() {
            if let Some(s) = score(&mut self.ranker, self.scope, empty_query, e) {
                self.results.push((s, i));
            }
        }
    }

    /// New query typed: rank again and jump back to the top.
    fn requery(&mut self) {
        let narrow = rank::narrows(&self.ranked_query, &self.query) && !self.query.trim_start().starts_with('@');
        self.ranker.set_query(&self.query);
        if narrow {
            let empty_query = self.empty_query();
            let (ranker, scope) = (&mut self.ranker, self.scope);
            let list = if scope == Scope::Files { &self.files } else { &self.entries };
            self.results.retain_mut(|r| match score(ranker, scope, empty_query, &list[r.1]) {
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
        let list = self.list();
        self.selected = keep
            .and_then(|p| self.results.iter().position(|r| list[r.1].path == p))
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
        self.results.get(self.selected).map(|r| &self.list()[r.1])
    }

    /// The folder an action works on: the highlighted folder, or the one holding the
    /// highlighted file.
    fn current_dir(&self) -> Option<PathBuf> {
        let e = self.current()?;
        if e.file { e.path.parent().map(Path::to_path_buf) } else { Some(e.path.clone()) }
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

    fn set_scope(&mut self, scope: Scope, cfg: &Config) {
        if scope == self.scope {
            return;
        }
        self.scope = scope;
        if scope == Scope::Files && self.files.is_empty() && self.files_rx.is_none() {
            self.list_files(cfg);
        }
        self.ranked_query.clear();
        self.requery();
    }

    fn step_scope(&mut self, dir: isize, cfg: &Config) {
        let i = SCOPES.iter().position(|(s, _)| *s == self.scope).unwrap_or(0) as isize;
        let next = SCOPES[(i + dir).rem_euclid(SCOPES.len() as isize) as usize].0;
        self.set_scope(next, cfg);
    }

    fn pick(&mut self) -> Option<Outcome> {
        let e = self.current()?;
        let (path, file) = (e.path.clone(), e.file);
        if file && path.is_file() {
            return path.parent().map(|p| Outcome::Picked(p.to_path_buf()));
        }
        if !file && path.is_dir() {
            return Some(Outcome::Picked(path));
        }
        self.set_status(format!("{} no longer exists", paths::contract(&path)));
        self.remove(&path);
        None
    }

    fn mouse(&mut self, m: MouseEvent, cfg: &Config) -> Option<Outcome> {
        if self.show_help {
            if let MouseEventKind::Down(_) = m.kind {
                self.show_help = false;
            }
            return None;
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.move_by(3),
            MouseEventKind::ScrollUp => self.move_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(&(_, _, _, scope)) =
                    self.tab_hits.iter().find(|(y, x0, x1, _)| m.row == *y && (*x0..=*x1).contains(&m.column))
                {
                    self.set_scope(scope, cfg);
                    return None;
                }
                let a = self.list_area;
                if m.row < a.y || m.row >= a.bottom() || m.column < a.x || m.column >= a.right() {
                    return None;
                }
                let row = self.scroll + (m.row - a.y) as usize;
                if row >= self.results.len() {
                    return None;
                }
                let double = self.last_click.is_some_and(|(at, r)| r == row && at.elapsed() < DOUBLE_CLICK);
                self.selected = row;
                if double {
                    self.last_click = None;
                    return self.pick();
                }
                self.last_click = Some((Instant::now(), row));
            }
            _ => {}
        }
        None
    }

    fn alias_key(&mut self, k: KeyEvent, cfg: &mut Config) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some((_, buf)) = self.alias_prompt.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.alias_prompt = None,
            KeyCode::Char('c' | 'g') if ctrl => self.alias_prompt = None,
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char('u') if ctrl => buf.clear(),
            KeyCode::Char(c) if !ctrl && (c.is_alphanumeric() || c == '-' || c == '_' || c == '.') => buf.push(c),
            KeyCode::Enter => {
                let (path, name) = self.alias_prompt.take().unwrap();
                self.save_alias(cfg, &path, name.trim());
            }
            _ => {}
        }
    }

    fn save_alias(&mut self, cfg: &mut Config, path: &Path, name: &str) {
        let target = paths::expand(&paths::contract(path));
        let before = cfg.aliases.clone();
        cfg.aliases.retain(|_, dir| paths::expand(dir) != target);
        let shown = paths::contract(path);
        if !name.is_empty() {
            cfg.aliases.insert(name.to_string(), shown.clone());
        }
        if let Err(e) = cfg.save() {
            cfg.aliases = before;
            self.set_status(format!("could not save the alias: {e}"));
            return;
        }
        self.signals.aliases = Signals::load_own().with_aliases(cfg).aliases;
        for e in &mut self.entries {
            rank::prepare(e, &self.signals);
        }
        self.refresh();
        self.set_status(if name.is_empty() {
            format!("removed the alias of {shown}")
        } else {
            format!("{} @{name} now jumps to {shown}", cfg.command)
        });
    }

    fn key(&mut self, k: KeyEvent, term: &mut Term, cfg: &mut Config) -> io::Result<Option<Outcome>> {
        if self.show_help {
            self.show_help = false;
            return Ok(None);
        }
        if self.alias_prompt.is_some() {
            self.alias_key(k, cfg);
            return Ok(None);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = self.list_height.max(1) as isize;
        let on_file = self.current().is_some_and(|e| e.file);
        match k.code {
            KeyCode::Esc => return Ok(Some(Outcome::Cancelled)),
            KeyCode::Char('c' | 'g' | 'q') if ctrl => return Ok(Some(Outcome::Cancelled)),
            KeyCode::Enter => return Ok(self.pick()),
            KeyCode::Right if ctrl || alt => self.step_scope(1, cfg),
            KeyCode::Left if ctrl || alt => self.step_scope(-1, cfg),
            KeyCode::Char('f') if ctrl => self.step_scope(1, cfg),
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
                if let Some(e) = self.current().filter(|e| !e.file) {
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
            KeyCode::Char('b') if ctrl => {
                if on_file {
                    self.set_status("only folders can be pinned");
                } else {
                    self.toggle_pin();
                }
            }
            KeyCode::Char('a') if ctrl => match self.current() {
                Some(e) if !e.file => {
                    let name = e.alias.clone().unwrap_or_default();
                    self.alias_prompt = Some((e.path.clone(), name));
                }
                Some(_) => self.set_status("aliases are for folders"),
                None => {}
            },
            KeyCode::Char('y') if ctrl => {
                if let Some(path) = self.current().map(|e| e.path.clone()) {
                    let shown = paths::contract(&path);
                    match actions::copy_path(&path) {
                        Ok(()) => self.set_status(format!("copied {shown}")),
                        Err(e) => self.set_status(format!("copy failed: {e}")),
                    }
                }
            }
            KeyCode::Char('t') if ctrl => {
                if let Some(path) = self.current_dir() {
                    match actions::new_tab(&path) {
                        Ok(what) => self.set_status(format!("opened a {what}")),
                        Err(e) => self.set_status(e),
                    }
                }
            }
            KeyCode::Char('o') if ctrl => {
                if let Some(path) = self.current_dir() {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let msg = match actions::open_in_file_manager(&path) {
                        Ok(()) => format!("opened {name}"),
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
                if cfg.compact {
                    tui::with_fullscreen(|t| settings::run(t, cfg, false))?;
                    term.clear()?;
                } else {
                    settings::run(term, cfg, false)?;
                }
                let rescan = before.priority_roots != cfg.priority_roots
                    || before.search_roots != cfg.search_roots
                    || before.exclude != cfg.exclude
                    || before.show_hidden != cfg.show_hidden
                    || before.respect_gitignore != cfg.respect_gitignore
                    || before.max_depth != cfg.max_depth;
                if !cfg.git_status {
                    self.git_info.clear();
                    self.rows_requested.clear();
                    self.git_pending = None;
                }
                self.git_enabled = cfg.git_status;
                if before.aliases != cfg.aliases {
                    self.signals.aliases = Signals::load_own().with_aliases(cfg).aliases;
                    for e in &mut self.entries {
                        rank::prepare(e, &self.signals);
                    }
                    self.refresh();
                }
                if rescan {
                    self.rescan(cfg, true);
                }
            }
            KeyCode::Char('r') if ctrl => {
                self.signals = Signals::load_own().with_aliases(cfg);
                self.zoxide = Some(spawn_zoxide());
                self.git_info.clear();
                self.rows_requested.clear();
                self.previews.clear();
                self.refresh();
                self.rescan(cfg, false);
                if self.scope == Scope::Files {
                    self.list_files(cfg);
                }
                self.set_status("rescanning");
            }
            KeyCode::Backspace => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(q);
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                let mut q = self.query.clone();
                q.push(c);
                self.set_query(q);
            }
            _ => {}
        }
        Ok(None)
    }

    /// Asks the worker for `path` and gives it a moment: a local folder answers well
    /// within that and shows up in this very frame, a slow one (network share, cloud
    /// folder) shows a placeholder instead of freezing the list.
    fn load_preview(&mut self, path: &Path) {
        if self.previews.contains_key(path) {
            return;
        }
        if self.preview_pending.as_deref() != Some(path) {
            self.previewer.request(path);
            self.preview_pending = Some(path.to_path_buf());
        }
        let deadline = Instant::now() + PREVIEW_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Some((p, pv)) = self.previewer.recv_timeout(left) else { break };
            let done = p == path;
            self.store_preview(p, pv);
            if done {
                break;
            }
        }
    }

    fn store_preview(&mut self, path: PathBuf, pv: Preview) {
        if self.preview_pending.as_ref() == Some(&path) {
            self.preview_pending = None;
        }
        if self.previews.len() >= 512 {
            self.previews.clear();
        }
        self.previews.insert(path, pv);
    }

    fn toggle_pin(&mut self) {
        let Some(path) = self.current().map(|e| e.path.clone()) else { return };
        let name = paths::contract(&path);
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
        let Some(e) = self.current() else { return Ok(None) };
        // A file opens itself, a folder opens as a project; either way the shell lands in
        // the folder afterwards.
        let (dir, target) = if e.file {
            let Some(dir) = e.path.parent() else { return Ok(None) };
            (dir.to_path_buf(), e.path.clone())
        } else {
            (e.path.clone(), e.path.clone())
        };
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
            let arg = if target == dir { PathBuf::from(".") } else { target };
            let _ = Command::new(program)
                .args(&args)
                .arg(arg)
                .current_dir(&dir)
                .stdin(input)
                .stdout(output.try_clone()?)
                .stderr(output)
                .status();
            return Ok(Some(Outcome::Picked(dir)));
        }
        let msg = match actions::spawn_detached(program, &args, &target) {
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

    fn draw_input(&mut self, f: &mut Frame, area: Rect) {
        let loading = self.scanning || (self.scope == Scope::Files && self.files_rx.is_some());
        let total = self.list().len();
        let counter = if loading {
            format!(" {} {}/{} ", SPINNER[self.frame % SPINNER.len()], self.results.len(), tui::thousands(total))
        } else {
            format!(" {}/{} ", self.results.len(), tui::thousands(total))
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(dim()))
            .title(Line::from(Span::styled(" dirhop ", Style::new().fg(accent()).bold())))
            .title(Line::from(Span::styled(counter, Style::new().fg(dim()))).right_aligned());
        let inner = block.inner(area);

        let (spans, cursor) = if let Some((path, buf)) = &self.alias_prompt {
            let label = format!("alias for {} › ", paths::contract(path));
            let cursor = label.chars().count() + buf.chars().count();
            let mut spans = vec![Span::styled(label, Style::new().fg(accent())), Span::raw(buf.clone())];
            spans.push(Span::styled("   enter saves · empty removes · esc cancels", Style::new().fg(dim())));
            (spans, cursor)
        } else {
            let mut spans = vec![Span::styled("› ", Style::new().fg(accent()).bold())];
            if self.query.is_empty() {
                let hint = match self.scope {
                    Scope::Files => "type to search files · enter goes to the file's folder",
                    _ => "type to search · @ for aliases · pinned and recent folders first",
                };
                spans.push(Span::styled(hint, Style::new().fg(dim())));
            } else {
                spans.push(Span::raw(self.query.clone()));
            }
            (spans, 2 + self.query.chars().count())
        };
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
        let x = inner.x + cursor as u16;
        f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));

        // Tabs sit in the bottom border of the search box.
        self.tab_hits.clear();
        let y = area.bottom().saturating_sub(1);
        let mut x = area.x + 2;
        let mut spans = Vec::new();
        for (scope, label) in SCOPES {
            let text = format!(" {label} ");
            let w = text.chars().count() as u16;
            if x + w + 1 >= area.right() {
                break;
            }
            let style = if *scope == self.scope {
                Style::new().fg(Color::Black).bg(accent()).bold()
            } else {
                Style::new().fg(dim())
            };
            self.tab_hits.push((y, x, x + w - 1, *scope));
            spans.push(Span::styled(text, style));
            spans.push(Span::styled("─", Style::new().fg(dim())));
            x += w + 1;
        }
        let tabs = Rect { x: area.x + 2, y, width: x.saturating_sub(area.x + 2), height: 1 };
        f.render_widget(Paragraph::new(Line::from(spans)), tabs);
    }

    fn empty_message(&self) -> &'static str {
        let aliases = self.query.trim_start().starts_with('@');
        match self.scope {
            _ if aliases && self.signals.aliases.is_empty() => {
                "  no aliases yet · ^a names the highlighted folder, then jump with hop @name"
            }
            _ if aliases => "  no alias matches",
            Scope::Files if self.files_rx.is_some() => "  listing files…",
            Scope::Files if self.files.is_empty() => "  no files found in the search folders",
            Scope::Files => "  no matching files",
            _ if self.scanning => "  scanning…",
            _ if self.entries.is_empty() => "  no folders found, check the search folders with ^s",
            Scope::Pinned if self.empty_query() => "  nothing pinned yet · ^b pins the highlighted folder",
            Scope::Recent if self.empty_query() => "  no jumps yet · folders you go to show up here",
            Scope::Repos if self.empty_query() => "  no git repos in the search folders",
            _ => "  no matching folders",
        }
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let h = area.height as usize;
        self.list_height = h;
        self.list_area = area;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + h {
            self.scroll = self.selected + 1 - h;
        }

        if self.results.is_empty() {
            f.render_widget(Paragraph::new(Span::styled(self.empty_message(), Style::new().fg(dim()))), area);
            return;
        }

        let overflow = self.results.len() > h;
        // Leave a gap column so the scrollbar doesn't merge with the preview's divider.
        let width = (area.width as usize).saturating_sub(if overflow { 2 } else { 0 });
        let mut lines = Vec::with_capacity(h);
        let mut visible_repos = Vec::new();
        let rows = self.scroll..(self.scroll + h).min(self.results.len());
        for row in rows {
            let idx = self.results[row].1;
            let e = if self.scope == Scope::Files { &self.files[idx] } else { &self.entries[idx] };
            let hl = self.ranker.highlights(e);
            let git = if e.repo { self.git_info.get(&e.path).and_then(Option::as_ref) } else { None };
            if e.repo {
                visible_repos.push(e.path.clone());
            }
            lines.push(row_line(e, &hl, row == self.selected, width, self.now, git));
        }
        f.render_widget(Paragraph::new(lines), area);

        // Status markers for the repos on screen, fetched in the background.
        visible_repos.retain(|p| !self.git_info.contains_key(p));
        if self.git_enabled && !visible_repos.is_empty() && visible_repos != self.rows_requested {
            self.rows_git.request(visible_repos.clone());
            self.rows_requested = visible_repos;
        }

        if overflow {
            let mut state = ScrollbarState::new(self.results.len().saturating_sub(h)).position(self.scroll);
            let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_symbol("▐")
                .thumb_style(Style::new().fg(dim()));
            let bar_area = Rect { width: area.width.saturating_sub(1), ..area };
            f.render_stateful_widget(bar, bar_area, &mut state);
        }
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::new().borders(Borders::LEFT).border_style(Style::new().fg(dim()));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(idx) = self.results.get(self.selected).map(|r| r.1) else { return };
        let path = self.list()[idx].path.clone();
        self.load_preview(&path);
        let e = &self.list()[idx];
        let pad = inner.inner(Margin::new(1, 0));
        let width = pad.width as usize;
        let Some(pv) = self.previews.get(&path) else {
            let lines = vec![
                Line::from(Span::styled(e.name().to_string(), Style::new().bold())),
                Line::from(Span::styled("…", Style::new().fg(dim()))),
            ];
            f.render_widget(Paragraph::new(lines), pad);
            return;
        };

        if e.file {
            let mut lines = vec![Line::from(Span::styled(e.name().to_string(), Style::new().bold()))];
            let folder = e.path.parent().map(paths::contract).unwrap_or_default();
            let size = pv.file_size.map(size_label).unwrap_or_default();
            lines.push(Line::from(Span::styled(
                truncate(&format!("{size} · in {folder}"), width),
                Style::new().fg(dim()),
            )));
            lines.push(Line::default());
            if let Some(err) = &pv.error {
                lines.push(Line::from(Span::styled(err.clone(), Style::new().fg(dim()))));
            }
            let room = (pad.height as usize).saturating_sub(lines.len());
            for l in pv.text.iter().take(room) {
                lines.push(Line::from(Span::raw(truncate(l, width))));
            }
            f.render_widget(Paragraph::new(lines), pad);
            return;
        }

        if self.git_enabled
            && pv.branch.is_some()
            && !self.git_info.contains_key(&path)
            && self.git_pending.as_ref() != Some(&path)
        {
            self.git.request(&path);
            self.git_pending = Some(path.clone());
        }
        let e = &self.list()[idx];
        let Some(pv) = self.previews.get(&path) else { return };

        let mut title = vec![Span::styled(e.name().to_string(), Style::new().bold())];
        if e.pinned {
            title.push(Span::styled("  ★ pinned", Style::new().fg(matched())));
        }
        if let Some(a) = &e.alias {
            title.push(Span::styled(format!("  @{a}"), Style::new().fg(accent())));
        }
        let mut lines = vec![Line::from(title)];

        if let Some(b) = &pv.branch {
            let mut spans = vec![
                Span::styled("git ", Style::new().fg(dim())),
                Span::styled(b.clone(), Style::new().fg(Color::Magenta)),
            ];
            match self.git_info.get(&path) {
                Some(Some(g)) => {
                    if g.ahead > 0 {
                        spans.push(Span::styled(format!("  ↑{}", g.ahead), Style::new().fg(accent())));
                    }
                    if g.behind > 0 {
                        spans.push(Span::styled(format!("  ↓{}", g.behind), Style::new().fg(accent())));
                    }
                    spans.push(if g.changed == 0 {
                        Span::styled("  ✓ clean", Style::new().fg(Color::Green))
                    } else {
                        Span::styled(format!("  ● {} changed", g.changed), Style::new().fg(matched()))
                    });
                }
                Some(None) => {}
                None if self.git_enabled => spans.push(Span::styled("  …", Style::new().fg(dim()))),
                None => {}
            }
            lines.push(Line::from(spans));
            if let Some(Some(GitInfo { last_commit: Some((hash, subject, time)), .. })) = self.git_info.get(&path) {
                let age = rank::age_label(self.now.saturating_sub(*time));
                let text = format!("{hash} {subject}");
                let room = width.saturating_sub(age.len() + 3);
                lines.push(Line::from(vec![
                    Span::styled(truncate(&text, room), Style::new().fg(dim())),
                    Span::styled(format!(" · {age}"), Style::new().fg(dim())),
                ]));
            }
        }
        let mut meta = Vec::new();
        if !pv.languages.is_empty() {
            meta.push(Span::styled(pv.languages.join(", "), Style::new().fg(Color::Green)));
            meta.push(Span::styled("  ·  ", Style::new().fg(dim())));
        }
        meta.push(Span::styled(
            format!("{} item{}", pv.total, if pv.total == 1 { "" } else { "s" }),
            Style::new().fg(dim()),
        ));
        lines.push(Line::from(meta));

        if let Some(text) = &pv.readme {
            lines.push(Line::default());
            let budget = (inner.height as usize / 4).clamp(1, 4);
            for l in wrap(text, width, budget) {
                lines.push(Line::from(Span::styled(l, Style::new().fg(dim()).add_modifier(Modifier::ITALIC))));
            }
        }
        lines.push(Line::default());
        if let Some(err) = &pv.error {
            lines.push(Line::from(Span::styled(err.clone(), Style::new().fg(Color::Red))));
        }
        let room = (inner.height as usize).saturating_sub(lines.len());
        for (i, (name, is_dir)) in pv.children.iter().enumerate() {
            if i + 1 == room && pv.children.len() > room {
                lines.push(Line::from(Span::styled(format!("  … {} more", pv.total - i), Style::new().fg(dim()))));
                break;
            }
            let hidden = name.starts_with('.');
            let line = if *is_dir {
                let style = if hidden { Style::new().fg(dim()) } else { Style::new().fg(Color::Blue) };
                Line::from(vec![Span::styled("▸ ", Style::new().fg(dim())), Span::styled(format!("{name}/"), style)])
            } else {
                let style = if hidden { Style::new().fg(dim()) } else { Style::new() };
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
            f.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), Style::new().fg(matched()))), area);
            return;
        }
        let on_file = self.current().is_some_and(|e| e.file);
        let mut hints = if self.alias_prompt.is_some() {
            vec![("enter", "save"), ("esc", "cancel"), ("^u", "clear")]
        } else if on_file {
            vec![("enter", "cd to its folder"), ("^e", "open file"), ("^y", "copy"), ("^o", "show")]
        } else {
            vec![("enter", "cd"), ("tab", "inside"), ("⇧tab", "up"), ("^b", "pin"), ("^a", "alias"), ("^y", "copy")]
        };
        if self.alias_prompt.is_none() {
            hints.push(("^f", "tabs"));
            if !on_file {
                hints.push(("^t", "new tab"));
                hints.push(("^o", "open"));
                if !cfg.editor.is_empty() {
                    hints.push(("^e", "editor"));
                }
            }
            hints.push(("^s", "settings"));
        }
        let help = if self.query.is_empty() { ("?", "help") } else { ("f1", "help") };
        let len = |(k, t): (&str, &str)| k.chars().count() + t.chars().count() + 3;

        let mut budget = (area.width as usize).saturating_sub(len(help) + 1);
        let mut spans = vec![Span::raw(" ")];
        for h in hints {
            if len(h) > budget {
                break;
            }
            budget -= len(h);
            spans.push(Span::styled(h.0, Style::new().fg(accent())));
            spans.push(Span::styled(format!(" {}  ", h.1), Style::new().fg(dim())));
        }
        spans.push(Span::styled(help.0, Style::new().fg(accent())));
        spans.push(Span::styled(format!(" {}", help.1), Style::new().fg(dim())));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn spawn_zoxide() -> Receiver<HashMap<PathBuf, f64>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(history::zoxide_scores());
    });
    rx
}

fn size_label(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}

fn draw_help(f: &mut Frame, cfg: &Config) {
    let keys: &[(&str, &str)] = &[
        ("enter", "cd into the folder (a file: into its folder)"),
        ("↑ ↓  ^p ^n", "move, or scroll and click with the mouse"),
        ("^f  ^← ^→", "switch tabs: all, repos, pinned, recent, files"),
        ("tab", "search inside the highlighted folder"),
        ("shift-tab", "go up a level"),
        ("^b", "pin or unpin (pinned folders stay on top)"),
        ("^a", "give the folder an alias for hop @name"),
        ("^y", "copy the path"),
        ("^t", "open in a new terminal tab"),
        ("^o", "open in the file manager"),
        ("^e", "open in your editor"),
        ("^s", "settings"),
        ("^r", "rescan"),
        ("^u  ^w", "clear the search / delete a word"),
        ("esc", "cancel"),
    ];
    let search: &[(&str, &str)] = &[
        ("'word", "exact match"),
        ("^word", "starts with"),
        ("word$", "ends with"),
        ("!word", "exclude"),
        ("@name", "aliases only"),
    ];
    let cmd = &cfg.command;
    let shell: &[(String, &str)] = &[
        (format!("{cmd} name"), "jump straight there if one folder clearly wins"),
        (format!("{cmd} @name"), "jump to an alias"),
        (format!("{cmd} -"), "back to the previous folder"),
        (format!("{cmd} .."), "paths work like cd"),
    ];

    let mut lines = vec![Line::default()];
    let row = |k: &str, t: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<12}"), Style::new().fg(accent())),
            Span::styled(t.to_string(), Style::new()),
        ])
    };
    let heading = |t: &'static str| Line::from(Span::styled(format!("  {t}"), Style::new().fg(dim()).bold()));
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
    let w = 70.min(area.width);
    let [popup] = Layout::vertical([Constraint::Length(h)]).flex(Flex::Center).areas(area);
    let [popup] = Layout::horizontal([Constraint::Length(w)]).flex(Flex::Center).areas(popup);
    f.buffer_mut().set_style(area, Style::new().fg(dim()).remove_modifier(Modifier::BOLD));
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(dim()))
        .title(Line::from(Span::styled(" help ", Style::new().fg(accent()).bold())))
        .title_bottom(Line::from(Span::styled(" any key closes ", Style::new().fg(dim()))).right_aligned());
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn row_line(e: &Entry, hl: &[usize], selected: bool, width: usize, now: u64, git: Option<&GitInfo>) -> Line<'static> {
    let base = if selected { Style::new().bg(selected_bg()) } else { Style::new() };
    let repo_color = match git {
        Some(g) if g.changed > 0 => matched(),
        Some(_) => Color::Green,
        None => accent(),
    };
    let mut spans = vec![
        Span::styled(if selected { "▌" } else { " " }, base.fg(accent())),
        if e.pinned {
            Span::styled("★ ", base.fg(matched()))
        } else if e.repo {
            Span::styled("◆ ", base.fg(repo_color))
        } else if e.priority && !e.file {
            Span::styled("· ", base.fg(dim()))
        } else {
            Span::styled("  ", base)
        },
    ];

    // Right-hand tags, most important first; they drop out when the row gets narrow.
    let mut tags: Vec<(String, Color)> = Vec::new();
    if let Some(g) = git.filter(|g| g.changed > 0) {
        tags.push((format!("●{}", g.changed), matched()));
    }
    if let Some(a) = &e.alias {
        tags.push((format!("@{a}"), accent()));
    }
    if let Some(t) = e.last_visit {
        tags.push((rank::age_label(now.saturating_sub(t)), dim()));
    }
    let name_len = e.name().chars().count() + 4;
    let mut right = 0;
    tags.retain(|(t, _)| {
        let w = t.chars().count() + 1;
        let keep = width >= 40 && right + w + name_len + 3 <= width;
        if keep {
            right += w;
        }
        keep
    });
    if right > 0 {
        right += 1;
    }

    let chars: Vec<char> = e.display.chars().collect();
    let avail = width.saturating_sub(3 + right);
    let skip = if chars.len() > avail { chars.len() - avail + 1 } else { 0 };
    if skip > 0 {
        spans.push(Span::styled("…", base.fg(dim())));
    }

    // Group runs of equally styled chars into one span.
    let mut run = String::new();
    let mut run_style = base;
    for (i, c) in chars.iter().enumerate().skip(skip) {
        let mut style = if i >= e.name_start { base.add_modifier(Modifier::BOLD) } else { base.fg(dim()) };
        if hl.binary_search(&i).is_ok() {
            style = base.fg(matched()).add_modifier(Modifier::BOLD);
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
    if !tags.is_empty() {
        if used + right <= width {
            spans.push(Span::styled(" ".repeat(width - used - right), base));
        }
        for (t, color) in tags {
            spans.push(Span::styled(format!(" {t}"), base.fg(color)));
        }
        spans.push(Span::styled(" ", base));
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
