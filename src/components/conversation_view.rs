use std::collections::{HashMap, HashSet};

use ratatui::{
    Frame,
    layout::{Rect, Size},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use ratatui_image::{Resize, picker::Picker, protocol::Protocol};
use uuid::Uuid;

use crate::action::Action;
use crate::components::Component;
use crate::model::conversation::Conversation;
use crate::model::message::{ContentPart, Message, Role};
use crate::store::Store;
use crate::ui::markdown::{self, CodeBlock};
use crate::ui::theme;

use ratatui::style::{Color, Style};

/// Fixed height (in terminal rows) allocated for each inline image.
const IMAGE_DISPLAY_HEIGHT: u16 = 12;

/// Number of lines to show from an inlined `<file>` block before truncating.
const FILE_PREVIEW_LINES: usize = 10;

/// A segment within a rendered message: either text or an image.
enum MessageSegment {
    Text {
        text: Text<'static>,
        code_blocks: Vec<CodeBlock>,
    },
    Image {
        attachment_id: Uuid,
        height: u16,
    },
}

/// A rendered message block with its computed height and role metadata.
struct MessageBlock {
    segments: Vec<MessageSegment>,
    role: Role,
    /// Total height including borders/padding if applicable.
    total_height: u16,
    /// Raw text content of the message (for copying to clipboard).
    raw_content: String,
}

/// Displays the conversation messages in a scrollable view.
///
/// User messages are displayed inside a bordered green box.
/// Assistant messages are rendered as markdown without a box.
/// System messages are shown dimmed.
/// Supports scrolling with PageUp/PageDown and the mouse wheel, and
/// auto-scrolls to the bottom when new content arrives.
pub struct ConversationView {
    /// Vertical scroll offset (in lines).
    scroll_offset: u16,
    /// Total content height from last render (for scroll bounds).
    content_height: u16,
    /// Visible height from last render.
    visible_height: u16,
    /// Whether to auto-scroll to bottom on next render.
    auto_scroll: bool,
    /// Animation tick counter (incremented on each Tick event).
    /// Used to animate the streaming dots indicator.
    anim_tick: u8,
    /// Terminal graphics protocol picker (Kitty/iTerm2/Sixel/halfblocks).
    picker: Option<Picker>,
    /// Cached encoded image protocols, keyed by attachment UUID.
    image_cache: HashMap<Uuid, Protocol>,
    /// Message IDs where inlined `<file>` blocks are shown expanded.
    /// Default: collapsed (showing first FILE_PREVIEW_LINES).
    expanded_files: HashSet<Uuid>,
    /// Absolute Y positions of each code block's first line across the full conversation.
    /// Recomputed on each render pass.
    code_block_positions: Vec<(u16, CodeBlock)>,
    /// Absolute Y ranges and raw content for each rendered message block.
    /// Recomputed on each render pass.
    message_block_positions: Vec<MessageBlockPosition>,
    /// The currently selected block (message or code block).
    selected: Option<Selection>,
    /// The Y offset of the conversation area on screen (set during render).
    render_area_y: u16,
}

/// What is currently selected in the conversation view.
#[derive(Clone, Debug, PartialEq)]
enum Selection {
    /// A whole message block (index into the rendered message blocks).
    Message(usize),
    /// A code block (index into code_block_positions).
    CodeBlock(usize),
}

/// Absolute Y range and raw text content for a rendered message block.
/// Used for mouse hit-testing and copying.
struct MessageBlockPosition {
    /// Absolute Y start (before scroll offset).
    y_start: u16,
    /// Absolute Y end (exclusive).
    y_end: u16,
    /// The raw text content of this message (for copying).
    content: String,
}

/// Spacing between messages (blank lines).
const MESSAGE_GAP: u16 = 1;

impl ConversationView {
    pub fn new() -> Self {
        // Try to detect the terminal's image protocol support.
        let picker = Picker::from_query_stdio().ok();

        Self {
            scroll_offset: 0,
            content_height: 0,
            visible_height: 0,
            auto_scroll: true,
            anim_tick: 0,
            picker,
            image_cache: HashMap::new(),
            expanded_files: HashSet::new(),
            code_block_positions: Vec::new(),
            message_block_positions: Vec::new(),
            selected: None,
            render_area_y: 0,
        }
    }

    /// Reset scroll and cached state for a new or switched conversation,
    /// preserving the terminal image protocol picker (which is expensive
    /// to re-detect).
    pub fn reset(&mut self) {
        self.scroll_offset = 0;
        self.content_height = 0;
        self.visible_height = 0;
        self.auto_scroll = true;
        self.anim_tick = 0;
        self.image_cache.clear();
        self.expanded_files.clear();
        self.code_block_positions.clear();
        self.message_block_positions.clear();
        self.selected = None;
    }

    /// Enable auto-scroll (called when new content arrives).
    #[allow(dead_code)]
    pub fn set_auto_scroll(&mut self, auto: bool) {
        self.auto_scroll = auto;
    }

    /// Toggle expanded/collapsed state for file blocks in the given message.
    pub fn toggle_file_expansion(&mut self, msg_id: Uuid) {
        if !self.expanded_files.remove(&msg_id) {
            self.expanded_files.insert(msg_id);
        }
    }

    /// Maximum scroll offset given content and visible heights.
    fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.visible_height)
    }

    /// Apply a yellow left accent to lines within the selected code block range.
    /// Returns a new `Text` with the highlight applied if the range is valid,
    /// or clones the original unchanged.
    fn highlight_code_block_lines(
        text: &Text<'static>,
        start_line: usize,
        end_line: usize,
    ) -> Text<'static> {
        let mut lines = text.lines.clone();
        let accent = Span::styled("▎", Style::new().fg(Color::Yellow));
        for i in start_line..end_line.min(lines.len()) {
            let mut spans = vec![accent.clone()];
            spans.extend(lines[i].spans.iter().cloned());
            lines[i] = Line::from(spans);
        }
        Text::from(lines)
    }

    /// Return the animated streaming dots string for the current tick.
    fn streaming_dots(anim_tick: u8) -> &'static str {
        match anim_tick % 3 {
            0 => ".",
            1 => "..",
            _ => "...",
        }
    }

    /// Estimate the height (in terminal rows) of a `Text` block given a content width.
    fn text_height(text: &Text<'_>, content_width: u16) -> u16 {
        text.lines
            .iter()
            .map(|line| {
                let line_width = line.width() as u16;
                if content_width == 0 || line_width == 0 {
                    1
                } else {
                    (line_width.saturating_sub(1) / content_width) + 1
                }
            })
            .sum()
    }

    /// Collapse `<file path="...">...</file>` blocks in text to show only
    /// the first FILE_PREVIEW_LINES lines, with a summary of remaining lines.
    fn collapse_file_blocks(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut remaining = text;

        while let Some(start) = remaining.find("<file path=\"") {
            // Copy text before the tag.
            result.push_str(&remaining[..start]);

            // Parse the tag: <file path="...">
            let after_tag_start = &remaining[start..];
            let Some(close_bracket) = after_tag_start.find('>') else {
                // Malformed tag — copy rest and stop.
                result.push_str(after_tag_start);
                remaining = "";
                break;
            };

            // Extract the path attribute.
            let tag = &after_tag_start[..close_bracket + 1];
            let path = after_tag_start
                .get(12..close_bracket) // after `<file path="`
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or("file");

            let content_start = &after_tag_start[close_bracket + 1..];

            // Find the closing </file> tag.
            let Some(end_pos) = content_start.find("</file>") else {
                result.push_str(after_tag_start);
                remaining = "";
                break;
            };

            let file_content = &content_start[..end_pos];
            let after_close = &content_start[end_pos + 7..]; // skip "</file>"

            // Count lines and truncate.
            let lines: Vec<&str> = file_content.lines().collect();
            let total = lines.len();

            if total <= FILE_PREVIEW_LINES + 2 {
                // Short enough — show in full.
                result.push_str(tag);
                result.push_str(file_content);
                result.push_str("</file>");
            } else {
                // Truncate: show first N lines + summary.
                result.push_str(&format!("── {} ", path));
                for _ in 0..40usize.saturating_sub(path.len() + 3) {
                    result.push('─');
                }
                result.push('\n');
                for line in &lines[..FILE_PREVIEW_LINES] {
                    result.push_str(line);
                    result.push('\n');
                }
                result.push_str(&format!(
                    "  ... {} more lines (press 'e' to expand)\n",
                    total - FILE_PREVIEW_LINES
                ));
                for _ in 0..40 {
                    result.push('─');
                }
            }

            remaining = after_close;
        }

        // Append any text after the last file block.
        result.push_str(remaining);
        result
    }

    /// Build the rendered segments for a single message.
    fn build_message_block(
        msg: &Message,
        content_width: u16,
        anim_tick: u8,
        expanded: bool,
    ) -> MessageBlock {
        if msg.compacted {
            let text = Text::from(Line::from(Span::styled(
                format!("{} [compacted]", msg.role),
                theme::DIMMED_STYLE,
            )));
            return MessageBlock {
                total_height: 1,
                segments: vec![MessageSegment::Text {
                    text,
                    code_blocks: Vec::new(),
                }],
                role: msg.role.clone(),
                raw_content: String::new(),
            };
        }

        let content_text = msg.text_content();
        let has_images = msg.has_images();

        let mut extracted_code_blocks: Vec<CodeBlock> = Vec::new();

        let text = if content_text.is_empty() && msg.streaming {
            Text::from(Line::from(Span::styled(
                Self::streaming_dots(anim_tick),
                theme::STREAMING_INDICATOR,
            )))
        } else {
            match msg.role {
                Role::Assistant => {
                    let md = if content_text.is_empty() {
                        markdown::RenderedMarkdown {
                            text: Text::default(),
                            code_blocks: Vec::new(),
                        }
                    } else {
                        markdown::render(&content_text)
                    };
                    extracted_code_blocks = md.code_blocks;
                    let mut rendered = md.text;
                    // Show tool calls if any
                    if !msg.tool_calls.is_empty() {
                        if !content_text.is_empty() {
                            rendered.push_line(Line::default());
                        }
                        for tc in &msg.tool_calls {
                            let args_str =
                                serde_json::to_string(&tc.fn_arguments).unwrap_or_default();
                            let args_display = if args_str.len() > 80 {
                                format!("{}...", &args_str[..77])
                            } else {
                                args_str
                            };
                            rendered.push_line(Line::from(vec![
                                Span::styled("⚡ ", ratatui::style::Style::new().fg(Color::Yellow)),
                                Span::styled(
                                    tc.fn_name.clone(),
                                    ratatui::style::Style::new().fg(Color::Cyan).bold(),
                                ),
                                Span::styled(
                                    format!("({})", args_display),
                                    ratatui::style::Style::new().dim(),
                                ),
                            ]));
                        }
                    }
                    // Per-message token usage annotation
                    if let Some(usage) = &msg.usage {
                        if !msg.streaming {
                            let mut parts = Vec::new();
                            if let Some(p) = usage.prompt_tokens {
                                parts.push(format!("\u{2191}{}", theme::format_tokens(p)));
                            }
                            if let Some(c) = usage.completion_tokens {
                                parts.push(format!("\u{2193}{}", theme::format_tokens(c)));
                            }
                            if !parts.is_empty() {
                                rendered.push_line(Line::from(Span::styled(
                                    parts.join(" "),
                                    ratatui::style::Style::new().fg(Color::DarkGray),
                                )));
                            }
                        }
                    }
                    if msg.streaming {
                        rendered.push_line(Line::from(Span::styled(
                            Self::streaming_dots(anim_tick),
                            theme::STREAMING_INDICATOR,
                        )));
                    }
                    rendered
                }
                Role::User => {
                    let display_text = if expanded {
                        content_text.clone()
                    } else {
                        Self::collapse_file_blocks(&content_text)
                    };
                    let lines: Vec<Line<'static>> = display_text
                        .lines()
                        .map(|l| {
                            // Style "... N more lines" hints and ── lines differently.
                            if l.starts_with("  ... ") && l.ends_with("to expand)") {
                                Line::from(Span::styled(
                                    l.to_string(),
                                    ratatui::style::Style::new().fg(Color::DarkGray).italic(),
                                ))
                            } else if l.chars().all(|c| c == '─')
                                || (l.starts_with("── ") && l.contains('─'))
                            {
                                Line::from(Span::styled(
                                    l.to_string(),
                                    ratatui::style::Style::new().fg(Color::DarkGray),
                                ))
                            } else {
                                Line::from(Span::styled(l.to_string(), theme::USER_CONTENT_STYLE))
                            }
                        })
                        .collect();
                    Text::from(lines)
                }
                Role::System => {
                    let lines: Vec<Line<'static>> = content_text
                        .lines()
                        .map(|l| {
                            Line::from(Span::styled(l.to_string(), theme::SYSTEM_CONTENT_STYLE))
                        })
                        .collect();
                    Text::from(lines)
                }
                Role::Tool => {
                    // Tool result messages — show compactly
                    let result_info = msg.tool_result.as_ref();
                    let fn_name = result_info.map(|r| r.fn_name.as_str()).unwrap_or("tool");
                    let success = result_info.map(|r| r.success).unwrap_or(true);

                    let icon = if success { "✓" } else { "✗" };
                    let icon_color = if success { Color::Green } else { Color::Red };

                    // Truncate long tool output for display
                    let content_display = if content_text.len() > 500 {
                        format!(
                            "{}...\n[{} bytes total]",
                            &content_text[..497],
                            content_text.len()
                        )
                    } else {
                        content_text.clone()
                    };

                    let mut lines = vec![Line::from(vec![
                        Span::styled(
                            format!("{} ", icon),
                            ratatui::style::Style::new().fg(icon_color),
                        ),
                        Span::styled(
                            fn_name.to_string(),
                            ratatui::style::Style::new().fg(Color::Cyan).dim(),
                        ),
                        Span::styled(" result:", ratatui::style::Style::new().dim()),
                    ])];

                    for line in content_display.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            ratatui::style::Style::new().dim(),
                        )));
                    }

                    Text::from(lines)
                }
            }
        };

        // Build segments: text first, then images.
        let mut segments = Vec::new();
        let mut content_height: u16 = 0;

        let text_h = Self::text_height(&text, content_width);
        content_height += text_h;
        segments.push(MessageSegment::Text {
            text,
            code_blocks: extracted_code_blocks,
        });

        // Add image segments for any Image content parts.
        if has_images {
            for part in &msg.content {
                if let ContentPart::Image { id, .. } = part {
                    content_height += IMAGE_DISPLAY_HEIGHT;
                    segments.push(MessageSegment::Image {
                        attachment_id: *id,
                        height: IMAGE_DISPLAY_HEIGHT,
                    });
                }
            }
        }

        // For user messages, add 2 for the border (top + bottom).
        let total_height = match msg.role {
            Role::User => content_height + 2,
            _ => content_height,
        };

        MessageBlock {
            segments,
            role: msg.role.clone(),
            total_height,
            raw_content: content_text,
        }
    }

    /// Ensure an image protocol is cached for the given attachment.
    /// Returns `true` if the protocol is available for rendering.
    fn ensure_image_cached(&mut self, attachment_id: &Uuid, store: &Store) -> bool {
        if self.image_cache.contains_key(attachment_id) {
            return true;
        }
        let picker = match self.picker.as_mut() {
            Some(p) => p,
            None => return false,
        };
        let (_, data) = match store.load_attachment(attachment_id) {
            Ok(Some(v)) => v,
            _ => return false,
        };
        let dyn_img = match image::load_from_memory(&data) {
            Ok(img) => img,
            Err(_) => return false,
        };
        // Encode at a fixed size — will be clipped to actual render area.
        let size = Size::new(80, IMAGE_DISPLAY_HEIGHT);
        match picker.new_protocol(dyn_img, size, Resize::Fit(None)) {
            Ok(proto) => {
                self.image_cache.insert(*attachment_id, proto);
                true
            }
            Err(_) => false,
        }
    }

    /// Render the conversation view with per-message blocks.
    pub fn render_with_conversation(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        conversation: &Conversation,
        store: &Store,
    ) {
        // Outer container (no border, just provides the area)
        let inner = area;
        self.visible_height = inner.height;
        self.render_area_y = area.y;

        // Reserve 1 column on the right for the scrollbar gutter.
        // Messages render inside `msg_inner`; the scrollbar uses the original `inner`.
        let msg_inner = Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        };

        if conversation.messages.is_empty() {
            let text = Text::from(vec![
                Line::default(),
                Line::from(Span::styled(
                    "No messages yet. Type a message below to start.",
                    theme::DIMMED_STYLE,
                )),
            ]);
            let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);
            self.content_height = 2;
            return;
        }

        // Content width available for text (accounting for user message borders + padding)
        let user_content_width = msg_inner.width.saturating_sub(6); // 2 border + 4 padding (2 each side)
        let assistant_content_width = msg_inner.width.saturating_sub(2); // 2 for left padding

        // Build all message blocks and compute total height
        let anim_tick = self.anim_tick;
        let blocks: Vec<MessageBlock> = conversation
            .messages
            .iter()
            .map(|msg| {
                let width = match msg.role {
                    Role::User => user_content_width,
                    _ => assistant_content_width,
                };
                let expanded = self.expanded_files.contains(&msg.id);
                Self::build_message_block(msg, width, anim_tick, expanded)
            })
            .collect();

        let total_height: u16 = blocks.iter().map(|b| b.total_height).sum::<u16>()
            + (blocks.len().saturating_sub(1) as u16) * MESSAGE_GAP;

        self.content_height = total_height;

        // Collect absolute Y positions of all code blocks and message blocks.
        self.code_block_positions.clear();
        self.message_block_positions.clear();
        let mut abs_y: u16 = 0;
        for block in &blocks {
            // Track message block position for mouse hit-testing.
            self.message_block_positions.push(MessageBlockPosition {
                y_start: abs_y,
                y_end: abs_y + block.total_height,
                content: block.raw_content.clone(),
            });
            // For user messages, content starts 1 row below abs_y (top border).
            let content_y = if block.role == Role::User {
                abs_y + 1
            } else {
                abs_y
            };
            for segment in &block.segments {
                if let MessageSegment::Text { code_blocks, .. } = segment {
                    for cb in code_blocks {
                        let cb_abs_y = content_y + cb.start_line as u16;
                        self.code_block_positions.push((cb_abs_y, cb.clone()));
                    }
                }
            }
            abs_y += block.total_height + MESSAGE_GAP;
        }

        // Auto-scroll to bottom if enabled
        if self.auto_scroll {
            self.scroll_offset = self.max_scroll();
        }

        // Render each message block, applying scroll offset
        let mut y_pos: i32 = -(self.scroll_offset as i32);
        let selected = self.selected.clone();

        for (block_idx, block) in blocks.iter().enumerate() {
            let block_top = y_pos;
            let block_bottom = y_pos + block.total_height as i32;

            // Skip if entirely above visible area
            if block_bottom <= 0 {
                y_pos += block.total_height as i32 + MESSAGE_GAP as i32;
                continue;
            }

            // Stop if entirely below visible area
            if block_top >= msg_inner.height as i32 {
                break;
            }

            let is_msg_selected = selected == Some(Selection::Message(block_idx));

            // Determine if a code block within this message block is selected,
            // and if so, which local line range should be highlighted.
            let selected_cb_range: Option<(usize, usize)> =
                if let Some(Selection::CodeBlock(sel_idx)) = &selected {
                    // Look up the selected code block's absolute position.
                    self.code_block_positions
                        .get(*sel_idx)
                        .and_then(|(sel_pos, sel_cb)| {
                            // Check if this code block belongs to the current message block.
                            let block_start = self.message_block_positions.get(block_idx)?.y_start;
                            let block_end = self.message_block_positions.get(block_idx)?.y_end;
                            if *sel_pos >= block_start && *sel_pos < block_end {
                                Some((sel_cb.start_line, sel_cb.end_line))
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };

            if block.role == Role::User {
                // User messages: render a single border encompassing all segments.
                let render_y = block_top.max(0) as u16 + msg_inner.y;
                let clip_top = if block_top < 0 {
                    (-block_top) as u16
                } else {
                    0
                };
                let available_height = (msg_inner.y + msg_inner.height).saturating_sub(render_y);
                let render_height = (block.total_height - clip_top).min(available_height);

                if render_height > 0 {
                    let msg_area = Rect {
                        x: msg_inner.x,
                        y: render_y,
                        width: msg_inner.width,
                        height: render_height,
                    };

                    // Draw the border frame across the entire block.
                    let border_style = if is_msg_selected {
                        Style::new().fg(Color::Yellow)
                    } else {
                        theme::USER_BORDER_STYLE
                    };
                    let border_block = Block::default()
                        .borders(Borders::ALL)
                        .border_style(border_style)
                        .padding(Padding::horizontal(1));
                    frame.render_widget(border_block, msg_area);

                    // Render segments inside the border.
                    // Border consumes 1 row top + 1 row bottom, 1 col left + 1 col right,
                    // plus 1 col horizontal padding each side.
                    let inner_x = msg_inner.x + 3; // 1 border + 1 padding + 1 padding
                    let inner_w = msg_inner.width.saturating_sub(6);
                    let mut seg_y = block_top + 1; // +1 for top border row

                    for segment in &block.segments {
                        match segment {
                            MessageSegment::Text { text, .. } => {
                                let seg_h = Self::text_height(text, user_content_width);
                                let seg_top = seg_y;
                                let seg_bottom = seg_y + seg_h as i32;

                                if seg_bottom > 0 && seg_top < msg_inner.height as i32 {
                                    let text_render_y = seg_top.max(0) as u16 + msg_inner.y;
                                    let text_clip = if seg_top < 0 { (-seg_top) as u16 } else { 0 };
                                    let text_avail = (msg_inner.y + msg_inner.height)
                                        .saturating_sub(text_render_y);
                                    let text_h = (seg_h - text_clip).min(text_avail);

                                    if text_h > 0 {
                                        let text_area = Rect {
                                            x: inner_x,
                                            y: text_render_y,
                                            width: inner_w,
                                            height: text_h,
                                        };
                                        let render_text = if let Some((cb_start, cb_end)) =
                                            selected_cb_range
                                        {
                                            Self::highlight_code_block_lines(text, cb_start, cb_end)
                                        } else {
                                            text.clone()
                                        };
                                        let paragraph = Paragraph::new(render_text)
                                            .wrap(Wrap { trim: false })
                                            .scroll((text_clip, 0));
                                        frame.render_widget(paragraph, text_area);
                                    }
                                }
                                seg_y += seg_h as i32;
                            }
                            MessageSegment::Image {
                                attachment_id,
                                height,
                            } => {
                                let seg_top = seg_y;
                                let seg_bottom = seg_y + *height as i32;

                                if seg_bottom > 0 && seg_top < msg_inner.height as i32 {
                                    let img_render_y = seg_top.max(0) as u16 + msg_inner.y;
                                    let img_avail = (msg_inner.y + msg_inner.height)
                                        .saturating_sub(img_render_y);
                                    let img_h = (*height).min(img_avail);

                                    if img_h > 0 {
                                        let img_area = Rect {
                                            x: inner_x,
                                            y: img_render_y,
                                            width: inner_w,
                                            height: img_h,
                                        };

                                        if self.ensure_image_cached(attachment_id, store) {
                                            if let Some(proto) = self.image_cache.get(attachment_id)
                                            {
                                                let img_widget = ratatui_image::Image::new(proto);
                                                frame.render_widget(img_widget, img_area);
                                            }
                                        } else {
                                            let placeholder =
                                                Paragraph::new(Line::from(Span::styled(
                                                    "[image]",
                                                    ratatui::style::Style::new().dim().italic(),
                                                )));
                                            frame.render_widget(placeholder, img_area);
                                        }
                                    }
                                }
                                seg_y += *height as i32;
                            }
                        }
                    }
                }
            } else {
                // Non-user messages: render segments sequentially without a border.
                // When selected, show a left accent bar.
                let mut seg_y = y_pos;

                for segment in &block.segments {
                    match segment {
                        MessageSegment::Text { text, .. } => {
                            let seg_h = Self::text_height(text, assistant_content_width);
                            let seg_top = seg_y;
                            let seg_bottom = seg_y + seg_h as i32;

                            if seg_bottom > 0 && seg_top < msg_inner.height as i32 {
                                let render_y = seg_top.max(0) as u16 + msg_inner.y;
                                let clip_top = if seg_top < 0 { (-seg_top) as u16 } else { 0 };
                                let available_height =
                                    (msg_inner.y + msg_inner.height).saturating_sub(render_y);
                                let render_height = (seg_h - clip_top).min(available_height);

                                if render_height > 0 {
                                    let msg_area = Rect {
                                        x: msg_inner.x,
                                        y: render_y,
                                        width: msg_inner.width,
                                        height: render_height,
                                    };
                                    let render_text =
                                        if let Some((cb_start, cb_end)) = selected_cb_range {
                                            Self::highlight_code_block_lines(text, cb_start, cb_end)
                                        } else {
                                            text.clone()
                                        };
                                    let padding_block = if is_msg_selected {
                                        Block::default()
                                            .borders(Borders::LEFT)
                                            .border_style(Style::new().fg(Color::Yellow))
                                    } else {
                                        Block::default().padding(Padding::horizontal(1))
                                    };
                                    let paragraph = Paragraph::new(render_text)
                                        .block(padding_block)
                                        .wrap(Wrap { trim: false })
                                        .scroll((clip_top, 0));
                                    frame.render_widget(paragraph, msg_area);
                                }
                            }
                            seg_y += seg_h as i32;
                        }
                        MessageSegment::Image {
                            attachment_id,
                            height,
                        } => {
                            let seg_top = seg_y;
                            let seg_bottom = seg_y + *height as i32;

                            if seg_bottom > 0 && seg_top < msg_inner.height as i32 {
                                let render_y = seg_top.max(0) as u16 + msg_inner.y;
                                let available_height =
                                    (msg_inner.y + msg_inner.height).saturating_sub(render_y);
                                let render_height = (*height).min(available_height);

                                if render_height > 0 {
                                    let img_area = Rect {
                                        x: msg_inner.x + 2,
                                        y: render_y,
                                        width: msg_inner.width.saturating_sub(4),
                                        height: render_height,
                                    };

                                    if self.ensure_image_cached(attachment_id, store) {
                                        if let Some(proto) = self.image_cache.get(attachment_id) {
                                            let img_widget = ratatui_image::Image::new(proto);
                                            frame.render_widget(img_widget, img_area);
                                        }
                                    } else {
                                        let placeholder = Paragraph::new(Line::from(Span::styled(
                                            "[image]",
                                            ratatui::style::Style::new().dim().italic(),
                                        )));
                                        frame.render_widget(placeholder, img_area);
                                    }
                                }
                            }
                            seg_y += *height as i32;
                        }
                    }
                }
            }

            y_pos += block.total_height as i32 + MESSAGE_GAP as i32;
        }

        // Render scrollbar if content exceeds visible area
        if self.content_height > self.visible_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None);

            let mut scrollbar_state = ScrollbarState::new(self.max_scroll() as usize)
                .position(self.scroll_offset as usize);

            let scrollbar_area = Rect {
                x: inner.x + inner.width - 1,
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
        }
    }
}

impl Component for ConversationView {
    fn update(&mut self, action: &Action) -> Option<Action> {
        match action {
            Action::Tick => {
                self.anim_tick = self.anim_tick.wrapping_add(1);
                None
            }
            Action::ScrollUp => {
                self.auto_scroll = false;
                self.scroll_offset = self.scroll_offset.saturating_sub(3);
                None
            }
            Action::ScrollDown => {
                self.auto_scroll = false;
                self.scroll_offset = (self.scroll_offset + 3).min(self.max_scroll());
                // Re-enable auto-scroll if we're at the bottom
                if self.scroll_offset >= self.max_scroll() {
                    self.auto_scroll = true;
                }
                None
            }
            Action::ScrollPageUp => {
                self.auto_scroll = false;
                let page = self.visible_height.max(1);
                self.scroll_offset = self.scroll_offset.saturating_sub(page);
                None
            }
            Action::ScrollPageDown => {
                self.auto_scroll = false;
                let page = self.visible_height.max(1);
                self.scroll_offset = (self.scroll_offset + page).min(self.max_scroll());
                if self.scroll_offset >= self.max_scroll() {
                    self.auto_scroll = true;
                }
                None
            }
            Action::ScrollToBottom => {
                self.auto_scroll = true;
                self.scroll_offset = self.max_scroll();
                None
            }
            Action::ClickAt(_col, row) => {
                // Map the screen row to an absolute content position.
                let abs_y = self.scroll_offset + row.saturating_sub(self.render_area_y);

                // First, check if the click lands on a code block (more specific).
                let code_hit =
                    self.code_block_positions
                        .iter()
                        .enumerate()
                        .find(|(_, (pos, cb))| {
                            let cb_end = *pos + (cb.end_line as u16 - cb.start_line as u16);
                            abs_y >= *pos && abs_y < cb_end
                        });
                if let Some((idx, _)) = code_hit {
                    self.selected = Some(Selection::CodeBlock(idx));
                    return None;
                }

                // Otherwise, check if the click lands on a message block.
                let msg_hit = self
                    .message_block_positions
                    .iter()
                    .enumerate()
                    .find(|(_, mbp)| abs_y >= mbp.y_start && abs_y < mbp.y_end);
                if let Some((idx, _)) = msg_hit {
                    self.selected = Some(Selection::Message(idx));
                } else {
                    // Clicked on gap / empty space — clear selection.
                    self.selected = None;
                }
                None
            }
            Action::CopySelection => {
                // Copy the content of the currently selected block.
                match &self.selected {
                    Some(Selection::CodeBlock(idx)) => self
                        .code_block_positions
                        .get(*idx)
                        .map(|(_, cb)| Action::CopyCodeBlockResult(cb.content.clone())),
                    Some(Selection::Message(idx)) => self
                        .message_block_positions
                        .get(*idx)
                        .map(|mbp| Action::CopyCodeBlockResult(mbp.content.clone())),
                    None => Some(Action::ExportError("No block selected".into())),
                }
            }
            Action::SubmitMessage { .. }
            | Action::AppendAssistantChunk(_)
            | Action::AssistantMessageComplete { .. } => {
                // New content: auto-scroll to bottom, clear selection.
                self.auto_scroll = true;
                self.selected = None;
                None
            }
            _ => None,
        }
    }

    fn render(&mut self, _frame: &mut Frame, _area: Rect) {
        // Use render_with_conversation instead — this is a no-op.
        // The Home component calls render_with_conversation directly.
    }
}
