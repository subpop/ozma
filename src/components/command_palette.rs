use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding},
};

use crate::action::{Action, ParamKind};

/// A command that can be executed from the command palette.
#[derive(Clone, Debug)]
struct Command {
    /// Display name shown in the palette.
    name: String,
    /// Shortcut hint shown right-aligned (e.g. "Ctrl+N").
    shortcut: String,
    /// The action to dispatch when selected.
    action: Action,
}

/// A searchable command palette overlay, opened with Ctrl+P.
///
/// Displays a filterable list of all available commands. The user can type
/// to filter, use Up/Down to navigate, and Enter to execute.
pub struct CommandPalette {
    /// Whether this overlay is currently visible.
    pub visible: bool,
    /// The current filter text.
    filter: String,
    /// All registered commands.
    commands: Vec<Command>,
    /// Indices into `commands` that match the current filter.
    filtered: Vec<usize>,
    /// Selection state for the filtered list.
    list_state: ListState,
}

impl CommandPalette {
    pub fn new() -> Self {
        let commands = Self::static_commands();

        let filtered: Vec<usize> = (0..commands.len()).collect();
        let mut list_state = ListState::default();
        if !filtered.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            visible: false,
            filter: String::new(),
            commands,
            filtered,
            list_state,
        }
    }

    /// The fixed set of commands (always present).
    fn static_commands() -> Vec<Command> {
        vec![
            Command {
                name: "New Session".into(),
                shortcut: "Ctrl+N".into(),
                action: Action::NewSession,
            },
            Command {
                name: "Open Sessions".into(),
                shortcut: "Ctrl+O".into(),
                action: Action::OpenSessionList,
            },
            Command {
                name: "Compact Context".into(),
                shortcut: "Ctrl+K".into(),
                action: Action::CompactContext,
            },
            Command {
                name: "Scroll Up".into(),
                shortcut: "PageUp".into(),
                action: Action::ScrollUp,
            },
            Command {
                name: "Scroll Down".into(),
                shortcut: "PageDown".into(),
                action: Action::ScrollDown,
            },
            Command {
                name: "Scroll to Bottom".into(),
                shortcut: String::new(),
                action: Action::ScrollToBottom,
            },
            Command {
                name: "Focus Input".into(),
                shortcut: String::new(),
                action: Action::FocusInput,
            },
            Command {
                name: "Switch Model".into(),
                shortcut: "Ctrl+M".into(),
                action: Action::OpenModelPicker,
            },
            Command {
                name: "Switch Persona".into(),
                shortcut: "Ctrl+.".into(),
                action: Action::OpenPersonaPicker,
            },
            Command {
                name: "Set Temperature".into(),
                shortcut: String::new(),
                action: Action::OpenParamInput(ParamKind::Temperature),
            },
            Command {
                name: "Set Top P".into(),
                shortcut: String::new(),
                action: Action::OpenParamInput(ParamKind::TopP),
            },
            Command {
                name: "Set Max Tokens".into(),
                shortcut: String::new(),
                action: Action::OpenParamInput(ParamKind::MaxTokens),
            },
            Command {
                name: "Export to Markdown".into(),
                shortcut: String::new(),
                action: Action::ExportMarkdown,
            },
            Command {
                name: "Export to JSON".into(),
                shortcut: String::new(),
                action: Action::ExportJson,
            },
            Command {
                name: "Copy as Markdown".into(),
                shortcut: String::new(),
                action: Action::ExportMarkdownClipboard,
            },
            Command {
                name: "Copy as JSON".into(),
                shortcut: String::new(),
                action: Action::ExportJsonClipboard,
            },
            Command {
                name: "Quit".into(),
                shortcut: "Ctrl+Q".into(),
                action: Action::Quit,
            },
        ]
    }

    /// Show the command palette.
    pub fn show(&mut self) {
        self.visible = true;
        self.filter.clear();
        self.refilter();
    }

    /// Hide the command palette.
    pub fn hide(&mut self) {
        self.visible = false;
        self.filter.clear();
    }

    /// Re-filter the command list based on the current filter text.
    fn refilter(&mut self) {
        let query = self.filter.to_lowercase();
        self.filtered = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, cmd)| {
                if query.is_empty() {
                    true
                } else {
                    cmd.name.to_lowercase().contains(&query)
                        || cmd.shortcut.to_lowercase().contains(&query)
                }
            })
            .map(|(i, _)| i)
            .collect();

        // Reset selection to first match
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Move selection up by one.
    fn select_previous(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered.len() - 1
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
        if self.filtered.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.filtered.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Handle a key event while the palette is visible.
    /// Returns the action to dispatch.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        if !self.visible {
            return Action::Noop;
        }

        match (key.code, key.modifiers) {
            // Close
            (KeyCode::Esc, _) => Action::CloseCommandPalette,
            (KeyCode::Char('p'), m) if m.contains(KeyModifiers::CONTROL) => {
                Action::CloseCommandPalette
            }

            // Quit still works
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,

            // Navigation
            (KeyCode::Up, _) => {
                self.select_previous();
                Action::Noop
            }
            (KeyCode::Down, _) => {
                self.select_next();
                Action::Noop
            }

            // Execute selected command
            (KeyCode::Enter, _) => {
                if let Some(selected) = self.list_state.selected() {
                    if let Some(&cmd_index) = self.filtered.get(selected) {
                        let action = self.commands[cmd_index].action.clone();
                        self.hide();
                        return action;
                    }
                }
                Action::Noop
            }

            // Backspace: delete filter char
            (KeyCode::Backspace, _) => {
                self.filter.pop();
                self.refilter();
                Action::Noop
            }

            // Printable characters: append to filter
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                self.refilter();
                Action::Noop
            }

            _ => Action::Noop,
        }
    }

    /// Render the command palette as a centered overlay.
    pub fn render_overlay(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        // Calculate overlay dimensions: 50% width, 50% height, centered
        let overlay_width = (area.width * 50 / 100).max(30).min(area.width);
        let overlay_height = (area.height * 50 / 100).max(8).min(area.height);

        let x = (area.width.saturating_sub(overlay_width)) / 2 + area.x;
        let y = (area.height.saturating_sub(overlay_height)) / 2 + area.y;
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        // Clear the background
        frame.render_widget(Clear, overlay_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Commands ")
            .title_alignment(Alignment::Center)
            .padding(Padding::horizontal(1));

        let inner = block.inner(overlay_area);

        // Build the filter input line
        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan).bold()),
            Span::styled(&self.filter, Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]);

        // Build list items from filtered commands
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&i| {
                let cmd = &self.commands[i];
                let name_span = Span::styled(cmd.name.clone(), Style::default().fg(Color::White));

                if cmd.shortcut.is_empty() {
                    ListItem::new(Line::from(vec![name_span]))
                } else {
                    // Calculate padding to right-align the shortcut
                    let available = inner.width.saturating_sub(2) as usize; // account for highlight symbol
                    let name_len = cmd.name.len();
                    let shortcut_len = cmd.shortcut.len();
                    let padding = available
                        .saturating_sub(name_len)
                        .saturating_sub(shortcut_len);
                    let pad_str = " ".repeat(padding);

                    ListItem::new(Line::from(vec![
                        name_span,
                        Span::raw(pad_str),
                        Span::styled(cmd.shortcut.clone(), Style::default().fg(Color::DarkGray)),
                    ]))
                }
            })
            .collect();

        // Render the block
        frame.render_widget(block.clone(), overlay_area);

        if inner.height < 2 {
            return;
        }

        // Render filter input on the first line of inner area
        let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
        frame.render_widget(ratatui::widgets::Paragraph::new(filter_line), filter_area);

        // Render list below the filter
        let list_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);

        if items.is_empty() {
            let no_match = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "No matching commands",
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

        // Help line at the bottom of overlay
        let help_area = Rect {
            x: overlay_area.x + 1,
            y: overlay_area.y + overlay_area.height - 1,
            width: overlay_area.width.saturating_sub(2),
            height: 1,
        };
        let help = ratatui::widgets::Paragraph::new(Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" run  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" close"),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(help, help_area);
    }
}
