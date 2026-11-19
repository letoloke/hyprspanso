//! Export snippets for use on a phone.
//!
//! The common target is LeanType (F-Droid, GPL-3.0), whose "all shortcuts as
//! text" editor uses one block per shortcut:
//!
//! ```text
//! === :rm ===
//! Ricardo Molina Peña
//!
//! === :date ===
//! %date:yyyy-MM-dd%
//! ```

use crate::config::{self, MatchEntry, VarKind};
use anyhow::{bail, Result};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

pub fn run(args: &[String]) -> Result<()> {
    let format = flag(args, "--format").unwrap_or_else(|| "leantype".into());
    let output = flag(args, "--output").map(PathBuf::from);

    let root = config::config_root();
    let set = config::load(&root);

    let (text, report) = match format.as_str() {
        "leantype" | "lean" => leantype(&set.entries),
        "tsv" | "tab" => tsv(&set.entries),
        "json" => json(&set.entries),
        other => bail!("unknown --format {other:?} (leantype, tsv, json)"),
    };

    match output {
        Some(path) => {
            fs::write(&path, &text)?;
            eprintln!("wrote {} ({} bytes)", path.display(), text.len());
        }
        None => print!("{text}"),
    }

    eprintln!(
        "exported {} shortcut(s) from {} match(es); skipped {}",
        report.exported,
        set.entries.len(),
        report.skipped.len()
    );
    for (reason, count) in report.reasons() {
        eprintln!("  {count:4}  {reason}");
    }
    if report.trimmed > 0 {
        eprintln!(
            "  {:4}  trailing whitespace removed from the trigger",
            report.trimmed
        );
    }
    Ok(())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

#[derive(Default)]
struct Report {
    exported: usize,
    trimmed: usize,
    skipped: Vec<(&'static str, String)>,
}

impl Report {
    fn skip(&mut self, reason: &'static str, trigger: &str) {
        self.skipped.push((reason, trigger.to_string()));
    }

    fn reasons(&self) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for (reason, _) in &self.skipped {
            match counts.iter_mut().find(|(name, _)| name == reason) {
                Some((_, count)) => *count += 1,
                None => counts.push(((*reason).to_string(), 1)),
            }
        }
        counts
    }
}

/// Phone keyboards reject shortcut names with spaces. espanso triggers often
/// end with one ("ytd ") to force a word boundary, which is safe to drop;
/// triggers with *internal* spaces cannot be represented.
fn clean_trigger(trigger: &str, report: &mut Report) -> Option<String> {
    let trimmed = trigger.trim_end();
    if trimmed.is_empty() {
        report.skip("empty trigger", trigger);
        return None;
    }
    if trimmed.chars().any(char::is_whitespace) {
        report.skip("trigger contains whitespace", trigger);
        return None;
    }
    if trimmed.len() != trigger.len() {
        report.trimmed += 1;
    }
    Some(trimmed.to_string())
}

fn leantype(entries: &[MatchEntry]) -> (String, Report) {
    let mut report = Report::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = String::new();

    for entry in entries {
        let Some(trigger) = clean_trigger(&entry.trigger, &mut report) else {
            continue;
        };
        if !seen.insert(trigger.clone()) {
            report.skip("duplicate trigger (kept the first)", &trigger);
            continue;
        }
        let text = match leantype_body(entry) {
            Ok(text) => text,
            Err(reason) => {
                report.skip(reason, &trigger);
                continue;
            }
        };
        if text.trim().is_empty() {
            report.skip("empty replacement", &trigger);
            continue;
        }
        out.push_str(&format!("=== {trigger} ===\n{text}\n\n"));
        report.exported += 1;
    }

    (out, report)
}

/// Replacement text with espanso variables translated to LeanType's `%date%`.
fn leantype_body(entry: &MatchEntry) -> Result<String, &'static str> {
    if entry.form.is_some() {
        return Err("form match");
    }
    let mut out = entry.replace.clone();
    for var in &entry.vars {
        let value = match &var.kind {
            VarKind::Date { format, offset } => {
                if *offset != 0 {
                    return Err("date offset not portable");
                }
                let pattern = strftime_to_java(format).ok_or("date format not portable")?;
                // LeanType's %date:PATTERN% only accepts this character set.
                let portable = pattern
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || " _():,./-".contains(ch));
                if !portable {
                    return Err("date format not portable");
                }
                format!("%date:{pattern}%")
            }
            _ => return Err("non-date vars/forms"),
        };
        out = out.replace(&format!("{{{{{}}}}}", var.name), &value);
    }
    if out.contains("{{") {
        return Err("unresolved placeholder");
    }
    Ok(out)
}

/// strftime -> java.text.SimpleDateFormat, for the specifiers we support.
fn strftime_to_java(format: &str) -> Option<String> {
    let mut out = String::with_capacity(format.len());
    let mut chars = format.chars();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            'Y' => out.push_str("yyyy"),
            'y' => out.push_str("yy"),
            'm' => out.push_str("MM"),
            'd' => out.push_str("dd"),
            'e' => out.push('d'),
            'H' => out.push_str("HH"),
            'I' => out.push_str("hh"),
            'M' => out.push_str("mm"),
            'S' => out.push_str("ss"),
            'p' => out.push('a'),
            'A' => out.push_str("EEEE"),
            'a' => out.push_str("EEE"),
            'B' => out.push_str("MMMM"),
            'b' | 'h' => out.push_str("MMM"),
            'j' => out.push_str("DDD"),
            'F' => out.push_str("yyyy-MM-dd"),
            'T' => out.push_str("HH:mm:ss"),
            'R' => out.push_str("HH:mm"),
            'D' => out.push_str("MM/dd/yy"),
            '%' => out.push('%'),
            _ => return None,
        }
    }
    Some(out)
}

fn tsv(entries: &[MatchEntry]) -> (String, Report) {
    let mut report = Report::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = String::new();
    for entry in entries {
        let Some(trigger) = clean_trigger(&entry.trigger, &mut report) else {
            continue;
        };
        if !seen.insert(trigger.clone()) {
            report.skip("duplicate trigger (kept the first)", &trigger);
            continue;
        }
        let body = match leantype_body(entry) {
            Ok(body) => body,
            Err(reason) => {
                report.skip(reason, &entry.trigger);
                continue;
            }
        };
        let flat = body.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n");
        out.push_str(&format!("{trigger}\t{flat}\n"));
        report.exported += 1;
    }
    (out, report)
}

fn json(entries: &[MatchEntry]) -> (String, Report) {
    let mut report = Report::default();
    let mut items = Vec::new();
    for entry in entries {
        let Some(trigger) = clean_trigger(&entry.trigger, &mut report) else {
            continue;
        };
        let body = match leantype_body(entry) {
            Ok(body) => body,
            Err(reason) => {
                report.skip(reason, &trigger);
                continue;
            }
        };
        items.push(serde_json::json!({
            "trigger": trigger,
            "replace": body,
            "word": entry.word,
            "propagate_case": entry.propagate_case,
        }));
        report.exported += 1;
    }
    let text = serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into());
    (text + "\n", report)
}

#[cfg(test)]
mod tests {
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
    fn converts_strftime_to_simpledateformat() {
        assert_eq!(strftime_to_java("%Y-%m-%d").as_deref(), Some("yyyy-MM-dd"));
        assert_eq!(strftime_to_java("%H:%M").as_deref(), Some("HH:mm"));
        assert_eq!(strftime_to_java("%F %T").as_deref(), Some("yyyy-MM-dd HH:mm:ss"));
        assert_eq!(strftime_to_java("%Q"), None);
    }

    #[test]
    fn exports_leantype_blocks_and_skips_unsupported() {
        let mut dynamic = entry(":date", "{{d}}");
        dynamic.vars = vec![crate::config::Var {
            name: "d".into(),
            kind: VarKind::Date {
                format: "%Y-%m-%d".into(),
                offset: 0,
            },
        }];
        let entries = vec![
            entry(":rm", "Ricardo"),
            entry("with space", "nope"),
            dynamic,
        ];
        let (text, report) = leantype(&entries);
        assert!(text.contains("=== :rm ===\nRicardo\n"));
        assert!(text.contains("=== :date ===\n%date:yyyy-MM-dd%\n"));
        assert_eq!(report.exported, 2);
        assert_eq!(report.skipped.len(), 1);
    }
}
