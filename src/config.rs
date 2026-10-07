use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Name of the shell function that opens the picker, e.g. `hop` or `p`.
    pub command: String,
    /// Key that opens the picker straight from the prompt, e.g. `ctrl-g`, `alt-j` or `none`.
    pub keybinding: String,
    /// Folders whose contents rank above everything else (your code, usually).
    pub priority_roots: Vec<String>,
    /// Folders that get searched in full.
    pub search_roots: Vec<String>,
    /// Folder names that are never entered.
    pub exclude: Vec<String>,
    pub show_hidden: bool,
    pub respect_gitignore: bool,
    /// Maximum depth below each root; 0 means unlimited.
    pub max_depth: usize,
    /// Command run by Ctrl-E; empty disables it.
    pub editor: String,
    pub auto_update: bool,
}

pub const PRIORITY_CANDIDATES: &[&str] = &[
    "~/Projects",
    "~/projects",
    "~/Developer",
    "~/dev",
    "~/code",
    "~/Code",
    "~/src",
    "~/repos",
    "~/git",
    "~/workspace",
    "~/XCode Projects",
];

impl Default for Config {
    fn default() -> Self {
        let priority_roots = unique_dirs(PRIORITY_CANDIDATES.iter().map(|p| p.to_string()));
        Self {
            command: "hop".into(),
            keybinding: "ctrl-g".into(),
            priority_roots,
            search_roots: vec!["~".into()],
            exclude: [
                "Library",
                "node_modules",
                "target",
                "DerivedData",
                "Pods",
                "Carthage",
                ".build",
                "build",
                "dist",
                "__pycache__",
                "venv",
                ".venv",
                "vendor",
                ".Trash",
                "Applications",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            show_hidden: false,
            respect_gitignore: true,
            max_depth: 0,
            editor: default_editor(),
            auto_update: true,
        }
    }
}

pub const EDITOR_CANDIDATES: &[&str] =
    &["code", "cursor", "zed", "idea", "subl", "fleet", "xed", "nvim", "vim", "hx", "nano", "micro", "emacs"];

fn default_editor() -> String {
    EDITOR_CANDIDATES.iter().find(|e| paths::which(e).is_some()).map(|e| e.to_string()).unwrap_or_default()
}

pub fn path() -> PathBuf {
    paths::config_dir().join("config.toml")
}

impl Config {
    pub fn exists() -> bool {
        path().is_file()
    }

    /// Loads the config, falling back to defaults for a missing or broken file.
    pub fn load() -> Self {
        fs::read_to_string(path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let p = path();
        fs::create_dir_all(p.parent().unwrap())?;
        let body = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = p.with_extension("toml.tmp");
        fs::write(&tmp, format!("# dirhop settings. Edit here or run `dirhop setup`.\n\n{body}"))?;
        fs::rename(tmp, p)
    }

    pub fn priority_paths(&self) -> Vec<PathBuf> {
        unique_dirs(self.priority_roots.iter().cloned()).iter().map(|r| paths::expand(r)).collect()
    }

    pub fn search_paths(&self) -> Vec<PathBuf> {
        unique_dirs(self.search_roots.iter().cloned()).iter().map(|r| paths::expand(r)).collect()
    }
}

/// Keeps existing folders only, once each. On case-insensitive file systems (macOS default)
/// `~/Projects` and `~/projects` are the same folder, so compare by inode instead of by name.
fn unique_dirs(roots: impl Iterator<Item = String>) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let mut seen = std::collections::HashSet::new();
    roots
        .filter(|r| match fs::metadata(paths::expand(r)) {
            Ok(m) if m.is_dir() => seen.insert((m.dev(), m.ino())),
            _ => false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip_and_partial_files() {
        let cfg = Config::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(toml::from_str::<Config>(&s).unwrap(), cfg);

        let partial: Config = toml::from_str("command = \"p\"").unwrap();
        assert_eq!(partial.command, "p");
        assert_eq!(partial.keybinding, "ctrl-g");
    }
}
