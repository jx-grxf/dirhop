//! Walks the configured roots in parallel and streams folders back in batches,
//! so the picker can show results while the scan is still running.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// Visited through dirhop or zoxide.
    pub visited: bool,
    /// Static ranking boost (priority root, git repo, history, pin); filled in by `rank::prepare`.
    pub bonus: i64,
    /// Sort key while the search is empty: pins, then recent folders, then the rest.
    pub idle: i64,
    /// Name saved for `hop @name`, filled in by `rank::prepare`.
    pub alias: Option<String>,
    /// A file from the files tab rather than a folder.
    pub file: bool,
}

impl Entry {
    pub fn with(path: PathBuf, priority: bool, repo: bool) -> Self {
        let display = paths::contract(&path);
        let name_start = display.rfind('/').map(|i| display[..=i].chars().count()).unwrap_or(0);
        let depth = display.matches('/').count();
        Self {
            path,
            display,
            name_start,
            priority,
            repo,
            depth,
            pinned: false,
            last_visit: None,
            visited: false,
            bonus: 0,
            idle: 0,
            alias: None,
            file: false,
        }
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

/// The files tab stops listing here; past this a search narrows things down better than
/// scrolling, and memory stays bounded in a large home folder.
pub const MAX_FILES: usize = 200_000;

pub fn spawn(cfg: &Config, tx: Sender<Msg>) {
    spawn_mode(cfg, tx, false);
}

/// Files instead of folders, for the files tab. Not kept in the index.
pub fn spawn_files(cfg: &Config, tx: Sender<Msg>) {
    spawn_mode(cfg, tx, true);
}

fn spawn_mode(cfg: &Config, tx: Sender<Msg>, files: bool) {
    let priority = cfg.priority_paths();
    let search = cfg.search_paths();
    let exclude: Arc<HashSet<String>> = Arc::new(cfg.exclude.iter().cloned().collect());
    let cfg = cfg.clone();
    thread::spawn(move || {
        let w = Walk {
            cfg: &cfg,
            exclude: &exclude,
            repos: Arc::new(Mutex::new(HashSet::new())),
            files,
            found: Arc::new(AtomicUsize::new(0)),
            tx: &tx,
        };
        // Priority roots first, so the important folders show up before anything else.
        for root in &priority {
            let skip: Vec<PathBuf> = priority.iter().filter(|p| *p != root).cloned().collect();
            w.run(root, true, &skip);
        }
        for root in &search {
            if priority.contains(root) {
                continue;
            }
            let skip: Vec<PathBuf> = priority.iter().chain(&search).filter(|p| *p != root).cloned().collect();
            w.run(root, false, &skip);
        }
        let repos = w.repos.clone();
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

fn home_cloud_dir(name: &str) -> bool {
    matches!(name, "OneDrive" | "iCloudDrive" | "Dropbox") || name.starts_with("OneDrive - ")
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

struct Walk<'a> {
    cfg: &'a Config,
    exclude: &'a Arc<HashSet<String>>,
    repos: Arc<Mutex<HashSet<PathBuf>>>,
    files: bool,
    found: Arc<AtomicUsize>,
    tx: &'a Sender<Msg>,
}

impl Walk<'_> {
    fn run(&self, root: &Path, priority: bool, skip: &[PathBuf]) {
        let skip: Arc<Vec<PathBuf>> = Arc::new(skip.to_vec());
        let exclude = self.exclude.clone();
        let repos = self.repos.clone();
        let files = self.files;
        let cfg = self.cfg;
        let show_hidden = cfg.show_hidden;
        let skip_cloud = !cfg.cloud_folders && root == paths::home();
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
                if !show_hidden && hidden(e) {
                    return false;
                }
                if !e.file_type().is_some_and(|t| t.is_dir()) {
                    return files;
                }
                if exclude.contains(name.as_ref()) || BUNDLE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
                    return false;
                }
                if skip_cloud && e.depth() == 1 && home_cloud_dir(&name) {
                    return false;
                }
                !skip.iter().any(|s| s == e.path())
            });

        builder.build_parallel().run(|| {
            let mut batch = Batch { buf: Vec::with_capacity(256), tx: self.tx.clone() };
            let found = self.found.clone();
            Box::new(move |res| {
                let Ok(e) = res else { return WalkState::Continue };
                let is_dir = e.file_type().is_some_and(|t| t.is_dir());
                if files && is_dir {
                    return WalkState::Continue;
                }
                if files && found.fetch_add(1, Ordering::Relaxed) >= MAX_FILES {
                    return WalkState::Quit;
                }
                let mut entry = Entry::with(e.into_path(), priority, false);
                entry.file = files;
                batch.push(entry);
                WalkState::Continue
            })
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_tree(tag: &str, dirs: &[&str]) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("dirhop-scan-{tag}-{}-{nanos}", std::process::id()));
        for d in dirs {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        root
    }

    #[test]
    fn search_root_under_excluded_folder_is_scanned() {
        let root = make_temp_tree("excl", &["Library/CloudStorage/OneDrive/Docs", "Library/Caches/pip", "Projects"]);
        let cloud = root.join("Library/CloudStorage");
        let cfg = Config {
            priority_roots: vec![],
            search_roots: vec![root.to_string_lossy().into(), cloud.to_string_lossy().into()],
            exclude: vec!["Library".into()],
            cloud_folders: false,
            ..Config::default()
        };

        let found: Vec<PathBuf> = collect(&cfg).into_iter().map(|e| e.path).collect();
        let _ = fs::remove_dir_all(&root);

        assert!(found.contains(&cloud.join("OneDrive")));
        assert!(found.contains(&cloud.join("OneDrive/Docs")));
        assert!(found.contains(&root.join("Projects")));
        assert!(!found.contains(&root.join("Library")));
        assert!(!found.contains(&root.join("Library/Caches")));
        assert!(!found.contains(&root.join("Library/Caches/pip")));
    }

    #[test]
    fn overlapping_search_roots_produce_no_duplicates() {
        let root = make_temp_tree("overlap", &["Library/CloudStorage/OneDrive/Docs", "Library/Caches"]);
        let cloud = root.join("Library/CloudStorage");
        let cfg = Config {
            priority_roots: vec![],
            search_roots: vec![root.to_string_lossy().into(), cloud.to_string_lossy().into()],
            exclude: vec![],
            cloud_folders: false,
            ..Config::default()
        };

        let found: Vec<PathBuf> = collect(&cfg).into_iter().map(|e| e.path).collect();
        let _ = fs::remove_dir_all(&root);

        let unique: HashSet<&PathBuf> = found.iter().collect();
        assert_eq!(found.len(), unique.len(), "expected no duplicate paths in {found:?}");
        assert!(unique.contains(&cloud.join("OneDrive")));
        assert!(unique.contains(&cloud.join("OneDrive/Docs")));
        assert!(unique.contains(&root.join("Library/Caches")));
    }

    #[test]
    fn home_cloud_dir_matches_common_cloud_roots() {
        assert!(home_cloud_dir("OneDrive"));
        assert!(home_cloud_dir("OneDrive - Contoso"));
        assert!(home_cloud_dir("iCloudDrive"));
        assert!(home_cloud_dir("Dropbox"));
        assert!(!home_cloud_dir("Projects"));
        assert!(!home_cloud_dir("OneDriveBackup"));
    }
}
