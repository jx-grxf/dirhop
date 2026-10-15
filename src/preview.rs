//! Right-hand pane: git branch and status, detected languages, the README's first lines
//! and the folder's contents.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

pub struct Preview {
    pub branch: Option<String>,
    pub readme: Option<String>,
    pub languages: Vec<&'static str>,
    /// (name, is_dir), directories first.
    pub children: Vec<(String, bool)>,
    pub total: usize,
    pub error: Option<String>,
}

const MAX_CHILDREN: usize = 400;

pub fn load(path: &Path) -> Preview {
    let mut children = Vec::new();
    let mut total = 0;
    let mut error = None;
    match fs::read_dir(path) {
        Ok(rd) => {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == ".DS_Store" {
                    continue;
                }
                total += 1;
                if children.len() < MAX_CHILDREN {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    children.push((name, is_dir));
                }
            }
        }
        Err(e) => error = Some(e.to_string()),
    }
    children.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    let readme = children.iter().find(|(n, d)| !d && is_readme(n)).and_then(|(n, _)| readme_excerpt(&path.join(n)));
    Preview { branch: git_branch(path), readme, languages: languages(&children), children, total, error }
}

fn is_readme(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(lower.as_str(), "readme" | "readme.md" | "readme.markdown" | "readme.txt" | "readme.rst")
}

fn readme_excerpt(path: &Path) -> Option<String> {
    let mut buf = Vec::new();
    fs::File::open(path).ok()?.take(8192).read_to_end(&mut buf).ok()?;
    excerpt(&String::from_utf8_lossy(&buf))
}

/// The first real paragraph of a README: skips headings, badges, HTML and code blocks,
/// and strips the most common inline Markdown.
pub fn excerpt(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let noise = t.starts_with(['#', '<', '!', '[', '|', '=', '-', '*', '_', '+', '`', ':', '.'])
            && !t.starts_with("**")
            && !t.starts_with("* ");
        if t.is_empty() || noise {
            if !out.is_empty() && (t.is_empty() || t.starts_with('#')) {
                break;
            }
            continue;
        }
        let t = t.trim_start_matches("> ").trim_start_matches("* ");
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&strip_inline(t));
        if out.len() > 400 {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `**bold**`, `` `code` `` and `[text](url)` -> plain text.
fn strip_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find("](").and_then(|close| after[close..].find(')').map(|end| (close, close + end))) {
            Some((close, end)) => {
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
}

pub struct GitInfo {
    pub ahead: u32,
    pub behind: u32,
    pub changed: u32,
    /// Short hash, subject and commit time of HEAD.
    pub last_commit: Option<(String, String, u64)>,
}

/// Runs `git status` off the UI thread. Requests that pile up while git is busy are
/// collapsed to the newest one, so scrolling fast never queues dozens of git processes.
pub struct GitWorker {
    tx: Sender<PathBuf>,
    rx: Receiver<(PathBuf, Option<GitInfo>)>,
}

impl GitWorker {
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(mut path) = req_rx.recv() {
                while let Ok(newer) = req_rx.try_recv() {
                    path = newer;
                }
                let info = git_info(&path);
                if res_tx.send((path, info)).is_err() {
                    break;
                }
            }
        });
        Self { tx: req_tx, rx: res_rx }
    }

    pub fn request(&self, path: &Path) {
        let _ = self.tx.send(path.to_path_buf());
    }

    pub fn try_recv(&self) -> Option<(PathBuf, Option<GitInfo>)> {
        self.rx.try_recv().ok()
    }
}

fn git(path: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_info(path: &Path) -> Option<GitInfo> {
    if !safe_to_run_git(path) {
        return None;
    }
    let status = git(path, &["status", "--porcelain=v2", "--branch", "--ignore-submodules=all"])?;
    let mut info = GitInfo { ahead: 0, behind: 0, changed: 0, last_commit: None };
    for line in status.lines() {
        if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut it = ab.split_whitespace();
            info.ahead = it.next().and_then(|a| a.trim_start_matches('+').parse().ok()).unwrap_or(0);
            info.behind = it.next().and_then(|b| b.trim_start_matches('-').parse().ok()).unwrap_or(0);
        } else if !line.starts_with('#') {
            info.changed += 1;
        }
    }
    info.last_commit = git(path, &["log", "-1", "--format=%h%x09%ct%x09%s"]).and_then(|l| {
        let mut it = l.trim_end().splitn(3, '\t');
        let hash = it.next()?.to_string();
        let time = it.next()?.parse().ok()?;
        Some((hash, it.next().unwrap_or_default().to_string(), time))
    });
    Some(info)
}

/// The repo's git dir for `path` or the nearest parent that has one.
fn git_dir(path: &Path) -> Option<PathBuf> {
    let mut dir = Some(path);
    for _ in 0..12 {
        let d = dir?;
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            // Worktrees and submodules: `.git` is a file pointing at the real dir.
            let body = fs::read_to_string(&dot_git).ok()?;
            return body.strip_prefix("gitdir:").map(|p| d.join(p.trim()));
        }
        dir = d.parent();
    }
    None
}

/// Reads the branch straight from `.git/HEAD` instead of spawning git.
fn git_branch(path: &Path) -> Option<String> {
    let head = fs::read_to_string(git_dir(path)?.join("HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => branch.to_string(),
        None => head.chars().take(7).collect(),
    })
}

/// `git status` honours the repo's own config, and a few keys there run programs: a
/// downloaded folder with a crafted `.git/config` could otherwise execute code as soon as
/// it is highlighted. fsmonitor is switched off on the command line and submodules are
/// skipped; filter drivers and includes can't be switched off, so such repos get no status.
/// Worktrees keep their config in the shared common dir plus `config.worktree`.
fn safe_to_run_git(path: &Path) -> bool {
    let Some(dir) = git_dir(path) else { return false };
    let mut configs = vec![dir.join("config"), dir.join("config.worktree")];
    if let Ok(common) = fs::read_to_string(dir.join("commondir")) {
        let common = dir.join(common.trim());
        configs.push(common.join("config"));
        configs.push(common.join("config.worktree"));
    }
    configs.iter().all(|f| match fs::read(f) {
        Ok(bytes) => {
            let compact: String = String::from_utf8_lossy(&bytes).to_ascii_lowercase().split_whitespace().collect();
            !["[filter", "[include"].iter().any(|k| compact.contains(k))
        }
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    })
}

pub fn languages(children: &[(String, bool)]) -> Vec<&'static str> {
    let has = |n: &str| children.iter().any(|(c, _)| c == n);
    let has_ext = |ext: &str| children.iter().any(|(c, _)| c.ends_with(ext));
    let mut out = Vec::new();
    let mut add = |cond: bool, lang: &'static str| {
        if cond && !out.contains(&lang) {
            out.push(lang);
        }
    };
    add(has("Cargo.toml"), "Rust");
    add(has("Package.swift") || has_ext(".xcodeproj") || has_ext(".xcworkspace"), "Swift");
    add(has("package.json") && (has("tsconfig.json") || has_ext(".ts")), "TypeScript");
    add(has("package.json") && !has("tsconfig.json") && !has_ext(".ts"), "JavaScript");
    add(has("deno.json") || has("deno.jsonc"), "Deno");
    add(has("go.mod"), "Go");
    add(
        has("pyproject.toml") || has("requirements.txt") || has("setup.py") || has("Pipfile") || has_ext(".py"),
        "Python",
    );
    add(has("build.gradle.kts") || has("settings.gradle.kts"), "Kotlin");
    add(has("pom.xml") || has("build.gradle") || has_ext(".java"), "Java");
    add(has_ext(".csproj") || has_ext(".sln"), "C#");
    add(has("CMakeLists.txt") || has_ext(".cpp") || has_ext(".hpp"), "C++");
    add(has_ext(".c") || has_ext(".h"), "C");
    add(has("Gemfile"), "Ruby");
    add(has("composer.json"), "PHP");
    add(has("pubspec.yaml"), "Dart");
    add(has("mix.exs"), "Elixir");
    add(has("build.zig"), "Zig");
    add(has_ext(".tf"), "Terraform");
    add(has("Dockerfile") || has("compose.yaml") || has("docker-compose.yml"), "Docker");
    add(has_ext(".ipynb"), "Jupyter");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kids(names: &[&str]) -> Vec<(String, bool)> {
        names.iter().map(|n| (n.to_string(), false)).collect()
    }

    #[test]
    fn readme_excerpt_skips_the_noise() {
        let md = "<div align=\"center\">\n<img src=\"x\">\n\n# dirhop\n\n[![CI](x)](y)\n\n\
                  **Jump to any folder** in a few keystrokes.\nUses [nucleo](https://x) and `ignore`.\n\n## Install\n";
        assert_eq!(excerpt(md).unwrap(), "Jump to any folder in a few keystrokes. Uses nucleo and ignore.");
        assert!(excerpt("# Title\n\n```sh\nmake\n```\n").is_none());
    }

    #[test]
    fn repos_with_command_running_config_are_skipped() {
        let root = std::env::temp_dir().join(format!("dirhop-gitcfg-{}", std::process::id()));
        let git = root.join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(git.join("config"), "[core]\n\tbare = false\n").unwrap();
        assert!(safe_to_run_git(&root));
        fs::write(git.join("config"), "[filter \"x\"]\n\tclean = touch /tmp/pwned\n").unwrap();
        assert!(!safe_to_run_git(&root));
        fs::write(git.join("config"), "[include]\n\tpath = ../evil\n").unwrap();
        assert!(!safe_to_run_git(&root));

        // A worktree whose shared config defines a filter.
        let common = root.join("common");
        fs::create_dir_all(&common).unwrap();
        fs::write(common.join("config"), "[Filter \"x\"]\n\tprocess = evil\n").unwrap();
        fs::write(git.join("config"), "[core]\n").unwrap();
        fs::write(git.join("commondir"), "../common\n").unwrap();
        assert!(!safe_to_run_git(&root));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn detects_languages() {
        assert_eq!(languages(&kids(&["Cargo.toml", "src"])), vec!["Rust"]);
        assert_eq!(languages(&kids(&["package.json", "tsconfig.json"])), vec!["TypeScript"]);
        assert_eq!(languages(&kids(&["App.xcodeproj", "Dockerfile"])), vec!["Swift", "Docker"]);
        assert!(languages(&kids(&["notes.md"])).is_empty());
    }
}
