mod config;
mod history;
mod paths;
mod picker;
mod preview;
mod rank;
mod scan;
mod settings;
mod shell;
mod tui;
mod update;

use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::sync::mpsc;

use config::Config;
use shell::Shell;

const HELP: &str = "\
dirhop: jump to any folder in a few keystrokes

USAGE
  dirhop [QUERY]          open the picker (prints the chosen folder)
  dirhop setup            settings: command name, shortcut, folders, editor, updates
  dirhop init <shell>     print shell integration for zsh, bash or fish
  dirhop query <QUERY>    print the best matches without a UI (-n N for more)
  dirhop update           update to the latest release now
  dirhop --version

After `dirhop setup` your shell gets a command (default `hop`) and a shortcut
(default Ctrl-G) that cd into the folder you pick.

PICKER KEYS
  type to search      enter cd    tab search inside    ^o open in Finder
  ^e open in editor   ^s settings ^r rescan             esc quit
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest =
        |from: usize| -> Vec<String> { args.iter().skip(from).filter(|a| a.as_str() != "--").cloned().collect() };
    let code = match args.first().map(String::as_str) {
        None => pick(&[]),
        Some("pick") => pick(&rest(1)),
        Some("setup" | "settings" | "config") => setup(),
        Some("init") => init(args.get(1).map(String::as_str)),
        Some("query") => query(&rest(1)),
        Some("update") => update::run_cli(),
        Some("-V" | "--version" | "version") => {
            println!("dirhop {}", update::VERSION);
            0
        }
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            0
        }
        Some(flag) if flag.starts_with('-') => {
            eprintln!("unknown option {flag}\n\n{HELP}");
            2
        }
        Some(_) => pick(&args),
    };
    ExitCode::from(code)
}

fn pick(query: &[String]) -> u8 {
    if !io::stderr().is_terminal() {
        eprintln!("dirhop needs a terminal");
        return 2;
    }
    let mut cfg = Config::load();
    tui::install_panic_hook();
    let outcome = (|| -> io::Result<picker::Outcome> {
        let mut term = tui::enter()?;
        if !Config::exists() {
            settings::run(&mut term, &mut cfg, true)?;
        }
        let updates = update::spawn_background(&cfg);
        picker::run(&mut term, &mut cfg, &query.join(" "), updates)
    })();
    tui::leave();
    match outcome {
        Ok(picker::Outcome::Picked(path)) => {
            history::record(&path);
            println!("{}", path.display());
            0
        }
        Ok(picker::Outcome::Cancelled) => 1,
        Err(e) => {
            eprintln!("dirhop: {e}");
            2
        }
    }
}

fn setup() -> u8 {
    let mut cfg = Config::load();
    let first = !Config::exists();
    tui::install_panic_hook();
    let saved = tui::enter().and_then(|mut term| settings::run(&mut term, &mut cfg, first));
    tui::leave();
    match saved {
        Ok(true) => {
            let key = shell::shortcut_label(&cfg.keybinding);
            eprintln!("Saved. Open a new terminal, then type `{}` or press {key}.", cfg.command);
            0
        }
        Ok(false) => 0,
        Err(e) => {
            eprintln!("dirhop: {e}");
            2
        }
    }
}

fn init(shell: Option<&str>) -> u8 {
    let Some(sh) = shell.and_then(Shell::parse).or_else(|| shell.is_none().then(Shell::detect).flatten()) else {
        eprintln!("usage: dirhop init <zsh|bash|fish>");
        return 2;
    };
    print!("{}", shell::init_script(sh, &Config::load()));
    0
}

fn query(words: &[String]) -> u8 {
    let mut limit = 1;
    let mut terms = Vec::new();
    let mut it = words.iter();
    while let Some(w) = it.next() {
        if w == "-n" {
            limit = it.next().and_then(|n| n.parse().ok()).unwrap_or(10);
        } else {
            terms.push(w.as_str());
        }
    }
    let cfg = Config::load();
    let history = history::scores();
    let (tx, rx) = mpsc::channel();
    scan::spawn(&cfg, tx);
    let mut ranker = rank::Ranker::new();
    ranker.set_query(&terms.join(" "));
    let mut hits = Vec::new();
    for msg in rx {
        match msg {
            scan::Msg::Batch(batch) => {
                for mut e in batch {
                    e.bonus = rank::bonus(&e, &history);
                    if let Some(s) = ranker.score(&e) {
                        hits.push((s, e.path));
                    }
                }
            }
            scan::Msg::Done => break,
        }
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    if hits.is_empty() {
        return 1;
    }
    for (_, p) in hits.into_iter().take(limit) {
        println!("{}", p.display());
    }
    0
}
