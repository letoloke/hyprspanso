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
use std::path::{Path, PathBuf};
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
        let arbiter = Arc::new(NodeArbiter::default());
        let mut next_device_id: u64 = 0;
        let mut last_signature = String::new();
        loop {
            let found = candidates();
            if debug() {
                // Only log when the device set changes: a per-3s line would
                // flood the journal on a long-running daemon.
                let signature = found
                    .iter()
                    .map(|(p, n, _)| format!("{} ({})", p.display(), n))
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
            for (path, name, group) in found {
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
                let arbiter = Arc::clone(&arbiter);
                next_device_id += 1;
                let device_id = next_device_id;
                thread::spawn(move || {
                    read_device(path.clone(), name, group, tx, arbiter, device_id);
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
fn candidates() -> Vec<(PathBuf, String, String)> {
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

        // Nodes belonging to the same physical device share the part of the
        // physical path before "/inputN".
        let phys = device.physical_path().unwrap_or("").to_string();
        let parent = phys
            .split("/input")
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        let group = if parent.is_empty() {
            format!("name:{name}")
        } else {
            parent
        };

        found.push((path, name, group));
    }

    found.sort();
    found
}

/// One physical keyboard can expose several event nodes (a wireless dongle
/// usually has an `input0` and an `input2`). Only one may be read at a time:
/// the others are either an identical echo or a *laggy replay* of the same
/// keys. Reading them together duplicates characters, and a replayed press
/// arriving after the real release leaves a modifier stuck down.
///
/// The first node to produce a key wins its group; another node may take over
/// once the winner has been silent for a while (device re-pair, suspend).
const NODE_STALE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct NodeArbiter {
    active: Mutex<HashMap<String, (PathBuf, Instant)>>,
}

impl NodeArbiter {
    fn accept(&self, group: &str, path: &Path) -> bool {
        let Ok(mut active) = self.active.lock() else {
            return true;
        };
        let now = Instant::now();
        match active.get_mut(group) {
            Some((owner, last)) => {
                if owner.as_path() != path {
                    if now.duration_since(*last) < NODE_STALE {
                        return false;
                    }
                    *owner = path.to_path_buf();
                }
                *last = now;
                true
            }
            None => {
                active.insert(group.to_string(), (path.to_path_buf(), now));
                true
            }
        }
    }
}

fn read_device(
    path: PathBuf,
    name: String,
    group: String,
    tx: Sender<Event>,
    arbiter: Arc<NodeArbiter>,
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
            let _ = device_id;
            if !arbiter.accept(&group, &path) {
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

    /// The first node of a physical keyboard is read; an echo node is ignored
    /// while the winner is active, and can take over once it goes quiet.
    #[test]
    fn arbiter_picks_one_node_per_keyboard() {
        let arbiter = NodeArbiter::default();
        let a = PathBuf::from("/dev/input/event13");
        let b = PathBuf::from("/dev/input/event27");
        let group = "usb-0000:c6:00.4-1.3";

        assert!(arbiter.accept(group, &a));
        assert!(!arbiter.accept(group, &b)); // echo while a is active
        assert!(arbiter.accept(group, &a));
        assert!(arbiter.accept("other-keyboard", &b)); // different device
    }
}
