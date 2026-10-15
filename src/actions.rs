//! What the picker can do with a folder besides cd-ing into it.

use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::paths;

pub fn spawn_detached(program: &str, args: &[&str], path: &Path) -> io::Result<()> {
    Command::new(program)
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

pub fn open_in_file_manager(path: &Path) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    spawn_detached(opener, &[], path).map_err(|e| format!("{opener}: {e}"))
}

/// The terminal itself, for handing it to an editor while stdout stays captured by the shell.
pub fn console() -> io::Result<(File, File)> {
    if cfg!(windows) {
        Ok((File::options().read(true).write(true).open("CONIN$")?, File::options().write(true).open("CONOUT$")?))
    } else {
        let tty = File::options().read(true).write(true).open("/dev/tty")?;
        Ok((tty.try_clone()?, tty))
    }
}

/// Copies through the system clipboard tool and OSC 52 both, so it also works over SSH
/// and in terminals where no clipboard tool is installed.
pub fn copy_path(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    let _ = write!(io::stderr(), "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = io::stderr().flush();

    if cfg!(windows) {
        // clip.exe mangles non-ASCII paths; Set-Clipboard doesn't, it just starts slowly.
        return Command::new("powershell")
            .args(["-NoProfile", "-Command", "Set-Clipboard -Value $env:DIRHOP_CLIP"])
            .env("DIRHOP_CLIP", text.as_ref())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    let tool: Option<(&str, &[&str])> = if cfg!(target_os = "macos") {
        Some(("pbcopy", &[]))
    } else if env::var_os("WAYLAND_DISPLAY").is_some() && paths::which("wl-copy").is_some() {
        Some(("wl-copy", &[]))
    } else if paths::which("xclip").is_some() {
        Some(("xclip", &["-selection", "clipboard"]))
    } else if paths::which("xsel").is_some() {
        Some(("xsel", &["--clipboard", "--input"]))
    } else {
        None
    };
    let Some((program, args)) = tool else { return Ok(()) };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    child.stdin.take().unwrap().write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    child.wait().map(|_| ()).map_err(|e| e.to_string())
}

/// Opens a new tab (or tmux/zellij window) in `path`, in whatever terminal dirhop runs in.
/// Returns what it opened, for the status line.
pub fn new_tab(path: &Path) -> Result<&'static str, String> {
    let p = path.to_string_lossy();
    let has = |var: &str| env::var_os(var).is_some_and(|v| !v.is_empty());
    let (label, program, args): (&str, &str, Vec<&str>) = if has("TMUX") {
        ("tmux window", "tmux", vec!["new-window", "-c", &p])
    } else if has("ZELLIJ") {
        ("zellij tab", "zellij", vec!["action", "new-tab", "--cwd", &p])
    } else if has("WEZTERM_PANE") {
        ("WezTerm tab", "wezterm", vec!["cli", "spawn", "--cwd", &p])
    } else if has("KITTY_WINDOW_ID") {
        ("kitty tab", "kitty", vec!["@", "launch", "--type=tab", "--cwd", &p])
    } else if has("WT_SESSION") {
        ("Windows Terminal tab", "wt", vec!["-w", "0", "nt", "-d", &p])
    } else if cfg!(target_os = "macos") {
        let app = match env::var("TERM_PROGRAM").as_deref() {
            Ok("iTerm.app") => "iTerm",
            Ok("ghostty") => "Ghostty",
            _ => "Terminal",
        };
        ("terminal window", "open", vec!["-a", app, &p])
    } else {
        return Err("new tabs work in tmux, zellij, WezTerm, kitty and Windows Terminal".into());
    };
    let out = Command::new(program).args(&args).stdin(Stdio::null()).output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(label)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{program}: {}", err.lines().next().unwrap_or("failed")))
    }
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
