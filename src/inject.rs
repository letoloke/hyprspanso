//! Text injection through zwp_virtual_keyboard_v1 (via `wtype`).

use anyhow::{bail, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

/// Above this many characters, typing char-by-char is slower than a paste.
const CLIPBOARD_THRESHOLD: usize = 120;

#[derive(Debug, Clone)]
pub struct Job {
    pub text: String,
    pub backspaces: usize,
    pub clipboard: bool,
}

pub fn start(rx: Receiver<Job>) {
    thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            if let Err(err) = run(&job) {
                eprintln!("hyprspanso: injection failed: {err:#}");
            }
        }
    });
}

fn run(job: &Job) -> Result<()> {
    let backspaces = job.backspaces.min(64);
    let (class, title) = crate::hypr::active_window()
        .map(|(c, t, _)| (c, t))
        .unwrap_or_default();
    let terminal = crate::hypr::is_terminal(&class, &title);

    if job.clipboard || job.text.chars().count() > CLIPBOARD_THRESHOLD {
        clipboard_inject(job, backspaces, terminal)
    } else {
        type_text(job, backspaces)
    }
}

fn wtype(args: &[&str]) -> Result<()> {
    let status = Command::new("wtype")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("wtype {args:?} exited with {status}");
    }
    Ok(())
}

fn backspaces(count: usize) -> Result<()> {
    if count == 0 {
        return Ok(());
    }
    let mut cmd = Command::new("wtype");
    for _ in 0..count {
        cmd.args(["-k", "BackSpace"]);
    }
    let status = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("wtype (backspace) exited with {status}");
    }
    Ok(())
}

/// `wtype` takes text as positional arguments (`-t` does not exist), so
/// backspaces travel in the first call and each newline becomes a Return.
fn type_text(job: &Job, backspaces_count: usize) -> Result<()> {
    let text = job.text.replace('\t', "    ");
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or("");

    let mut cmd = Command::new("wtype");
    // -d 2 keeps Qt/GTK from dropping characters typed in the same frame.
    cmd.args(["-d", "2"]);
    for _ in 0..backspaces_count {
        cmd.args(["-k", "BackSpace"]);
    }
    if !first.is_empty() {
        cmd.arg("--").arg(first);
    }
    let status = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("wtype exited with {status}");
    }

    for line in lines {
        wtype(&["-k", "Return"])?;
        if line.is_empty() {
            continue;
        }
        if line == "-" {
            // `wtype -` means "read stdin"; a lone dash must be typed as a key.
            wtype(&["-k", "minus"])?;
        } else {
            wtype(&["-d", "2", "--", line])?;
        }
    }
    Ok(())
}

fn clipboard_inject(job: &Job, backspaces_count: usize, terminal: bool) -> Result<()> {
    backspaces(backspaces_count)?;

    // Save the clipboard so we can put the user's data back.
    let saved = read_clipboard();
    write_clipboard(&job.text)?;
    // The clipboard owner needs a moment before the paste keystroke lands.
    thread::sleep(Duration::from_millis(80));

    if terminal {
        wtype(&["-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl"])?;
    } else {
        wtype(&["-M", "ctrl", "-k", "v", "-m", "ctrl"])?;
    }

    thread::sleep(Duration::from_millis(180));
    if let Some(old) = saved {
        if !old.is_empty() {
            let _ = write_clipboard(&old);
        }
    }
    Ok(())
}

fn read_clipboard() -> Option<String> {
    let out = Command::new("timeout")
        .args(["1", "wl-paste", "--no-newline"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn write_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("wl-copy exited with {status}");
    }
    Ok(())
}
