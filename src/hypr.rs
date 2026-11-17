//! Hyprland integration: IPC queries, the event socket, keymap dump and focus.

use crate::event::Event;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::SystemTime;

fn hypr_json(command: &str) -> Option<Value> {
    let out = Command::new("hyprctl")
        .arg("-j")
        .arg(command)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// (class, title, address) of the focused window.
pub fn active_window() -> Option<(String, String, String)> {
    let v = hypr_json("activewindow")?;
    let class = v.get("class")?.as_str()?.to_string();
    let title = v.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let address = v
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some((class, title, address))
}

pub fn focus_window(address: &str) {
    if address.is_empty() {
        return;
    }

    // Hyprland 0.56+ routes dispatchers through Lua: `hl.dsp.focus(...)`.
    let lua = format!("hl.dsp.focus({{ window = \"address:{address}\" }})");
    let modern = Command::new("hyprctl")
        .args(["dispatch", &lua])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if modern {
        return;
    }

    // Older Hyprland syntax.
    let _ = Command::new("hyprctl")
        .args(["dispatch", "focuswindow", &format!("address:{address}")])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status();
}

/// The compositor's compiled keymap, groups and all.
pub fn dump_keymap() -> Option<String> {
    let out = Command::new("xkbcli")
        .args(["dump-keymap-wayland"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// `(layout, variant)` of the main keyboard, e.g. ("us,latam", "").
pub fn main_layout() -> (String, String) {
    if let Some(v) = hypr_json("devices") {
        if let Some(keyboards) = v.get("keyboards").and_then(Value::as_array) {
            let main = keyboards
                .iter()
                .find(|k| k.get("main").and_then(Value::as_bool).unwrap_or(false))
                .or_else(|| keyboards.first());
            if let Some(kb) = main {
                let layout = kb
                    .get("layout")
                    .and_then(Value::as_str)
                    .unwrap_or("us")
                    .to_string();
                let variant = kb
                    .get("variant")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                return (layout, variant);
            }
        }
    }
    ("us".into(), String::new())
}

pub fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|dir| !dir.trim().is_empty())
        .unwrap_or_else(|| "/tmp".into());
    let base = PathBuf::from(&runtime).join("hypr");

    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        let path = base.join(&sig).join(".socket2.sock");
        if path.exists() {
            return Some(path);
        }
    }

    // Fallback: newest instance socket.
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&base).ok()?.flatten() {
        let path = entry.path().join(".socket2.sock");
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                    best = Some((modified, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Streams `activelayout` and `activewindow` events into the engine.
pub fn spawn_listener(tx: Sender<Event>) {
    thread::spawn(move || loop {
        let Some(path) = socket_path() else {
            thread::sleep(std::time::Duration::from_secs(2));
            continue;
        };
        let Ok(stream) = UnixStream::connect(&path) else {
            thread::sleep(std::time::Duration::from_secs(2));
            continue;
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(rest) = line.strip_prefix("activelayout>>") {
                if let Some((_, layout)) = rest.split_once(',') {
                    if tx.send(Event::Layout(layout.to_string())).is_err() {
                        return;
                    }
                }
            } else if let Some(rest) = line.strip_prefix("activewindow>>") {
                let (class, title) = rest.split_once(',').unwrap_or((rest, ""));
                if tx
                    .send(Event::Window(class.to_string(), title.to_string()))
                    .is_err()
                {
                    return;
                }
            }
        }
        // Socket died (compositor restart): retry.
        thread::sleep(std::time::Duration::from_millis(500));
    });
}

pub fn is_terminal(class: &str, title: &str) -> bool {
    const TERMS: &[&str] = &[
        "alacritty",
        "kitty",
        "foot",
        "ghostty",
        "wezterm",
        "org.wezfurlong.wezterm",
        "com.mitchellh.ghostty",
        "xterm",
        "urxvt",
    ];
    let hay = format!("{class} {title}").to_lowercase();
    TERMS.iter().any(|t| hay.contains(t))
}
