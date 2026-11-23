use std::collections::{BTreeSet, HashMap};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding},
};

use crate::action::Action;
use crate::config::McpServerConfig;

#[derive(Clone, Debug)]
struct McpEntry {
    name: String,
    transport: String,
    tool_count: Option<usize>,
    enabled_for_session: bool,
}

pub struct McpServerPicker {
    pub visible: bool,
    filter: String,
    entries: Vec<McpEntry>,
    filtered: Vec<usize>,
    list_state: ListState,
}

impl McpServerPicker {
    pub fn new() -> Self {
        Self {
            visible: false,
            filter: String::new(),
            entries: Vec::new(),
            filtered: Vec::new(),
            list_state: ListState::default(),
        }
    }

    /// Show picker with current config and session disabled set.
    /// Only config-enabled servers are shown (config-disabled are hidden, as they're not actionable per-session).
    pub fn show(
        &mut self,
        configs: &[McpServerConfig],
        disabled: &BTreeSet<String>,
        tool_counts: &HashMap<String, usize>,
    ) {
        self.visible = true;
        self.filter.clear();
        self.entries = configs
            .iter()
            .filter(|c| c.enabled)
            .map(|c| McpEntry {
                name: c.name.clone(),
                transport: c.transport.clone(),
                tool_count: tool_counts.get(&c.name).copied(),
                enabled_for_session: !disabled.contains(&c.name),
            })
            .collect();
        // Sort alphabetically for stable UI
        self.entries.sort_by(|a, b| a.name.cmp(&b.name));
        self.refilter();
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.filter.clear();
    }

    /// Update entries after a toggle without closing.
    pub fn update_after_toggle(
        &mut self,
        configs: &[McpServerConfig],
        disabled: &BTreeSet<String>,
        tool_counts: &HashMap<String, usize>,
    ) {
        // Preserve selection index
        let selected_name = self
            .list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.entries.get(idx))
            .map(|e| e.name.clone());

        self.entries = configs
            .iter()
            .filter(|c| c.enabled)
            .map(|c| McpEntry {
                name: c.name.clone(),
                transport: c.transport.clone(),
                tool_count: tool_counts.get(&c.name).copied(),
                enabled_for_session: !disabled.contains(&c.name),
            })
            .collect();
        self.entries.sort_by(|a, b| a.name.cmp(&b.name));
        self.refilter();

        // Restore selection to same name if possible
        if let Some(name) = selected_name {
            if let Some(pos) = self
                .filtered
                .iter()
                .position(|&idx| self.entries[idx].name == name)
            {
                self.list_state.select(Some(pos));
            }
        }
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
                    || e.transport.to_lowercase().contains(&query)
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
            (KeyCode::Esc, _) => Action::CloseMcpServerPicker,
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,
            (KeyCode::Up, _) => {
                self.select_previous();
                Action::Noop
            }
            (KeyCode::Down, _) => {
                self.select_next();
                Action::Noop
            }
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                if let Some(selected) = self.list_state.selected() {
                    if let Some(&idx) = self.filtered.get(selected) {
                        let name = self.entries[idx].name.clone();
                        return Action::ToggleMcpServer(name);
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

        let overlay_width = (area.width * 55 / 100).max(40).min(area.width);
        let overlay_height = (area.height * 45 / 100).max(10).min(area.height);

        let x = (area.width.saturating_sub(overlay_width)) / 2 + area.x;
        let y = (area.height.saturating_sub(overlay_height)) / 2 + area.y;
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        frame.render_widget(Clear, overlay_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" MCP Servers (session) ")
            .title_alignment(Alignment::Center)
            .padding(Padding::horizontal(1));

        let inner = block.inner(overlay_area);

        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan).bold()),
            Span::styled(&self.filter, Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]);

        let available = inner.width.saturating_sub(2) as usize;

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&i| {
                let entry = &self.entries[i];
                let checkbox = if entry.enabled_for_session {
                    "[x]"
                } else {
                    "[ ]"
                };
                let checkbox_style = if entry.enabled_for_session {
                    Style::default().fg(Color::Green).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let name_style = if entry.enabled_for_session {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                let detail = match entry.tool_count {
                    Some(n) => format!(
                        "{} · {} tool{}",
                        entry.transport,
                        n,
                        if n == 1 { "" } else { "s" }
                    ),
                    None => format!("{} · not connected", entry.transport),
                };
                let detail_style = Style::default().fg(Color::DarkGray);

                // Right-align detail if space
                let prefix_len = checkbox.len() + 1 + entry.name.len();
                let detail_len = detail.len();
                let padding = available
                    .saturating_sub(prefix_len)
                    .saturating_sub(detail_len);
                let pad_str = " ".repeat(padding);

                ListItem::new(Line::from(vec![
                    Span::styled(checkbox, checkbox_style),
                    Span::raw(" "),
                    Span::styled(entry.name.as_str(), name_style),
                    Span::raw(pad_str),
                    Span::styled(detail, detail_style),
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

        if self.entries.is_empty() {
            let msg = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "No MCP servers configured (add to config.json)",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Center);
            frame.render_widget(msg, list_area);
        } else if items.is_empty() {
            let msg = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "No matching servers",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Center);
            frame.render_widget(msg, list_area);
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
            Span::styled("Enter/Space", Style::default().fg(Color::Cyan)),
            Span::raw(" toggle  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" close"),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(help, help_area);
    }
}
