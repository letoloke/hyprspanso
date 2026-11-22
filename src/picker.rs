//! Disambiguation picker: several matches share one trigger.
//!
//! The daemon erases the typed trigger and opens this window; choosing a row
//! sends `PICK <trigger> <index>` back and the daemon inserts that entry.

use crate::config::{self, MatchEntry};
use crate::matcher;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub fn run(trigger: Option<String>, var: Option<String>, target: Option<String>) -> Result<()> {
    let trigger = trigger.context("missing --trigger")?;
    let root = config::config_root();
    let set = config::load(&root);

    // A `--var` picker lists the values of one `type: choice` variable;
    // otherwise it disambiguates several matches that share the trigger.
    let (group, values) = if let Some(var) = &var {
        let entry = set
            .entries
            .iter()
            .find(|entry| entry.trigger == trigger && !matcher::choice_values(entry, var).is_empty())
            .with_context(|| format!("no `choice` variable {var:?} for {trigger:?}"))?;
        let values = matcher::choice_values(entry, var);
        (vec![entry.clone()], values)
    } else {
        let group: Vec<MatchEntry> = set
            .entries
            .iter()
            .filter(|entry| entry.trigger == trigger)
            .cloned()
            .collect();
        if group.len() < 2 {
            anyhow::bail!("trigger {trigger:?} has no alternatives");
        }
        (group, Vec::new())
    };

    let sock = crate::daemon::socket_path();
    let mut stream = UnixStream::connect(&sock).ok();
    if let Some(stream) = stream.as_mut() {
        let _ = writeln!(stream, "SEARCH");
    }

    // Preselect the row chosen last time for this trigger.
    let preferred = if var.is_none() {
        remembered_choice(&trigger).and_then(|(index, signature)| {
            group
                .iter()
                .position(|entry| config::entry_signature(entry) == signature)
                .or_else(|| (index < group.len()).then_some(index))
        })
    } else {
        None
    };

    let mut app = PickerApp {
        trigger,
        var,
        target,
        group,
        values,
        selected: preferred.unwrap_or(0),
        list_state: ListState::default().with_selected(preferred.or(Some(0))),
        status: if stream.is_some() {
            "choose · ↑↓ then Enter".to_string()
        } else {
            "daemon not running: nothing will be inserted".to_string()
        },
        stream,
    };

    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

struct PickerApp {
    trigger: String,
    var: Option<String>,
    target: Option<String>,
    group: Vec<MatchEntry>,
    values: Vec<String>,
    selected: usize,
    list_state: ListState,
    status: String,
    stream: Option<UnixStream>,
}

impl PickerApp {
    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if !event::poll(Duration::from_millis(150))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

            match key.code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Enter => {
                    if let Err(err) = self.submit() {
                        self.status = format!("cannot submit: {err}");
                        continue;
                    }
                    return Ok(());
                }
                KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
                KeyCode::Char(c @ '1'..='9') => {
                    let index = c as usize - '1' as usize;
                    if index < self.len() {
                        self.selected = index;
                        self.list_state.select(Some(index));
                        if self.submit().is_ok() {
                            return Ok(());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn len(&self) -> usize {
        if self.var.is_some() {
            self.values.len()
        } else {
            self.group.len()
        }
    }

    fn move_by(&mut self, delta: i32) {
        let len = self.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
        self.list_state.select(Some(self.selected));
    }

    fn submit(&mut self) -> Result<()> {
        if self.stream.is_none() {
            if let Ok(mut stream) = UnixStream::connect(&crate::daemon::socket_path()) {
                let _ = writeln!(stream, "SEARCH");
                self.stream = Some(stream);
            }
        }
        let Some(stream) = self.stream.as_mut() else {
            anyhow::bail!("daemon not running");
        };
        let index = self.selected;
        if self.var.is_none() {
            if let Some(entry) = self.group.get(index) {
                remember_choice(&self.trigger, index, &config::entry_signature(entry));
            }
        }
        if let Some(target) = &self.target {
            writeln!(stream, "TARGET\t{target}")?;
        }
        match &self.var {
            Some(var) => writeln!(stream, "PICKVAR\t{}\t{var}\t{index}", self.trigger)?,
            None => writeln!(stream, "PICK\t{}\t{index}", self.trigger)?,
        }
        stream.flush()?;
        Ok(())
    }

    fn describe(entry: &MatchEntry) -> String {
        if let Some(label) = &entry.label {
            return label.clone();
        }
        match &entry.form {
            Some(form) => format!(
                "(form) {}",
                form.template.lines().next().unwrap_or("").trim()
            ),
            None => {
                let rendered = matcher::render(entry);
                let first = rendered.lines().next().unwrap_or("").trim();
                let mut text: String = first.chars().take(90).collect();
                if rendered.lines().count() > 1 {
                    text.push('…');
                }
                text
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(frame.area());

        let header = Paragraph::new(Line::from(vec![
            Span::styled(
                "pick  ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(self.trigger.clone()),
            Span::styled(
                match &self.var {
                    Some(var) => format!("   {var}"),
                    None => format!("   {} matches", self.group.len()),
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" hyprspanso "));
        frame.render_widget(header, areas[0]);

        let columns = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(areas[1]);

        let items: Vec<ListItem> = if self.var.is_some() {
            self.values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{} ", index + 1),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(value.clone()),
                    ]))
                })
                .collect()
        } else {
            self.group
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{} ", index + 1),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(Self::describe(entry)),
                    ]))
                })
                .collect()
        };

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" choices "))
            .highlight_style(
                Style::default()
                    .bg(Color::Indexed(236))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        frame.render_stateful_widget(list, columns[0], &mut self.list_state);

        let preview = if let (Some(var), Some(entry)) = (&self.var, self.group.first()) {
            let mut chosen = std::collections::HashMap::new();
            if let Some(value) = self.values.get(self.selected) {
                chosen.insert(var.clone(), value.clone());
            }
            matcher::render_with(entry, chrono::Local::now(), &chosen)
        } else {
            self.group
                .get(self.selected)
                .map(|entry| match &entry.form {
                    Some(form) => form.template.clone(),
                    None => matcher::render(entry),
                })
                .unwrap_or_default()
        };
        let lines: Vec<Line> = preview
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect();
        let preview = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" preview "))
            .wrap(Wrap { trim: false });
        frame.render_widget(preview, columns[1]);

        let footer = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("↑↓", Style::default().fg(Color::Cyan)),
                Span::raw(" move  "),
                Span::styled("1-9", Style::default().fg(Color::Cyan)),
                Span::raw(" pick  "),
                Span::styled("Enter", Style::default().fg(Color::Cyan)),
                Span::raw(" insert  "),
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(" cancel"),
            ]),
            Line::from(Span::styled(
                self.status.clone(),
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        frame.render_widget(footer, areas[2]);
    }
}

/// Small state file: which row was picked last for each trigger.
fn choices_path() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty() && PathBuf::from(value).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".local/state")
        });
    base.join("hyprspanso").join("choices.json")
}

fn remembered_choice(trigger: &str) -> Option<(usize, String)> {
    let text = std::fs::read_to_string(choices_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = value.get(trigger)?;
    let index = entry.get("index")?.as_u64()? as usize;
    let signature = entry.get("sig")?.as_str()?.to_string();
    Some((index, signature))
}

fn remember_choice(trigger: &str, index: usize, signature: &str) {
    let path = choices_path();
    let mut value: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let Some(map) = value.as_object_mut() else {
        return;
    };
    map.insert(
        trigger.to_string(),
        serde_json::json!({ "index": index, "sig": signature }),
    );

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(&path, text + "\n");
    }
}
