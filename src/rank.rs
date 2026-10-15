//! Scoring: fuzzy match quality plus a static boost for folders that matter more.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatchConfig, Matcher, Utf32Str};

use crate::history::{self, Visit};
use crate::pins;
use crate::scan::Entry;

/// Everything besides the query that moves a folder up: where you've been and what you pinned.
pub struct Signals {
    pub history: HashMap<PathBuf, Visit>,
    pub pins: HashSet<PathBuf>,
}

impl Signals {
    pub fn load() -> Self {
        Self { history: history::scores(), pins: pins::load() }
    }
}

/// Fills in the per-entry ranking fields from `signals`.
pub fn prepare(e: &mut Entry, signals: &Signals) {
    let visit = signals.history.get(&e.path);
    e.pinned = signals.pins.contains(&e.path);
    e.last_visit = visit.and_then(|v| v.last);

    let mut b = 0;
    if e.priority {
        b += 40;
    }
    if e.repo {
        b += 60;
    }
    if e.pinned {
        b += 150;
    }
    if let Some(v) = visit {
        b += ((v.score.ln_1p() * 20.0) as i64).min(120);
    }
    e.bonus = b - (e.depth.min(15) as i64) * 4;

    e.idle = if e.pinned {
        2_000_000 + e.bonus
    } else if let Some(v) = visit.filter(|v| v.score > 0.0) {
        1_000_000 + ((v.score * 100.0) as i64).min(900_000)
    } else {
        e.bonus
    };
}

/// True when every folder matching `new` also matches `old`, so only the previous results
/// need rescoring. Appending to a negation (`!foo` -> `!foob`) or after an anchor (`foo$`)
/// widens or changes the match, so those fall back to a full pass.
pub fn narrows(old: &str, new: &str) -> bool {
    new.len() > old.len() && new.starts_with(old) && !new.contains('!') && !old.ends_with(['$', '\\'])
}

pub fn age_label(secs: u64) -> String {
    match secs {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s if s < 14 * 86_400 => format!("{}d", s / 86_400),
        s if s < 60 * 86_400 => format!("{}w", s / (7 * 86_400)),
        s if s < 365 * 86_400 => format!("{}mo", s / (30 * 86_400)),
        s => format!("{}y", s / (365 * 86_400)),
    }
}

pub struct Ranker {
    matcher: Matcher,
    pattern: Pattern,
    query: String,
    buf: Vec<char>,
}

impl Ranker {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(MatchConfig::DEFAULT.match_paths()),
            pattern: Pattern::parse("", CaseMatching::Smart, Normalization::Smart),
            query: String::new(),
            buf: Vec::new(),
        }
    }

    pub fn set_query(&mut self, q: &str) {
        self.pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        self.query = q.trim().to_lowercase();
    }

    pub fn score(&mut self, e: &Entry) -> Option<i64> {
        if self.pattern.atoms.is_empty() {
            return Some(e.idle);
        }
        let full = self.pattern.score(Utf32Str::new(&e.display, &mut self.buf), &mut self.matcher)? as i64;
        let name = e.name();
        let on_name = self.pattern.score(Utf32Str::new(name, &mut self.buf), &mut self.matcher).map_or(0, |s| s as i64);
        let lower = name.to_lowercase();
        let exact = if lower == self.query {
            80
        } else if lower.starts_with(&self.query) {
            40
        } else {
            0
        };
        Some(full + on_name + exact + e.bonus)
    }

    /// Char positions in `display` to highlight. Prefers a match inside the folder name.
    pub fn highlights(&mut self, e: &Entry) -> Vec<usize> {
        if self.pattern.atoms.is_empty() {
            return Vec::new();
        }
        let mut idx = Vec::new();
        let on_name =
            self.pattern.indices(Utf32Str::new(e.name(), &mut self.buf), &mut self.matcher, &mut idx).is_some();
        let offset = if on_name {
            e.name_start
        } else {
            idx.clear();
            self.pattern.indices(Utf32Str::new(&e.display, &mut self.buf), &mut self.matcher, &mut idx);
            0
        };
        let mut out: Vec<usize> = idx.into_iter().map(|i| i as usize + offset).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;

    fn none() -> Signals {
        Signals { history: HashMap::new(), pins: HashSet::new() }
    }

    fn entry(rel: &str, priority: bool, repo: bool) -> Entry {
        let mut e = Entry::with(paths::home().join(rel), priority, repo);
        prepare(&mut e, &none());
        e
    }

    #[test]
    fn empty_query_puts_pins_then_recents_first() {
        let mut signals = none();
        let pinned = paths::home().join("Documents/notes");
        let visited = paths::home().join("Downloads/x");
        signals.pins.insert(pinned.clone());
        signals.history.insert(visited.clone(), Visit { score: 2.0, last: Some(0) });
        let mut r = Ranker::new();
        let mut score = |p: PathBuf, repo: bool| {
            let mut e = Entry::with(p, true, repo);
            prepare(&mut e, &signals);
            r.score(&e).unwrap()
        };
        let pin = score(pinned, false);
        let recent = score(visited, false);
        let repo = score(paths::home().join("Projects/app"), true);
        assert!(pin > recent && recent > repo);
    }

    #[test]
    fn narrowing_rules() {
        assert!(narrows("", "a"));
        assert!(narrows("we", "wea"));
        assert!(narrows("we", "we ap"));
        assert!(!narrows("wea", "we"));
        assert!(!narrows("!we", "!wea"));
        assert!(!narrows("we$", "we$a"));
        assert!(!narrows("we", "we"));
    }

    #[test]
    fn project_repo_beats_same_name_elsewhere() {
        let mut r = Ranker::new();
        r.set_query("oeffigo");
        let repo = entry("Projects/Private/OeffiGo", true, true);
        let docs = entry("Documents/OeffiGo", false, false);
        let sub = entry("Projects/Private/OeffiGo/ios/Sources", true, false);
        let site = entry("Projects/Private/oeffigo-website", true, true);
        let s = |e: &Entry, r: &mut Ranker| r.score(e).unwrap();
        assert!(s(&repo, &mut r) > s(&docs, &mut r));
        assert!(s(&repo, &mut r) > s(&sub, &mut r));
        assert!(s(&repo, &mut r) > s(&site, &mut r));
        assert!(s(&site, &mut r) > s(&sub, &mut r));

        // A repo starting with the query beats a deep, non-repo folder with the exact name.
        let deep = entry("Projects/Private/desktop/src/pages/oeffigo", true, false);
        assert!(s(&site, &mut r) > s(&deep, &mut r));
    }

    #[test]
    fn no_match_is_none() {
        let mut r = Ranker::new();
        r.set_query("zzzqqq");
        assert!(r.score(&entry("Documents", false, false)).is_none());
    }

    #[test]
    fn highlights_land_on_the_name() {
        let mut r = Ranker::new();
        r.set_query("oef");
        let e = entry("Projects/OeffiGo", true, true);
        let h = r.highlights(&e);
        assert_eq!(h, vec![e.name_start, e.name_start + 1, e.name_start + 2]);
    }
}
