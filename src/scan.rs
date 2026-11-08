//! Walks the configured roots in parallel and streams folders back in batches,
//! so the picker can show results while the scan is still running.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
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
    pub pinned: bool,
    /// Unix time of the last jump here through dirhop.
    pub last_visit: Option<u64>,
    /// Static ranking boost (priority root, git repo, history, pin); filled in by `rank::prepare`.
    pub bonus: i64,
    /// Sort key while the search is empty: pins, then recent folders, then the rest.
    pub idle: i64,
}

impl Entry {
    pub fn with(path: PathBuf, priority: bool, repo: bool) -> Self {
        let display = paths::contract(&path);
        let name_start = display.rfind('/').map(|i| display[..=i].chars().count()).unwrap_or(0);
        let depth = display.matches('/').count();
        Self { path, display, name_start, priority, repo, depth, pinned: false, last_visit: None, bonus: 0, idle: 0 }
    }

    pub fn name(&self) -> &str {
        self.display.rsplit('/').next().unwrap_or(&self.display)
    }
}

/// Folders arrive with `repo` unset; which of them are git repos is only known once the
/// walker has passed their `.git`, so the full set comes with `Done`.
pub enum Msg {
    Batch(Vec<Entry>),
    Done(HashSet<PathBuf>),
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
        let repos = Arc::new(Mutex::new(HashSet::new()));
        // Priority roots first, so the important folders show up before anything else.
        for root in &priority {
            walk(root, true, &cfg, &exclude, &[], &repos, &tx);
        }
        for root in &search {
            walk(root, false, &cfg, &exclude, &priority, &repos, &tx);
        }
        let repos = std::mem::take(&mut *repos.lock().unwrap());
        let _ = tx.send(Msg::Done(repos));
    });
}

/// Runs a full scan and waits for it.
pub fn collect(cfg: &Config) -> Vec<Entry> {
    let (tx, rx) = mpsc::channel();
    spawn(cfg, tx);
    let mut out = Vec::new();
    for msg in rx {
        match msg {
            Msg::Batch(batch) => out.extend(batch),
            Msg::Done(repos) => {
                mark_repos(&mut out, &repos);
                break;
            }
        }
    }
    out
}

pub fn mark_repos(entries: &mut [Entry], repos: &HashSet<PathBuf>) {
    for e in entries {
        e.repo = repos.contains(&e.path);
    }
}

#[cfg(windows)]
fn hidden(e: &ignore::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    // The walker keeps the attributes it got while listing the folder, so this is no extra call.
    e.file_name().to_string_lossy().starts_with('.')
        || e.metadata().is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
}

#[cfg(not(windows))]
fn hidden(e: &ignore::DirEntry) -> bool {
    e.file_name().as_encoded_bytes().starts_with(b".")
}

fn walk(
    root: &Path,
    priority: bool,
    cfg: &Config,
    exclude: &Arc<HashSet<String>>,
    skip: &[PathBuf],
    repos: &Arc<Mutex<HashSet<PathBuf>>>,
    tx: &Sender<Msg>,
) {
    let skip: Arc<Vec<PathBuf>> = Arc::new(skip.to_vec());
    let exclude = exclude.clone();
    let repos = repos.clone();
    let show_hidden = cfg.show_hidden;
    let mut builder = WalkBuilder::new(root);
    // Hidden entries are filtered below rather than by the walker, so `.git` folders still
    // pass by and mark their parent as a repo without a stat per folder. For the same reason
    // the walker isn't asked to check for `.git` itself (require_git): that is two more
    // lookups in every folder, and a .gitignore outside a repo is rare enough to just honour.
    builder
        .hidden(false)
        .require_git(false)
        .git_ignore(cfg.respect_gitignore)
        .git_global(cfg.respect_gitignore)
        .git_exclude(cfg.respect_gitignore)
        .ignore(cfg.respect_gitignore)
        .parents(false)
        .follow_links(false)
        .max_depth((cfg.max_depth > 0).then_some(cfg.max_depth))
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            // Worktrees and submodules have a `.git` file, so check before the is-dir test.
            if name == ".git" {
                if let Some(parent) = e.path().parent() {
                    repos.lock().unwrap().insert(parent.to_path_buf());
                }
                return false;
            }
            if !e.file_type().is_some_and(|t| t.is_dir()) {
                return false;
            }
            if (!show_hidden && hidden(e)) || exclude.contains(name.as_ref()) {
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
                batch.push(Entry::with(e.into_path(), priority, false));
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
