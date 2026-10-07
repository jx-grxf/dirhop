//! Self-update from GitHub releases. Uses the system `curl` and `tar` to keep the binary small;
//! every download is checked against the published SHA-256 before it replaces anything.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::paths;

pub const REPO: &str = "jx-grxf/dirhop";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const TARGET: &str = env!("DIRHOP_TARGET");
const CHECK_INTERVAL_SECS: u64 = 24 * 3600;

pub struct Release {
    pub version: String,
    archive_url: String,
    checksum_url: String,
}

fn asset_name() -> String {
    format!("dirhop-{TARGET}.tar.gz")
}

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "-H", "User-Agent: dirhop"])
        .args(args)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.trim().trim_start_matches("curl: ").to_string());
    }
    Ok(out.stdout)
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Returns the latest release if it is newer than this binary and has an asset for this platform.
pub fn latest() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = curl(&["--max-time", "8", "-H", "Accept: application/vnd.github+json", &url])?;
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("bad response: {e}"))?;
    let tag = json["tag_name"].as_str().ok_or("no release found")?;
    if !is_newer(tag, VERSION) {
        return Ok(None);
    }
    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    let find = |name: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let name = asset_name();
    let archive_url = find(&name).ok_or_else(|| format!("{tag} has no build for {TARGET}"))?;
    let checksum_url = find(&format!("{name}.sha256")).ok_or_else(|| format!("{tag} has no checksum"))?;
    Ok(Some(Release { version: tag.trim_start_matches('v').to_string(), archive_url, checksum_url }))
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())
}

/// Builds from a cargo target dir or a package manager shouldn't overwrite themselves.
fn managed_elsewhere(exe: &Path) -> Option<&'static str> {
    let s = exe.to_string_lossy();
    if s.contains("/target/debug/") || s.contains("/target/release/") {
        Some("this is a development build")
    } else if s.contains("/Cellar/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        Some("installed with Homebrew, run `brew upgrade dirhop`")
    } else if s.starts_with("/nix/") {
        Some("installed with Nix")
    } else {
        None
    }
}

pub fn install(rel: &Release) -> Result<(), String> {
    let exe = current_exe()?;
    if let Some(why) = managed_elsewhere(&exe) {
        return Err(why.to_string());
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("dirhop-update-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = install_into(rel, &exe, &tmp);
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn install_into(rel: &Release, exe: &Path, tmp: &Path) -> Result<(), String> {
    let archive = curl(&["--max-time", "120", &rel.archive_url])?;
    let expected = String::from_utf8(curl(&["--max-time", "20", &rel.checksum_url])?).map_err(|e| e.to_string())?;
    let expected = expected.split_whitespace().next().unwrap_or_default().to_lowercase();
    let actual: String = Sha256::digest(&archive).iter().map(|b| format!("{b:02x}")).collect();
    if expected.len() != 64 || expected != actual {
        return Err("checksum mismatch, update skipped".into());
    }

    let archive_path = tmp.join("dirhop.tar.gz");
    fs::write(&archive_path, &archive).map_err(|e| e.to_string())?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive_path)
        .arg("-C")
        .arg(tmp)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    let fresh = tmp.join("dirhop");
    if !status.success() || !fresh.is_file() {
        return Err("archive did not contain a dirhop binary".into());
    }

    // Stage next to the target and rename: atomic, and a running dirhop keeps its old inode.
    let dir = exe.parent().ok_or("binary has no parent dir")?;
    let staged = dir.join(".dirhop.new");
    fs::copy(&fresh, &staged).map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    fs::rename(&staged, exe).map_err(|e| {
        let _ = fs::remove_file(&staged);
        e.to_string()
    })
}

fn stamp_file() -> PathBuf {
    paths::data_dir().join("last-update-check")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn due() -> bool {
    let last = fs::read_to_string(stamp_file()).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    now().saturating_sub(last) >= CHECK_INTERVAL_SECS
}

fn mark_checked() {
    let f = stamp_file();
    if fs::create_dir_all(f.parent().unwrap()).is_ok() {
        let _ = fs::write(f, now().to_string());
    }
}

/// Checks (at most once a day) in the background; the receiver gets a line for the status bar.
pub fn spawn_background(cfg: &Config) -> Option<Receiver<String>> {
    if !cfg.auto_update || !due() || std::env::var_os("DIRHOP_NO_UPDATE").is_some() {
        return None;
    }
    if current_exe().ok().as_deref().and_then(managed_elsewhere).is_some() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Ok(found) = latest() {
            mark_checked();
            if let Some(rel) = found {
                let msg = match install(&rel) {
                    Ok(()) => format!("updated to v{}, active next launch", rel.version),
                    Err(e) => format!("v{} available ({e})", rel.version),
                };
                let _ = tx.send(msg);
            }
        }
    });
    Some(rx)
}

/// `dirhop update`
pub fn run_cli() -> u8 {
    eprintln!("dirhop v{VERSION} ({TARGET}), checking {REPO}…");
    match latest() {
        Ok(None) => {
            mark_checked();
            eprintln!("Already up to date.");
            0
        }
        Ok(Some(rel)) => {
            mark_checked();
            eprintln!("Installing v{}…", rel.version);
            match install(&rel) {
                Ok(()) => {
                    eprintln!("Updated to v{}.", rel.version);
                    0
                }
                Err(e) => {
                    eprintln!("Update failed: {e}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("Update check failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }
}
