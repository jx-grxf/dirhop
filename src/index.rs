//! On-disk copy of the last scan. The picker shows it on the first frame and lets the
//! background scan add new folders and drop deleted ones; `hop name` and `dirhop query`
//! answer from it without scanning at all.

use std::fs;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::history;
use crate::paths;
use crate::scan::{self, Entry};
use crate::tui;

const MAGIC: &str = "dirhop-index 1";
/// Older than this, a command that answered from the index refreshes it in the background.
const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

pub fn file() -> PathBuf {
    paths::data_dir().join("index.txt")
}

/// Changes whenever a setting that affects the scan changes, which invalidates the index.
fn fingerprint(cfg: &Config) -> String {
    let key = format!(
        "{:?}|{:?}|{:?}|{}|{}|{}|{}",
        cfg.priority_paths(),
        cfg.search_paths(),
        cfg.exclude,
        cfg.show_hidden,
        cfg.respect_gitignore,
        cfg.max_depth,
        paths::home().display()
    );
    Sha256::digest(key.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

pub struct Index {
    pub entries: Vec<Entry>,
    pub age: Duration,
    /// How long the scan that wrote it took.
    pub took: Option<Duration>,
    /// False when the index was written with different scan settings than the current ones.
    pub current: bool,
}

pub fn load(cfg: &Config) -> Option<Index> {
    read(cfg).filter(|i| i.current)
}

/// The index on disk, whether or not it matches the current settings.
pub fn read(cfg: &Config) -> Option<Index> {
    let body = fs::read_to_string(file()).ok()?;
    let mut lines = body.lines();
    let mut header = lines.next()?.strip_prefix(MAGIC)?.split_whitespace();
    let current = header.next()? == fingerprint(cfg);
    let written: u64 = header.next()?.parse().ok()?;
    let took = header.next().and_then(|ms| ms.parse().ok()).map(Duration::from_millis);
    let entries = lines
        .filter_map(|line| {
            let (flags, path) = line.split_once('\t')?;
            let flags = flags.as_bytes();
            Some(Entry::with(PathBuf::from(path), flags.first() == Some(&b'p'), flags.get(1) == Some(&b'r')))
        })
        .collect();
    Some(Index { entries, age: Duration::from_secs(history::now().saturating_sub(written)), took, current })
}

pub fn save(cfg: &Config, entries: &[Entry], took: Duration) -> io::Result<()> {
    let mut body = format!("{MAGIC} {} {} {}\n", fingerprint(cfg), history::now(), took.as_millis());
    body.reserve(entries.len() * 48);
    for e in entries {
        let Some(path) = e.path.to_str().filter(|p| !p.contains('\n')) else { continue };
        body.push(if e.priority { 'p' } else { '-' });
        body.push(if e.repo { 'r' } else { '-' });
        body.push('\t');
        body.push_str(path);
        body.push('\n');
    }
    let f = file();
    fs::create_dir_all(f.parent().unwrap())?;
    // A per-process temp name keeps two dirhops finishing at once from clobbering each other.
    let tmp = f.with_extension(format!("txt.{}", std::process::id()));
    fs::write(&tmp, body)?;
    fs::rename(tmp, f)
}

/// The index if there is one (refreshed in the background when it's old), otherwise a full scan.
pub fn load_or_scan(cfg: &Config) -> Vec<Entry> {
    match load(cfg) {
        Some(idx) => {
            if idx.age > STALE_AFTER {
                refresh_in_background();
            }
            idx.entries
        }
        None => match rebuild(cfg, |_| {}) {
            Ok((entries, _)) => entries,
            Err(_) => scan::collect(cfg),
        },
    }
}

/// Starts `dirhop index --quiet` detached. Its stdio is closed, so `$(dirhop …)` in the
/// shell doesn't wait for it.
fn refresh_in_background() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .args(["index", "--quiet"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Scans everything and writes the index. `progress` gets the running folder count.
pub fn rebuild(cfg: &Config, mut progress: impl FnMut(usize)) -> io::Result<(Vec<Entry>, Duration)> {
    let start = Instant::now();
    let (tx, rx) = mpsc::channel();
    scan::spawn(cfg, tx);
    let mut entries = Vec::new();
    for msg in rx {
        match msg {
            scan::Msg::Batch(batch) => {
                entries.extend(batch);
                progress(entries.len());
            }
            scan::Msg::Done => break,
        }
    }
    let took = start.elapsed();
    save(cfg, &entries, took)?;
    Ok((entries, took))
}

/// `dirhop index [now|show]`, also reachable as `dirhop --index …` and `hop --index …`.
pub fn run_cli(args: &[String]) -> u8 {
    let quiet = args.iter().any(|a| a == "--quiet" || a == "-q");
    match args.iter().find(|a| !a.starts_with('-')).map(String::as_str) {
        None | Some("now" | "rebuild") => rebuild_cli(quiet),
        Some("show" | "status") => crate::index_view::run(),
        Some(other) => {
            eprintln!("usage: dirhop index [now|show]  (unknown: {other})");
            2
        }
    }
}

fn rebuild_cli(quiet: bool) -> u8 {
    let cfg = Config::load();
    let live = !quiet && io::stderr().is_terminal();
    let mut frame = 0;
    let mut last = Instant::now();
    let result = rebuild(&cfg, |n| {
        if live && last.elapsed() > Duration::from_millis(60) {
            last = Instant::now();
            frame += 1;
            eprint!("\r\x1b[2K{} scanning… {}", tui::SPINNER[frame % tui::SPINNER.len()], tui::thousands(n));
        }
    });
    if live {
        eprint!("\r\x1b[2K");
    }
    match result {
        Ok((entries, took)) => {
            if !quiet {
                let repos = entries.iter().filter(|e| e.repo).count();
                eprintln!(
                    "Indexed {} folders ({} git repos) in {} ms → {}",
                    tui::thousands(entries.len()),
                    tui::thousands(repos),
                    took.as_millis(),
                    paths::contract(&file())
                );
            }
            0
        }
        Err(e) => {
            eprintln!("dirhop: cannot write {}: {e}", paths::contract(&file()));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_follows_scan_settings_only() {
        let a = Config::default();
        let b = Config { command: "p".into(), ..a.clone() };
        let c = Config { show_hidden: !a.show_hidden, ..a.clone() };
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }
}
