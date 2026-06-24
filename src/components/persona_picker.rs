use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding},
};

use crate::action::Action;
use crate::persona::Persona;

/// A persona entry in the picker.
#[derive(Clone, Debug)]
struct PersonaEntry {
    name: &'static str,
    description: &'static str,
    active: bool,
}

/// A filterable persona picker overlay.
pub struct PersonaPicker {
    pub visible: bool,
    filter: String,
    entries: Vec<PersonaEntry>,
    filtered: Vec<usize>,
    list_state: ListState,
}

impl PersonaPicker {
    pub fn new() -> Self {
        Self {
            visible: false,
            filter: String::new(),
            entries: Vec::new(),
            filtered: Vec::new(),
            list_state: ListState::default(),
        }
    }

    /// Show the picker, populating entries from all personas.
    pub fn show(&mut self, active: &Persona) {
        self.visible = true;
        self.filter.clear();
        self.entries = Persona::all()
            .iter()
            .map(|p| PersonaEntry {
                name: p.name(),
                description: p.description(),
                active: p == active,
            })
            .collect();
        self.refilter();
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.filter.clear();
    }

    fn refilter(&mut self) {
        let query = self.filter.to_lowercase();
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                query.is_empty()
                    || e.name.to_lowercase().contains(&query)
                    || e.description.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();

        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    fn select_previous(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(0) | None => self.filtered.len() - 1,
            Some(i) => i - 1,
        };
        self.list_state.select(Some(i));
    }

    fn select_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i >= self.filtered.len() - 1 => 0,
            Some(i) => i + 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        if !self.visible {
            return Action::Noop;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Action::ClosePersonaPicker,
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,
            (KeyCode::Up, _) => {
                self.select_previous();
                Action::Noop
            }
            (KeyCode::Down, _) => {
                self.select_next();
                Action::Noop
            }
            (KeyCode::Enter, _) => {
                if let Some(selected) = self.list_state.selected() {
                    if let Some(&idx) = self.filtered.get(selected) {
                        let name = self.entries[idx].name.to_string();
                        self.hide();
                        return Action::SwitchPersona(name);
                    }
                }
                Action::Noop
            }
            (KeyCode::Backspace, _) => {
                self.filter.pop();
                self.refilter();
                Action::Noop
            }
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                self.refilter();
                Action::Noop
            }
            _ => Action::Noop,
        }
    }

    pub fn render_overlay(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let overlay_width = (area.width * 50 / 100).max(30).min(area.width);
        let overlay_height = (area.height * 40 / 100).max(8).min(area.height);

        let x = (area.width.saturating_sub(overlay_width)) / 2 + area.x;
        let y = (area.height.saturating_sub(overlay_height)) / 2 + area.y;
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        frame.render_widget(Clear, overlay_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Switch Persona ")
            .title_alignment(Alignment::Center)
            .padding(Padding::horizontal(1));

        let inner = block.inner(overlay_area);

        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Yellow).bold()),
            Span::styled(&self.filter, Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]);

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&i| {
                let entry = &self.entries[i];
                let prefix = if entry.active { "* " } else { "  " };
                let prefix_style = if entry.active {
                    Style::default().fg(Color::Green).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let name_style = if entry.active {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let name_len = prefix.len() + entry.name.len();
                let desc_len = entry.description.len();
                let available = inner.width.saturating_sub(2) as usize;
                let padding = available.saturating_sub(name_len).saturating_sub(desc_len);
                let pad_str = " ".repeat(padding);

                ListItem::new(Line::from(vec![
                    Span::styled(prefix, prefix_style),
                    Span::styled(entry.name, name_style),
                    Span::raw(pad_str),
                    Span::styled(entry.description, Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect();

        frame.render_widget(block.clone(), overlay_area);

        if inner.height < 2 {
            return;
        }

        let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
        frame.render_widget(ratatui::widgets::Paragraph::new(filter_line), filter_area);

        let list_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);

        if items.is_empty() {
            let no_match = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "No matching personas",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Center);
            frame.render_widget(no_match, list_area);
        } else {
            let list = List::new(items)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> ");

            frame.render_stateful_widget(list, list_area, &mut self.list_state);
        }

        let help_area = Rect {
            x: overlay_area.x + 1,
            y: overlay_area.y + overlay_area.height - 1,
            width: overlay_area.width.saturating_sub(2),
            height: 1,
        };
        let help = ratatui::widgets::Paragraph::new(Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" select  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(help, help_area);
    }
}
