use std::env;
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_*` wins everywhere; otherwise the platform's usual place.
fn base_dir(xdg: &str, windows_var: &str, unix_fallback: &str) -> PathBuf {
    let from_env = |var: &str| env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = from_env(xdg) {
        return dir;
    }
    if cfg!(windows)
        && let Some(dir) = from_env(windows_var)
    {
        return dir;
    }
    home().join(unix_fallback)
}

pub fn config_dir() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", "APPDATA", ".config").join("dirhop")
}

pub fn data_dir() -> PathBuf {
    base_dir("XDG_DATA_HOME", "LOCALAPPDATA", ".local/share").join("dirhop")
}

/// `~/foo` -> `/Users/me/foo`. Anything else is returned unchanged.
pub fn expand(raw: &str) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        home()
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\").filter(|_| cfg!(windows))) {
        home().join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// `/Users/me/foo` -> `~/foo`, for display and matching. Always uses `/`, also on Windows,
/// so typing a path into the search works the same everywhere.
pub fn contract(path: &Path) -> String {
    let home = home();
    let s = match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    };
    if cfg!(windows) { s.replace('\\', "/") } else { s }
}

/// Finds an executable by name on `$PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| executable_in(&dir, name))
}

#[cfg(unix)]
fn executable_in(dir: &Path, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).then_some(p)
}

#[cfg(windows)]
fn executable_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let exts = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let plain = dir.join(name);
    if Path::new(name).extension().is_some() && plain.is_file() {
        return Some(plain);
    }
    exts.split(';').filter(|e| !e.is_empty()).map(|ext| dir.join(format!("{name}{ext}"))).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_and_contract_roundtrip() {
        let p = expand("~/Projects/x");
        assert_eq!(p, home().join("Projects/x"));
        assert_eq!(contract(&p), "~/Projects/x");
        assert_eq!(contract(&home()), "~");
        assert_eq!(expand("/tmp"), PathBuf::from("/tmp"));
    }
}
