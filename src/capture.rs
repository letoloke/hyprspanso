//! evdev keyboard capture.
//!
//! Reads /dev/input directly (needs the user in the `input` group) and only
//! observes: devices are never grabbed, so the keyboard keeps working normally.
//! Injections go through the Wayland virtual-keyboard protocol and therefore
//! never show up here, which removes the feedback loop espanso's uinput
//! backend has to filter manually.

use crate::event::Event;
use evdev::{Device, EventType, KeyCode};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Devices that must never be read, or we would re-expand our own output.
const DENY: &[&str] = &["espanso virtual", "ydotool", "hyprspanso", "wtype"];

pub fn start(tx: Sender<Event>) {
    thread::spawn(move || {
        // Paths currently being read. A reader removes its own path when the
        // device goes away (wireless keyboards re-enumerate, USB resets,
        // suspend/resume), otherwise the evdev node number would be considered
        // "already open" forever and the keyboard would silently stop working.
        let opened: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let dedupe = Arc::new(Dedupe::new());
        let mut next_device_id: u64 = 0;
        let mut last_signature = String::new();
        loop {
            let found = candidates();
            if debug() {
                // Only log when the device set changes: a per-3s line would
                // flood the journal on a long-running daemon.
                let signature = found
                    .iter()
                    .map(|(p, n)| format!("{} ({})", p.display(), n))
                    .collect::<Vec<_>>()
                    .join(", ");
                if signature != last_signature {
                    eprintln!(
                        "hyprspanso: scan: {} keyboard device(s): {}",
                        found.len(),
                        signature
                    );
                    last_signature = signature;
                }
            }
            for (path, name) in found {
                let should_open = opened
                    .lock()
                    .map(|mut guard| guard.insert(path.clone()))
                    .unwrap_or(false);
                if !should_open {
                    continue;
                }
                if debug() {
                    eprintln!("hyprspanso: opening {} ({})", path.display(), name);
                }
                let tx = tx.clone();
                let opened = Arc::clone(&opened);
                let dedupe = Arc::clone(&dedupe);
                next_device_id += 1;
                let device_id = next_device_id;
                thread::spawn(move || {
                    read_device(path.clone(), name, tx, dedupe, device_id);
                    // Give a re-plugged node a moment before retrying it.
                    thread::sleep(Duration::from_secs(1));
                    if let Ok(mut guard) = opened.lock() {
                        guard.remove(&path);
                    }
                });
            }
            thread::sleep(Duration::from_secs(3));
        }
    });
}

fn debug() -> bool {
    std::env::var_os("HYPRSPANSO_DEBUG").is_some()
}

fn is_keyboard(device: &Device) -> bool {
    let Some(keys) = device.supported_keys() else {
        return false;
    };
    keys.contains(KeyCode::KEY_A)
        && keys.contains(KeyCode::KEY_SPACE)
        && keys.contains(KeyCode::KEY_ENTER)
}

/// Several event nodes can expose the same physical keyboard (e.g. a wireless
/// dongle with an "input0" and an "input2" node). Reading both would duplicate
/// every character, so group by physical path and keep the richest node.
/// Every node that can produce keyboard events.
///
/// A single physical keyboard can expose several event nodes (a wireless
/// dongle usually has an "input0" and an "input2"), and which one carries the
/// letters can change after a re-pair or suspend. Reading all of them and
/// de-duplicating identical events is more reliable than guessing the "right"
/// node by capability count.
fn candidates() -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();

    for (path, device) in evdev::enumerate() {
        if !is_keyboard(&device) {
            continue;
        }
        let name = device.name().unwrap_or("unknown").to_string();
        let lower = name.to_lowercase();
        if DENY.iter().any(|deny| lower.contains(deny)) {
            continue;
        }
        found.push((path, name));
    }

    found.sort();
    found
}

/// Two nodes of the same keyboard report the same press within a few
/// milliseconds. Human typing never repeats an identical key+state that fast,
/// so anything inside this window is treated as a duplicate.
const DEDUPE_WINDOW: Duration = Duration::from_millis(10);

struct Dedupe {
    recent: Mutex<HashMap<(u16, i32), (u64, Instant)>>,
}

impl Dedupe {
    fn new() -> Self {
        Self {
            recent: Mutex::new(HashMap::new()),
        }
    }

    /// `device` identifies the event node. Only *another* node reporting the
    /// same key and state within the window is treated as a duplicate; a single
    /// device's own quick press/release (switch chatter) is always delivered,
    /// otherwise its modifier state can drift from the compositor's.
    fn accept(&self, device: u64, code: u16, value: i32) -> bool {
        let now = Instant::now();
        let Ok(mut recent) = self.recent.lock() else {
            return true;
        };
        if let Some((seen_on, seen)) = recent.get(&(code, value)) {
            if *seen_on != device && now.duration_since(*seen) < DEDUPE_WINDOW {
                return false;
            }
        }
        if recent.len() > 256 {
            recent.retain(|_, (_, seen)| now.duration_since(*seen) < Duration::from_secs(1));
        }
        recent.insert((code, value), (device, now));
        true
    }
}

fn read_device(
    path: PathBuf,
    name: String,
    tx: Sender<Event>,
    dedupe: Arc<Dedupe>,
    device_id: u64,
) {
    let mut device = match Device::open(&path) {
        Ok(d) => d,
        Err(err) => {
            eprintln!(
                "hyprspanso: cannot open {} ({name}): {err}\n\
                 hint: add your user to the input group: sudo usermod -aG input $USER",
                path.display()
            );
            return;
        }
    };

    if debug() {
        eprintln!("hyprspanso: reader started for {}", path.display());
    }
    let _ = tx.send(Event::DeviceChanged(name.clone()));
    let mut announced = false;
    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(err) => {
                if debug() {
                    eprintln!(
                        "hyprspanso: reader for {} stopped: {err}",
                        path.display()
                    );
                }
                let _ = tx.send(Event::DeviceChanged(name.clone()));
                return;
            }
        };
        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }
            let (code, value) = (event.code(), event.value());
            if debug() && !announced {
                announced = true;
                eprintln!(
                    "hyprspanso: keys arriving from {} ({name})",
                    path.display()
                );
            }
            if !dedupe.accept(device_id, code, value) {
                continue;
            }
            let sent = tx.send(Event::Key { code, value });
            if sent.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_drops_repeated_identical_events() {
        let dedupe = Dedupe::new();
        assert!(dedupe.accept(1, 30, 1)); // first press
        assert!(!dedupe.accept(2, 30, 1)); // same press from a second node
        assert!(dedupe.accept(1, 30, 1)); // the same node may repeat (chatter)
        assert!(dedupe.accept(1, 30, 0)); // release is a different state
        assert!(dedupe.accept(1, 31, 1)); // a different key is fine
    }
}
