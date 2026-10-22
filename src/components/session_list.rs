use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding},
};

use crate::acp::SessionSummary;
use crate::action::Action;
use crate::components::Component;

/// An overlay component that displays a list of saved conversation sessions.
///
/// Keybindings:
/// - Up/Down or j/k: navigate the list
/// - Enter: load the selected conversation
/// - d: delete the selected conversation
/// - Esc or Ctrl+O: close the overlay
/// - Ctrl+N: new conversation
pub struct SessionList {
    /// Whether this overlay is currently visible.
    pub visible: bool,
    /// The list of conversation summaries.
    sessions: Vec<SessionSummary>,
    /// List selection state.
    list_state: ListState,
}

impl SessionList {
    pub fn new() -> Self {
        Self {
            visible: false,
            sessions: Vec::new(),
            list_state: ListState::default(),
        }
    }

    /// Show the overlay with the given session list.
    pub fn show(&mut self, sessions: Vec<SessionSummary>) {
        self.sessions = sessions;
        self.visible = true;
        // Select the first item if available
        if !self.sessions.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    /// Hide the overlay.
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Get the currently selected session index.
    fn selected_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    /// Move selection up by one.
    fn select_previous(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.selected_index() {
            Some(i) => {
                if i == 0 {
                    self.sessions.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Move selection down by one.
    fn select_next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.selected_index() {
            Some(i) => {
                if i >= self.sessions.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Render the session list as a centered overlay.
    pub fn render_overlay(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        // Calculate overlay dimensions: 60% width, 70% height, centered
        let overlay_width = (area.width * 60 / 100).max(40).min(area.width);
        let overlay_height = (area.height * 70 / 100).max(10).min(area.height);

        let horizontal_margin = (area.width.saturating_sub(overlay_width)) / 2;
        let vertical_margin = (area.height.saturating_sub(overlay_height)) / 2;

        let overlay_area = Rect {
            x: area.x + horizontal_margin,
            y: area.y + vertical_margin,
            width: overlay_width,
            height: overlay_height,
        };

        // Clear the background
        frame.render_widget(Clear, overlay_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Sessions ")
            .title_alignment(Alignment::Center)
            .padding(Padding::horizontal(1));

        let inner = block.inner(overlay_area);

        // Build list items
        let items: Vec<ListItem> = self
            .sessions
            .iter()
            .map(|s| {
                let date = s.updated_at.format("%Y-%m-%d %H:%M");
                let mut meta = format!(
                    "{} / {} · {} msgs · {}",
                    s.provider, s.model, s.message_count, date
                );
                if let Some(cwd) = s.cwd.as_deref() {
                    let short = shorten_path(cwd, 32);
                    meta = format!("{meta} · {short}");
                }
                let line = Line::from(vec![
                    Span::styled(format!("{} ", s.title), Style::default().fg(Color::White)),
                    Span::styled(meta, Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            })
            .collect();

        if items.is_empty() {
            // Show empty state
            let empty_msg = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "No saved sessions",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Center);
            frame.render_widget(block, overlay_area);
            // Render the empty message in the inner area with vertical centering
            let centered = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .split(inner);
            frame.render_widget(empty_msg, centered[1]);
        } else {
            let list = List::new(items)
                .block(block)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> ");

            frame.render_stateful_widget(list, overlay_area, &mut self.list_state);
        }

        // Help line at the bottom of overlay
        let help_area = Rect {
            x: overlay_area.x + 1,
            y: overlay_area.y + overlay_area.height - 1,
            width: overlay_area.width.saturating_sub(2),
            height: 1,
        };
        let help = ratatui::widgets::Paragraph::new(Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" open  "),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::raw(" delete  "),
            Span::styled("Ctrl+N", Style::default().fg(Color::Cyan)),
            Span::raw(" new  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" close"),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(help, help_area);
    }
}

impl Component for SessionList {
    fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        if !self.visible {
            return Action::Noop;
        }

        match (key.code, key.modifiers) {
            // Close overlay
            (KeyCode::Esc, _) => Action::CloseSessionList,
            (KeyCode::Char('o'), m) if m.contains(KeyModifiers::CONTROL) => {
                Action::CloseSessionList
            }

            // Quit still works from overlay
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,

            // Navigation
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.select_previous();
                Action::Noop
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.select_next();
                Action::Noop
            }

            // Load selected session
            (KeyCode::Enter, _) => {
                if let Some(i) = self.selected_index() {
                    if let Some(session) = self.sessions.get(i) {
                        Action::LoadSession(session.id.clone())
                    } else {
                        Action::Noop
                    }
                } else {
                    Action::Noop
                }
            }

            // Delete selected session
            (KeyCode::Char('d'), m) if m.is_empty() => {
                if let Some(i) = self.selected_index() {
                    if let Some(session) = self.sessions.get(i) {
                        let id = session.id.clone();
                        // Remove from local list
                        self.sessions.remove(i);
                        // Adjust selection
                        if self.sessions.is_empty() {
                            self.list_state.select(None);
                        } else if i >= self.sessions.len() {
                            self.list_state.select(Some(self.sessions.len() - 1));
                        }
                        Action::DeleteSession(id)
                    } else {
                        Action::Noop
                    }
                } else {
                    Action::Noop
                }
            }

            // New session
            (KeyCode::Char('n'), m) if m.contains(KeyModifiers::CONTROL) => Action::NewSession,

            _ => Action::Noop,
        }
    }

    fn render(&mut self, _frame: &mut Frame, _area: Rect) {
        // Use render_overlay instead — this is called via the trait
        // but the Home component calls render_overlay directly for overlay semantics.
    }
}

/// Truncate a path for the session list meta line, keeping the tail.
fn shorten_path(path: &str, max_chars: usize) -> String {
    if path.chars().count() <= max_chars {
        return path.to_string();
    }
    let tail: String = path
        .chars()
        .rev()
        .take(max_chars.saturating_sub(1))
        .collect();
    let tail: String = tail.chars().rev().collect();
    format!("…{tail}")
}
