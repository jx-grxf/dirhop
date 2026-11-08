//! `hop name` without the picker: go straight to a folder when the answer is obvious.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::index;
use crate::rank::{self, Ranker, Signals};

/// What `hop <query>` should do before any UI shows up.
pub fn resolve(cfg: &Config, query: &str) -> Option<PathBuf> {
    if let Some(name) = query.trim().strip_prefix('@') {
        return alias(cfg, name);
    }
    as_path(query).or_else(|| if cfg.jump_directly { clear_winner(cfg, query) } else { None })
}

/// `hop @name`: the exact alias, or the only one starting with `name`. Anything else opens
/// the picker with the `@` search, which lists the aliases.
fn alias(cfg: &Config, name: &str) -> Option<PathBuf> {
    let name = name.trim().to_lowercase();
    let dir = cfg.aliases.iter().find(|(k, _)| k.to_lowercase() == name).map(|(_, v)| v).or_else(|| {
        let mut hits = cfg.aliases.iter().filter(|(k, _)| k.to_lowercase().starts_with(&name));
        let first = hits.next()?;
        hits.next().is_none().then_some(first.1)
    })?;
    let p = crate::paths::expand(dir);
    p.is_dir().then_some(p)
}

/// `hop ..`, `hop ~/x`, `hop ./src` and absolute paths are taken literally.
fn as_path(query: &str) -> Option<PathBuf> {
    let q = query.trim();
    let looks_like_path = q.starts_with(['.', '~', '/'])
        || q.contains('/')
        || (cfg!(windows) && (q.contains('\\') || q.as_bytes().get(1) == Some(&b':')));
    if !looks_like_path {
        return None;
    }
    let p = crate::paths::expand(q);
    let p = if p.is_relative() { std::env::current_dir().ok()?.join(p) } else { p };
    if !p.is_dir() {
        return None;
    }
    normalize(&p)
}

#[cfg(unix)]
fn normalize(p: &Path) -> Option<PathBuf> {
    p.canonicalize().ok()
}

/// `canonicalize` on Windows returns `\\?\C:\…`, which some shells print oddly.
#[cfg(windows)]
fn normalize(p: &Path) -> Option<PathBuf> {
    std::path::absolute(p).ok()
}

fn clear_winner(cfg: &Config, query: &str) -> Option<PathBuf> {
    let signals = Signals::load();
    let mut ranker = Ranker::new();
    ranker.set_query(query);
    let mut scored: Vec<(i64, PathBuf)> = index::load_or_scan(cfg)
        .into_iter()
        .filter_map(|mut e| {
            rank::prepare(&mut e, &signals);
            ranker.score(&e).map(|s| (s, e.path))
        })
        .collect();
    scored.sort_unstable_by_key(|s| std::cmp::Reverse(s.0));
    // The index can be a few minutes old; skip folders that are gone.
    let first = scored.iter().position(|(_, p)| p.is_dir())?;
    let best = scored[first].0;
    let runner_up = scored.get(first + 1).map(|s| s.0);
    decisive(best, runner_up).then(|| scored.swap_remove(first).1)
}

/// The best match has to lead by a clear margin, otherwise the picker opens. Measured on a
/// real home folder: an exact folder-name hit leads by 70-130, same-named folders tie at 0
/// and loose fuzzy hits differ by 10-40.
fn decisive(best: i64, runner_up: Option<i64>) -> bool {
    runner_up.is_none_or(|second| best - second >= 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margin() {
        assert!(decisive(300, None));
        assert!(decisive(682, Some(610)));
        assert!(!decisive(206, Some(174)));
        assert!(!decisive(322, Some(322)));
    }

    #[test]
    fn aliases_by_name_or_unique_prefix() {
        let home = crate::paths::home();
        let mut cfg = Config::default();
        cfg.aliases.insert("home".into(), "~".into());
        cfg.aliases.insert("hosting".into(), "~".into());
        cfg.aliases.insert("gone".into(), "~/definitely-not-here".into());
        assert_eq!(alias(&cfg, "home"), Some(home.clone()));
        assert_eq!(alias(&cfg, "HOS"), Some(home));
        assert_eq!(alias(&cfg, "ho"), None);
        assert_eq!(alias(&cfg, "gone"), None);
    }

    #[test]
    fn literal_paths() {
        assert!(as_path("..").is_some());
        assert!(as_path("~").is_some());
        assert!(as_path("weather").is_none());
        assert!(as_path("./definitely-not-here").is_none());
    }
}
