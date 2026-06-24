use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use ratatui_image::{Resize, picker::Picker, protocol::Protocol};
use tui_textarea::TextArea;
use uuid::Uuid;

use crate::action::Action;
use crate::components::Component;

/// An image attachment pending submission (held in memory until the message is sent).
#[derive(Clone, Debug)]
pub struct PendingAttachment {
    pub id: Uuid,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Supported image extensions for `@path` references.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// Marker character for image attachments in the text buffer.
/// One marker per attachment, in insertion order.
const ATTACHMENT_MARKER: char = '📎';

/// A multi-line text input component for composing messages.
///
/// Uses `tui-textarea` for editing. Key bindings follow CUA (Common User Access):
/// - Enter: submit the message (when not empty)
/// - Alt+Enter / Shift+Enter: insert a newline
/// - Ctrl+Q: quit the application
/// - Ctrl+C: copy selected block to clipboard
/// - Esc: clear the input (if non-empty)
/// - Up/Down: navigate through sent message history (when on first/last line)
/// Height of the attachment preview bar (rows).
const ATTACHMENT_BAR_HEIGHT: u16 = 3;

/// Maximum number of completion candidates shown in the popup.
const MAX_COMPLETIONS: usize = 8;

/// State for the inline @-path completion popup.
struct PathCompletion {
    /// Row in the textarea where the `@` token starts.
    start_row: usize,
    /// Column in the textarea where the `@` token starts (the `@` itself).
    start_col: usize,
    /// Matching filesystem entries (display names, possibly with trailing `/`).
    candidates: Vec<String>,
    /// Currently highlighted index in `candidates`.
    selected: usize,
}

pub struct Input<'a> {
    textarea: TextArea<'a>,
    /// Whether this component currently has focus.
    focused: bool,
    /// Previously sent messages (oldest first).
    history: Vec<String>,
    /// Current position in the history. `None` means we're editing a new message (draft).
    /// `Some(i)` means we're viewing `history[i]`.
    history_index: Option<usize>,
    /// Saved draft text when navigating into history.
    draft: String,
    /// Image attachments queued for the next submit.
    pending_attachments: Vec<PendingAttachment>,
    /// Terminal graphics protocol picker for thumbnails.
    picker: Option<Picker>,
    /// Cached thumbnail protocols for pending attachments.
    thumbnail_cache: HashMap<Uuid, Protocol>,
    /// Active path completion state (shown when typing `@path`).
    completion: Option<PathCompletion>,
}

impl<'a> Input<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Type a message... (Enter to send, Alt+Enter for newline)");
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Message "),
        );
        let picker = Picker::from_query_stdio().ok();
        Self {
            textarea,
            focused: true,
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            pending_attachments: Vec::new(),
            picker,
            thumbnail_cache: HashMap::new(),
            completion: None,
        }
    }

    /// Whether there are pending attachments (used by Home to adjust layout).
    pub fn has_attachments(&self) -> bool {
        !self.pending_attachments.is_empty()
    }

    /// Total height needed for this component.
    pub fn desired_height(&self) -> u16 {
        if self.has_attachments() {
            5 + ATTACHMENT_BAR_HEIGHT
        } else {
            5
        }
    }

    /// Whether the input has focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Set focus state and update border styling.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.update_border_style();
    }

    /// Update the border color based on focus state.
    fn update_border_style(&mut self) {
        let border_color = if self.focused {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        self.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(" Message "),
        );
    }

    /// Extract the current text and clear the input.
    fn take_text(&mut self) -> String {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        // Clear the textarea by selecting all and deleting
        self.textarea.select_all();
        self.textarea.cut();
        text
    }

    /// Insert text at the cursor (used for paste events).
    pub fn insert_text(&mut self, text: &str) {
        self.textarea.insert_str(text);
    }

    /// Push a submitted message into the history ring.
    pub fn push_history(&mut self, text: &str) {
        // Don't duplicate the most recent entry.
        if self.history.last().map(|s| s.as_str()) != Some(text) {
            self.history.push(text.to_string());
        }
        self.history_index = None;
        self.draft.clear();
    }

    /// Get the current textarea content as a single string.
    fn current_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Replace the textarea contents with the given text.
    fn set_text(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
    }

    /// Whether the cursor is on the first line of the textarea.
    fn cursor_on_first_line(&self) -> bool {
        self.textarea.cursor().0 == 0
    }

    /// Whether the cursor is on the last line of the textarea.
    fn cursor_on_last_line(&self) -> bool {
        self.textarea.cursor().0 == self.textarea.lines().len().saturating_sub(1)
    }

    /// Navigate to the previous history entry (Up).
    /// Returns true if navigation happened.
    fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        match self.history_index {
            None => {
                // Save current text as draft, go to most recent history entry
                self.draft = self.current_text();
                let idx = self.history.len() - 1;
                self.history_index = Some(idx);
                self.set_text(&self.history[idx].clone());
                true
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.history_index = Some(new_idx);
                self.set_text(&self.history[new_idx].clone());
                true
            }
            _ => false, // Already at oldest entry
        }
    }

    /// Navigate to the next history entry (Down).
    /// Returns true if navigation happened.
    fn history_next(&mut self) -> bool {
        match self.history_index {
            Some(idx) => {
                if idx + 1 < self.history.len() {
                    let new_idx = idx + 1;
                    self.history_index = Some(new_idx);
                    self.set_text(&self.history[new_idx].clone());
                } else {
                    // Return to draft
                    self.history_index = None;
                    let draft = self.draft.clone();
                    self.set_text(&draft);
                }
                true
            }
            None => false, // Already at draft
        }
    }

    /// Attempt to paste an image from the system clipboard.
    /// Returns true if an image was found and attached.
    fn try_paste_clipboard_image(&mut self) -> bool {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return false;
        };
        let Ok(img_data) = clipboard.get_image() else {
            return false;
        };

        // Convert RGBA pixels to PNG bytes via the `image` crate.
        let Some(rgba) = image::RgbaImage::from_raw(
            img_data.width as u32,
            img_data.height as u32,
            img_data.bytes.into_owned(),
        ) else {
            return false;
        };
        let mut png_bytes: Vec<u8> = Vec::new();
        let dyn_img = image::DynamicImage::ImageRgba8(rgba);
        if dyn_img
            .write_to(
                &mut std::io::Cursor::new(&mut png_bytes),
                image::ImageFormat::Png,
            )
            .is_err()
        {
            return false;
        }

        let id = Uuid::new_v4();

        // Cache a thumbnail protocol for rendering in the attachment bar.
        self.cache_thumbnail(id, &dyn_img);

        self.pending_attachments.push(PendingAttachment {
            id,
            mime_type: "image/png".into(),
            data: png_bytes,
        });

        // Insert an atomic placeholder character in the textarea.
        self.textarea.insert_char(ATTACHMENT_MARKER);
        self.textarea.insert_char(' ');
        true
    }

    /// Get the full text content of the textarea as a single string.
    fn full_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Count attachment marker chars in the textarea.
    fn marker_count(&self) -> usize {
        self.full_text()
            .chars()
            .filter(|&c| c == ATTACHMENT_MARKER)
            .count()
    }

    /// After a text-modifying key event, reconcile pending attachments with
    /// the marker chars still present in the text. Attachments are kept
    /// in insertion order; markers also appear in insertion order, so we
    /// trim from the end when counts diverge.
    fn reconcile_attachments(&mut self) {
        let count = self.marker_count();
        while self.pending_attachments.len() > count {
            if let Some(removed) = self.pending_attachments.pop() {
                self.thumbnail_cache.remove(&removed.id);
            }
        }
    }

    /// Try to atomically delete the marker and its trailing space before
    /// the cursor. Returns true if a marker token was deleted.
    fn try_delete_marker_before_cursor(&mut self) -> bool {
        let (row, col) = self.textarea.cursor();
        let line = &self.textarea.lines()[row];
        if col == 0 {
            return false;
        }

        let prev_char = line.chars().nth(col - 1);

        if prev_char == Some(ATTACHMENT_MARKER) {
            // Cursor is right after "📎|". Delete the marker backward.
            self.textarea.delete_char();
            // Eat trailing space if it's now under the cursor.
            let (r, c) = self.textarea.cursor();
            if r < self.textarea.lines().len() {
                if self.textarea.lines()[r].chars().nth(c) == Some(' ') {
                    self.textarea.delete_next_char();
                }
            }
            self.reconcile_attachments();
            return true;
        }

        if prev_char == Some(' ') && col >= 2 {
            // Cursor is after the trailing space "📎 |". Delete space then marker.
            if line.chars().nth(col - 2) == Some(ATTACHMENT_MARKER) {
                self.textarea.delete_char(); // delete the space
                self.textarea.delete_char(); // delete the marker
                self.reconcile_attachments();
                return true;
            }
        }

        false
    }

    /// Encode a thumbnail protocol for the given image and cache it.
    fn cache_thumbnail(&mut self, id: Uuid, dyn_img: &image::DynamicImage) {
        if let Some(picker) = self.picker.as_mut() {
            let size = Size::new(10, ATTACHMENT_BAR_HEIGHT);
            if let Ok(proto) = picker.new_protocol(dyn_img.clone(), size, Resize::Fit(None)) {
                self.thumbnail_cache.insert(id, proto);
            }
        }
    }

    /// Scan `text` for `@path` tokens pointing to files.
    /// Returns `(cleaned_text, attachments)`.
    /// - Image files (`png`, `jpg`, etc.) become binary `PendingAttachment`s.
    /// - Text files have their contents inlined as `<file path="...">...</file>`.
    /// - Unreadable paths are kept as literal `@path` text.
    fn extract_file_attachments(text: &str) -> (String, Vec<PendingAttachment>) {
        let mut attachments = Vec::new();
        let mut cleaned = String::with_capacity(text.len());
        let mut chars = text.char_indices().peekable();

        while let Some((i, ch)) = chars.next() {
            if ch != '@' {
                cleaned.push(ch);
                continue;
            }

            // Collect the token after '@' (non-whitespace).
            let start = i + 1;
            let mut end = start;
            while let Some(&(j, c)) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                end = j + c.len_utf8();
                chars.next();
            }

            if end == start {
                // Lone '@' — keep it.
                cleaned.push('@');
                continue;
            }

            let raw_token = &text[start..end];

            // Strip trailing sentence punctuation that isn't part of the path.
            let token = raw_token
                .trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')'));
            let trailing = &raw_token[token.len()..];

            if token.is_empty() {
                cleaned.push('@');
                cleaned.push_str(trailing);
                continue;
            }

            // Expand `~` to home directory.
            let expanded: String;
            let resolved = if token == "~" || token.starts_with("~/") {
                if let Some(home) = dirs::home_dir() {
                    expanded = format!("{}{}", home.display(), &token[1..]);
                    expanded.as_str()
                } else {
                    token
                }
            } else {
                token
            };
            let path = PathBuf::from(resolved);

            // Check if this is a readable file at all.
            if !path.is_file() {
                // Not a file — keep the original text.
                cleaned.push('@');
                cleaned.push_str(raw_token);
                continue;
            }

            // Check image extension.
            let is_image = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
                .unwrap_or(false);

            if is_image {
                // Binary image attachment.
                match std::fs::read(&path) {
                    Ok(data) => {
                        let ext = path.extension().unwrap().to_str().unwrap().to_lowercase();
                        let mime_type = match ext.as_str() {
                            "jpg" | "jpeg" => "image/jpeg",
                            "png" => "image/png",
                            "gif" => "image/gif",
                            "webp" => "image/webp",
                            "bmp" => "image/bmp",
                            _ => "application/octet-stream",
                        }
                        .to_string();
                        let filename = path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let id = Uuid::new_v4();
                        attachments.push(PendingAttachment {
                            id,
                            mime_type,
                            data,
                        });
                        cleaned.push_str(&format!("[attached: {}]", filename));
                        cleaned.push_str(trailing);
                    }
                    Err(_) => {
                        cleaned.push('@');
                        cleaned.push_str(raw_token);
                    }
                }
            } else {
                // Text file — inline its contents.
                match std::fs::read_to_string(&path) {
                    Ok(contents) => {
                        let display_path = token; // keep the user's original path form
                        cleaned.push_str(&format!(
                            "<file path=\"{}\">\n{}\n</file>",
                            display_path, contents
                        ));
                        cleaned.push_str(trailing);
                    }
                    Err(_) => {
                        // Binary or unreadable — keep original text.
                        cleaned.push('@');
                        cleaned.push_str(raw_token);
                    }
                }
            }
        }

        (cleaned, attachments)
    }

    /// Remove attachment markers (📎 placeholders, `[attached: ...]`) from the message text.
    /// These are visual indicators for the user, not content for the model.
    fn strip_attachment_markers(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        for ch in text.chars() {
            if ch == ATTACHMENT_MARKER {
                continue;
            }
            result.push(ch);
        }
        // Remove file attachment markers: [attached: filename.ext]
        while let Some(start) = result.find("[attached: ") {
            if let Some(end) = result[start..].find(']') {
                let remove_end = start + end + 1;
                let remove_end = if result.as_bytes().get(remove_end) == Some(&b' ') {
                    remove_end + 1
                } else {
                    remove_end
                };
                result.replace_range(start..remove_end, "");
            } else {
                break;
            }
        }
        result.trim().to_string()
    }

    // ── @-path completion ────────────────────────────────────────────

    /// Scan backward from the cursor to find an `@` token on the current line.
    /// Returns `(row, col_of_at, text_after_at)` if found.
    fn find_at_token(&self) -> Option<(usize, usize, String)> {
        let (row, col) = self.textarea.cursor();
        let line = &self.textarea.lines()[row];
        let before_cursor = &line[..col.min(line.len())];

        // Walk backward to find `@`.
        let at_pos = before_cursor.rfind('@')?;

        // The text between `@` and the cursor is the partial path.
        let after_at = &before_cursor[at_pos + 1..];

        // Reject if there's a space between `@` and cursor — that means
        // the user moved on to a new word.
        if after_at.contains(' ') {
            return None;
        }

        Some((row, at_pos, after_at.to_string()))
    }

    /// Update the completion state based on the current cursor position.
    /// Called after every relevant key event.
    fn update_completion(&mut self) {
        let Some((row, col, prefix)) = self.find_at_token() else {
            self.completion = None;
            return;
        };

        // Don't trigger on a bare `@` with nothing after it — wait for
        // at least one character so we know the user intends a path.
        if prefix.is_empty() {
            // Keep any existing completion open (user just typed `@` and
            // will type more), but don't open a new one.
            if self.completion.is_none() {
                return;
            }
        }

        let candidates = Self::list_path_candidates(&prefix);

        if candidates.is_empty() {
            self.completion = None;
            return;
        }

        // Preserve selection index if the prefix just grew/shrank.
        let prev_selected = self.completion.as_ref().map(|c| c.selected).unwrap_or(0);

        self.completion = Some(PathCompletion {
            start_row: row,
            start_col: col,
            selected: prev_selected.min(candidates.len().saturating_sub(1)),
            candidates,
        });
    }

    /// List filesystem entries matching `partial` as a path prefix.
    /// - `~` or `~/...` expands to the user's home directory.
    /// - If `partial` starts with `/`, resolve from filesystem root.
    /// - Otherwise, resolve relative to CWD.
    /// - Split into (directory, partial_name) — list directory, filter by prefix.
    fn list_path_candidates(partial: &str) -> Vec<String> {
        if partial.is_empty() {
            // List CWD entries.
            return Self::read_dir_filtered(&PathBuf::from("."), "");
        }

        // Expand `~` to home directory.
        let expanded: String;
        let partial = if partial == "~" || partial.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                expanded = format!("{}{}", home.display(), &partial[1..]);
                expanded.as_str()
            } else {
                partial
            }
        } else {
            partial
        };

        let path = PathBuf::from(partial);

        // If the partial ends with `/`, list that directory's contents.
        if partial.ends_with('/') {
            return Self::read_dir_filtered(&path, "");
        }

        // Split into parent dir and the partial filename.
        // `PathBuf::from("RE").parent()` returns `Some("")`, not `None`,
        // so we also need to handle the empty-string case.
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let partial_name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

        Self::read_dir_filtered(&PathBuf::from(parent), partial_name)
    }

    /// Read entries from `dir`, filter by `prefix`, return display names.
    /// Directories get a trailing `/`. Results sorted alphabetically,
    /// dotfiles included but sorted after normal entries.
    fn read_dir_filtered(dir: &PathBuf, prefix: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };

        let prefix_lower = prefix.to_lowercase();
        let mut results: Vec<(bool, String)> = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };

            if !prefix.is_empty() && !name_str.to_lowercase().starts_with(&prefix_lower) {
                continue;
            }

            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let display = if is_dir {
                format!("{}/", name_str)
            } else {
                name_str.to_string()
            };
            let is_dot = name_str.starts_with('.');
            results.push((is_dot, display));
        }

        // Sort: non-dot first, then alphabetical.
        results.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        results
            .into_iter()
            .map(|(_, name)| name)
            .take(MAX_COMPLETIONS)
            .collect()
    }

    /// Whether the completion popup is currently visible.
    fn completion_visible(&self) -> bool {
        self.completion
            .as_ref()
            .map_or(false, |c| !c.candidates.is_empty())
    }

    /// Accept the currently selected completion candidate.
    /// Replaces the partial text after `@` with the selected entry.
    fn accept_completion(&mut self) {
        let Some(comp) = self.completion.take() else {
            return;
        };
        let Some(candidate) = comp.candidates.get(comp.selected) else {
            return;
        };
        let candidate = candidate.clone();

        // We need to replace text from (start_row, start_col+1) to current cursor
        // with the candidate. The `@` at start_col stays.
        let (cur_row, cur_col) = self.textarea.cursor();

        // Only handle single-line @tokens (which they always are).
        if cur_row != comp.start_row {
            return;
        }

        let line = self.textarea.lines()[cur_row].clone();
        let before_at = &line[..comp.start_col + 1]; // includes the `@`
        let after_cursor = &line[cur_col..];

        let new_line = format!("{}{}{}", before_at, candidate, after_cursor);
        let new_cursor_col = comp.start_col + 1 + candidate.len();

        // Replace the line: rebuild full text with this line swapped.
        let mut all_lines: Vec<String> = self.textarea.lines().to_vec();
        all_lines[cur_row] = new_line;
        let full_text = all_lines.join("\n");
        self.set_text(&full_text);

        // Position cursor after the inserted text.
        // set_text puts cursor at the end; we need to move it to the right spot.
        self.textarea.move_cursor(tui_textarea::CursorMove::Top);
        for _ in 0..cur_row {
            self.textarea.move_cursor(tui_textarea::CursorMove::Down);
        }
        self.textarea.move_cursor(tui_textarea::CursorMove::Head);
        for _ in 0..new_cursor_col {
            self.textarea.move_cursor(tui_textarea::CursorMove::Forward);
        }

        // If the candidate ends with `/`, immediately re-trigger completion
        // so the user can keep drilling into subdirectories.
        if candidate.ends_with('/') {
            self.update_completion();
        }
    }

    /// Dismiss the completion popup without inserting anything.
    fn dismiss_completion(&mut self) {
        self.completion = None;
    }

    /// Build content parts from the text and any pending attachments, then clear state.
    /// Returns `None` if the message is effectively empty.
    pub fn take_message(&mut self) -> Option<(String, Vec<PendingAttachment>)> {
        let raw_text = self.take_text();
        let trimmed = raw_text.trim();
        if trimmed.is_empty() && self.pending_attachments.is_empty() {
            return None;
        }

        let (cleaned_text, file_attachments) = Self::extract_file_attachments(trimmed);

        // Combine clipboard-pasted attachments with file-reference attachments.
        let mut all_attachments = std::mem::take(&mut self.pending_attachments);
        all_attachments.extend(file_attachments);

        // Strip display-only markers that should not be sent to the model.
        let msg_text = Self::strip_attachment_markers(&cleaned_text);
        self.push_history(&msg_text);

        // Clear thumbnail cache — attachments are being consumed.
        self.thumbnail_cache.clear();

        Some((msg_text, all_attachments))
    }
}

impl Component for Input<'_> {
    fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        if !self.focused {
            return Action::Noop;
        }

        // ── Completion popup intercepts ──────────────────────────────
        // When the popup is visible, certain keys navigate/accept/dismiss
        // it instead of their normal behavior.
        if self.completion_visible() {
            match (key.code, key.modifiers) {
                // Tab: accept the selected completion.
                (KeyCode::Tab, _) => {
                    self.accept_completion();
                    return Action::Noop;
                }
                // Enter (no modifiers): accept the selected completion.
                (KeyCode::Enter, m) if m.is_empty() => {
                    self.accept_completion();
                    return Action::Noop;
                }
                // Up: move selection up in the popup.
                (KeyCode::Up, m) if m.is_empty() => {
                    if let Some(comp) = self.completion.as_mut() {
                        comp.selected = comp.selected.saturating_sub(1);
                    }
                    return Action::Noop;
                }
                // Down: move selection down in the popup.
                (KeyCode::Down, m) if m.is_empty() => {
                    if let Some(comp) = self.completion.as_mut() {
                        comp.selected =
                            (comp.selected + 1).min(comp.candidates.len().saturating_sub(1));
                    }
                    return Action::Noop;
                }
                // Esc: dismiss the popup without inserting.
                (KeyCode::Esc, _) => {
                    self.dismiss_completion();
                    return Action::Noop;
                }
                // Any other key falls through to normal handling below,
                // and update_completion() is called afterward.
                _ => {}
            }
        }

        // ── Normal key handling ──────────────────────────────────────
        let action = match (key.code, key.modifiers) {
            // CUA: Ctrl+Q quits
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,

            // Ctrl+V: paste image from clipboard (falls through to text paste if no image)
            (KeyCode::Char('v'), m) if m.contains(KeyModifiers::CONTROL) => {
                if !self.try_paste_clipboard_image() {
                    // No image in clipboard — delegate to textarea for normal paste.
                    self.textarea.input(key);
                }
                Action::Noop
            }

            // Backspace: atomic marker deletion, then fallback to normal.
            (KeyCode::Backspace, _) => {
                if !self.try_delete_marker_before_cursor() {
                    self.textarea.input(key);
                    self.reconcile_attachments();
                }
                Action::Noop
            }

            // Delete: forward-delete might hit an ORC too.
            (KeyCode::Delete, _) => {
                self.textarea.input(key);
                self.reconcile_attachments();
                Action::Noop
            }

            // Enter submits the message (if non-empty)
            (KeyCode::Enter, m) if m.is_empty() => match self.take_message() {
                Some((text, attachments)) => Action::SubmitMessage { text, attachments },
                None => Action::Noop,
            },

            // Alt+Enter or Shift+Enter inserts a newline
            (KeyCode::Enter, m)
                if m.contains(KeyModifiers::ALT) || m.contains(KeyModifiers::SHIFT) =>
            {
                self.textarea.insert_newline();
                Action::Noop
            }

            // Up arrow: navigate to previous history entry (when on first line)
            (KeyCode::Up, m) if m.is_empty() && self.cursor_on_first_line() => {
                if !self.history_prev() {
                    // No history to navigate — let textarea handle it
                    self.textarea.input(key);
                }
                Action::Noop
            }

            // Down arrow: navigate to next history entry (when on last line)
            (KeyCode::Down, m) if m.is_empty() && self.cursor_on_last_line() => {
                if !self.history_next() {
                    // No history navigation — let textarea handle it
                    self.textarea.input(key);
                }
                Action::Noop
            }

            // Esc: clear input if non-empty
            (KeyCode::Esc, _) => {
                let is_empty = self.textarea.lines().iter().all(|l| l.is_empty());
                if !is_empty {
                    self.textarea.select_all();
                    self.textarea.cut();
                    self.reconcile_attachments();
                }
                // Reset history browsing
                self.history_index = None;
                self.draft.clear();
                Action::Noop
            }

            // All other keys: delegate to tui-textarea
            _ => {
                self.textarea.input(key);
                Action::Noop
            }
        };

        // After any text-modifying key, re-evaluate completion state.
        self.update_completion();

        action
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        if self.pending_attachments.is_empty() {
            frame.render_widget(&self.textarea, area);
            return;
        }

        // When attachments are present, we draw our own outer border and
        // render the thumbnail + borderless textarea inside it.
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Message ");
        let inner = outer_block.inner(area);
        frame.render_widget(outer_block, area);

        // Split inner area: thumbnail bar on top, text input below.
        let chunks = Layout::vertical([
            Constraint::Length(ATTACHMENT_BAR_HEIGHT),
            Constraint::Fill(1),
        ])
        .split(inner);

        let att_area = chunks[0];
        let count = self.pending_attachments.len();

        if self.picker.is_some() {
            // Render thumbnails side by side.
            let thumb_width = att_area.width / count.max(1) as u16;
            for (i, att) in self.pending_attachments.iter().enumerate() {
                let x = att_area.x + (i as u16) * thumb_width;
                let w = if i == count - 1 {
                    att_area.width - (i as u16) * thumb_width
                } else {
                    thumb_width
                };
                let thumb_rect = Rect::new(x, att_area.y, w, att_area.height);

                if let Some(proto) = self.thumbnail_cache.get(&att.id) {
                    let img_widget = ratatui_image::Image::new(proto);
                    frame.render_widget(img_widget, thumb_rect);
                } else {
                    Self::render_text_attachment_label(frame, thumb_rect, att);
                }
            }
        } else {
            // No graphics protocol — text-only indicator.
            let labels: Vec<Span> = self
                .pending_attachments
                .iter()
                .enumerate()
                .flat_map(|(i, att)| {
                    let mut spans = vec![
                        Span::styled("📎 ", Style::default().fg(Color::Yellow)),
                        Span::styled(
                            format!("{}({}KB)", att.mime_type, att.data.len() / 1024),
                            Style::default().fg(Color::Gray),
                        ),
                    ];
                    if i < count - 1 {
                        spans.push(Span::raw("  "));
                    }
                    spans
                })
                .collect();
            let bar = Paragraph::new(Line::from(labels));
            frame.render_widget(bar, att_area);
        }

        // Render textarea without its own border (outer border already drawn).
        self.textarea.set_block(Block::default());
        frame.render_widget(&self.textarea, chunks[1]);

        // Restore the textarea's normal block for when attachments are cleared.
        self.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Message "),
        );
    }
}

impl Input<'_> {
    /// Render the @-path completion popup above the input area.
    /// `input_area` is the Rect where the input was rendered;
    /// the popup floats above it.
    pub fn render_completion_popup(&self, frame: &mut Frame, input_area: Rect) {
        let Some(comp) = &self.completion else {
            return;
        };
        if comp.candidates.is_empty() {
            return;
        }

        let count = comp.candidates.len() as u16;
        // Popup height: 1 row per candidate + 2 for border.
        let popup_height = count + 2;
        // Popup width: widest candidate + padding + border.
        let max_name_len = comp.candidates.iter().map(|c| c.len()).max().unwrap_or(10) as u16;
        let popup_width = (max_name_len + 4).min(input_area.width);

        // Position: above the input area, left-aligned to the input.
        let popup_y = input_area.y.saturating_sub(popup_height);
        let popup_x = input_area.x;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear background and draw border.
        frame.render_widget(Clear, popup_area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Path ");

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        // Build list items.
        let items: Vec<ListItem> = comp
            .candidates
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let style = if i == comp.selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else if name.ends_with('/') {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Span::styled(name.as_str(), style))
            })
            .collect();

        let list = List::new(items);
        frame.render_widget(list, inner);
    }

    fn render_text_attachment_label(frame: &mut Frame, area: Rect, att: &PendingAttachment) {
        let label = Paragraph::new(Line::from(vec![
            Span::styled("📎 ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("image ({}KB)", att.data.len() / 1024),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
        frame.render_widget(label, area);
    }
}
