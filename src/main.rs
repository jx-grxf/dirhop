mod actions;
mod config;
mod history;
mod index;
mod index_view;
mod jump;
mod paths;
mod picker;
mod pins;
mod preview;
mod rank;
mod scan;
mod settings;
mod shell;
mod trace;
mod tui;
mod update;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use config::Config;
use shell::Shell;

const HELP: &str = "\
dirhop: jump to any folder in a few keystrokes

USAGE
  dirhop [QUERY]          open the picker (prints the chosen folder)
  dirhop setup            settings: command name, shortcut, folders, editor, updates
  dirhop init <shell>     print shell integration for zsh, bash, fish or powershell
  dirhop query <QUERY>    print the best matches without a UI (-n N for more,
                          --scores to see the ranking)
  dirhop index [now]      rebuild the folder index now (also --index now)
  dirhop index show       what's in the index (also --index show)
  dirhop update           update to the latest release now
  dirhop --version

After `dirhop setup` your shell gets a command (default `hop`) and a shortcut
(default Ctrl-G) that cd into the folder you pick. `hop name` jumps straight
there when one folder clearly matches, `hop -` goes back, `hop ..` and other
paths work like cd. `hop --dev` runs the checkout set as dev_dir in the config.

PICKER KEYS
  enter cd          tab search inside   shift-tab up a level   ^b pin
  ^y copy path      ^t new tab          ^o open in file manager
  ^e open in editor ^s settings         ^r rescan              ? help
";

fn main() -> ExitCode {
    trace::start();
    #[cfg(windows)]
    keep_std_handles_private();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest =
        |from: usize| -> Vec<String> { args.iter().skip(from).filter(|a| a.as_str() != "--").cloned().collect() };
    let code = match args.first().map(String::as_str) {
        None => pick(&[]),
        Some("pick") => pick(&rest(1)),
        Some("setup" | "settings" | "config") => setup(),
        Some("init") => init(args.get(1).map(String::as_str)),
        Some("query") => query(&rest(1)),
        Some("index" | "--index") => index::run_cli(&args[1..]),
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

/// Windows hands inheritable handles to every child process, whatever its own stdio is set
/// to. PowerShell reads `$dir = dirhop pick` until the last holder of that pipe exits, so a
/// git status, the update check or the background index rebuild kept the jump waiting.
/// Children that should get our stdio still do: `Stdio::inherit` duplicates the handle.
#[cfg(windows)]
fn keep_std_handles_private() {
    type Handle = *mut std::ffi::c_void;
    const STD_INPUT: u32 = -10i32 as u32;
    const STD_OUTPUT: u32 = -11i32 as u32;
    const STD_ERROR: u32 = -12i32 as u32;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
    }
    for which in [STD_INPUT, STD_OUTPUT, STD_ERROR] {
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

fn pick(query: &[String]) -> u8 {
    let mut cfg = Config::load();
    trace::mark(|| "config loaded".into());
    if query.first().is_some_and(|a| a == "--dev") {
        return dev(&cfg, &query[1..]);
    }
    // `hop --index show`: nothing on stdout, so the shell function stays where it is.
    if query.first().is_some_and(|a| a == "--index") {
        return index::run_cli(&query[1..]);
    }
    let query = query.join(" ");
    if !query.trim().is_empty()
        && Config::exists()
        && let Some(dir) = jump::resolve(&cfg, &query)
    {
        history::record(&dir);
        trace::mark(|| "direct jump resolved".into());
        trace::print();
        println!("{}", dir.display());
        return 0;
    }
    if !io::stderr().is_terminal() {
        eprintln!("dirhop needs a terminal");
        return 2;
    }
    tui::install_panic_hook();
    let outcome = (|| -> io::Result<picker::Outcome> {
        let mut term = tui::enter()?;
        trace::mark(|| "terminal ready".into());
        if !Config::exists() {
            settings::run(&mut term, &mut cfg, true)?;
        }
        let updates = update::spawn_background(&cfg);
        picker::run(&mut term, &mut cfg, &query, updates)
    })();
    tui::leave();
    trace::print();
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

/// `hop --dev`: build and run the checkout in `dev_dir` with the same arguments. Its stdout
/// goes straight to the shell function, so picking a folder works exactly like the real thing.
fn dev(cfg: &Config, args: &[String]) -> u8 {
    let dir = std::env::var("DIRHOP_DEV_DIR").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| cfg.dev_dir.clone());
    let manifest = paths::expand(&dir).join("Cargo.toml");
    if dir.is_empty() || !manifest.is_file() {
        eprintln!(
            "dirhop: set dev_dir in {} (or DIRHOP_DEV_DIR) to your dirhop checkout",
            paths::contract(&config::path())
        );
        return 2;
    }
    let status = std::process::Command::new("cargo")
        .args(["run", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .args(["--", "pick", "--"])
        .args(args)
        .status();
    match status {
        Ok(s) => s.code().map_or(1, |c| c as u8),
        Err(e) => {
            eprintln!("dirhop: cargo: {e}");
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
        eprintln!("usage: dirhop init <zsh|bash|fish|powershell>");
        return 2;
    };
    print!("{}", shell::init_script(sh, &Config::load()));
    0
}

fn query(words: &[String]) -> u8 {
    let mut limit = 1;
    let mut show_scores = false;
    let mut terms = Vec::new();
    let mut it = words.iter();
    while let Some(w) = it.next() {
        if w == "-n" {
            limit = it.next().and_then(|n| n.parse().ok()).unwrap_or(10);
        } else if w == "--scores" {
            show_scores = true;
        } else {
            terms.push(w.as_str());
        }
    }
    let cfg = Config::load();
    let signals = rank::Signals::load();
    let mut ranker = rank::Ranker::new();
    ranker.set_query(&terms.join(" "));
    let mut hits: Vec<_> = index::load_or_scan(&cfg)
        .into_iter()
        .filter_map(|mut e| {
            rank::prepare(&mut e, &signals);
            ranker.score(&e).map(|s| (s, e.path))
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let mut printed = 0;
    for (score, p) in hits.into_iter().filter(|(_, p)| p.is_dir()).take(limit) {
        if show_scores {
            print!("{score:>6}  ");
        }
        println!("{}", p.display());
        printed += 1;
    }
    if printed == 0 { 1 } else { 0 }
}
