//! Shell integration: a function that `cd`s into the picked folder plus a key binding.
//! A child process can't change its parent's directory, so this part has to live in the shell.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use crate::config::Config;
use crate::paths;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
    Pwsh,
}

impl Shell {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            "powershell" | "pwsh" => Some(Self::Pwsh),
            _ => None,
        }
    }

    pub fn detect() -> Option<Self> {
        match env::var("SHELL") {
            Ok(sh) if !sh.is_empty() => {
                let name = sh.rsplit(['/', '\\']).next()?;
                Self::parse(name.strip_suffix(".exe").unwrap_or(name))
            }
            _ if cfg!(windows) => Some(Self::Pwsh),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
            Self::Pwsh => "powershell",
        }
    }

    pub fn rc_file(self) -> PathBuf {
        match self {
            Self::Zsh => env::var_os("ZDOTDIR").map(PathBuf::from).unwrap_or_else(paths::home).join(".zshrc"),
            Self::Bash => paths::home().join(".bashrc"),
            Self::Fish => {
                let cfg =
                    env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| paths::home().join(".config"));
                cfg.join("fish/conf.d/dirhop.fish")
            }
            Self::Pwsh => powershell_profile().clone(),
        }
    }
}

/// The profile every PowerShell host loads. Asking PowerShell is the only reliable way to
/// find it (OneDrive moves Documents), and it takes a moment, so ask once.
fn powershell_profile() -> &'static PathBuf {
    static PROFILE: OnceLock<PathBuf> = OnceLock::new();
    PROFILE.get_or_init(|| {
        let program = if paths::which("pwsh").is_some() { "pwsh" } else { "powershell" };
        Command::new(program)
            .args(["-NoLogo", "-NoProfile", "-Command", "$PROFILE.CurrentUserAllHosts"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let dir = if program == "pwsh" { "PowerShell" } else { "WindowsPowerShell" };
                paths::home().join("Documents").join(dir).join("profile.ps1")
            })
    })
}

/// Shortcut choices offered in the setup screen: (config value, label).
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("ctrl-g", "Ctrl-G"),
    ("alt-j", "Alt-J"),
    ("alt-g", "Alt-G"),
    ("ctrl-y", "Ctrl-Y"),
    ("ctrl-f", "Ctrl-F"),
    ("ctrl-t", "Ctrl-T"),
    ("none", "off"),
];

pub fn shortcut_label(value: &str) -> String {
    SHORTCUTS.iter().find(|(v, _)| *v == value).map(|(_, l)| l.to_string()).unwrap_or_else(|| value.to_string())
}

/// `ctrl-g` / `alt-j` -> the escape sequence each shell's bind syntax expects.
fn key_sequence(binding: &str, shell: Shell) -> Option<String> {
    let (modifier, key) = binding.split_once('-')?;
    let key = key.chars().next().filter(|c| c.is_ascii_alphabetic())?.to_ascii_lowercase();
    Some(match (modifier, shell) {
        ("ctrl", Shell::Zsh) => format!("^{}", key.to_ascii_uppercase()),
        ("ctrl", Shell::Bash) => format!("\\C-{key}"),
        ("ctrl", Shell::Fish) => format!("\\c{key}"),
        ("ctrl", Shell::Pwsh) => format!("Ctrl+{key}"),
        ("alt", Shell::Pwsh) => format!("Alt+{key}"),
        ("alt", _) => format!("\\e{key}"),
        _ => return None,
    })
}

fn quote(s: &str, shell: Shell) -> String {
    match shell {
        Shell::Pwsh => format!("'{}'", s.replace('\'', "''")),
        _ => format!("'{}'", s.replace('\'', "'\\''")),
    }
}

fn exe() -> String {
    let exe = env::current_exe();
    // canonicalize() on Windows yields a `\\?\` path that PowerShell's & can't always run.
    let exe = if cfg!(windows) { exe } else { exe.and_then(|p| p.canonicalize()) };
    exe.map(|p| stable_path(&p.to_string_lossy())).unwrap_or_else(|_| "dirhop".into())
}

/// Homebrew's Cellar path has the version in it and disappears on `brew upgrade`;
/// the link in the prefix's `bin` stays.
fn stable_path(exe: &str) -> String {
    match exe.split_once("/Cellar/dirhop/") {
        Some((prefix, _)) => format!("{prefix}/bin/dirhop"),
        None => exe.to_string(),
    }
}

pub fn init_script(shell: Shell, cfg: &Config) -> String {
    let exe = quote(&exe(), shell);
    let cmd = &cfg.command;
    let key = key_sequence(&cfg.keybinding, shell);
    let mut s = String::new();
    match shell {
        Shell::Zsh => {
            s.push_str(&format!(
                r#"# dirhop shell integration (zsh)
unalias {cmd} 2>/dev/null
{cmd}() {{
  if [[ $# -eq 1 && $1 == - ]]; then
    builtin cd -
    return
  fi
  local dir
  dir="$(command {exe} pick -- "$@")" || return
  [[ -n $dir ]] && builtin cd -- "$dir"
}}
"#
            ));
            if let Some(k) = key {
                s.push_str(&format!(
                    r#"_dirhop_widget() {{
  local dir
  dir="$(command {exe} pick </dev/tty)"
  if [[ -z $dir ]]; then
    zle redisplay
    return 0
  fi
  zle push-line
  BUFFER=" builtin cd -- ${{(q)dir}}"
  zle accept-line
  local ret=$?
  zle reset-prompt
  return $ret
}}
zle -N _dirhop_widget
bindkey -M emacs '{k}' _dirhop_widget
bindkey -M viins '{k}' _dirhop_widget
"#
                ));
            }
        }
        Shell::Bash => {
            s.push_str(&format!(
                r#"# dirhop shell integration (bash)
unalias {cmd} 2>/dev/null
{cmd}() {{
  if [ $# -eq 1 ] && [ "$1" = - ]; then
    builtin cd -
    return
  fi
  local dir
  dir="$(command {exe} pick -- "$@")" || return
  [ -n "$dir" ] && builtin cd -- "$dir"
}}
"#
            ));
            if let Some(k) = key {
                s.push_str(&format!(
                    r#"_dirhop_widget() {{
  local dir
  dir="$(command {exe} pick </dev/tty)" && [ -n "$dir" ] && builtin cd -- "$dir"
}}
[[ $- == *i* ]] && bind -x '"{k}": _dirhop_widget'
"#
                ));
            }
        }
        Shell::Fish => {
            s.push_str(&format!(
                r#"# dirhop shell integration (fish)
function {cmd} --description 'jump to a folder with dirhop'
    if test (count $argv) -eq 1; and test "$argv[1]" = -
        cd -
        return
    end
    set -l dir (command {exe} pick -- $argv); or return
    test -n "$dir"; and builtin cd -- $dir
end
"#
            ));
            if let Some(k) = key {
                s.push_str(&format!(
                    r#"function _dirhop_widget
    set -l dir (command {exe} pick </dev/tty)
    test -n "$dir"; and builtin cd -- $dir
    commandline -f repaint
end
bind {k} _dirhop_widget
bind -M insert {k} _dirhop_widget 2>/dev/null
"#
                ));
            }
        }
        Shell::Pwsh => {
            // Native output is decoded with the console code page; force UTF-8 so folder
            // names with umlauts survive the trip.
            s.push_str(&format!(
                r#"# dirhop shell integration (powershell)
Remove-Item -Force -ErrorAction SilentlyContinue Alias:{cmd}
function global:_dirhop_pick([string[]]$Query) {{
    $prev = [Console]::OutputEncoding
    [Console]::OutputEncoding = [Text.Encoding]::UTF8
    try {{ & {exe} pick @Query }} finally {{ [Console]::OutputEncoding = $prev }}
}}
function global:{cmd} {{
    if ($args.Count -eq 1 -and $args[0] -eq '-') {{ Set-Location -; return }}
    $dir = _dirhop_pick $args
    if ($LASTEXITCODE -eq 0 -and $dir) {{ Set-Location -LiteralPath $dir }}
}}
"#
            ));
            if let Some(k) = key {
                s.push_str(&format!(
                    r#"if (Get-Module PSReadLine) {{
    Set-PSReadLineKeyHandler -Chord '{k}' -BriefDescription 'dirhop' -ScriptBlock {{
        $dir = _dirhop_pick @()
        if ($LASTEXITCODE -eq 0 -and $dir) {{ Set-Location -LiteralPath $dir }}
        [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt()
    }}
}}
"#
                ));
            }
        }
    }
    s
}

fn init_line(shell: Shell) -> String {
    let exe = quote(&exe(), shell);
    match shell {
        Shell::Zsh | Shell::Bash => format!("[ -x {exe} ] && eval \"$({exe} init {})\"", shell.name()),
        Shell::Fish => format!("test -x {exe}; and {exe} init fish | source"),
        Shell::Pwsh => {
            format!("if (Test-Path {exe}) {{ Invoke-Expression ((& {exe} init powershell) -join \"`n\") }}")
        }
    }
}

pub fn is_installed(shell: Shell) -> bool {
    fs::read_to_string(shell.rc_file()).map(|s| s.contains(" init ") && s.contains("dirhop")).unwrap_or(false)
}

pub fn install(shell: Shell) -> io::Result<PathBuf> {
    let rc = shell.rc_file();
    if is_installed(shell) {
        return Ok(rc);
    }
    if let Some(dir) = rc.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut body = fs::read_to_string(&rc).unwrap_or_default();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&format!(
        "\n# dirhop: fuzzy folder jumper (https://github.com/jx-grxf/dirhop)\n{}\n",
        init_line(shell)
    ));
    fs::write(&rc, body)?;
    Ok(rc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_sequences() {
        assert_eq!(key_sequence("ctrl-g", Shell::Zsh).unwrap(), "^G");
        assert_eq!(key_sequence("ctrl-g", Shell::Bash).unwrap(), "\\C-g");
        assert_eq!(key_sequence("ctrl-g", Shell::Fish).unwrap(), "\\cg");
        assert_eq!(key_sequence("alt-j", Shell::Zsh).unwrap(), "\\ej");
        assert_eq!(key_sequence("ctrl-g", Shell::Pwsh).unwrap(), "Ctrl+g");
        assert_eq!(key_sequence("alt-j", Shell::Pwsh).unwrap(), "Alt+j");
        assert!(key_sequence("none", Shell::Zsh).is_none());
    }

    #[test]
    fn homebrew_path_survives_upgrades() {
        assert_eq!(stable_path("/opt/homebrew/Cellar/dirhop/0.2.0/bin/dirhop"), "/opt/homebrew/bin/dirhop");
        assert_eq!(stable_path("/Users/me/.local/bin/dirhop"), "/Users/me/.local/bin/dirhop");
    }

    #[test]
    fn powershell_script() {
        let cfg = Config { command: "j".into(), keybinding: "ctrl-g".into(), ..Config::default() };
        let s = init_script(Shell::Pwsh, &cfg);
        assert!(s.contains("function global:j {"));
        assert!(s.contains("-Chord 'Ctrl+g'"));
        assert_eq!(quote("C:\\it's", Shell::Pwsh), "'C:\\it''s'");
    }

    #[test]
    fn script_uses_configured_name() {
        let cfg = Config { command: "p".into(), keybinding: "none".into(), ..Config::default() };
        let s = init_script(Shell::Zsh, &cfg);
        assert!(s.contains("\np() {"));
        assert!(!s.contains("bindkey"));
    }
}
