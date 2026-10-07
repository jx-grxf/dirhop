//! Walks the configured roots in parallel and streams folders back in batches,
//! so the picker can show results while the scan is still running.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;

use ignore::{WalkBuilder, WalkState};

use crate::config::Config;
use crate::paths;

pub struct Entry {
    pub path: PathBuf,
    /// `~`-contracted path, which is what gets matched and shown.
    pub display: String,
    /// Char offset where the folder name starts inside `display`.
    pub name_start: usize,
    pub priority: bool,
    pub repo: bool,
    pub depth: usize,
    /// Static ranking boost (priority root, git repo, history); filled in by the ranker.
    pub bonus: i64,
}

impl Entry {
    pub fn new(path: PathBuf, priority: bool) -> Self {
        let display = paths::contract(&path);
        let name_start = display.rfind('/').map(|i| display[..=i].chars().count()).unwrap_or(0);
        let depth = display.matches('/').count();
        let repo = path.join(".git").exists();
        Self { path, display, name_start, priority, repo, depth, bonus: 0 }
    }

    pub fn name(&self) -> &str {
        self.display.rsplit('/').next().unwrap_or(&self.display)
    }
}

pub enum Msg {
    Batch(Vec<Entry>),
    Done,
}

/// Directory suffixes that are really "files" on macOS and never worth jumping into.
const BUNDLE_SUFFIXES: &[&str] = &[
    ".app",
    ".framework",
    ".bundle",
    ".xcassets",
    ".lproj",
    ".photoslibrary",
    ".xcframework",
    ".imageset",
    ".appiconset",
    ".colorset",
    ".xcodeproj",
    ".xcworkspace",
    ".dSYM",
    ".xcdatamodeld",
    ".musiclibrary",
    ".tvlibrary",
    ".fcpbundle",
];

pub fn spawn(cfg: &Config, tx: Sender<Msg>) {
    let priority = cfg.priority_paths();
    let search = cfg.search_paths();
    let exclude: Arc<HashSet<String>> = Arc::new(cfg.exclude.iter().cloned().collect());
    let cfg = cfg.clone();
    thread::spawn(move || {
        // Priority roots first, so the important folders show up before anything else.
        for root in &priority {
            walk(root, true, &cfg, &exclude, &[], &tx);
        }
        for root in &search {
            walk(root, false, &cfg, &exclude, &priority, &tx);
        }
        let _ = tx.send(Msg::Done);
    });
}

fn walk(root: &Path, priority: bool, cfg: &Config, exclude: &Arc<HashSet<String>>, skip: &[PathBuf], tx: &Sender<Msg>) {
    let skip: Arc<Vec<PathBuf>> = Arc::new(skip.to_vec());
    let exclude = exclude.clone();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!cfg.show_hidden)
        .git_ignore(cfg.respect_gitignore)
        .git_global(cfg.respect_gitignore)
        .git_exclude(cfg.respect_gitignore)
        .ignore(cfg.respect_gitignore)
        .parents(false)
        .follow_links(false)
        .max_depth((cfg.max_depth > 0).then_some(cfg.max_depth))
        .filter_entry(move |e| {
            if !e.file_type().is_some_and(|t| t.is_dir()) {
                return false;
            }
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if name == ".git" || exclude.contains(name.as_ref()) {
                return false;
            }
            if BUNDLE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
                return false;
            }
            !skip.iter().any(|s| s == e.path())
        });

    builder.build_parallel().run(|| {
        let mut batch = Batch { buf: Vec::with_capacity(256), tx: tx.clone() };
        Box::new(move |res| {
            if let Ok(e) = res {
                batch.push(Entry::new(e.into_path(), priority));
            }
            WalkState::Continue
        })
    });
}

/// Collects entries per walker thread and sends them in chunks; flushes the rest on drop.
struct Batch {
    buf: Vec<Entry>,
    tx: Sender<Msg>,
}

impl Batch {
    fn push(&mut self, e: Entry) {
        self.buf.push(e);
        if self.buf.len() >= 256 {
            let _ = self.tx.send(Msg::Batch(std::mem::take(&mut self.buf)));
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let _ = self.tx.send(Msg::Batch(std::mem::take(&mut self.buf)));
        }
    }
}
