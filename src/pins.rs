//! Folders pinned with Ctrl-B. They lead the list when the search is empty
//! and get a large boost while searching.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths;

fn file() -> PathBuf {
    paths::data_dir().join("pins.txt")
}

pub fn load() -> HashSet<PathBuf> {
    fs::read_to_string(file())
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default()
}

/// Pins or unpins `path`; returns whether it is pinned afterwards.
pub fn toggle(pins: &mut HashSet<PathBuf>, path: &Path) -> io::Result<bool> {
    let pinned = if pins.remove(path) {
        false
    } else {
        pins.insert(path.to_path_buf());
        true
    };
    let mut sorted: Vec<_> = pins.iter().filter_map(|p| p.to_str()).collect();
    sorted.sort_unstable();
    let f = file();
    fs::create_dir_all(f.parent().unwrap())?;
    let tmp = f.with_extension("txt.tmp");
    fs::write(&tmp, sorted.iter().map(|p| format!("{p}\n")).collect::<String>())?;
    fs::rename(tmp, f)?;
    Ok(pinned)
}
