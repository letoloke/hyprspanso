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
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

/// Devices that must never be read, or we would re-expand our own output.
const DENY: &[&str] = &["espanso virtual", "ydotool", "hyprspanso", "wtype"];

pub fn start(tx: Sender<Event>) {
    thread::spawn(move || {
        let mut opened: HashSet<PathBuf> = HashSet::new();
        loop {
            for (path, name) in candidates() {
                if !opened.insert(path.clone()) {
                    continue;
                }
                let tx = tx.clone();
                thread::spawn(move || read_device(path, name, tx));
            }
            thread::sleep(Duration::from_secs(3));
        }
    });
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
fn candidates() -> Vec<(PathBuf, String)> {
    let mut best: HashMap<String, (PathBuf, String, usize)> = HashMap::new();

    for (path, device) in evdev::enumerate() {
        if !is_keyboard(&device) {
            continue;
        }
        let name = device.name().unwrap_or("unknown").to_string();
        let lower = name.to_lowercase();
        if DENY.iter().any(|deny| lower.contains(deny)) {
            continue;
        }

        let key_count = device
            .supported_keys()
            .map(|keys| keys.iter().count())
            .unwrap_or(0);

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

        match best.get_mut(&group) {
            Some(current) if key_count <= current.2 => {}
            Some(current) => *current = (path, name, key_count),
            None => {
                best.insert(group, (path, name, key_count));
            }
        }
    }

    best.into_values()
        .map(|(path, name, _)| (path, name))
        .collect()
}

fn read_device(path: PathBuf, name: String, tx: Sender<Event>) {
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

    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(_) => return,
        };
        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }
            let sent = tx.send(Event::Key {
                code: event.code(),
                value: event.value(),
            });
            if sent.is_err() {
                return;
            }
        }
    }
}
