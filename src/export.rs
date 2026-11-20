//! Export snippets for use on a phone.
//!
//! The target is LeanType (F-Droid, GPL-3.0), whose "bulk editor" is plain text,
//! one block per shortcut:
//!
//! ```text
//! === :rm ===
//! Ricardo Molina Peña
//!
//! === :date ===
//! %date:yyyy-MM-dd%
//! ```

use crate::config::{self, FormFieldKind, MatchEntry, VarKind};
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

pub fn run(args: &[String]) -> Result<()> {
    let format = flag(args, "--format").unwrap_or_else(|| "leantype".into());
    let root = config::config_root();

    // Default destination: next to the match files, so a synced config folder
    // carries the export with it. `--stdout` prints instead.
    let output = if args.iter().any(|arg| arg == "--stdout") {
        None
    } else {
        Some(
            flag(args, "--output")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("hyprspanso-snippets.txt")),
        )
    };
    let forms = match flag(args, "--forms").unwrap_or_else(|| "placeholder".into()).as_str() {
        "placeholder" => FormMode::Placeholder,
        "clipboard" => FormMode::Clipboard,
        "empty" => FormMode::Empty,
        other => bail!("unknown --forms {other:?} (placeholder, clipboard, empty)"),
    };
    let number_duplicates = match flag(args, "--duplicates")
        .unwrap_or_else(|| "number".into())
        .as_str()
    {
        "number" => true,
        "skip" => false,
        other => bail!("unknown --duplicates {other:?} (number, skip)"),
    };
    let verbose = args.iter().any(|arg| arg == "-v" || arg == "--verbose");

    let set = config::load(&root);

    let mut report = Report::default();
    let text = match format.as_str() {
        "leantype" | "lean" => leantype(&set.entries, forms, number_duplicates, &mut report),
        "tsv" | "tab" => tsv(&set.entries, forms, number_duplicates, &mut report),
        "json" => json(&set.entries, forms, number_duplicates, &mut report),
        other => bail!("unknown --format {other:?} (leantype, tsv, json)"),
    };

    match &output {
        Some(path) => {
            fs::write(path, &text)?;
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
    if verbose {
        for (reason, trigger) in &report.skipped {
            eprintln!("        skipped {trigger:?}: {reason}");
        }
    }
    if report.numbered > 0 {
        eprintln!(
            "  {:4}  duplicate trigger(s) numbered (airfm, airfm2, airfm3, …)",
            report.numbered
        );
    }
    if report.variants > 0 {
        eprintln!(
            "  {:4}  extra variant(s) from choice/random variables",
            report.variants
        );
    }
    if report.forms > 0 {
        eprintln!(
            "  {:4}  form template(s) exported {}",
            report.forms,
            match forms {
                FormMode::Placeholder => "(fill the [[placeholders]] after inserting)",
                FormMode::Clipboard => "(placeholders became %clipboard%: copy the value, then expand)",
                FormMode::Empty => "(placeholders removed)",
            }
        );
    }
    if report.trimmed > 0 {
        eprintln!(
            "  {:4}  whitespace removed from the trigger",
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormMode {
    Placeholder,
    Clipboard,
    Empty,
}

#[derive(Default)]
struct Report {
    exported: usize,
    trimmed: usize,
    forms: usize,
    numbered: usize,
    variants: usize,
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

/// Phone keyboards allow only one shortcut per name, so repeated triggers are
/// exported with a number: `airfm`, `airfm2`, `airfm3`, … The first keeps the
/// plain name so the most-used variant stays the shortest to type.
fn number_trigger(
    base: &str,
    used: &mut HashSet<String>,
    next: &mut HashMap<String, usize>,
    report: &mut Report,
) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    report.numbered += 1;
    let mut n = next.get(base).copied().unwrap_or(1) + 1;
    loop {
        let candidate = format!("{base}{n}");
        if used.insert(candidate.clone()) {
            next.insert(base.to_string(), n);
            return candidate;
        }
        n += 1;
    }
}

fn clean_trigger(trigger: &str, report: &mut Report) -> Option<String> {
    let cleaned: String = trigger.chars().filter(|ch| !ch.is_whitespace()).collect();
    if cleaned.is_empty() {
        report.skip("empty trigger", trigger);
        return None;
    }
    if cleaned != trigger {
        report.trimmed += 1;
    }
    Some(cleaned)
}

/// All texts one match can produce. Static matches give one; `choice`/`random`
/// variables give one per value, so every option survives the export.
fn variants(entry: &MatchEntry, forms: FormMode) -> Result<Vec<String>, &'static str> {
    if let Some(form) = &entry.form {
        let mut out = form.template.clone();
        for field in &form.fields {
            let value = match &field.kind {
                FormFieldKind::Choice { values, .. } => {
                    values.first().cloned().unwrap_or_default()
                }
                _ => match forms {
                    FormMode::Placeholder => format!("[[{}]]", field.name),
                    FormMode::Clipboard => "%clipboard%".to_string(),
                    FormMode::Empty => String::new(),
                },
            };
            out = out.replace(&format!("[[{}]]", field.name), &value);
        }
        return Ok(vec![out]);
    }

    let mut base = entry.replace.clone();
    let mut choices: Vec<(String, Vec<String>)> = Vec::new();

    for var in &entry.vars {
        let placeholder = format!("{{{{{}}}}}", var.name);
        match &var.kind {
            VarKind::Date { format, offset } => {
                if *offset != 0 {
                    return Err("date offset not portable");
                }
                base = base.replace(&placeholder, &date_placeholders(format)?);
            }
            VarKind::Echo { echo } => {
                base = base.replace(&placeholder, echo);
            }
            VarKind::Choice { values } | VarKind::Random { values } => {
                choices.push((var.name.clone(), values.clone()));
            }
        }
    }

    // Cartesian product of the choice values, capped so a pathological match
    // cannot produce hundreds of shortcuts.
    let mut combos: Vec<HashMap<String, String>> = vec![HashMap::new()];
    for (name, values) in &choices {
        let mut next_combos = Vec::new();
        for combo in &combos {
            for value in values {
                let mut copy = combo.clone();
                copy.insert(name.clone(), value.clone());
                next_combos.push(copy);
            }
        }
        if next_combos.is_empty() {
            return Err("choice variable without values");
        }
        if next_combos.len() > 64 {
            return Err("too many choice combinations");
        }
        combos = next_combos;
    }

    let mut out = Vec::with_capacity(combos.len());
    for combo in combos {
        let mut text = base.clone();
        for (name, value) in &combo {
            text = text.replace(&format!("{{{{{}}}}}", name), value);
        }
        if text.contains("{{") {
            return Err("unresolved placeholder");
        }
        out.push(text);
    }
    Ok(out)
}

/// strftime -> LeanType's `%date:java-pattern%`, leaving literals outside the
/// token so formats may contain emoji or arbitrary text:
/// `%F %H horas %M min ➡️` -> `%date:yyyy-MM-dd% %date:HH% horas %date:mm% min ➡️`.
fn date_placeholders(format: &str) -> Result<String, &'static str> {
    let mut out = String::new();
    let mut pattern = String::new();
    let mut chars = format.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            // Keep punctuation/space inside the token when another specifier
            // follows, so `%Y-%m-%d` becomes one `%date:yyyy-MM-dd%`. Letters
            // must stay outside or SimpleDateFormat reads them as patterns.
            let safe = !ch.is_alphabetic() && " _():,./-".contains(ch);
            if safe && !pattern.is_empty() && chars.peek() == Some(&'%') {
                pattern.push(ch);
                continue;
            }
            if !pattern.is_empty() {
                out.push_str(&format!("%date:{pattern}%"));
                pattern.clear();
            }
            out.push(ch);
            continue;
        }
        let spec = chars.next().ok_or("date format ends with %")?;
        let mapped = match spec {
            'Y' => "yyyy",
            'y' => "yy",
            'm' => "MM",
            'd' => "dd",
            'e' => "d",
            'H' => "HH",
            'I' => "hh",
            'M' => "mm",
            'S' => "ss",
            'p' => "a",
            'A' => "EEEE",
            'a' => "EEE",
            'B' => "MMMM",
            'b' | 'h' => "MMM",
            'j' => "DDD",
            'F' => "yyyy-MM-dd",
            'T' => "HH:mm:ss",
            'R' => "HH:mm",
            'D' => "MM/dd/yy",
            '%' => "%",
            _ => return Err("date format not portable"),
        };
        pattern.push_str(mapped);
    }

    if !pattern.is_empty() {
        out.push_str(&format!("%date:{pattern}%"));
    }
    Ok(out)
}

fn leantype(
    entries: &[MatchEntry],
    forms: FormMode,
    number_duplicates: bool,
    report: &mut Report,
) -> String {
    let mut used: HashSet<String> = HashSet::new();
    let mut next: HashMap<String, usize> = HashMap::new();
    let mut out = String::new();

    for entry in entries {
        let Some(base) = clean_trigger(&entry.trigger, report) else {
            continue;
        };
        let texts = match variants(entry, forms) {
            Ok(texts) => texts,
            Err(reason) => {
                report.skip(reason, &base);
                continue;
            }
        };

        for (index, text) in texts.iter().enumerate() {
            let trigger = if number_duplicates {
                if index == 0 {
                    number_trigger(&base, &mut used, &mut next, report)
                } else {
                    report.variants += 1;
                    number_trigger(&base, &mut used, &mut next, report)
                }
            } else if index == 0 {
                if !used.insert(base.clone()) {
                    report.skip("duplicate trigger (kept the first)", &base);
                    continue;
                }
                base.clone()
            } else {
                // With --duplicates skip, only the first variant is kept.
                continue;
            };

            if text.trim().is_empty() {
                report.skip("empty replacement", &trigger);
                continue;
            }
            out.push_str(&format!("=== {trigger} ===\n{text}\n\n"));
            report.exported += 1;
            if entry.form.is_some() {
                report.forms += 1;
            }
        }
    }
    out
}

fn tsv(
    entries: &[MatchEntry],
    forms: FormMode,
    number_duplicates: bool,
    report: &mut Report,
) -> String {
    let mut used: HashSet<String> = HashSet::new();
    let mut next: HashMap<String, usize> = HashMap::new();
    let mut out = String::new();

    for entry in entries {
        let Some(base) = clean_trigger(&entry.trigger, report) else {
            continue;
        };
        let texts = match variants(entry, forms) {
            Ok(texts) => texts,
            Err(reason) => {
                report.skip(reason, &base);
                continue;
            }
        };
        for text in texts {
            let trigger = if number_duplicates {
                number_trigger(&base, &mut used, &mut next, report)
            } else {
                if !used.insert(base.clone()) {
                    report.skip("duplicate trigger (kept the first)", &base);
                    break;
                }
                base.clone()
            };
            let flat = text
                .replace('\\', "\\\\")
                .replace('\t', "\\t")
                .replace('\n', "\\n");
            out.push_str(&format!("{trigger}\t{flat}\n"));
            report.exported += 1;
        }
    }
    out
}

fn json(
    entries: &[MatchEntry],
    forms: FormMode,
    number_duplicates: bool,
    report: &mut Report,
) -> String {
    let mut used: HashSet<String> = HashSet::new();
    let mut next: HashMap<String, usize> = HashMap::new();
    let mut items = Vec::new();

    for entry in entries {
        let Some(base) = clean_trigger(&entry.trigger, report) else {
            continue;
        };
        let texts = match variants(entry, forms) {
            Ok(texts) => texts,
            Err(reason) => {
                report.skip(reason, &base);
                continue;
            }
        };
        for text in texts {
            let trigger = if number_duplicates {
                number_trigger(&base, &mut used, &mut next, report)
            } else {
                if !used.insert(base.clone()) {
                    report.skip("duplicate trigger (kept the first)", &base);
                    break;
                }
                base.clone()
            };
            items.push(serde_json::json!({
                "trigger": trigger,
                "replace": text,
                "word": entry.word,
                "propagate_case": entry.propagate_case,
            }));
            report.exported += 1;
        }
    }
    let text = serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into());
    text + "\n"
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
    fn date_fragments_keep_literals_outside() {
        assert_eq!(
            date_placeholders("%F %H horas %M min ➡️").as_deref(),
            Ok("%date:yyyy-MM-dd HH% horas %date:mm% min ➡️")
        );
        assert_eq!(
            date_placeholders("%Y-%m-%d").as_deref(),
            Ok("%date:yyyy-MM-dd%")
        );
        assert!(date_placeholders("%Q").is_err());
    }

    #[test]
    fn choice_variables_become_numbered_variants() {
        let mut e = entry("rerere", "{{output}}");
        e.vars = vec![crate::config::Var {
            name: "output".into(),
            kind: VarKind::Choice {
                values: vec!["one".into(), "two".into()],
            },
        }];
        let mut report = Report::default();
        let text = leantype(&[e], FormMode::Placeholder, true, &mut report);
        assert!(text.contains("=== rerere ===\none\n"));
        assert!(text.contains("=== rerere2 ===\ntwo\n"));
        assert_eq!(report.exported, 2);
        assert_eq!(report.variants, 1);
    }

    #[test]
    fn forms_are_exported_as_templates() {
        use crate::config::{FormField, FormSpec};

        let mut form = entry("inforecepcion", "");
        form.form = Some(FormSpec {
            template: "Hola [[name]]! Bienvenido".into(),
            fields: vec![FormField {
                name: "name".into(),
                kind: FormFieldKind::Text,
            }],
        });

        let mut report = Report::default();
        let placeholder = leantype(
            &[form.clone()],
            FormMode::Placeholder,
            true,
            &mut report,
        );
        assert!(placeholder.contains("Hola [[name]]! Bienvenido"));

        let clipboard =
            leantype(&[form.clone()], FormMode::Clipboard, true, &mut report);
        assert!(clipboard.contains("Hola %clipboard%! Bienvenido"));

        let empty = leantype(&[form], FormMode::Empty, true, &mut report);
        assert!(empty.contains("Hola ! Bienvenido"));
    }

    #[test]
    fn whitespace_is_removed_from_triggers() {
        let entries = vec![entry("with space", "collapsed"), entry("trailing ", "x")];
        let mut report = Report::default();
        let text = leantype(&entries, FormMode::Placeholder, true, &mut report);
        assert!(text.contains("=== withspace ===\ncollapsed\n"));
        assert!(text.contains("=== trailing ===\nx\n"));
        assert_eq!(report.trimmed, 2);
    }
}
