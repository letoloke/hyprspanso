//! Fuzzy search window. Runs in a terminal, picks a trigger, hands it to the
//! daemon over the IPC socket, then exits so focus returns to where you were.

use crate::config::{self, MatchEntry};
use anyhow::Result;
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

pub fn run(target: Option<String>) -> Result<()> {
    let root = config::config_root();
    let set = config::load(&root);
    let sock = crate::daemon::socket_path();

    let mut app = App {
        entries: set.entries,
        skipped: set.skipped.len(),
        query: String::new(),
        visible: Vec::new(),
        selected: 0,
        list_state: ListState::default(),
        status: String::new(),
        stream: None,
        target,
        root,
        sock,
    };
    app.refresh();
    app.connect();

    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

struct App {
    entries: Vec<MatchEntry>,
    skipped: usize,
    query: String,
    visible: Vec<usize>,
    selected: usize,
    list_state: ListState,
    status: String,
    stream: Option<UnixStream>,
    target: Option<String>,
    root: PathBuf,
    sock: PathBuf,
}

impl App {
    /// Holds the IPC connection open for the whole session: the daemon pauses
    /// expansion while a SEARCH client is connected.
    fn connect(&mut self) -> bool {
        match UnixStream::connect(&self.sock) {
            Ok(mut stream) => {
                if writeln!(stream, "SEARCH").is_ok() && stream.flush().is_ok() {
                    self.stream = Some(stream);
                    self.status = format!("daemon ok · {}", self.sock.display());
                    return true;
                }
                self.stream = None;
                self.status = "daemon did not answer on the socket".to_string();
                false
            }
            Err(err) => {
                self.stream = None;
                self.status =
                    format!("daemon NOT running ({err}) — systemctl --user start hyprspanso");
                false
            }
        }
    }

    fn send_selection(&mut self, trigger: &str) -> Result<()> {
        if self.stream.is_none() && !self.connect() {
            anyhow::bail!("daemon is not running");
        }
        let stream = self.stream.as_mut().expect("connected above");
        if let Some(target) = &self.target {
            writeln!(stream, "TARGET\t{target}")?;
        }
        writeln!(stream, "SELECT\t{trigger}")?;
        stream.flush()?;
        Ok(())
    }

    fn refresh(&mut self) {
        self.visible = rank(&self.entries, &self.query);
        self.selected = 0;
        self.list_state.select(Some(0));
        self.list_state = ListState::default().with_selected(Some(0));
    }

    fn reload(&mut self) {
        let set = config::load(&self.root);
        self.entries = set.entries;
        self.skipped = set.skipped.len();
        self.refresh();
        self.status = format!("reloaded {} matches", self.entries.len());
    }

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
            let alt = key.modifiers.contains(KeyModifiers::ALT);

            match key.code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Enter => {
                    if let Some(&index) = self.visible.get(self.selected) {
                        let trigger = self.entries[index].trigger.clone();
                        match self.send_selection(&trigger) {
                            Ok(()) => return Ok(()),
                            Err(err) => {
                                self.status = format!("cannot reach daemon: {err}");
                            }
                        }
                    }
                }
                KeyCode::Down => self.move_by(1),
                KeyCode::Up => self.move_by(-1),
                KeyCode::PageDown => self.move_by(10),
                KeyCode::PageUp => self.move_by(-10),
                KeyCode::Char('n') if ctrl => self.move_by(1),
                KeyCode::Char('p') if ctrl => self.move_by(-1),
                KeyCode::Char('u') if ctrl => {
                    self.query.clear();
                    self.refresh();
                }
                KeyCode::Char('w') if ctrl => {
                    while let Some(c) = self.query.pop() {
                        if c.is_alphanumeric() {
                            break;
                        }
                    }
                    while self.query.chars().last().map(char::is_alphanumeric) == Some(true) {
                        self.query.pop();
                    }
                    self.refresh();
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.refresh();
                }
                KeyCode::F(5) => self.reload(),
                KeyCode::Char('j') if alt => self.move_by(1),
                KeyCode::Char('k') if alt => self.move_by(-1),
                KeyCode::Char(c) if !ctrl && !alt => {
                    self.query.push(c);
                    self.refresh();
                }
                _ => {}
            }
        }
    }

    fn move_by(&mut self, delta: i32) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
        self.list_state.select(Some(self.selected));
    }

    fn draw(&mut self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(frame.area());

        let title = format!(
            " hyprspanso · {} matches{} ",
            self.entries.len(),
            if self.skipped > 0 {
                format!(" ({} skipped)", self.skipped)
            } else {
                String::new()
            }
        );

        let query = Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::raw(self.query.as_str()),
            Span::styled("▏", Style::default().fg(Color::DarkGray)),
        ]))
        .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(query, areas[0]);

        let columns = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(areas[1]);

        let items: Vec<ListItem> = self
            .visible
            .iter()
            .map(|&index| {
                let entry = &self.entries[index];
                let mut tags = Vec::new();
                if entry.word {
                    tags.push("word");
                }
                if entry.force_clipboard {
                    tags.push("clip");
                }
                let tag = if tags.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", tags.join(","))
                };
                let preview = first_line(&entry.replace);
                ListItem::new(Line::from(vec![
                    Span::styled(
                        entry.trigger.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(tag, Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("   {preview}"),
                        Style::default().fg(Color::Gray),
                    ),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" trigger / replace "))
            .highlight_style(
                Style::default()
                    .bg(Color::Indexed(236))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        frame.render_stateful_widget(list, columns[0], &mut self.list_state);

        let preview = if let Some(&index) = self.visible.get(self.selected) {
            let entry = &self.entries[index];
            let mut lines = vec![
                Line::from(Span::styled(
                    &entry.trigger,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            for line in entry.replace.split('\n') {
                lines.push(Line::from(line.to_string()));
            }
            Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" replace · {} ", entry.source)),
            )
            .wrap(Wrap { trim: false })
        } else {
            Paragraph::new("no match")
                .block(Block::default().borders(Borders::ALL).title(" replace "))
        };
        frame.render_widget(preview, columns[1]);

        let help = Line::from(vec![
            Span::styled("↑↓", Style::default().fg(Color::Cyan)),
            Span::raw(" move  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" insert  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" quit  "),
            Span::styled("Ctrl-U", Style::default().fg(Color::Cyan)),
            Span::raw(" clear  "),
            Span::styled("F5", Style::default().fg(Color::Cyan)),
            Span::raw(" reload"),
        ]);
        let footer = Paragraph::new(vec![
            help,
            Line::from(Span::styled(
                self.status.clone(),
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        frame.render_widget(footer, areas[2]);
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    let mut out: String = line.chars().take(80).collect();
    if line.chars().count() > 80 || text.lines().count() > 1 {
        out.push('…');
    }
    out
}

/// Empty query keeps espanso's order (file order), otherwise a small fuzzy
/// score over trigger and replace text.
fn rank(entries: &[MatchEntry], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..entries.len()).collect();
    }
    let needle = query.trim();
    let mut scored: Vec<(i32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let trigger = fuzzy(&entry.trigger, needle);
            let replace = fuzzy(&entry.replace, needle);
            let score = match (trigger, replace) {
                (Some(t), Some(r)) => (t + 20).max(r),
                (Some(t), None) => t + 20,
                (None, Some(r)) => r,
                (None, None) => return None,
            };
            Some((score, index))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
}

fn fuzzy(haystack: &str, needle: &str) -> Option<i32> {
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let needle: Vec<char> = needle.to_lowercase().chars().collect();
    if needle.is_empty() {
        return Some(0);
    }
    let mut hay_index = 0;
    let mut last: Option<usize> = None;
    let mut score = 0;
    for wanted in needle {
        while hay_index < hay.len() && hay[hay_index] != wanted {
            hay_index += 1;
        }
        if hay_index >= hay.len() {
            return None;
        }
        let mut bonus = 10;
        if last.map(|l| hay_index == l + 1).unwrap_or(false) {
            bonus += 15;
        }
        if hay_index == 0
            || !hay[hay_index - 1].is_alphanumeric()
        {
            bonus += 10;
        }
        score += bonus;
        last = Some(hay_index);
        hay_index += 1;
    }
    Some(score)
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
        }
    }

    #[test]
    fn fuzzy_is_subsequence_and_case_insensitive() {
        assert!(fuzzy(":bookingan", "bkn").is_some());
        assert!(fuzzy("Be Right Back", "brb").is_some());
        assert!(fuzzy("booking", "xyz").is_none());
    }

    #[test]
    fn fuzzy_prefers_contiguous() {
        let contiguous = fuzzy("lavanderias", "lavan").unwrap();
        let scattered = fuzzy("l-a-v-a-n", "lavan").unwrap();
        assert!(contiguous > scattered);
    }

    #[test]
    fn rank_matches_replace_text_too() {
        let entries = vec![
            entry(":shrug", "¯\\_(ツ)_/¯"),
            entry(":sig", "Best regards,\nRicardo"),
            entry(":bank", "BBVA 0123456789"),
        ];
        let by_trigger = rank(&entries, "sig");
        assert_eq!(by_trigger.first().copied(), Some(1));
        let by_replace = rank(&entries, "bbva");
        assert_eq!(by_replace.first().copied(), Some(2));
    }
}
