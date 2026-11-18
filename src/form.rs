//! Interactive `form:` window.
//!
//! Triggered by the daemon: it erases the typed trigger, opens this window in
//! a floating terminal, and waits for the field values over the IPC socket.
//! Filling the fields and pressing Enter sends them back and the daemon
//! inserts the rendered template into the window you came from.

use crate::config::{self, FormFieldKind};
use crate::matcher;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

struct FieldState {
    name: String,
    kind: FormFieldKind,
    value: String,
    index: usize,
}

pub fn run(trigger: Option<String>, target: Option<String>) -> Result<()> {
    let trigger = trigger.context("missing --trigger")?;
    let root = config::config_root();
    let set = config::load(&root);

    let entry = set
        .entries
        .iter()
        .find(|entry| entry.trigger == trigger)
        .with_context(|| format!("form trigger {trigger:?} not found"))?;
    let spec = entry.form.clone().context("match has no `form`")?;

    let fields: Vec<FieldState> = spec
        .fields
        .iter()
        .map(|field| {
            let (value, index) = match &field.kind {
                FormFieldKind::Choice { values, .. } => {
                    (values.first().cloned().unwrap_or_default(), 0)
                }
                _ => (String::new(), 0),
            };
            FieldState {
                name: field.name.clone(),
                kind: field.kind.clone(),
                value,
                index,
            }
        })
        .collect();

    let sock = crate::daemon::socket_path();
    let mut stream = UnixStream::connect(&sock).ok();
    let mut status = if stream.is_some() {
        "daemon ok".to_string()
    } else {
        "daemon not running: the form cannot be inserted".to_string()
    };
    if let Some(stream) = stream.as_mut() {
        // A search client pauses expansion while the form is open.
        if writeln!(stream, "SEARCH").is_err() {
            stream.flush().ok();
        }
    }
    if fields.is_empty() {
        status = "this form has no [[fields]]".to_string();
    }

    let mut app = FormApp {
        trigger,
        target,
        spec,
        fields,
        selected: 0,
        list_state: ListState::default().with_selected(Some(0)),
        status,
        stream,
    };

    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

struct FormApp {
    trigger: String,
    target: Option<String>,
    spec: config::FormSpec,
    fields: Vec<FieldState>,
    selected: usize,
    list_state: ListState,
    status: String,
    stream: Option<UnixStream>,
}

impl FormApp {
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
                KeyCode::Tab | KeyCode::Down => self.move_by(1),
                KeyCode::BackTab | KeyCode::Up => self.move_by(-1),
                KeyCode::Backspace => {
                    if let Some(field) = self.fields.get_mut(self.selected) {
                        field.value.pop();
                    }
                }
                KeyCode::Char('u') if ctrl => {
                    if let Some(field) = self.fields.get_mut(self.selected) {
                        field.value.clear();
                    }
                }
                KeyCode::Left => self.cycle(-1),
                KeyCode::Right => self.cycle(1),
                KeyCode::Char(c) if !ctrl => {
                    if let Some(field) = self.fields.get_mut(self.selected) {
                        if !matches!(field.kind, FormFieldKind::Choice { .. }) {
                            field.value.push(c);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn move_by(&mut self, delta: i32) {
        if self.fields.is_empty() {
            return;
        }
        let len = self.fields.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
        self.list_state.select(Some(self.selected));
    }

    fn cycle(&mut self, delta: i32) {
        let Some(field) = self.fields.get_mut(self.selected) else {
            return;
        };
        let FormFieldKind::Choice { values, .. } = &field.kind else {
            return;
        };
        if values.is_empty() {
            return;
        }
        let len = values.len() as i32;
        field.index = (field.index as i32 + delta).rem_euclid(len) as usize;
        field.value = values[field.index].clone();
    }

    fn values(&self) -> HashMap<String, String> {
        self.fields
            .iter()
            .map(|field| (field.name.clone(), field.value.clone()))
            .collect()
    }

    fn submit(&mut self) -> Result<()> {
        if self.stream.is_none() {
            let sock = crate::daemon::socket_path();
            if let Ok(mut stream) = UnixStream::connect(&sock) {
                let _ = writeln!(stream, "SEARCH");
                self.stream = Some(stream);
            }
        }
        let values = self.values();
        let Some(stream) = self.stream.as_mut() else {
            anyhow::bail!("daemon not running");
        };

        if let Some(target) = &self.target {
            writeln!(stream, "TARGET\t{target}")?;
        }
        let json = serde_json::to_string(&values)?;
        writeln!(stream, "FORM\t{}\t{json}", self.trigger)?;
        stream.flush()?;
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(frame.area());

        let header = Paragraph::new(Line::from(vec![
            Span::styled("form  ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(self.trigger.clone()),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" hyprspanso "));
        frame.render_widget(header, areas[0]);

        let columns = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(areas[1]);

        let items: Vec<ListItem> = self
            .fields
            .iter()
            .map(|field| {
                let value = match &field.kind {
                    FormFieldKind::Choice { values, .. } => {
                        format!("◀ {} ▶", values.get(field.index).cloned().unwrap_or_default())
                    }
                    _ => {
                        if field.value.is_empty() {
                            "…".to_string()
                        } else {
                            field.value.clone()
                        }
                    }
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<12}", field.name),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(value),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" fields "))
            .highlight_style(
                Style::default()
                    .bg(Color::Indexed(236))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        frame.render_stateful_widget(list, columns[0], &mut self.list_state);

        let preview = matcher::render_form(&self.spec, &self.values());
        let mut lines: Vec<Line> = preview
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(""));
        }
        let preview = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" preview "))
            .wrap(Wrap { trim: false });
        frame.render_widget(preview, columns[1]);

        let footer = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Tab/↑↓", Style::default().fg(Color::Cyan)),
                Span::raw(" field  "),
                Span::styled("←→", Style::default().fg(Color::Cyan)),
                Span::raw(" choice  "),
                Span::styled("Enter", Style::default().fg(Color::Cyan)),
                Span::raw(" insert  "),
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(" cancel  "),
                Span::styled("Ctrl-U", Style::default().fg(Color::Cyan)),
                Span::raw(" clear"),
            ]),
            Line::from(Span::styled(
                self.status.clone(),
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        frame.render_widget(footer, areas[2]);
    }
}
