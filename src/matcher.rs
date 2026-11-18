//! Trigger matching against a small rolling buffer.

use crate::config::{FormSpec, MatchEntry, VarKind};
use chrono::{DateTime, Duration, Local};
use std::cmp::Reverse;
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;

pub type Index = HashMap<char, Vec<usize>>;

pub const BUFFER_MAX: usize = 64;

/// Candidates are bucketed by the last character of their trigger, longest
/// trigger first, so ":abc" wins over ":ab" without a full trie.
pub fn build_index(entries: &[MatchEntry]) -> Index {
    let mut index: Index = HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(c) = entry.trigger.chars().last() {
            index.entry(c).or_default().push(i);
        }
    }
    for bucket in index.values_mut() {
        bucket.sort_by_key(|&i| Reverse(entries[i].trigger.chars().count()));
    }
    index
}

pub struct Hit<'a> {
    pub entry: &'a MatchEntry,
    pub backspaces: usize,
    pub typed: String,
}

pub fn find<'a>(entries: &'a [MatchEntry], index: &Index, buffer: &str) -> Option<Hit<'a>> {
    let last = buffer.chars().last()?;
    for &i in index.get(&last)? {
        let entry = &entries[i];
        if !buffer.ends_with(&entry.trigger) {
            continue;
        }
        if entry.word {
            let start = buffer.len() - entry.trigger.len();
            if let Some(before) = buffer[..start].chars().last() {
                if before.is_alphanumeric() || before == '_' {
                    continue;
                }
            }
        }
        return Some(Hit {
            entry,
            backspaces: entry.trigger.graphemes(true).count(),
            typed: buffer[buffer.len() - entry.trigger.len()..].to_string(),
        });
    }
    None
}

/// Render `{{name}}` placeholders for the declared `vars`.
pub fn render(entry: &MatchEntry) -> String {
    render_at(entry, Local::now())
}

pub fn render_at(entry: &MatchEntry, now: DateTime<Local>) -> String {
    if entry.vars.is_empty() {
        return entry.replace.clone();
    }
    let mut out = entry.replace.clone();
    for var in &entry.vars {
        let value = match &var.kind {
            VarKind::Date { format, offset } => {
                let stamp = now + Duration::seconds(*offset);
                stamp.format(format).to_string()
            }
            VarKind::Echo { echo } => echo.clone(),
        };
        out = out.replace(&format!("{{{{{}}}}}", var.name), &value);
    }
    out
}

/// Fill a `form:` template with the values the user typed.
pub fn render_form(spec: &FormSpec, values: &HashMap<String, String>) -> String {
    let mut out = spec.template.clone();
    for field in &spec.fields {
        let value = values.get(&field.name).map(String::as_str).unwrap_or("");
        out = out.replace(&format!("[[{}]]", field.name), value);
    }
    out
}

/// `propagate_case`: `:Espanso` -> `Hi there!` capitalized, `:ESPANSO` -> all caps.
pub fn apply_case(text: &str, typed: &str, propagate: bool) -> String {
    if !propagate {
        return text.to_string();
    }
    let letters: Vec<char> = typed.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return text.to_string();
    }
    if letters.iter().all(|c| c.is_uppercase()) {
        return text.to_uppercase();
    }
    if letters.first().map(|c| c.is_uppercase()).unwrap_or(false) {
        let mut chars = text.chars();
        if let Some(first) = chars.next() {
            return first.to_uppercase().collect::<String>() + chars.as_str();
        }
    }
    text.to_string()
}

pub fn trim_buffer(buffer: &mut String) {
    let len = buffer.chars().count();
    if len <= BUFFER_MAX {
        return;
    }
    let drop_chars = len - BUFFER_MAX;
    if let Some((byte, _)) = buffer.char_indices().nth(drop_chars) {
        buffer.drain(..byte);
    }
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
            path: std::path::PathBuf::from("test.yml"),
            vars: Vec::new(),
            form: None,
        }
    }

    #[test]
    fn matches_suffix() {
        let entries = vec![entry(":brb", "be right back"), entry(":b", "b")];
        let index = build_index(&entries);
        let hit = find(&entries, &index, "hello:brb").expect("hit");
        assert_eq!(hit.entry.replace, "be right back");
        assert_eq!(hit.backspaces, 4);
    }

    #[test]
    fn longest_wins() {
        let entries = vec![entry(":a", "A"), entry(":ab", "AB")];
        let index = build_index(&entries);
        let hit = find(&entries, &index, "x:ab").expect("hit");
        assert_eq!(hit.entry.replace, "AB");
    }

    #[test]
    fn word_boundary() {
        let mut e = entry("nnn", "ñ");
        e.word = true;
        let entries = vec![e];
        let index = build_index(&entries);
        assert!(find(&entries, &index, "annn").is_none());
        assert!(find(&entries, &index, " nnn").is_some());
        assert!(find(&entries, &index, "nnn").is_some());
    }

    #[test]
    fn case_propagation() {
        assert_eq!(apply_case("hello", "Hello", true), "Hello");
        assert_eq!(apply_case("hello", "HELLO", true), "HELLO");
        assert_eq!(apply_case("hello", "hello", true), "hello");
        assert_eq!(apply_case("hello", "Hello", false), "hello");
    }

    #[test]
    fn renders_date_and_echo_vars() {
        use chrono::TimeZone;
        use crate::config::{Var, VarKind};

        let mut e = entry(":date", "{{fecha}} a las {{hora}} — {{firma}}");
        e.vars = vec![
            Var {
                name: "fecha".into(),
                kind: VarKind::Date {
                    format: "%F".into(),
                    offset: 0,
                },
            },
            Var {
                name: "hora".into(),
                kind: VarKind::Date {
                    format: "%H:%M".into(),
                    offset: 3600,
                },
            },
            Var {
                name: "firma".into(),
                kind: VarKind::Echo {
                    echo: "Ricardo".into(),
                },
            },
        ];
        let now = Local.with_ymd_and_hms(2026, 11, 17, 14, 30, 0).unwrap();
        assert_eq!(
            render_at(&e, now),
            "2026-11-17 a las 15:30 — Ricardo"
        );
    }

    #[test]
    fn renders_form_fields() {
        use crate::config::{FormField, FormFieldKind};

        let spec = FormSpec {
            template: "Hola [[name]]!\nSala: [[room]]".into(),
            fields: vec![
                FormField {
                    name: "name".into(),
                    kind: FormFieldKind::Text,
                },
                FormField {
                    name: "room".into(),
                    kind: FormFieldKind::Choice {
                        values: vec!["Búho".into(), "Tortuga".into()],
                        list: true,
                    },
                },
            ],
        };
        let mut values = HashMap::new();
        values.insert("name".to_string(), "Ana".to_string());
        values.insert("room".to_string(), "Tortuga".to_string());
        assert_eq!(render_form(&spec, &values), "Hola Ana!\nSala: Tortuga");
    }

    #[test]
    fn leaves_unknown_placeholders_alone() {
        let e = entry("x", "{{missing}}");
        assert_eq!(render(&e), "{{missing}}");
    }

    #[test]
    fn buffer_trim() {
        let mut b = "á".repeat(BUFFER_MAX + 10);
        trim_buffer(&mut b);
        assert_eq!(b.chars().count(), BUFFER_MAX);
    }
}
