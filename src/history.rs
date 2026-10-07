//! Remembers which folders you jump to, so they float to the top next time.
//! Scores from zoxide are merged in when it is installed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths;

fn file() -> PathBuf {
    paths::data_dir().join("history.tsv")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

struct Record {
    count: f64,
    last: u64,
}

fn read() -> HashMap<PathBuf, Record> {
    let Ok(body) = fs::read_to_string(file()) else { return HashMap::new() };
    body.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let count = parts.next()?.parse().ok()?;
            let last = parts.next()?.parse().ok()?;
            let path = PathBuf::from(parts.next()?);
            Some((path, Record { count, last }))
        })
        .collect()
}

/// Same weighting zoxide uses: recent visits count for more.
fn frecency(r: &Record, now: u64) -> f64 {
    let age = now.saturating_sub(r.last);
    let weight = match age {
        a if a < 3600 => 4.0,
        a if a < 86_400 => 2.0,
        a if a < 604_800 => 0.5,
        _ => 0.25,
    };
    r.count * weight
}

pub fn scores() -> HashMap<PathBuf, f64> {
    let now = now();
    let mut out: HashMap<PathBuf, f64> = read().iter().map(|(p, r)| (p.clone(), frecency(r, now))).collect();
    if let Ok(o) = Command::new("zoxide").args(["query", "--list", "--score"]).output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let line = line.trim_start();
            if let Some((score, path)) = line.split_once(' ')
                && let Ok(score) = score.parse::<f64>()
            {
                *out.entry(PathBuf::from(path.trim_start())).or_default() += score;
            }
        }
    }
    out
}

pub fn record(path: &Path) {
    let mut all = read();
    let entry = all.entry(path.to_path_buf()).or_insert(Record { count: 0.0, last: 0 });
    entry.count = (entry.count + 1.0).min(10_000.0);
    entry.last = now();

    // Forget folders that no longer exist so the file stays small.
    all.retain(|p, _| p.is_dir());
    let mut body = String::new();
    for (p, r) in &all {
        body.push_str(&format!("{}\t{}\t{}\n", r.count, r.last, p.display()));
    }
    let f = file();
    if fs::create_dir_all(f.parent().unwrap()).is_ok() {
        let tmp = f.with_extension("tsv.tmp");
        if fs::write(&tmp, body).is_ok() {
            let _ = fs::rename(tmp, f);
        }
    }
}
