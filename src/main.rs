mod capture;
mod config;
mod daemon;
mod event;
mod hypr;
mod inject;
mod keymap;
mod matcher;
mod tui;

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

const APP_ID: &str = "org.omarchy.hyprspanso";

/// The single starter match file shipped with every install (3 examples).
const EXAMPLES: &str = include_str!("../examples/matches.yml");

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    match command {
        "daemon" => run_daemon(&args),
        "check" => config::check(),
        "init" => run_init(&args),
        "list" => run_list(&args),
        "search" => tui::run(flag_value(&args, "--target")),
        "launcher" => run_launcher(),
        "reload" => ipc_command("RELOAD", false),
        "toggle" => ipc_command("TOGGLE", true),
        "status" => ipc_command("STATUS", true),
        "help" | "-h" | "--help" => {
            usage();
            Ok(())
        }
        other => {
            eprintln!("hyprspanso: unknown command `{other}`\n");
            usage();
            std::process::exit(2);
        }
    }
}

fn run_daemon(args: &[String]) -> Result<()> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    daemon::run(dry_run)
}

fn run_init(args: &[String]) -> Result<()> {
    let force = args.iter().any(|a| a == "--force");
    let root = config::config_root();
    let match_dir = config::match_dir(&root);
    let target = match_dir.join("matches.yml");

    let existing = config::load(&root);
    if !force && !existing.files.is_empty() {
        println!(
            "{} already contains {} match file(s); leaving them alone:",
            match_dir.display(),
            existing.files.len()
        );
        for file in &existing.files {
            println!("  {}", file.display());
        }
        println!();
        println!("Run `hyprspanso init --force` to write the examples anyway");
        println!("(that overwrites {}).", target.display());
        return Ok(());
    }

    std::fs::create_dir_all(&match_dir)?;
    std::fs::write(&target, EXAMPLES)?;
    println!("Wrote 3 starter examples to {}", target.display());
    println!("Edit the file, then `hyprspanso reload` (or wait 2 seconds).");
    Ok(())
}

fn run_list(args: &[String]) -> Result<()> {
    let filter = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .map(|s| s.to_lowercase());

    let root = config::config_root();
    let set = config::load(&root);
    println!("{} (from {})", set.summary(), root.display());

    for entry in &set.entries {
        if let Some(filter) = &filter {
            let haystack = format!("{} {}", entry.trigger, entry.replace).to_lowercase();
            if !haystack.contains(filter) {
                continue;
            }
        }
        let rendered = matcher::render(entry);
        let preview = rendered.lines().next().unwrap_or("");
        let preview: String = preview.chars().take(70).collect();
        println!("{:<24} {}", entry.trigger, preview);
    }
    if !set.skipped.is_empty() {
        println!("\n{} skipped (vars/forms/images), see `hyprspanso check`", set.skipped.len());
    }
    Ok(())
}

fn run_launcher() -> Result<()> {
    let target = hypr::active_window()
        .map(|(_, _, address)| address)
        .filter(|address| !address.is_empty());

    let exe = std::env::current_exe()
        .context("cannot resolve own binary path")?
        .to_string_lossy()
        .to_string();

    let mut command: Vec<String> = vec![exe, "search".into()];
    if let Some(target) = target {
        command.push("--target".into());
        command.push(target);
    }

    // Omarchy ships a helper that opens a styled floating terminal for TUIs.
    // Do not wait: the helper execs into the terminal, which lives as long as
    // the search window is open.
    if which("omarchy-launch-tui") {
        let spawned = Command::new("omarchy-launch-tui")
            .arg(format!("--app-id={APP_ID}"))
            .args(&command)
            .stdin(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return Ok(());
        }
    }

    Command::new("xdg-terminal-exec")
        .arg(format!("--app-id={APP_ID}"))
        .arg("-e")
        .args(&command)
        .stdin(Stdio::null())
        .spawn()
        .context("cannot find a terminal launcher (omarchy-launch-tui / xdg-terminal-exec)")?;

    Ok(())
}

fn ipc_command(command: &str, read_reply: bool) -> Result<()> {
    let sock = daemon::socket_path();
    let mut stream = UnixStream::connect(&sock)
        .with_context(|| format!("daemon not running? cannot connect to {}", sock.display()))?;
    writeln!(stream, "{command}")?;
    stream.flush()?;

    if read_reply {
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        print!("{}", line);
    }
    Ok(())
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .any(|path| path.is_file())
        })
        .unwrap_or(false)
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn usage() {
    println!(
        "hyprspanso — espanso-compatible text expander for Hyprland/Wayland

USAGE:
    hyprspanso <command>

COMMANDS:
    daemon [--dry-run]   Run the expander daemon
    init [--force]       Write the 3 starter examples to match/matches.yml
    check                Validate the espanso match files and print a summary
    list [filter]        Print loaded triggers/replacements
    search               Open the fuzzy search window (needs a terminal)
    launcher             Open the search window in a floating terminal
    reload               Ask the running daemon to reload match files
    toggle               Enable/disable expansion
    status               Show daemon state

FILES:
    Matches are read from $XDG_CONFIG_HOME/espanso/match/**
    `hyprspanso init` writes one file with three examples when none exist.
    Override with HYPRSPANSO_CONFIG=/path/to/espanso

CONFIG FORMAT:
    Only `trigger`/`triggers` + `replace` are supported, plus `word`,
    `propagate_case` and `force_clipboard`. Matches using `vars`, `form`,
    `script`, `image`, `html` or `markdown` are skipped and reported."
    );
}
