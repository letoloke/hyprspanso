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
    inject::start(job_rx);
    ensure_wayland_env();

    let shared = Shared {
        enabled: Arc::new(AtomicBool::new(true)),
        searching: Arc::new(AtomicUsize::new(0)),
        matches: Arc::new(AtomicUsize::new(set.entries.len())),
        diag: Arc::new(Diag::default()),
    };

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
    let mut selection: Option<String> = None;
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
            selection = Some(rest.to_string());
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
                shared
                    .diag
                    .last_chars
                    .lock()
                    .map(|chars| format!(" last_decoded={chars:?}"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let _ = writeln!(
                writer,
                "events={events} matches={matched} last_event={age} buffer={} held_mods={} matches_loaded={}{last_chars}",
                shared.diag.buffer_len.load(Ordering::Relaxed),
                if mods.is_empty() {
                    "none".to_string()
                } else {
                    mods.join(",")
                },
                shared.matches.load(Ordering::Relaxed)
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

    if let Some(trigger) = selection {
        let _ = tx.send(Event::Select { trigger, target });
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

struct Engine {
    root: PathBuf,
    entries: Vec<MatchEntry>,
    index: Index,
    decoder: Decoder,
    buffer: String,
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
            buffer: String::new(),
            last_focus: None,
            last_focus_class: String::new(),
            mods: Mods::default(),
            job_tx,
            shared,
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Key { code, value } => self.on_key(code, value),
            Event::Layout(layout) => {
                self.decoder.set_layout(&layout);
                self.buffer.clear();
                self.mods.reset();
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
                    self.decoder.reset_modifiers();
                }
            }
            Event::DeviceChanged(_) => {
                // A keyboard that re-enumerated can swallow a key release.
                self.mods.reset();
                self.decoder.reset_modifiers();
                self.buffer.clear();
            }
            Event::Reload => self.reload(),
            Event::Select { trigger, target } => self.select(&trigger, target.as_deref()),
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

    fn select(&mut self, trigger: &str, target: Option<&str>) {
        let Some(entry) = self.entries.iter().find(|e| e.trigger == trigger) else {
            eprintln!("hyprspanso: selected trigger no longer exists: {trigger}");
            return;
        };
        if entry.form.is_some() {
            let address = target
                .filter(|address| !address.is_empty())
                .map(str::to_string)
                .or_else(|| crate::hypr::active_window().map(|(_, _, a)| a))
                .unwrap_or_default();
            if let Err(err) = launch_form_window(&entry.trigger, &address) {
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

        let _ = self.job_tx.send(Job {
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

        let _ = self.job_tx.send(Job {
            text,
            backspaces: 0,
            clipboard,
        });
    }

    fn on_key(&mut self, code: u16, value: i32) {
        self.shared.diag.events.fetch_add(1, Ordering::Relaxed);
        self.shared
            .diag
            .last_event_ms
            .store(now_ms(), Ordering::Relaxed);
        let key = KeyCode(code);
        let text = self.decoder.feed(code, value);

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
        if hit.entry.form.is_some() {
            // Erase the typed trigger first, like espanso does, then collect
            // the field values in a separate window.
            let backspaces = hit.backspaces;
            let trigger = hit.entry.trigger.clone();
            let target = crate::hypr::active_window()
                .map(|(_, _, address)| address)
                .unwrap_or_default();
            self.buffer.clear();
            let _ = self.job_tx.send(Job {
                text: String::new(),
                backspaces,
                clipboard: false,
            });
            if let Err(err) = launch_form_window(&trigger, &target) {
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
        let _ = self.job_tx.send(Job {
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

fn launch_form_window(trigger: &str, target: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let mut args: Vec<String> = vec![
        exe,
        "form".into(),
        "--trigger".into(),
        trigger.into(),
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
