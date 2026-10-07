//! Setup / settings screen. Opened by `dirhop setup`, on first launch and with Ctrl-S in the picker.

use std::io;
use std::time::Duration;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::config::{Config, EDITOR_CANDIDATES};
use crate::paths;
use crate::shell::{self, SHORTCUTS, Shell};
use crate::tui::{ACCENT, DIM, MATCH, SELECTED_BG, Term};
use crate::update;

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Command,
    Shortcut,
    Priority,
    Search,
    Exclude,
    Hidden,
    Gitignore,
    Editor,
    AutoUpdate,
    ShellIntegration,
    CheckUpdate,
    Save,
}

const FIELDS: &[Field] = &[
    Field::Command,
    Field::Shortcut,
    Field::Priority,
    Field::Search,
    Field::Exclude,
    Field::Hidden,
    Field::Gitignore,
    Field::Editor,
    Field::AutoUpdate,
    Field::ShellIntegration,
    Field::CheckUpdate,
    Field::Save,
];

struct Screen {
    draft: Config,
    original: Config,
    welcome: bool,
    selected: usize,
    editing: Option<String>,
    message: Option<(String, bool)>,
    confirm_discard: bool,
    shell: Option<Shell>,
    shell_installed: bool,
    /// Whether saving should add the integration line to the rc file.
    want_shell: bool,
}

/// Returns true when the user saved.
pub fn run(term: &mut Term, cfg: &mut Config, welcome: bool) -> io::Result<bool> {
    let shell = Shell::detect();
    let shell_installed = shell.is_some_and(shell::is_installed);
    let mut s = Screen {
        draft: cfg.clone(),
        original: cfg.clone(),
        welcome,
        selected: 0,
        editing: None,
        message: None,
        confirm_discard: false,
        shell,
        shell_installed,
        want_shell: !shell_installed && shell.is_some(),
    };

    loop {
        term.draw(|f| s.draw(f))?;
        if !event::poll(Duration::from_millis(500))? {
            continue;
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);

        if let Some(buf) = s.editing.as_mut() {
            match k.code {
                KeyCode::Esc => s.editing = None,
                KeyCode::Enter => {
                    let value = s.editing.take().unwrap();
                    s.commit(value);
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char('u') if ctrl => buf.clear(),
                KeyCode::Char(c) if !ctrl => buf.push(c),
                _ => {}
            }
            continue;
        }

        let field = FIELDS[s.selected];
        match k.code {
            KeyCode::Char('c') if ctrl => return Ok(false),
            KeyCode::Char('s') if ctrl => {
                if s.save()? {
                    *cfg = s.draft.clone();
                    return Ok(true);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                if s.draft == s.original || s.confirm_discard {
                    return Ok(false);
                }
                s.confirm_discard = true;
                s.message = Some(("Unsaved changes. Ctrl-S saves, Esc again discards.".into(), true));
                continue;
            }
            KeyCode::Up | KeyCode::Char('k') => s.selected = s.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => s.selected = (s.selected + 1).min(FIELDS.len() - 1),
            KeyCode::Left | KeyCode::Char('h') => s.cycle(field, -1),
            KeyCode::Right | KeyCode::Char('l') => s.cycle(field, 1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if field == Field::Save {
                    if s.save()? {
                        *cfg = s.draft.clone();
                        return Ok(true);
                    }
                } else {
                    s.activate(field);
                }
            }
            _ => {}
        }
        s.confirm_discard = false;
    }
}

impl Screen {
    fn value(&self, f: Field) -> String {
        let c = &self.draft;
        let check = |b: bool| if b { "[x]".to_string() } else { "[ ]".to_string() };
        match f {
            Field::Command => c.command.clone(),
            Field::Shortcut => format!("‹ {} ›", shell::shortcut_label(&c.keybinding)),
            Field::Priority => c.priority_roots.join(", "),
            Field::Search => c.search_roots.join(", "),
            Field::Exclude => c.exclude.join(", "),
            Field::Hidden => check(c.show_hidden),
            Field::Gitignore => check(c.respect_gitignore),
            Field::Editor => format!("‹ {} ›", if c.editor.is_empty() { "off" } else { &c.editor }),
            Field::AutoUpdate => check(c.auto_update),
            Field::ShellIntegration => match self.shell {
                None => "unknown shell, see README".into(),
                Some(sh) if self.shell_installed => format!("✓ installed in {}", paths::contract(&sh.rc_file())),
                Some(sh) => format!("{} add to {}", check(self.want_shell), paths::contract(&sh.rc_file())),
            },
            Field::CheckUpdate => format!("v{}  (enter to check now)", update::VERSION),
            Field::Save => String::new(),
        }
    }

    fn label(f: Field) -> &'static str {
        match f {
            Field::Command => "Command",
            Field::Shortcut => "Shortcut",
            Field::Priority => "Priority folders",
            Field::Search => "Search folders",
            Field::Exclude => "Skip folder names",
            Field::Hidden => "Show hidden folders",
            Field::Gitignore => "Respect .gitignore",
            Field::Editor => "Editor (Ctrl-E)",
            Field::AutoUpdate => "Automatic updates",
            Field::ShellIntegration => "Shell integration",
            Field::CheckUpdate => "Version",
            Field::Save => "Save",
        }
    }

    fn hint(&self, f: Field) -> String {
        let cmd = &self.draft.command;
        match f {
            Field::Command => {
                format!("Type `{cmd}` to open the picker, `{cmd} name` to start with a search. Enter to edit.")
            }
            Field::Shortcut => {
                let mut h = "Opens the picker from any prompt. ←/→ to change.".to_string();
                if self.draft.keybinding.starts_with("alt-") && cfg!(target_os = "macos") {
                    h.push_str(" Needs “Option as Meta” in your terminal settings.");
                }
                if self.draft.keybinding == "ctrl-f" {
                    h.push_str(" Replaces forward-char (and autosuggest accept).");
                }
                if self.draft.keybinding == "ctrl-t" {
                    h.push_str(" Replaces fzf's file picker if you use it.");
                }
                h
            }
            Field::Priority => "These rank above everything else. Comma separated, ~ works. Enter to edit.".into(),
            Field::Search => "Everything below these is searchable. Comma separated. Enter to edit.".into(),
            Field::Exclude => "Folders with these names are never entered. Enter to edit.".into(),
            Field::Hidden => "Include dot-folders like ~/.config.".into(),
            Field::Gitignore => "Skip folders your repos ignore (build output, caches).".into(),
            Field::Editor => {
                "Ctrl-E opens the highlighted folder with this. ←/→ picks an installed one, Enter types your own."
                    .into()
            }
            Field::AutoUpdate => format!("Checks GitHub once a day and installs new releases of {}.", update::REPO),
            Field::ShellIntegration => {
                "Needed so the shell can cd into the folder. Takes effect in new terminals.".into()
            }
            Field::CheckUpdate => "Look for a newer release right now.".into(),
            Field::Save => "Save settings (Ctrl-S works anywhere).".into(),
        }
    }

    fn activate(&mut self, f: Field) {
        let c = &mut self.draft;
        match f {
            Field::Command => self.editing = Some(c.command.clone()),
            Field::Priority => self.editing = Some(c.priority_roots.join(", ")),
            Field::Search => self.editing = Some(c.search_roots.join(", ")),
            Field::Exclude => self.editing = Some(c.exclude.join(", ")),
            Field::Editor => self.editing = Some(c.editor.clone()),
            Field::Shortcut => self.cycle(f, 1),
            Field::Hidden => c.show_hidden = !c.show_hidden,
            Field::Gitignore => c.respect_gitignore = !c.respect_gitignore,
            Field::AutoUpdate => c.auto_update = !c.auto_update,
            Field::ShellIntegration => {
                if !self.shell_installed && self.shell.is_some() {
                    self.want_shell = !self.want_shell;
                }
            }
            Field::CheckUpdate => self.check_update(),
            Field::Save => {}
        }
    }

    fn cycle(&mut self, f: Field, dir: isize) {
        let step = |len: usize, cur: Option<usize>| -> usize {
            let cur = cur.unwrap_or(0) as isize;
            (cur + dir).rem_euclid(len as isize) as usize
        };
        match f {
            Field::Shortcut => {
                let cur = SHORTCUTS.iter().position(|(v, _)| *v == self.draft.keybinding);
                self.draft.keybinding = SHORTCUTS[step(SHORTCUTS.len(), cur)].0.to_string();
            }
            Field::Editor => {
                let mut options: Vec<String> =
                    EDITOR_CANDIDATES.iter().filter(|e| paths::which(e).is_some()).map(|e| e.to_string()).collect();
                if !self.draft.editor.is_empty() && !options.contains(&self.draft.editor) {
                    options.push(self.draft.editor.clone());
                }
                options.push(String::new());
                let cur = options.iter().position(|o| *o == self.draft.editor);
                self.draft.editor = options[step(options.len(), cur)].clone();
            }
            Field::Hidden | Field::Gitignore | Field::AutoUpdate | Field::ShellIntegration => self.activate(f),
            _ => {}
        }
    }

    fn commit(&mut self, value: String) {
        let list = |v: &str| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>();
        let field = FIELDS[self.selected];
        match field {
            Field::Command => {
                let v = value.trim();
                let valid = v.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                if !valid {
                    self.message = Some(("Use letters, digits, - and _ only.".into(), true));
                    return;
                }
                self.draft.command = v.to_string();
                self.message =
                    paths::which(v).map(|p| (format!("Heads up: this hides {} in new shells.", p.display()), true));
            }
            Field::Priority => self.draft.priority_roots = list(&value),
            Field::Search => {
                let roots = list(&value);
                if roots.is_empty() {
                    self.message = Some(("At least one search folder is needed.".into(), true));
                    return;
                }
                self.draft.search_roots = roots;
            }
            Field::Exclude => self.draft.exclude = list(&value),
            Field::Editor => self.draft.editor = value.trim().to_string(),
            _ => {}
        }
        if let Some(missing) = self.missing_folder() {
            self.message = Some((format!("{missing} does not exist (kept anyway)."), true));
        }
    }

    fn missing_folder(&self) -> Option<String> {
        self.draft.priority_roots.iter().chain(&self.draft.search_roots).find(|r| !paths::expand(r).is_dir()).cloned()
    }

    fn check_update(&mut self) {
        self.message = Some(match update::latest() {
            Ok(None) => (format!("You're on the latest version (v{}).", update::VERSION), false),
            Ok(Some(rel)) => match update::install(&rel) {
                Ok(()) => (format!("Updated to v{}. Restart dirhop to use it.", rel.version), false),
                Err(e) => (format!("v{} is available, but installing failed: {e}", rel.version), true),
            },
            Err(e) => (format!("Update check failed: {e}"), true),
        });
    }

    fn save(&mut self) -> io::Result<bool> {
        self.draft.save()?;
        let mut msg = format!("Saved to {}.", paths::contract(&crate::config::path()));
        if let Some(sh) = self.shell
            && self.want_shell
            && !self.shell_installed
        {
            match shell::install(sh) {
                Ok(rc) => {
                    self.shell_installed = true;
                    msg.push_str(&format!(" Added to {}.", paths::contract(&rc)));
                }
                Err(e) => {
                    self.message = Some((format!("Saved, but writing the shell file failed: {e}"), true));
                    return Ok(false);
                }
            }
        }
        self.original = self.draft.clone();
        self.message = Some((msg, false));
        Ok(true)
    }

    fn draw(&self, f: &mut Frame) {
        let area = f.area();
        let title = if self.welcome { " welcome to dirhop · quick setup " } else { " dirhop settings " };
        let block = Block::bordered()
            .border_style(Style::new().fg(DIM))
            .title(Line::from(Span::styled(title, Style::new().fg(ACCENT).bold())));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let [intro, list, help, foot] = Layout::vertical([
            Constraint::Length(if self.welcome { 3 } else { 1 }),
            Constraint::Length(FIELDS.len() as u16 + 2),
            Constraint::Min(2),
            Constraint::Length(1),
        ])
        .areas(inner.inner(Margin::new(1, 0)));

        if self.welcome {
            let text = vec![
                Line::from("Pick a command name and a shortcut, then save. Everything can be changed later with"),
                Line::from(vec![
                    Span::styled("dirhop setup", Style::new().fg(ACCENT)),
                    Span::raw(" or Ctrl-S inside the picker."),
                ]),
            ];
            f.render_widget(Paragraph::new(text), intro);
        }

        let label_w = 22;
        let mut lines = Vec::new();
        for (i, field) in FIELDS.iter().enumerate() {
            let selected = i == self.selected;
            let base = if selected { Style::new().bg(SELECTED_BG) } else { Style::new() };
            if *field == Field::Save {
                lines.push(Line::default());
                let style =
                    if selected { base.fg(Color::Black).bg(ACCENT).bold() } else { Style::new().fg(ACCENT).bold() };
                lines.push(Line::from(Span::styled("  [ Save ]  ", style)));
                continue;
            }
            let marker = Span::styled(if selected { "▌ " } else { "  " }, base.fg(ACCENT));
            let label = Span::styled(format!("{:<label_w$}", Self::label(*field)), base.add_modifier(Modifier::BOLD));
            let value = match (&self.editing, selected) {
                (Some(buf), true) => Span::styled(format!("{buf}▏"), base.fg(MATCH)),
                _ => Span::styled(self.value(*field), base),
            };
            let mut spans = vec![marker, label, value];
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            let width = list.width as usize;
            if selected && used < width {
                spans.push(Span::styled(" ".repeat(width - used), base));
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), list);

        let mut help_lines = vec![Line::from(Span::styled(self.hint(FIELDS[self.selected]), Style::new().fg(DIM)))];
        if let Some((msg, err)) = &self.message {
            help_lines.push(Line::default());
            let color = if *err { Color::Red } else { Color::Green };
            help_lines.push(Line::from(Span::styled(msg.clone(), Style::new().fg(color))));
        }
        f.render_widget(Paragraph::new(help_lines).wrap(Wrap { trim: true }), help);

        let key = |k: &'static str| Span::styled(k, Style::new().fg(ACCENT));
        let txt = |t: &'static str| Span::styled(t, Style::new().fg(DIM));
        let footer = if self.editing.is_some() {
            Line::from(vec![key("enter"), txt(" apply  "), key("esc"), txt(" cancel  "), key("^u"), txt(" clear")])
        } else {
            Line::from(vec![
                key("↑↓"),
                txt(" move  "),
                key("enter"),
                txt(" edit/toggle  "),
                key("←→"),
                txt(" change  "),
                key("^s"),
                txt(" save  "),
                key("esc"),
                txt(" back"),
            ])
        };
        f.render_widget(Paragraph::new(footer), foot);
    }
}
