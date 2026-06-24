use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};

use crate::action::{Action, ParamKind};

/// A small text-input overlay for setting a single model parameter.
pub struct ParamInput {
    /// Whether this overlay is currently visible.
    pub visible: bool,
    /// Which parameter is being edited.
    kind: ParamKind,
    /// The text the user is typing.
    buffer: String,
    /// The current value (for display).
    current_display: String,
}

impl ParamInput {
    pub fn new() -> Self {
        Self {
            visible: false,
            kind: ParamKind::Temperature,
            buffer: String::new(),
            current_display: String::from("default"),
        }
    }

    /// Show the overlay for a specific parameter kind.
    pub fn show(&mut self, kind: ParamKind, current: Option<String>) {
        self.visible = true;
        self.kind = kind;
        self.buffer.clear();
        self.current_display = current.unwrap_or_else(|| "default".to_string());
    }

    /// Hide the overlay.
    pub fn hide(&mut self) {
        self.visible = false;
        self.buffer.clear();
    }

    fn label(&self) -> &str {
        match self.kind {
            ParamKind::Temperature => "Temperature",
            ParamKind::TopP => "Top P",
            ParamKind::MaxTokens => "Max Tokens",
        }
    }

    fn hint(&self) -> &str {
        match self.kind {
            ParamKind::Temperature => "0.0 - 2.0 (empty to clear)",
            ParamKind::TopP => "0.0 - 1.0 (empty to clear)",
            ParamKind::MaxTokens => "positive integer (empty to clear)",
        }
    }

    /// Parse the buffer and return the appropriate Set action, or None on error.
    fn parse_and_build_action(&self) -> Option<Action> {
        let trimmed = self.buffer.trim();

        // Empty input clears the override
        if trimmed.is_empty() {
            return match self.kind {
                ParamKind::Temperature => Some(Action::SetTemperature(None)),
                ParamKind::TopP => Some(Action::SetTopP(None)),
                ParamKind::MaxTokens => Some(Action::SetMaxTokens(None)),
            };
        }

        match self.kind {
            ParamKind::Temperature => {
                let val: f64 = trimmed.parse().ok()?;
                if !(0.0..=2.0).contains(&val) {
                    return None;
                }
                Some(Action::SetTemperature(Some(val)))
            }
            ParamKind::TopP => {
                let val: f64 = trimmed.parse().ok()?;
                if !(0.0..=1.0).contains(&val) {
                    return None;
                }
                Some(Action::SetTopP(Some(val)))
            }
            ParamKind::MaxTokens => {
                let val: u32 = trimmed.parse().ok()?;
                if val == 0 {
                    return None;
                }
                Some(Action::SetMaxTokens(Some(val)))
            }
        }
    }

    /// Handle a key event while the overlay is visible.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        if !self.visible {
            return Action::Noop;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Action::CloseParamInput,
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => Action::CloseParamInput,

            (KeyCode::Enter, _) => {
                if let Some(action) = self.parse_and_build_action() {
                    self.hide();
                    action
                } else {
                    // Invalid input — stay open (user can see hint)
                    Action::Noop
                }
            }

            (KeyCode::Backspace, _) => {
                self.buffer.pop();
                Action::Noop
            }

            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                self.buffer.push(c);
                Action::Noop
            }

            _ => Action::Noop,
        }
    }

    /// Render the param input as a small centered overlay.
    pub fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let width = 40u16.min(area.width);
        let height = 7u16.min(area.height);
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let popup = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .title(format!(" Set {} ", self.label()))
            .title_alignment(Alignment::Center)
            .padding(Padding::horizontal(1));

        let inner = block.inner(popup);

        let lines = vec![
            Line::from(vec![
                Span::styled("Current: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&self.current_display, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Magenta).bold()),
                Span::styled(&self.buffer, Style::default().fg(Color::White)),
                Span::styled("_", Style::default().fg(Color::DarkGray)),
            ]),
            Line::default(),
            Line::from(Span::styled(
                self.hint(),
                Style::default().fg(Color::DarkGray),
            )),
        ];

        frame.render_widget(block, popup);
        frame.render_widget(Paragraph::new(lines), inner);
    }
}
