//! Scoring: fuzzy match quality plus a static boost for folders that matter more.

use std::collections::HashMap;
use std::path::PathBuf;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatchConfig, Matcher, Utf32Str};

use crate::scan::Entry;

pub fn bonus(e: &Entry, history: &HashMap<PathBuf, f64>) -> i64 {
    let mut b = 0;
    if e.priority {
        b += 40;
    }
    if e.repo {
        b += 60;
    }
    if let Some(score) = history.get(&e.path) {
        b += ((score.ln_1p() * 20.0) as i64).min(120);
    }
    b - (e.depth.min(15) as i64) * 4
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
            return Some(e.bonus);
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

    fn entry(rel: &str, priority: bool, repo: bool) -> Entry {
        let mut e = Entry::new(paths::home().join(rel), priority);
        e.repo = repo;
        e.bonus = bonus(&e, &HashMap::new());
        e
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
