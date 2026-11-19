//! The daemon: capture -> match -> inject, plus the search IPC socket.

use crate::config::{self, MatchEntry};
use crate::event::Event;
use crate::inject::{self, Job};
use crate::keymap::Decoder;
use crate::matcher::{self, Index};
use anyhow::{bail, Context, Result};
use evdev::KeyCode;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("HYPRSPANSO_SOCKET") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let runtime = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.trim().is_empty() && Path::new(&dir).is_absolute() => dir,
        _ => format!("/tmp/hyprspanso-{}", unsafe { libc::getuid() }),
    };
    PathBuf::from(runtime).join("hyprspanso.sock")
}

/// Counters for `hyprspanso diag`: enough to tell "no keys arriving" from
/// "keys arriving but nothing matching" without logging what you type.
#[derive(Default)]
pub struct Diag {
    events: AtomicU64,
    matches: AtomicU64,
    last_event_ms: AtomicU64,
    /// Last decoded key presses as `keycode:text`, for `diag --keys`.
    recent_keys: Mutex<std::collections::VecDeque<String>>,
    buffer_len: AtomicUsize,
    mods: AtomicU64,
    /// Last 64 decoded characters, for `diag --keys`. Memory only.
    last_chars: Mutex<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct Shared {
    pub enabled: Arc<AtomicBool>,
    pub searching: Arc<AtomicUsize>,
    pub matches: Arc<AtomicUsize>,
    pub pending: Arc<AtomicUsize>,
    pub inject_ok: Arc<AtomicU64>,
    pub inject_failed: Arc<AtomicU64>,
    pub diag: Arc<Diag>,
}

fn bind_socket(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            bail!("hyprspanso is already running (socket {})", path.display());
        }
        let _ = std::fs::remove_file(path);
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("cannot bind {}", path.display()))?;
    Ok(listener)
}

pub fn run(dry_run: bool) -> Result<()> {
    let (root, source) = config::resolve_root();
    let set = config::load(&root);
    print_summary(&root, source, &set);

    if dry_run {
        println!("\n--dry-run: config looks fine, not starting");
        return Ok(());
    }

    let (tx, rx) = mpsc::channel::<Event>();
    let (job_tx, job_rx) = mpsc::channel::<Job>();

    ensure_wayland_env();

    let shared = Shared {
        enabled: Arc::new(AtomicBool::new(true)),
        searching: Arc::new(AtomicUsize::new(0)),
        matches: Arc::new(AtomicUsize::new(set.entries.len())),
        pending: Arc::new(AtomicUsize::new(0)),
        inject_ok: Arc::new(AtomicU64::new(0)),
        inject_failed: Arc::new(AtomicU64::new(0)),
        diag: Arc::new(Diag::default()),
    };

    inject::start(
        job_rx,
        Arc::clone(&shared.pending),
        Arc::clone(&shared.inject_ok),
        Arc::clone(&shared.inject_failed),
    );

    let sock = socket_path();
    let listener = bind_socket(&sock)?;

    spawn_ipc(listener, tx.clone(), shared.clone());

    let decoder = Decoder::new()?;
    eprintln!(
        "hyprspanso: keymap groups: {}",
        decoder.layout_names().join(", ")
    );

    crate::hypr::spawn_listener(tx.clone());
    crate::capture::start(tx.clone());
    spawn_config_watcher(root.clone(), tx.clone());
    spawn_signal_bridge(tx.clone(), shared.clone());

    eprintln!("hyprspanso: listening (socket {})", sock.display());
    if set.entries.is_empty() {
        eprintln!(
            "hyprspanso: no usable matches found under {}",
            config::match_dir(&root).display()
        );
    }

    let mut engine = Engine::new(root, set.entries, decoder, job_tx, shared);
    for event in rx {
        engine.handle(event);
    }
    Ok(())
}

fn print_summary(root: &Path, source: config::RootSource, set: &config::MatchSet) {
    eprintln!(
        "hyprspanso: config root {} ({})",
        root.display(),
        source.label()
    );
    if !config::match_dir(root).is_dir() {
        eprintln!(
            "hyprspanso: no match directory at {}",
            config::match_dir(root).display()
        );
        eprintln!(
            "hyprspanso: run `hyprspanso init --config-dir <folder>` to choose one"
        );
    }
    eprintln!("hyprspanso: loaded {}", set.summary());
    for warning in &set.warnings {
        eprintln!("hyprspanso: warning: {warning}");
    }
    if !set.skipped.is_empty() {
        eprintln!("hyprspanso: skipped matches that need the espanso engine:");
        for skipped in set.skipped.iter().take(10) {
            eprintln!(
                "  - {:32} {} [{}]",
                skipped.trigger, skipped.reason, skipped.source
            );
        }
        if set.skipped.len() > 10 {
            eprintln!("  ... and {} more (run `hyprspanso check`)", set.skipped.len() - 10);
        }
    }
}

fn spawn_config_watcher(root: PathBuf, tx: Sender<Event>) {
    thread::spawn(move || {
        let mut last = config::snapshot(&root);
        loop {
            thread::sleep(Duration::from_secs(2));
            let now = config::snapshot(&root);
            if now != last {
                last = now;
                if tx.send(Event::Reload).is_err() {
                    return;
                }
            }
        }
    });
}

fn spawn_ipc(listener: UnixListener, tx: Sender<Event>, shared: Shared) {
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            let shared = shared.clone();
            thread::spawn(move || handle_client(stream, tx, shared));
        }
    });
}

struct SearchGuard(Arc<AtomicUsize>);

impl SearchGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for SearchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn handle_client(stream: UnixStream, tx: Sender<Event>, shared: Shared) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut writer = write_half;
    let reader = BufReader::new(stream);
    let mut target: Option<String> = None;
    let mut selection: Option<(String, usize)> = None;
    // Only the search TUI pauses expansion. Short-lived CLI clients
    // (status/toggle/reload) must not.
    let mut search_guard: Option<SearchGuard> = None;

    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "SEARCH" {
            if search_guard.is_none() {
                search_guard = Some(SearchGuard::new(shared.searching.clone()));
            }
        } else if let Some(rest) = line.strip_prefix("TARGET\t") {
            target = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("SELECT\t") {
            selection = Some(parse_choice(rest));
        } else if let Some(rest) = line.strip_prefix("PICKVAR\t") {
            let mut parts = rest.split('\t');
            let trigger = parts.next().unwrap_or_default().to_string();
            let var = parts.next().unwrap_or_default().to_string();
            let index = parts
                .next()
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let _ = tx.send(Event::PickVar {
                trigger,
                var,
                index,
                target: target.clone(),
            });
        } else if let Some(rest) = line.strip_prefix("PICK\t") {
            selection = Some(parse_choice(rest));
        } else if let Some(rest) = line.strip_prefix("FORM\t") {
            if let Some((trigger, json)) = rest.split_once('\t') {
                let values = serde_json::from_str::<HashMap<String, String>>(json)
                    .unwrap_or_default();
                let _ = tx.send(Event::FormValues {
                    trigger: trigger.to_string(),
                    values,
                    target: target.clone(),
                });
            }
        } else if line == "RELOAD" {
            let _ = tx.send(Event::Reload);
            let _ = writeln!(writer, "ok");
        } else if line == "TOGGLE" {
            let enabled = shared.enabled.load(Ordering::SeqCst);
            shared.enabled.store(!enabled, Ordering::SeqCst);
            let _ = writeln!(writer, "{}", if enabled { "disabled" } else { "enabled" });
        } else if line == "DIAG" || line == "DIAG_KEYS" {
            let with_keys = line == "DIAG_KEYS";
            let events = shared.diag.events.load(Ordering::Relaxed);
            let matched = shared.diag.matches.load(Ordering::Relaxed);
            let last = shared.diag.last_event_ms.load(Ordering::Relaxed);
            let age = if last == 0 {
                "never".to_string()
            } else {
                format!("{}s ago", now_ms().saturating_sub(last) / 1000)
            };
            let bits = shared.diag.mods.load(Ordering::Relaxed);
            let mut mods = Vec::new();
            if bits & 1 != 0 {
                mods.push("ctrl");
            }
            if bits & 2 != 0 {
                mods.push("alt");
            }
            if bits & 4 != 0 {
                mods.push("logo");
            }
            let last_chars = if with_keys {
                let decoded = shared
                    .diag
                    .last_chars
                    .lock()
                    .map(|chars| format!(" last_decoded={chars:?}"))
                    .unwrap_or_default();
                let keys = shared
                    .diag
                    .recent_keys
                    .lock()
                    .map(|keys| {
                        let joined: Vec<&str> = keys.iter().map(String::as_str).collect();
                        format!(" raw_keys=[{}]", joined.join(" "))
                    })
                    .unwrap_or_default();
                format!("{decoded}{keys}")
            } else {
                String::new()
            };
            let _ = writeln!(
                writer,
                "events={events} matches={matched} last_event={age} buffer={} held_mods={} matches_loaded={} inserted={} insert_failed={}{last_chars}",
                shared.diag.buffer_len.load(Ordering::Relaxed),
                if mods.is_empty() {
                    "none".to_string()
                } else {
                    mods.join(",")
                },
                shared.matches.load(Ordering::Relaxed),
                shared.inject_ok.load(Ordering::Relaxed),
                shared.inject_failed.load(Ordering::Relaxed)
            );
        } else if line == "STATUS" {
            let _ = writeln!(
                writer,
                "enabled={} matches={} searching={}",
                shared.enabled.load(Ordering::SeqCst),
                shared.matches.load(Ordering::SeqCst),
                shared.searching.load(Ordering::SeqCst) > 0
            );
        }
    }

    if let Some((trigger, index)) = selection {
        let _ = tx.send(Event::Choose {
            trigger,
            index,
            target,
        });
    }
    drop(search_guard);
}

fn spawn_signal_bridge(tx: Sender<Event>, shared: Shared) {
    use std::sync::atomic::AtomicI32;
    static SIGNAL: AtomicI32 = AtomicI32::new(0);

    extern "C" fn handler(sig: libc::c_int) {
        SIGNAL.store(sig, Ordering::SeqCst);
    }

    unsafe {
        libc::signal(libc::SIGUSR1, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR2, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
    }

    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(150));
        match SIGNAL.swap(0, Ordering::SeqCst) {
            libc::SIGUSR1 => {
                let enabled = !shared.enabled.load(Ordering::SeqCst);
                shared.enabled.store(enabled, Ordering::SeqCst);
                eprintln!(
                    "hyprspanso: {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }
            libc::SIGUSR2 => {
                let _ = tx.send(Event::Reload);
            }
            libc::SIGTERM | libc::SIGINT => {
                eprintln!("hyprspanso: exiting");
                std::process::exit(0);
            }
            _ => {}
        }
    });
}

/// systemd user services do not always inherit WAYLAND_DISPLAY (uwsm usually
/// imports it, but be defensive): wl-copy/wtype children need it.
fn ensure_wayland_env() {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return;
    }
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let runtime = PathBuf::from(runtime);
    if let Ok(entries) = std::fs::read_dir(&runtime) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("wayland-") && !name.contains(".lock") {
                unsafe { std::env::set_var("WAYLAND_DISPLAY", &name) };
                return;
            }
        }
    }
}

struct ChoiceFlow {
    chosen: HashMap<String, String>,
    target: String,
}

struct Engine {
    root: PathBuf,
    entries: Vec<MatchEntry>,
    index: Index,
    decoder: Decoder,
    /// In-flight `choice` picks per trigger (a form-like flow can span
    /// several picker windows when a match has more than one choice var).
    choice_flows: HashMap<String, ChoiceFlow>,
    buffer: String,
    last_device: Option<String>,
    last_focus: Option<String>,
    last_focus_class: String,
    mods: Mods,
    job_tx: Sender<Job>,
    shared: Shared,
}

#[derive(Default)]
struct Mods {
    ctrl: bool,
    alt: bool,
    logo: bool,
}

impl Mods {
    fn any(&self) -> bool {
        self.ctrl || self.alt || self.logo
    }

    fn update(&mut self, code: KeyCode, down: bool) {
        let active = down;
        match code {
            KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => self.ctrl = active,
            KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => self.alt = active,
            KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => self.logo = active,
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.ctrl = false;
        self.alt = false;
        self.logo = false;
    }
}

fn is_clear_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_ESC
            | KeyCode::KEY_ENTER
            | KeyCode::KEY_KPENTER
            | KeyCode::KEY_TAB
            | KeyCode::KEY_LEFT
            | KeyCode::KEY_RIGHT
            | KeyCode::KEY_UP
            | KeyCode::KEY_DOWN
            | KeyCode::KEY_HOME
            | KeyCode::KEY_END
            | KeyCode::KEY_PAGEUP
            | KeyCode::KEY_PAGEDOWN
            | KeyCode::KEY_DELETE
            | KeyCode::KEY_INSERT
    )
}

impl Engine {
    fn queue(&self, job: Job) {
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        if self.job_tx.send(job).is_err() {
            self.shared.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn new(
        root: PathBuf,
        entries: Vec<MatchEntry>,
        decoder: Decoder,
        job_tx: Sender<Job>,
        shared: Shared,
    ) -> Self {
        let index = matcher::build_index(&entries);
        Self {
            root,
            entries,
            index,
            decoder,
            choice_flows: HashMap::new(),
            buffer: String::new(),
            last_device: None,
            last_focus: None,
            last_focus_class: String::new(),
            mods: Mods::default(),
            job_tx,
            shared,
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Key {
                code,
                value,
                device,
            } => self.on_key(code, value, &device),
            Event::Layout { keyboard, layout } => {
                // Remember the layout for that keyboard. It only changes the
                // decoding group if we are currently typing on it; each
                // keyboard keeps its own layout in the decoder.
                let applied = self.decoder.note_layout(
                    &keyboard,
                    &layout,
                    self.last_device.as_deref(),
                );
                if applied {
                    self.buffer.clear();
                    self.mods.reset();
                }
            }
            Event::Focus { address, class } => {
                let changed = if !address.is_empty() {
                    self.last_focus.as_deref() != Some(address.as_str())
                } else {
                    !class.is_empty() && class != self.last_focus_class
                };
                if !address.is_empty() {
                    self.last_focus = Some(address);
                }
                if !class.is_empty() {
                    self.last_focus_class = class;
                }
                if changed {
                    self.buffer.clear();
                    self.decoder.reset_state();
                }
            }
            Event::DeviceChanged(_) => {
                // A keyboard that re-enumerated can swallow a key release.
                self.mods.reset();
                self.decoder.reset_state();
                self.buffer.clear();
            }
            Event::Reload => self.reload(),
            Event::Choose {
                trigger,
                index,
                target,
            } => self.choose(&trigger, index, target.as_deref()),
            Event::PickVar {
                trigger,
                var,
                index,
                target,
            } => self.pick_var(&trigger, &var, index, target.as_deref()),
            Event::FormValues {
                trigger,
                values,
                target,
            } => self.form_submit(&trigger, &values, target.as_deref()),
        }
    }

    fn reload(&mut self) {
        let set = config::load(&self.root);
        eprintln!("hyprspanso: reloaded {}", set.summary());
        self.shared
            .matches
            .store(set.entries.len(), Ordering::SeqCst);
        self.index = matcher::build_index(&set.entries);
        self.entries = set.entries;
        self.buffer.clear();
    }

    fn entries_for(&self, trigger: &str) -> Vec<&MatchEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.trigger == trigger)
            .collect()
    }

    fn choose(&mut self, trigger: &str, index: usize, target: Option<&str>) {
        let group = self.entries_for(trigger);
        let Some(entry) = group.get(index).or_else(|| group.first()).copied() else {
            eprintln!("hyprspanso: selected trigger no longer exists: {trigger}");
            return;
        };
        if let Some(var) = matcher::pending_choice(entry, &HashMap::new()) {
            let var_name = var.name.clone();
            let trigger = entry.trigger.clone();
            let address = target
                .filter(|address| !address.is_empty())
                .map(str::to_string)
                .or_else(|| crate::hypr::active_window().map(|(_, _, a)| a))
                .unwrap_or_default();
            self.choice_flows.insert(
                trigger.clone(),
                ChoiceFlow {
                    chosen: HashMap::new(),
                    target: address.clone(),
                },
            );
            if let Err(err) = launch_picker(&trigger, Some(&var_name), &address) {
                eprintln!("hyprspanso: cannot open choice window: {err:#}");
            }
            return;
        }

        if entry.form.is_some() {
            let address = target
                .filter(|address| !address.is_empty())
                .map(str::to_string)
                .or_else(|| crate::hypr::active_window().map(|(_, _, a)| a))
                .unwrap_or_default();
            if let Err(err) = launch_form_window(&entry.trigger, index, &address) {
                eprintln!("hyprspanso: cannot open form window: {err:#}");
            }
            return;
        }

        let text = matcher::render(entry);
        let clipboard = entry.force_clipboard || text.chars().count() > 120;

        if let Some(address) = target {
            crate::hypr::focus_window(address);
            thread::sleep(Duration::from_millis(120));
        } else {
            // The TUI terminal is closing; give focus time to come back.
            thread::sleep(Duration::from_millis(250));
        }

        self.queue(Job {
            text,
            backspaces: 0,
            clipboard,
        });
    }

    fn form_submit(
        &mut self,
        trigger: &str,
        values: &HashMap<String, String>,
        target: Option<&str>,
    ) {
        let Some(entry) = self.entries.iter().find(|entry| entry.trigger == trigger) else {
            eprintln!("hyprspanso: form trigger no longer exists: {trigger}");
            return;
        };
        let Some(spec) = entry.form.as_ref() else {
            eprintln!("hyprspanso: {trigger} is not a form");
            return;
        };

        let text = matcher::render_form(spec, values);
        let clipboard = text.chars().count() > 120;
        self.shared.diag.matches.fetch_add(1, Ordering::Relaxed);

        if let Some(address) = target.filter(|address| !address.is_empty()) {
            crate::hypr::focus_window(address);
            thread::sleep(Duration::from_millis(120));
        } else {
            thread::sleep(Duration::from_millis(250));
        }

        self.queue(Job {
            text,
            backspaces: 0,
            clipboard,
        });
    }

    fn pick_var(&mut self, trigger: &str, var: &str, index: usize, target: Option<&str>) {
        let Some(entry) = self.entries.iter().find(|entry| entry.trigger == trigger) else {
            eprintln!("hyprspanso: choice trigger no longer exists: {trigger}");
            return;
        };
        let values = matcher::choice_values(entry, var);
        let Some(value) = values.get(index).cloned() else {
            eprintln!("hyprspanso: choice value {index} out of range for {var}");
            return;
        };

        let flow = self
            .choice_flows
            .entry(trigger.to_string())
            .or_insert_with(|| ChoiceFlow {
                chosen: HashMap::new(),
                target: target.map(str::to_string).unwrap_or_default(),
            });
        if flow.target.is_empty() {
            if let Some(target) = target {
                flow.target = target.to_string();
            }
        }
        flow.chosen.insert(var.to_string(), value);

        if let Some(next) = matcher::pending_choice(entry, &flow.chosen) {
            let next_name = next.name.clone();
            let target = flow.target.clone();
            if let Err(err) = launch_picker(trigger, Some(&next_name), &target) {
                eprintln!("hyprspanso: cannot open choice window: {err:#}");
            }
            return;
        }

        let Some(flow) = self.choice_flows.remove(trigger) else {
            return;
        };
        let text = matcher::render_with(entry, chrono::Local::now(), &flow.chosen);
        let clipboard = entry.force_clipboard || text.chars().count() > 120;
        self.shared.diag.matches.fetch_add(1, Ordering::Relaxed);

        if flow.target.is_empty() {
            thread::sleep(Duration::from_millis(250));
        } else {
            crate::hypr::focus_window(&flow.target);
            thread::sleep(Duration::from_millis(120));
        }

        self.queue(Job {
            text,
            backspaces: 0,
            clipboard,
        });
    }

    fn on_key(&mut self, code: u16, value: i32, device: &str) {
        self.decoder.prepare_device(device);
        if self.last_device.as_deref() != Some(device) {
            self.last_device = Some(device.to_string());
            self.buffer.clear();
            self.mods.reset();
        }

        self.shared.diag.events.fetch_add(1, Ordering::Relaxed);
        self.shared
            .diag
            .last_event_ms
            .store(now_ms(), Ordering::Relaxed);
        let key = KeyCode(code);
        self.decoder.tick();
        let text = self.decoder.feed(code, value);

        if let Ok(mut keys) = self.shared.diag.recent_keys.lock() {
            let shown = text.clone().unwrap_or_default();
            keys.push_back(format!(
                "{code}:{value}:{shown}{}",
                self.decoder.state_summary()
            ));
            while keys.len() > 48 {
                keys.pop_front();
            }
        }

        if value == 0 {
            self.mods.update(key, false);
            return;
        }
        if value == 1 {
            self.mods.update(key, true);
        }

        if key == KeyCode::KEY_BACKSPACE {
            self.buffer.pop();
            return;
        }
        if is_clear_key(key) {
            self.buffer.clear();
            return;
        }
        if self.mods.any() {
            self.buffer.clear();
            return;
        }
        if self.shared.searching.load(Ordering::SeqCst) > 0
            || !self.shared.enabled.load(Ordering::SeqCst)
        {
            self.buffer.clear();
            return;
        }

        if self.shared.pending.load(Ordering::SeqCst) > 0 {
            // wtype is still typing the previous replacement; matching now
            // would interleave and fire a second insertion on top of it.
            self.buffer.clear();
            self.record_state();
            return;
        }

        let Some(text) = text else {
            self.record_state();
            return;
        };
        self.buffer.push_str(&text);
        matcher::trim_buffer(&mut self.buffer);
        if let Ok(mut last) = self.shared.diag.last_chars.lock() {
            last.push_str(&text);
            let count = last.chars().count();
            if count > 64 {
                let cut = count - 64;
                if let Some((byte, _)) = last.char_indices().nth(cut) {
                    last.drain(..byte);
                }
            }
        }
        self.check_match();
        self.record_state();
    }

    fn record_state(&self) {
        let bits = (u64::from(self.mods.ctrl))
            | (u64::from(self.mods.alt) << 1)
            | (u64::from(self.mods.logo) << 2);
        self.shared.diag.mods.store(bits, Ordering::Relaxed);
        self.shared
            .diag
            .buffer_len
            .store(self.buffer.chars().count(), Ordering::Relaxed);
    }

    fn check_match(&mut self) {
        if debug_keys() {
            eprintln!("hyprspanso: buffer={:?}", self.buffer);
        }
        let Some(hit) = matcher::find(&self.entries, &self.index, &self.buffer) else {
            return;
        };
        if let Some(var) = matcher::pending_choice(hit.entry, &HashMap::new()) {
            let var_name = var.name.clone();
            let backspaces = hit.backspaces;
            let trigger = hit.entry.trigger.clone();
            let target = crate::hypr::active_window()
                .map(|(_, _, address)| address)
                .unwrap_or_default();
            self.buffer.clear();
            self.queue(Job {
                text: String::new(),
                backspaces,
                clipboard: false,
            });
            self.choice_flows.insert(
                trigger.clone(),
                ChoiceFlow {
                    chosen: HashMap::new(),
                    target: target.clone(),
                },
            );
            if let Err(err) = launch_picker(&trigger, Some(&var_name), &target) {
                eprintln!("hyprspanso: cannot open choice window: {err:#}");
            }
            return;
        }

        {
            let group = self.entries_for(&hit.entry.trigger);
            if group.len() > 1 {
                let backspaces = hit.backspaces;
                let trigger = hit.entry.trigger.clone();
                let target = crate::hypr::active_window()
                    .map(|(_, _, address)| address)
                    .unwrap_or_default();
                self.buffer.clear();
                self.queue(Job {
                    text: String::new(),
                    backspaces,
                    clipboard: false,
                });
                if let Err(err) = launch_picker(&trigger, None, &target) {
                    eprintln!("hyprspanso: cannot open picker window: {err:#}");
                }
                return;
            }
        }

        if hit.entry.form.is_some() {
            // Erase the typed trigger first, like espanso does, then collect
            // the field values in a separate window.
            let backspaces = hit.backspaces;
            let trigger = hit.entry.trigger.clone();
            let target = crate::hypr::active_window()
                .map(|(_, _, address)| address)
                .unwrap_or_default();
            self.buffer.clear();
            self.queue(Job {
                text: String::new(),
                backspaces,
                clipboard: false,
            });
            if let Err(err) = launch_form_window(&trigger, 0, &target) {
                eprintln!("hyprspanso: cannot open form window: {err:#}");
            }
            return;
        }

        let rendered = matcher::render(hit.entry);
        let text = matcher::apply_case(&rendered, &hit.typed, hit.entry.propagate_case);
        let clipboard = hit.entry.force_clipboard || text.chars().count() > 120;
        let backspaces = hit.backspaces;
        self.shared.diag.matches.fetch_add(1, Ordering::Relaxed);

        if debug() {
            eprintln!(
                "hyprspanso: matched {:?} -> {:?} ({} backspaces, clipboard={clipboard})",
                hit.entry.trigger, text, backspaces
            );
        }

        self.buffer.clear();
        self.queue(Job {
            text,
            backspaces,
            clipboard,
        });
    }
}

fn debug() -> bool {
    std::env::var_os("HYPRSPANSO_DEBUG").is_some()
}

fn debug_keys() -> bool {
    std::env::var_os("HYPRSPANSO_DEBUG_KEYS").is_some()
}

fn launch_form_window(trigger: &str, index: usize, target: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let mut args: Vec<String> = vec![
        exe,
        "form".into(),
        "--trigger".into(),
        trigger.into(),
        "--index".into(),
        index.to_string(),
    ];
    if !target.is_empty() {
        args.push("--target".into());
        args.push(target.into());
    }

    if which("omarchy-launch-tui") {
        let mut cmd = Command::new("omarchy-launch-tui");
        cmd.arg("--app-id=org.omarchy.hyprspanso")
            .args(&args)
            .stdin(Stdio::null());
        if cmd.spawn().is_ok() {
            return Ok(());
        }
    }

    Command::new("xdg-terminal-exec")
        .arg("--app-id=org.omarchy.hyprspanso")
        .arg("-e")
        .args(&args)
        .stdin(Stdio::null())
        .spawn()?;
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

fn launch_picker(trigger: &str, var: Option<&str>, target: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let mut args: Vec<String> = vec![exe, "picker".into(), "--trigger".into(), trigger.into()];
    if let Some(var) = var {
        args.push("--var".into());
        args.push(var.into());
    }
    if !target.is_empty() {
        args.push("--target".into());
        args.push(target.into());
    }

    if which("omarchy-launch-tui") {
        let mut cmd = Command::new("omarchy-launch-tui");
        cmd.arg("--app-id=org.omarchy.hyprspanso")
            .args(&args)
            .stdin(Stdio::null());
        if cmd.spawn().is_ok() {
            return Ok(());
        }
    }

    Command::new("xdg-terminal-exec")
        .arg("--app-id=org.omarchy.hyprspanso")
        .arg("-e")
        .args(&args)
        .stdin(Stdio::null())
        .spawn()?;
    Ok(())
}

/// `trigger` or `trigger\t<index>` from the IPC clients.
fn parse_choice(rest: &str) -> (String, usize) {
    match rest.split_once('\t') {
        Some((trigger, index)) => (
            trigger.to_string(),
            index.trim().parse::<usize>().unwrap_or(0),
        ),
        None => (rest.to_string(), 0),
    }
}
