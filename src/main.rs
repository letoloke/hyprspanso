mod capture;
mod config;
mod daemon;
mod event;
mod form;
mod hypr;
mod inject;
mod keymap;
mod matcher;
mod picker;
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
        "config" => run_config(&args),
        "setup-hyprland" => run_setup_hyprland(&args),
        "list" => run_list(&args),
        "search" => tui::run(flag_value(&args, "--target")),
        "form" => form::run(
            flag_value(&args, "--trigger"),
            flag_value(&args, "--index")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            flag_value(&args, "--target"),
        ),
        "picker" => picker::run(flag_value(&args, "--trigger"), flag_value(&args, "--target")),
        "launcher" => run_launcher(),
        "reload" => ipc_command("RELOAD", false),
        "toggle" => ipc_command("TOGGLE", true),
        "status" => ipc_command("STATUS", true),
        "diag" => ipc_command(
            if args.iter().any(|a| a == "--keys") {
                "DIAG_KEYS"
            } else {
                "DIAG"
            },
            true,
        ),
        "version" | "-V" | "--version" => {
            println!("hyprspanso {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
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
    use std::io::IsTerminal;

    let force = args.iter().any(|a| a == "--force");
    let no_input = args.iter().any(|a| a == "--no-input") || !std::io::stdin().is_terminal();
    let explicit = flag_value(args, "--config-dir").or_else(|| flag_value(args, "--dir"));

    let (current, source) = config::resolve_root();

    let explicit_given = explicit.is_some();
    let chosen = if let Some(dir) = explicit {
        config::expand_path(&dir)
    } else if no_input {
        current.clone()
    } else {
        println!("hyprspanso reads every .yml/.yaml under <dir>/match/.");
        println!("Point it at your espanso config folder, or at an empty folder to start fresh.");
        print!("Directory [{}]: ", current.display());
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let line = line.trim();
        if line.is_empty() {
            current.clone()
        } else {
            config::expand_path(line)
        }
    };

    // Persist the choice so the daemon (a systemd service, no terminal) finds
    // it later without an env var or a symlink.
    if explicit_given || (!no_input && source != config::RootSource::Env) {
        let mut settings = config::load_settings();
        settings.config_dir = Some(chosen.clone());
        config::save_settings(&settings)?;
        println!(
            "Saved config_dir = {} in {}",
            chosen.display(),
            config::settings_path().display()
        );
    }

    let match_dir = config::match_dir(&chosen);
    std::fs::create_dir_all(&match_dir)?;
    let target = match_dir.join("matches.yml");

    let existing = config::load(&chosen);
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

    std::fs::write(&target, EXAMPLES)?;
    println!("Wrote 3 starter examples to {}", target.display());
    println!("Edit the file, then `hyprspanso reload` (or wait 2 seconds).");
    Ok(())
}

fn run_config(args: &[String]) -> Result<()> {
    match args.get(1).map(String::as_str) {
        Some("set") => {
            let Some(dir) = args.get(2) else {
                anyhow::bail!("usage: hyprspanso config set <directory>");
            };
            let path = config::expand_path(dir);
            let mut settings = config::load_settings();
            settings.config_dir = Some(path.clone());
            config::save_settings(&settings)?;
            println!("config_dir = {}", path.display());
            println!("Restart the daemon to apply it: systemctl --user restart hyprspanso");
            Ok(())
        }
        Some("unset") => {
            let mut settings = config::load_settings();
            settings.config_dir = None;
            config::save_settings(&settings)?;
            println!("Cleared {}", config::settings_path().display());
            Ok(())
        }
        _ => {
            let (root, source) = config::resolve_root();
            let set = config::load(&root);
            println!("config root : {} ({})", root.display(), source.label());
            println!("matches dir : {}", config::match_dir(&root).display());
            println!("settings    : {}", config::settings_path().display());
            println!(
                "matches     : {} from {} file(s)",
                set.entries.len(),
                set.files.len()
            );
            if let Ok(env) = std::env::var("HYPRSPANSO_CONFIG") {
                if !env.trim().is_empty() {
                    println!("note        : HYPRSPANSO_CONFIG={env} overrides the saved setting");
                }
            }
            Ok(())
        }
    }
}

const HYPR_LUA_RULE: &str = r#"-- hyprspanso: floating snippet windows (search, picker, forms)
o.window("^org\\.omarchy\\.hyprspanso$", {
  float = true,
  size = "1000 620",
  center = true,
  rounding = 6,
  border_size = 2,
})"#;

const HYPR_CONF_RULE: &str = r#"# hyprspanso: floating snippet windows (search, picker, forms)
windowrule = float, class:^(org\.omarchy\.hyprspanso)$
windowrule = size 1000 620, class:^(org\.omarchy\.hyprspanso)$
windowrule = center, class:^(org\.omarchy\.hyprspanso)$"#;

/// Install the window rule that makes the search/picker/form windows float.
/// The TUI runs in a normal terminal window, so without this it opens tiled.
fn run_setup_hyprland(args: &[String]) -> Result<()> {
    use std::fs;

    if args.iter().any(|a| a == "--print") {
        println!("{HYPR_LUA_RULE}\n\n--- or, for a hyprland.conf ---\n\n{HYPR_CONF_RULE}");
        return Ok(());
    }

    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".config")
        });
    let dir = base.join("hypr");
    let lua = dir.join("hyprland.lua");
    let conf = dir.join("hyprland.conf");

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if lua.is_file() {
        let text = fs::read_to_string(&lua)?;
        if text.contains("hyprspanso") {
            println!("{} already configures hyprspanso; nothing to do", lua.display());
        } else {
            let backup = dir.join(format!("hyprland.lua.bak.{stamp}"));
            fs::copy(&lua, &backup)?;
            let module = dir.join("hyprspanso.lua");
            fs::write(&module, format!("{HYPR_LUA_RULE}\n"))?;
            fs::write(
                &lua,
                format!("{text}\nrequire(\"hypr.hyprspanso\")\n"),
            )?;
            println!("wrote {}", module.display());
            println!("added require(\"hypr.hyprspanso\") to {}", lua.display());
            println!("backup: {}", backup.display());
        }
    } else if conf.is_file() {
        let text = fs::read_to_string(&conf)?;
        if text.contains("hyprspanso") {
            println!("{} already configures hyprspanso; nothing to do", conf.display());
        } else {
            let backup = dir.join(format!("hyprland.conf.bak.{stamp}"));
            fs::copy(&conf, &backup)?;
            fs::write(&conf, format!("{text}\n{HYPR_CONF_RULE}\n"))?;
            println!("appended the floating rule to {}", conf.display());
            println!("backup: {}", backup.display());
        }
    } else {
        println!(
            "no hyprland.lua or hyprland.conf found in {}; add this by hand:",
            dir.display()
        );
        println!("{HYPR_LUA_RULE}");
        return Ok(());
    }

    let _ = Command::new("hyprctl")
        .arg("reload")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    println!("reloaded Hyprland");
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
        let rendered = match &entry.form {
            Some(form) => format!("(form) {}", form.template.lines().next().unwrap_or("")),
            None => matcher::render(entry),
        };
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
    init [--config-dir DIR] [--force] [--no-input]
                         Choose the match folder, save it, write the examples
    config [set DIR|unset]
                         Show where matches are read from, or change it
    setup-hyprland [--print]
                         Add the floating-window rule to your Hyprland config
    check                Validate the espanso match files and print a summary
    list [filter]        Print loaded triggers/replacements
    search               Open the fuzzy search window (needs a terminal)
    form --trigger T     Fill a match's `form:` fields (used by the daemon)
    picker --trigger T   Choose between matches sharing a trigger
    launcher             Open the search window in a floating terminal
    reload               Ask the running daemon to reload match files
    toggle               Enable/disable expansion
    status               Show daemon state
    version              Print the installed version
    diag [--keys]        Counters: keys seen, matches, buffer, held modifiers
                         (--keys adds the last 64 decoded characters)

FILES:
    Matches are read from <config-dir>/match/** (default ~/.config/espanso).
    `hyprspanso init` asks for the folder on first run, saves it in
    ~/.config/hyprspanso/config.json, and writes three examples if none exist.
    `hyprspanso config` shows the resolved folder and where it came from.
    HYPRSPANSO_CONFIG=/path overrides the saved setting.
    Override with HYPRSPANSO_CONFIG=/path/to/espanso

CONFIG FORMAT:
    Only `trigger`/`triggers` + `replace` are supported, plus `word`,
    `propagate_case` and `force_clipboard`. Matches using `vars`, `form`,
    `script`, `image`, `html` or `markdown` are skipped and reported."
    );
}
