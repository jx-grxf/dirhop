use std::env;
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("dirhop")
}

pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("dirhop")
}

/// `~/foo` -> `/Users/me/foo`. Anything else is returned unchanged.
pub fn expand(raw: &str) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        home()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// `/Users/me/foo` -> `~/foo`, for display.
pub fn contract(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// Finds an executable by name on `$PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
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
