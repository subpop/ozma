use ratatui::style::{Color, Modifier, Style};
use tui_markdown::StyleSheet;

// ── Role colors ────────────────────────────────────────────────────
#[allow(dead_code)]
pub const USER_ROLE_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
#[allow(dead_code)]
pub const ASSISTANT_ROLE_STYLE: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
#[allow(dead_code)]
pub const SYSTEM_ROLE_STYLE: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);

// ── Content styles ─────────────────────────────────────────────────
pub const USER_CONTENT_STYLE: Style = Style::new().fg(Color::White);
pub const SYSTEM_CONTENT_STYLE: Style = Style::new().fg(Color::DarkGray);

// ── UI chrome ──────────────────────────────────────────────────────
#[allow(dead_code)]
pub const BORDER_STYLE: Style = Style::new().fg(Color::DarkGray);
pub const USER_BORDER_STYLE: Style = Style::new().fg(Color::Cyan);
#[allow(dead_code)]
pub const BORDER_FOCUSED_STYLE: Style = Style::new().fg(Color::Cyan);
pub const STATUS_BAR_LABEL: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::Cyan)
    .add_modifier(Modifier::BOLD);
pub const STATUS_BAR_PROVIDER: Style = Style::new().fg(Color::Yellow);
pub const STATUS_BAR_HINT: Style = Style::new().fg(Color::DarkGray);
pub const DIMMED_STYLE: Style = Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM);
pub const STREAMING_INDICATOR: Style = Style::new().fg(Color::Gray);
pub const ACTIVITY_STYLE: Style = Style::new().fg(Color::Cyan);
pub const PARAM_OVERRIDE_STYLE: Style = Style::new().fg(Color::Magenta);

// ── Token usage colors ────────────────────────────────────────────
pub const TOKEN_NORMAL: Style = Style::new().fg(Color::DarkGray);
pub const TOKEN_WARNING: Style = Style::new().fg(Color::Yellow);
pub const TOKEN_CRITICAL: Style = Style::new().fg(Color::Red);

pub fn token_usage_style(usage: f64) -> Style {
    if usage >= 0.9 {
        TOKEN_CRITICAL
    } else if usage >= 0.75 {
        TOKEN_WARNING
    } else {
        TOKEN_NORMAL
    }
}

/// Format a token count for compact display: 0, 1.2k, 45k, 1.2M.
pub fn format_tokens(count: i32) -> String {
    let n = count.unsigned_abs();
    if n < 1_000 {
        format!("{}", n)
    } else if n < 1_000_000 {
        let k = n as f64 / 1_000.0;
        if k < 10.0 {
            format!("{:.1}k", k)
        } else {
            format!("{:.0}k", k)
        }
    } else {
        let m = n as f64 / 1_000_000.0;
        format!("{:.1}M", m)
    }
}

// ── Markdown style sheet ──────────────────────────────────────────

/// Custom markdown style sheet for ozma.
///
/// Provides cohesive styling that matches the overall theme.
#[derive(Debug, Clone)]
pub struct OzStyleSheet;

impl StyleSheet for OzStyleSheet {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::new().fg(Color::Cyan).bold(),
            2 => Style::new().fg(Color::Green).bold(),
            3 => Style::new().fg(Color::Yellow).bold(),
            _ => Style::new().fg(Color::White).bold(),
        }
    }

    fn code(&self) -> Style {
        Style::new().fg(Color::White).bg(Color::Rgb(40, 40, 40))
    }

    fn link(&self) -> Style {
        Style::new().fg(Color::Blue).underlined()
    }

    fn blockquote(&self) -> Style {
        Style::new().fg(Color::DarkGray).italic()
    }

    fn heading_meta(&self) -> Style {
        Style::new().fg(Color::DarkGray).dim()
    }

    fn metadata_block(&self) -> Style {
        Style::new().fg(Color::DarkGray)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tokens_thresholds() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_200), "1.2k");
        assert_eq!(format_tokens(45_000), "45k");
        assert_eq!(format_tokens(1_200_000), "1.2M");
        assert_eq!(format_tokens(-500), "500");
    }

    #[test]
    fn token_usage_style_bands() {
        assert_eq!(token_usage_style(0.0), TOKEN_NORMAL);
        assert_eq!(token_usage_style(0.74), TOKEN_NORMAL);
        assert_eq!(token_usage_style(0.75), TOKEN_WARNING);
        assert_eq!(token_usage_style(0.89), TOKEN_WARNING);
        assert_eq!(token_usage_style(0.9), TOKEN_CRITICAL);
    }
}
