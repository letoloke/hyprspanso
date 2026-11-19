//! Read-only, espanso-compatible match loading.
//!
//! Only `trigger`/`triggers` + `replace` are supported. Anything that needs
//! espanso's engine (vars, forms, scripts, images, html/markdown) is reported
//! as skipped instead of silently disappearing.

use anyhow::Result;
use serde_yaml::Value;
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
    /// An interactive `form:` match: the user fills fields, then the template
    /// is rendered and inserted.
    pub form: Option<FormSpec>,
    /// espanso's `label`: shown instead of the replacement in the search and
    /// disambiguation windows.
    pub label: Option<String>,
}

/// An espanso `form:` match.
#[derive(Debug, Clone)]
pub struct FormSpec {
    pub template: String,
    pub fields: Vec<FormField>,
}

#[derive(Debug, Clone)]
pub struct FormField {
    pub name: String,
    pub kind: FormFieldKind,
}

#[derive(Debug, Clone)]
pub enum FormFieldKind {
    Text,
    Multiline,
    Choice { values: Vec<String>, list: bool },
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
    /// `type: choice` — the user picks one of `values` in a window.
    Choice { values: Vec<String> },
    /// `type: random` — one of `values` is picked automatically.
    Random { values: Vec<String> },
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

/// Where the path came from, shown by `hyprspanso config` and the daemon log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    Env,
    Settings,
    Default,
}

impl RootSource {
    pub fn label(self) -> &'static str {
        match self {
            RootSource::Env => "HYPRSPANSO_CONFIG",
            RootSource::Settings => "saved setting",
            RootSource::Default => "default",
        }
    }
}

/// hyprspanso's own files (settings, not your snippets).
pub fn state_dir() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(&p);
        if !p.trim().is_empty() && path.is_absolute() {
            return path.join("hyprspanso");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/hyprspanso")
}

pub fn settings_path() -> PathBuf {
    state_dir().join("config.json")
}

#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// espanso-style directory whose `match/` subdirectory holds the YAML.
    pub config_dir: Option<PathBuf>,
}

pub fn load_settings() -> Settings {
    let path = settings_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return Settings::default();
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("hyprspanso: ignoring {}: {err}", path.display());
            return Settings::default();
        }
    };
    Settings {
        config_dir: value
            .get("config_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty()),
    }
}

pub fn save_settings(settings: &Settings) -> Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let value = serde_json::json!({
        "config_dir": settings
            .config_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
    });
    fs::write(&path, serde_json::to_string_pretty(&value)? + "\n")?;
    Ok(())
}

/// `~/x`, `~`, relative and absolute paths from the command line.
pub fn expand_path(input: &str) -> PathBuf {
    let trimmed = input.trim();
    let home = std::env::var("HOME").unwrap_or_default();
    if trimmed == "~" {
        return PathBuf::from(home);
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return PathBuf::from(home).join(rest);
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

pub fn resolve_root() -> (PathBuf, RootSource) {
    if let Ok(p) = std::env::var("HYPRSPANSO_CONFIG") {
        if !p.trim().is_empty() {
            return (PathBuf::from(p), RootSource::Env);
        }
    }
    if let Some(dir) = load_settings().config_dir {
        return (dir, RootSource::Settings);
    }
    (default_root(), RootSource::Default)
}

pub fn config_root() -> PathBuf {
    resolve_root().0
}

fn default_root() -> PathBuf {
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

    // Duplicate triggers are kept: they form a disambiguation group, like
    // espanso's picker.
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

        let form = parse_form(&map);

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

        let mut replace = map.get("replace").and_then(scalar).unwrap_or_default();
        if form.is_none() && !map.contains_key("replace") {
            let trigger = first_trigger(&map).unwrap_or_else(|| "<no trigger>".into());
            set.skipped.push(Skipped {
                trigger,
                reason: "no `replace` or `form` value".into(),
                source: source.clone(),
            });
            continue;
        }

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
                form: form.clone(),
                label: map.get("label").and_then(scalar),
            });
        }
    }
}

fn unsupported(map: &serde_yaml::Mapping) -> Option<String> {
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

/// Parse `form:` + `form_fields:`. Field order follows the placeholders in the
/// template, because that is the order the user sees in espanso.
fn parse_form(map: &serde_yaml::Mapping) -> Option<FormSpec> {
    let template = map.get("form").and_then(scalar)?;
    let mut template = template;
    while template.ends_with('\n') || template.ends_with('\r') {
        template.pop();
    }

    let defs = match map.get("form_fields") {
        Some(Value::Mapping(defs)) => defs.clone(),
        _ => serde_yaml::Mapping::new(),
    };

    let mut fields: Vec<FormField> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rest = template.as_str();

    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let token = &after[..end];
        rest = &after[end + 2..];

        let name = token.split('|').next().unwrap_or("").trim().to_string();
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }

        let kind = match defs.get(name.as_str()) {
            Some(Value::Mapping(definition)) => {
                let field_type = definition
                    .get("type")
                    .and_then(scalar)
                    .unwrap_or_default();
                let multiline = definition
                    .get("multiline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                match field_type.as_str() {
                    "choice" | "list" => {
                        let values = match definition.get("values") {
                            Some(Value::Sequence(items)) => {
                                items.iter().filter_map(scalar).collect()
                            }
                            _ => Vec::new(),
                        };
                        FormFieldKind::Choice {
                            values,
                            list: field_type == "list",
                        }
                    }
                    _ if multiline => FormFieldKind::Multiline,
                    _ => FormFieldKind::Text,
                }
            }
            _ => FormFieldKind::Text,
        };

        fields.push(FormField { name, kind });
    }

    Some(FormSpec { template, fields })
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
            "choice" | "random" => {
                let key = if params.contains_key("values") {
                    "values"
                } else {
                    "choices"
                };
                let values = match params.get(key) {
                    Some(Value::Sequence(items)) => {
                        items.iter().filter_map(scalar).collect::<Vec<_>>()
                    }
                    _ => Vec::new(),
                };
                if values.is_empty() {
                    return Err(format!("`vars` type `{kind}` without values"));
                }
                if kind == "choice" {
                    VarKind::Choice { values }
                } else {
                    VarKind::Random { values }
                }
            }
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
    println!(
        "duplicates  : {} output group(s), {} trigger group(s), {} exact",
        duplicate_output_groups(&set.entries).len(),
        duplicate_trigger_groups(&set.entries).len(),
        exact_duplicate_groups(&set.entries).len()
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_path_keeps_absolute() {
        assert_eq!(expand_path("/tmp/snippets"), PathBuf::from("/tmp/snippets"));
    }

    #[test]
    fn expand_path_makes_relative_absolute() {
        let path = expand_path("relative/dir");
        assert!(path.is_absolute(), "{path:?} should be absolute");
        assert!(path.ends_with("relative/dir"));
    }
}

/// Index groups of matches that share the same replacement text.
/// Forms have no static output and are ignored.
pub fn duplicate_output_groups(entries: &[MatchEntry]) -> Vec<Vec<usize>> {
    use std::collections::HashMap;
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.form.is_some() {
            continue;
        }
        // `{{vars}}` render differently every time; not comparable as output.
        if entry.replace.contains("{{") {
            continue;
        }
        groups.entry(entry.replace.as_str()).or_default().push(index);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().filter(|group| group.len() > 1).collect();
    out.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| entries[a[0]].replace.cmp(&entries[b[0]].replace))
    });
    out
}

/// Index groups of matches that share the same trigger (disambiguation groups).
pub fn duplicate_trigger_groups(entries: &[MatchEntry]) -> Vec<Vec<usize>> {
    use std::collections::HashMap;
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        groups.entry(entry.trigger.as_str()).or_default().push(index);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().filter(|group| group.len() > 1).collect();
    out.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| entries[a[0]].trigger.cmp(&entries[b[0]].trigger))
    });
    out
}

/// Index groups that are truly redundant: same trigger *and* same output.
pub fn exact_duplicate_groups(entries: &[MatchEntry]) -> Vec<Vec<usize>> {
    use std::collections::HashMap;
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        groups
            .entry((entry.trigger.as_str(), entry.replace.as_str()))
            .or_default()
            .push(index);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().filter(|group| group.len() > 1).collect();
    out.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| entries[a[0]].trigger.cmp(&entries[b[0]].trigger))
    });
    out
}

#[cfg(test)]
mod duplicate_tests {
    use super::*;

    fn entry(trigger: &str, replace: &str) -> MatchEntry {
        MatchEntry {
            trigger: trigger.into(),
            replace: replace.into(),
            word: false,
            propagate_case: false,
            force_clipboard: false,
            source: "test".into(),
            path: PathBuf::from("test.yml"),
            vars: Vec::new(),
            form: None,
            label: None,
        }
    }

    #[test]
    fn groups_identical_outputs_only() {
        let entries = vec![
            entry("a", "same"),
            entry("b", "same"),
            entry("c", "other"),
            entry("d", "{{dynamic}}"),
            entry("e", "{{dynamic}}"),
        ];
        let groups = duplicate_output_groups(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![0, 1]);
    }

    #[test]
    fn groups_shared_triggers_and_exact_pairs() {
        let entries = vec![
            entry("x", "one"),
            entry("x", "two"),
            entry("y", "two"),
            entry("y", "two"),
        ];
        assert_eq!(duplicate_trigger_groups(&entries).len(), 2);
        let exact = exact_duplicate_groups(&entries);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0], vec![2, 3]);
    }
}
