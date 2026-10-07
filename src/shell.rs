//! Shell integration: a function that `cd`s into the picked folder plus a key binding.
//! A child process can't change its parent's directory, so this part has to live in the shell.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::config::Config;
use crate::paths;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    pub fn detect() -> Option<Self> {
        let sh = env::var("SHELL").ok()?;
        Self::parse(sh.rsplit('/').next()?)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
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
        }
    }
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
        ("alt", _) => format!("\\e{key}"),
        _ => return None,
    })
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn exe() -> String {
    env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "dirhop".into())
}

pub fn init_script(shell: Shell, cfg: &Config) -> String {
    let exe = quote(&exe());
    let cmd = &cfg.command;
    let key = key_sequence(&cfg.keybinding, shell);
    let mut s = String::new();
    match shell {
        Shell::Zsh => {
            s.push_str(&format!(
                r#"# dirhop shell integration (zsh)
unalias {cmd} 2>/dev/null
{cmd}() {{
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
    }
    s
}

fn init_line(shell: Shell) -> String {
    let exe = quote(&exe());
    match shell {
        Shell::Zsh | Shell::Bash => format!("[ -x {exe} ] && eval \"$({exe} init {})\"", shell.name()),
        Shell::Fish => format!("test -x {exe}; and {exe} init fish | source"),
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
        assert!(key_sequence("none", Shell::Zsh).is_none());
    }

    #[test]
    fn script_uses_configured_name() {
        let cfg = Config { command: "p".into(), keybinding: "none".into(), ..Config::default() };
        let s = init_script(Shell::Zsh, &cfg);
        assert!(s.contains("\np() {"));
        assert!(!s.contains("bindkey"));
    }
}
