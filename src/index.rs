//! On-disk copy of the last scan. The picker shows it on the first frame and lets the
//! background scan add new folders and drop deleted ones; `hop name` and `dirhop query`
//! answer from it without scanning at all.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::history;
use crate::paths;
use crate::scan::{self, Entry};

const MAGIC: &str = "dirhop-index 1";
/// Older than this, a command that answered from the index refreshes it in the background.
const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

fn file() -> PathBuf {
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
}

pub fn load(cfg: &Config) -> Option<Index> {
    let body = fs::read_to_string(file()).ok()?;
    let mut lines = body.lines();
    let mut header = lines.next()?.strip_prefix(MAGIC)?.split_whitespace();
    if header.next()? != fingerprint(cfg) {
        return None;
    }
    let written: u64 = header.next()?.parse().ok()?;
    let entries = lines
        .filter_map(|line| {
            let (flags, path) = line.split_once('\t')?;
            let flags = flags.as_bytes();
            Some(Entry::with(PathBuf::from(path), flags.first() == Some(&b'p'), flags.get(1) == Some(&b'r')))
        })
        .collect();
    Some(Index { entries, age: Duration::from_secs(history::now().saturating_sub(written)) })
}

pub fn save(cfg: &Config, entries: &[Entry]) -> io::Result<()> {
    let mut body = format!("{MAGIC} {} {}\n", fingerprint(cfg), history::now());
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
        None => {
            let entries = scan::collect(cfg);
            let _ = save(cfg, &entries);
            entries
        }
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

/// `dirhop index`
pub fn run_cli(quiet: bool) -> u8 {
    let cfg = Config::load();
    let start = Instant::now();
    let entries = scan::collect(&cfg);
    match save(&cfg, &entries) {
        Ok(()) => {
            if !quiet {
                eprintln!("Indexed {} folders in {} ms.", entries.len(), start.elapsed().as_millis());
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
