//! Read-only, espanso-compatible match loading.
//!
//! Only `trigger`/`triggers` + `replace` are supported. Anything that needs
//! espanso's engine (vars, forms, scripts, images, html/markdown) is reported
//! as skipped instead of silently disappearing.

use anyhow::Result;
use serde_yaml::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct MatchEntry {
    pub trigger: String,
    pub replace: String,
    pub word: bool,
    pub propagate_case: bool,
    pub force_clipboard: bool,
    /// File name, for display.
    pub source: String,
    /// Full path, so the search window can open it in an editor.
    pub path: PathBuf,
    /// espanso-style dynamic variables rendered at expansion time.
    pub vars: Vec<Var>,
}

/// A `vars:` entry from an espanso match file.
#[derive(Debug, Clone)]
pub struct Var {
    pub name: String,
    pub kind: VarKind,
}

#[derive(Debug, Clone)]
pub enum VarKind {
    /// `type: date` — strftime `format`, plus an optional `offset` in seconds.
    Date { format: String, offset: i64 },
    /// `type: echo` — literal text.
    Echo { echo: String },
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub trigger: String,
    pub reason: String,
    pub source: String,
}

#[derive(Debug, Default)]
pub struct MatchSet {
    pub entries: Vec<MatchEntry>,
    pub skipped: Vec<Skipped>,
    pub warnings: Vec<String>,
    pub files: Vec<PathBuf>,
}

impl MatchSet {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} matches from {} files",
            self.entries.len(),
            self.files.len()
        );
        if !self.skipped.is_empty() {
            s.push_str(&format!(", {} skipped", self.skipped.len()));
        }
        s
    }
}

pub fn config_root() -> PathBuf {
    if let Ok(p) = std::env::var("HYPRSPANSO_CONFIG") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        // The spec says relative values must be ignored, and an empty
        // variable is common in shell scripts.
        let path = PathBuf::from(&p);
        if !p.trim().is_empty() && path.is_absolute() {
            return path.join("espanso");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/espanso")
}

pub fn match_dir(root: &Path) -> PathBuf {
    root.join("match")
}

pub fn load(root: &Path) -> MatchSet {
    let mut set = MatchSet::default();
    let dir = match_dir(root);

    if !dir.is_dir() {
        set.warnings.push(format!(
            "no match directory at {} (set HYPRSPANSO_CONFIG to override)",
            dir.display()
        ));
        return set;
    }

    let mut files = Vec::new();
    collect(&dir, &mut files);
    files.sort();

    for file in &files {
        parse_file(file, &mut set);
    }

    // First definition wins, like espanso's "last loaded" is undefined anyway.
    let mut seen = HashSet::new();
    let mut kept = Vec::with_capacity(set.entries.len());
    for entry in set.entries.drain(..) {
        if seen.insert(entry.trigger.clone()) {
            kept.push(entry);
        } else {
            set.skipped.push(Skipped {
                trigger: entry.trigger,
                reason: "duplicate trigger (kept the first one)".into(),
                source: entry.source,
            });
        }
    }
    set.entries = kept;
    set.files = files;
    set
}

/// Cheap fingerprint of the match tree, used to detect edits without inotify
/// (which is unreliable on some synced folders like Insync/Google Drive).
pub fn snapshot(root: &Path) -> Vec<(PathBuf, u64, u64)> {
    let dir = match_dir(root);
    let mut files = Vec::new();
    collect(&dir, &mut files);
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let (mtime, len) = fs::metadata(&p)
                .ok()
                .map(|m| {
                    let t = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    (t, m.len())
                })
                .unwrap_or((0, 0));
            (p, mtime, len)
        })
        .collect()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect(&path, out);
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "yml" || ext == "yaml" {
            out.push(path);
        }
    }
}

fn parse_file(path: &Path, set: &mut MatchSet) {
    let source = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            set.warnings.push(format!("{}: {e}", path.display()));
            return;
        }
    };

    let doc: Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            // Insync can expose a half-written file for a moment; keep the
            // previous load instead of crashing the daemon.
            set.warnings
                .push(format!("{}: YAML error: {e}", path.display()));
            return;
        }
    };

    let items: Vec<Value> = match &doc {
        Value::Sequence(seq) => seq.clone(),
        Value::Mapping(map) => match map.get("matches") {
            Some(Value::Sequence(seq)) => seq.clone(),
            // A file whose matches are all commented out is valid and empty.
            Some(Value::Null) | None => return,
            Some(_) => {
                set.warnings
                    .push(format!("{}: `matches:` is not a list", path.display()));
                return;
            }
        },
        _ => return,
    };

    for item in items {
        let Value::Mapping(map) = item else {
            continue;
        };

        let vars = match parse_vars(&map) {
            Ok(vars) => vars,
            Err(reason) => {
                let trigger = first_trigger(&map).unwrap_or_else(|| "<no trigger>".into());
                set.skipped.push(Skipped {
                    trigger,
                    reason,
                    source: source.clone(),
                });
                continue;
            }
        };

        if let Some(reason) = unsupported(&map) {
            let trigger = first_trigger(&map).unwrap_or_else(|| "<no trigger>".into());
            set.skipped.push(Skipped {
                trigger,
                reason,
                source: source.clone(),
            });
            continue;
        }

        let Some(mut replace) = map.get("replace").and_then(scalar) else {
            let trigger = first_trigger(&map).unwrap_or_else(|| "<no trigger>".into());
            set.skipped.push(Skipped {
                trigger,
                reason: "no `replace` value".into(),
                source: source.clone(),
            });
            continue;
        };

        // `replace: |` keeps a trailing newline; espanso does not type it.
        while replace.ends_with('\n') || replace.ends_with('\r') {
            replace.pop();
        }

        let word = map.get("word").and_then(Value::as_bool).unwrap_or(false);
        let propagate_case = map
            .get("propagate_case")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let force_clipboard = map
            .get("force_clipboard")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        for trigger in triggers(&map) {
            if trigger.trim().is_empty() {
                continue;
            }
            set.entries.push(MatchEntry {
                trigger,
                replace: replace.clone(),
                word,
                propagate_case,
                force_clipboard,
                source: source.clone(),
                path: path.to_path_buf(),
                vars: vars.clone(),
            });
        }
    }
}

fn unsupported(map: &serde_yaml::Mapping) -> Option<String> {
    if map.contains_key("form") {
        return Some("uses `form` (interactive form)".into());
    }
    if let Some(t) = map.get("type") {
        let kind = scalar(t).unwrap_or_default();
        if kind != "text" {
            return Some(format!("uses `type: {kind}`"));
        }
    }
    for key in ["script", "image", "html", "markdown", "choices", "params"] {
        if map.contains_key(key) {
            return Some(format!("uses `{key}`"));
        }
    }
    None
}

/// Only the variable types we can evaluate without the espanso engine. Anything
/// else skips the match instead of expanding `{{name}}` literally.
fn parse_vars(map: &serde_yaml::Mapping) -> Result<Vec<Var>, String> {
    let Some(Value::Sequence(seq)) = map.get("vars") else {
        return Ok(Vec::new());
    };

    let mut vars = Vec::with_capacity(seq.len());
    for item in seq {
        let Value::Mapping(spec) = item else {
            return Err("malformed `vars` entry".into());
        };
        let name = spec
            .get("name")
            .and_then(scalar)
            .ok_or_else(|| "var without a `name`".to_string())?;
        let kind = spec.get("type").and_then(scalar).unwrap_or_default();
        let params = match spec.get("params") {
            Some(Value::Mapping(params)) => params.clone(),
            _ => serde_yaml::Mapping::new(),
        };

        let var_kind = match kind.as_str() {
            "date" => {
                let format = params
                    .get("format")
                    .and_then(scalar)
                    .unwrap_or_else(|| "%Y-%m-%d".to_string());
                let offset = params
                    .get("offset")
                    .and_then(|value| match value {
                        Value::Number(number) => number.as_i64(),
                        Value::String(text) => text.trim().parse::<i64>().ok(),
                        _ => None,
                    })
                    .unwrap_or(0);
                VarKind::Date { format, offset }
            }
            "echo" => VarKind::Echo {
                echo: params.get("echo").and_then(scalar).unwrap_or_default(),
            },
            other => return Err(format!("uses `vars` type `{other}`")),
        };

        vars.push(Var {
            name,
            kind: var_kind,
        });
    }
    Ok(vars)
}

fn first_trigger(map: &serde_yaml::Mapping) -> Option<String> {
    triggers(map).into_iter().next()
}

fn triggers(map: &serde_yaml::Mapping) -> Vec<String> {
    if let Some(t) = map.get("trigger").and_then(scalar) {
        return vec![t];
    }
    if let Some(Value::Sequence(seq)) = map.get("triggers") {
        return seq.iter().filter_map(scalar).collect();
    }
    Vec::new()
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some(String::new()),
        _ => None,
    }
}

/// Load and print a summary, used by `hyprspanso check`.
pub fn check() -> Result<()> {
    let root = config_root();
    let set = load(&root);
    println!("config root : {}", root.display());
    println!("matches dir : {}", match_dir(&root).display());
    println!("files       : {}", set.files.len());
    println!("matches     : {}", set.entries.len());
    println!("skipped     : {}", set.skipped.len());

    if !set.warnings.is_empty() {
        println!("\nwarnings:");
        for w in &set.warnings {
            println!("  ! {w}");
        }
    }
    if !set.skipped.is_empty() {
        println!("\nskipped (unsupported by hyprspanso):");
        for s in &set.skipped {
            println!("  - {:40} {} [{}]", s.trigger, s.reason, s.source);
        }
    }
    Ok(())
}
