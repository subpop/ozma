use chrono::Local;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};

use crate::action::Action;
use crate::components::Component;
use crate::ui::theme;

/// Maximum number of retained log lines. Oldest lines are dropped when the
/// cap is exceeded, keeping memory bounded for chatty servers.
const MAX_LINES: usize = 2000;

/// One captured stderr line from an MCP server process.
struct McpLogLine {
    /// Local wall-clock time the line was captured (HH:MM:SS).
    time: String,
    /// Name of the MCP server that produced the line.
    server: String,
    /// The stderr line content.
    text: String,
}

/// An overlay that displays stderr output captured from stdio MCP servers.
///
/// Keybindings:
/// - Up/Down or j/k: scroll
/// - PageUp/PageDown: scroll by page
/// - Home/g, End/G: jump to top/bottom
/// - Esc or Ctrl+L: close the overlay
pub struct McpLogView {
    /// Whether this overlay is currently visible.
    pub visible: bool,
    /// Captured log lines (bounded to MAX_LINES).
    lines: Vec<McpLogLine>,
    /// Vertical scroll offset, in lines.
    scroll_offset: usize,
    /// Height of the visible area, tracked during render.
    visible_height: usize,
    /// Whether the view follows newly appended lines (pinned to bottom).
    auto_scroll: bool,
}

impl McpLogView {
    pub fn new() -> Self {
        Self {
            visible: false,
            lines: Vec::new(),
            scroll_offset: 0,
            visible_height: 0,
            auto_scroll: true,
        }
    }

    /// Show the overlay, pinned to the most recent lines.
    pub fn show(&mut self) {
        self.visible = true;
        self.auto_scroll = true;
        self.scroll_to_bottom();
    }

    /// Hide the overlay.
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Append a captured stderr line from the named server.
    pub fn push(&mut self, server: &str, text: &str) {
        self.lines.push(McpLogLine {
            time: Local::now().format("%H:%M:%S").to_string(),
            server: server.to_string(),
            text: text.to_string(),
        });
        if self.lines.len() > MAX_LINES {
            self.lines.drain(..self.lines.len() - MAX_LINES);
            self.scroll_offset = self.scroll_offset.saturating_sub(1);
        }
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Maximum scroll offset given the current content and viewport height.
    fn max_offset(&self) -> usize {
        self.lines.len().saturating_sub(self.visible_height)
    }

    /// Pin the viewport to the bottom of the log.
    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_offset();
    }

    /// Scroll up by `n` lines, detaching from the tail.
    fn scroll_up(&mut self, n: usize) {
        self.auto_scroll = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Scroll down by `n` lines, re-attaching to the tail at the bottom.
    fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = (self.scroll_offset + n).min(self.max_offset());
        if self.scroll_offset >= self.max_offset() {
            self.auto_scroll = true;
        }
    }

    /// Render the log as a centered overlay.
    pub fn render_overlay(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        // 80% width, 80% height, centered — logs benefit from the space.
        let overlay_width = (area.width * 80 / 100).max(40).min(area.width);
        let overlay_height = (area.height * 80 / 100).max(10).min(area.height);

        let horizontal_margin = (area.width.saturating_sub(overlay_width)) / 2;
        let vertical_margin = (area.height.saturating_sub(overlay_height)) / 2;

        let overlay_area = Rect {
            x: area.x + horizontal_margin,
            y: area.y + vertical_margin,
            width: overlay_width,
            height: overlay_height,
        };

        frame.render_widget(Clear, overlay_area);

        let title = if self.lines.is_empty() {
            " MCP Server Logs ".to_string()
        } else {
            format!(" MCP Server Logs ({}) ", self.lines.len())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(title)
            .title_alignment(Alignment::Center)
            .padding(Padding::horizontal(1));

        let inner = block.inner(overlay_area);
        self.visible_height = inner.height as usize;

        if self.auto_scroll {
            self.scroll_to_bottom();
        }

        if self.lines.is_empty() {
            let empty = Paragraph::new(Line::from(Span::styled(
                "No MCP server output captured",
                theme::DIMMED_STYLE,
            )))
            .alignment(Alignment::Center);
            let centered = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .split(inner);
            frame.render_widget(block, overlay_area);
            frame.render_widget(empty, centered[1]);
        } else {
            let lines: Vec<Line> = self
                .lines
                .iter()
                .map(|l| {
                    Line::from(vec![
                        Span::styled(format!("{} ", l.time), theme::DIMMED_STYLE),
                        Span::styled(
                            format!("[{}] ", l.server),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(l.text.clone(), Style::default().fg(Color::White)),
                    ])
                })
                .collect();
            let paragraph = Paragraph::new(lines)
                .block(block)
                .scroll((self.scroll_offset as u16, 0));
            frame.render_widget(paragraph, overlay_area);
        }

        // Help line at the bottom of the overlay
        let help_area = Rect {
            x: overlay_area.x + 1,
            y: overlay_area.y + overlay_area.height - 1,
            width: overlay_area.width.saturating_sub(2),
            height: 1,
        };
        let help = Paragraph::new(Line::from(vec![
            Span::styled("j/k ↑/↓", Style::default().fg(Color::Cyan)),
            Span::raw(" scroll  "),
            Span::styled("PgUp/PgDn", Style::default().fg(Color::Cyan)),
            Span::raw(" page  "),
            Span::styled("g/G", Style::default().fg(Color::Cyan)),
            Span::raw(" top/bottom  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" close"),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(help, help_area);
    }
}

impl Component for McpLogView {
    fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        if !self.visible {
            return Action::Noop;
        }

        let page = self.visible_height.saturating_sub(2).max(1);

        match (key.code, key.modifiers) {
            // Close overlay
            (KeyCode::Esc, _) => Action::CloseMcpLog,
            (KeyCode::Char('l'), m) if m.contains(KeyModifiers::CONTROL) => Action::CloseMcpLog,

            // Quit still works from overlay
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,

            // Scrolling
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.scroll_up(1);
                Action::Noop
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.scroll_down(1);
                Action::Noop
            }
            (KeyCode::PageUp, _) => {
                self.scroll_up(page);
                Action::Noop
            }
            (KeyCode::PageDown, _) => {
                self.scroll_down(page);
                Action::Noop
            }
            (KeyCode::Home | KeyCode::Char('g'), _) => {
                self.auto_scroll = false;
                self.scroll_offset = 0;
                Action::Noop
            }
            (KeyCode::End | KeyCode::Char('G'), _) => {
                self.auto_scroll = true;
                self.scroll_to_bottom();
                Action::Noop
            }

            _ => Action::Noop,
        }
    }

    fn render(&mut self, _frame: &mut Frame, _area: Rect) {
        // Use render_overlay instead — Home calls it directly for overlay semantics.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_appends_and_follows_tail() {
        let mut view = McpLogView::new();
        view.visible_height = 10;
        for i in 0..25 {
            view.push("srv", &format!("line {i}"));
        }
        assert_eq!(view.lines.len(), 25);
        // Auto-scroll keeps the viewport pinned to the bottom.
        assert!(view.auto_scroll);
        assert_eq!(view.scroll_offset, 25 - 10);
        assert_eq!(view.lines.last().unwrap().text, "line 24");
    }

    #[test]
    fn buffer_is_bounded() {
        let mut view = McpLogView::new();
        for i in 0..(MAX_LINES + 50) {
            view.push("srv", &format!("line {i}"));
        }
        assert_eq!(view.lines.len(), MAX_LINES);
        // Oldest lines were dropped; the newest survive.
        assert_eq!(
            view.lines.last().unwrap().text,
            format!("line {}", MAX_LINES + 49)
        );
    }

    #[test]
    fn scrolling_detaches_and_reattaches() {
        let mut view = McpLogView::new();
        view.visible_height = 10;
        for i in 0..30 {
            view.push("srv", &format!("line {i}"));
        }
        assert_eq!(view.scroll_offset, 20);

        // Scrolling up detaches from the tail.
        view.scroll_up(5);
        assert!(!view.auto_scroll);
        assert_eq!(view.scroll_offset, 15);

        // New lines no longer move the viewport.
        view.push("srv", "more");
        assert_eq!(view.scroll_offset, 15);

        // Scrolling back to the bottom re-attaches.
        view.scroll_down(20);
        assert!(view.auto_scroll);
        assert_eq!(view.scroll_offset, view.max_offset());
    }
}
