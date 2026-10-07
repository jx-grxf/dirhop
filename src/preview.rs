//! Right-hand pane: git branch, detected languages and the folder's contents.

use std::fs;
use std::path::Path;

pub struct Preview {
    pub branch: Option<String>,
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
    Preview { branch: git_branch(path), languages: languages(&children), children, total, error }
}

/// Reads the branch straight from `.git/HEAD` instead of spawning git.
fn git_branch(path: &Path) -> Option<String> {
    let mut dir = Some(path);
    for _ in 0..12 {
        let d = dir?;
        let dot_git = d.join(".git");
        let git_dir = if dot_git.is_dir() {
            Some(dot_git)
        } else if dot_git.is_file() {
            // Worktrees and submodules: `.git` is a file pointing at the real dir.
            let body = fs::read_to_string(&dot_git).ok()?;
            body.strip_prefix("gitdir:").map(|p| d.join(p.trim()))
        } else {
            None
        };
        if let Some(g) = git_dir {
            let head = fs::read_to_string(g.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_string(),
                None => head.chars().take(7).collect(),
            });
        }
        dir = d.parent();
    }
    None
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
    fn detects_languages() {
        assert_eq!(languages(&kids(&["Cargo.toml", "src"])), vec!["Rust"]);
        assert_eq!(languages(&kids(&["package.json", "tsconfig.json"])), vec!["TypeScript"]);
        assert_eq!(languages(&kids(&["App.xcodeproj", "Dockerfile"])), vec!["Swift", "Docker"]);
        assert!(languages(&kids(&["notes.md"])).is_empty());
    }
}
