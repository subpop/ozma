use std::borrow::Cow;

use pulldown_cmark::{CodeBlockKind, Event, Options as CmarkOptions, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Style},
    text::{Line, Span, Text},
};
use tui_markdown::Options;

use crate::ui::theme::OzStyleSheet;

/// A code block extracted from markdown, with its raw content and position
/// in the rendered `Text` output.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CodeBlock {
    /// Language tag from the fence (e.g. "rust", "python"), or detected via heuristics.
    pub language: Option<String>,
    /// Whether the language was detected by heuristics (true) vs explicit in the fence (false).
    pub detected: bool,
    /// Raw source code content (no fences, no language tag).
    pub content: String,
    /// Line index in the rendered `Text` where this code block starts
    /// (the ` ```lang ` delimiter line).
    pub start_line: usize,
    /// Line index in the rendered `Text` after this code block ends
    /// (exclusive — one past the ` ``` ` closing delimiter line).
    pub end_line: usize,
}

/// Result of rendering markdown: the styled `Text` plus extracted code block metadata.
pub struct RenderedMarkdown {
    pub text: Text<'static>,
    pub code_blocks: Vec<CodeBlock>,
}

/// Render a markdown string to a ratatui `Text` widget using the ozma style sheet.
///
/// Uses `tui-markdown` with syntax highlighting for code blocks (via syntect).
/// Pre-processes the markdown to convert loose lists (which have blank lines
/// between items) to tight lists, working around a tui-markdown rendering bug
/// where loose list items split the bullet/number marker onto a separate line
/// from the item text.
///
/// Also pre-annotates unfenced code blocks with detected languages, and
/// extracts code block metadata for line numbers, navigation, and copy.
///
/// Returns `RenderedMarkdown` with owned `Text<'static>` and code block info.
pub fn render(content: &str) -> RenderedMarkdown {
    let preprocessed = tighten_lists(content);
    let annotated = annotate_unfenced_blocks(&preprocessed);
    let source = if annotated != preprocessed {
        &annotated
    } else {
        &preprocessed
    };

    // Extract code block metadata before rendering.
    let mut code_blocks = extract_code_blocks(source);

    // Render via tui-markdown.
    let options = Options::new(OzStyleSheet);
    let rendered = tui_markdown::from_str_with_options(source, &options);
    let mut text = into_owned(rendered);

    // Calibrate code block positions against actual rendered output.
    let total_lines = text.lines.len();
    for cb in &mut code_blocks {
        if cb.end_line > total_lines {
            cb.end_line = total_lines;
        }
    }

    // Strip fence delimiter lines (```lang and ```) from the rendered output,
    // then add line numbers. Order matters: fences must be gone before numbering.
    strip_fences(&mut text, &mut code_blocks);
    add_line_numbers(&mut text, &code_blocks);

    RenderedMarkdown { text, code_blocks }
}

/// Convert a `Text<'_>` with borrowed spans into a `Text<'static>` with owned strings.
fn into_owned(text: Text<'_>) -> Text<'static> {
    let lines: Vec<Line<'static>> = text
        .lines
        .into_iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| Span::styled(span.content.into_owned(), span.style))
                .collect();
            Line::from(spans).style(line.style)
        })
        .collect();
    Text::from(lines)
}

/// Style used for line number gutter in code blocks.
const LINE_NUMBER_STYLE: Style = Style::new().fg(Color::DarkGray);

/// Remove the fence delimiter lines (` ```lang ` and ` ``` `) from rendered code blocks,
/// and adjust `CodeBlock` indices to reflect the new line positions.
///
/// After this, code blocks in `text` contain only the actual code lines (no fences).
fn strip_fences(text: &mut Text<'static>, code_blocks: &mut [CodeBlock]) {
    // Collect fence line indices to remove (in reverse order for safe removal).
    let mut to_remove: Vec<usize> = Vec::new();
    for cb in code_blocks.iter() {
        let closing = cb.end_line.saturating_sub(1);
        if closing < text.lines.len() {
            to_remove.push(closing);
        }
        if cb.start_line < text.lines.len() {
            to_remove.push(cb.start_line);
        }
    }
    // Sort descending so we remove from the end first (indices stay valid).
    to_remove.sort_unstable();
    to_remove.dedup();
    to_remove.reverse();

    for idx in &to_remove {
        text.lines.remove(*idx);
    }

    // Rebuild code block positions: each block loses its 2 fence lines,
    // and all positions shift by the number of fence lines removed before them.
    let mut removed_sorted = to_remove.clone();
    removed_sorted.sort_unstable(); // ascending for counting

    for cb in code_blocks.iter_mut() {
        // Count how many removed lines are strictly before the old start_line.
        // The opening fence *is* at start_line; removing it means the first code
        // line slides into that position, so we only subtract lines before it.
        let removed_before_start = removed_sorted
            .iter()
            .filter(|&&r| r < cb.start_line)
            .count();
        // Count how many removed lines are before the old end_line.
        let removed_before_end = removed_sorted.iter().filter(|&&r| r < cb.end_line).count();

        cb.start_line = cb.start_line.saturating_sub(removed_before_start);
        cb.end_line = cb.end_line.saturating_sub(removed_before_end);
    }
}

/// Add line numbers to code blocks in the rendered `Text`.
///
/// For each code block, prepends a line number gutter (`" 1 │ "`) to each code line.
/// Must be called *after* `strip_fences()` — all lines in the code block range are
/// code lines (no fences to skip).
fn add_line_numbers(text: &mut Text<'static>, code_blocks: &[CodeBlock]) {
    for cb in code_blocks {
        let code_line_count = cb.end_line.saturating_sub(cb.start_line);
        if code_line_count == 0 {
            continue;
        }

        // Determine gutter width based on max line number.
        let width = digit_count(code_line_count);

        // Prepend numbered gutter to each code line.
        for (num, i) in (cb.start_line..cb.end_line.min(text.lines.len())).enumerate() {
            let gutter = format!("{:>width$} │ ", num + 1, width = width);
            let gutter_span = Span::styled(gutter, LINE_NUMBER_STYLE);
            text.lines[i].spans.insert(0, gutter_span);
        }
    }
}

/// Count the number of decimal digits in a number.
fn digit_count(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    ((n as f64).log10().floor() as usize) + 1
}

/// Extract code block metadata by parsing markdown with pulldown-cmark.
///
/// Walks the event stream and tracks code block boundaries, counting rendered
/// lines to match the output of `tui-markdown`. The line counting mirrors
/// tui-markdown's behavior:
/// - A blank separator line before each code block (if there's preceding content)
/// - A ` ```lang ` opening fence line
/// - One line per source code line
/// - A ` ``` ` closing fence line
fn extract_code_blocks(source: &str) -> Vec<CodeBlock> {
    let parser = Parser::new_ext(source, CmarkOptions::all());

    let mut blocks = Vec::new();
    let mut rendered_line: usize = 0;
    let mut has_preceding_content = false;

    // Track whether we're inside a code block.
    let mut in_code_block = false;
    let mut cb_language: Option<String> = None;
    let mut cb_detected = false;
    let mut cb_content = String::new();
    let mut cb_start_line: usize = 0;

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                cb_content.clear();

                // tui-markdown inserts a blank line before code blocks if there's prior content.
                if has_preceding_content {
                    rendered_line += 1; // blank separator
                }

                cb_start_line = rendered_line;

                // The opening ``` fence line.
                rendered_line += 1;

                match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let lang_str = lang.as_ref().trim();
                        if lang_str.is_empty() {
                            cb_language = None;
                            cb_detected = false;
                        } else {
                            cb_language = Some(lang_str.to_string());
                            cb_detected = false;
                        }
                    }
                    CodeBlockKind::Indented => {
                        cb_language = None;
                        cb_detected = false;
                    }
                }
            }
            Event::Text(text) if in_code_block => {
                cb_content.push_str(&text);
                // Count rendered lines for this text chunk.
                // tui-markdown renders one line per source line within code blocks.
                let line_count = text.lines().count();
                // If the text ends with a newline, lines() won't count the trailing empty,
                // but tui-markdown still renders each actual line.
                rendered_line += line_count;
            }
            Event::End(TagEnd::CodeBlock) => {
                // The closing ``` fence line.
                rendered_line += 1;

                // Remove trailing newline from content if present.
                if cb_content.ends_with('\n') {
                    cb_content.pop();
                }

                blocks.push(CodeBlock {
                    language: cb_language.take(),
                    detected: cb_detected,
                    content: std::mem::take(&mut cb_content),
                    start_line: cb_start_line,
                    end_line: rendered_line,
                });

                in_code_block = false;
                has_preceding_content = true;
            }

            // Non-code-block events: count rendered lines to keep our position in sync.
            Event::Start(Tag::Paragraph) => {
                if has_preceding_content {
                    rendered_line += 1; // blank separator before paragraph
                }
            }
            Event::End(TagEnd::Paragraph) => {
                has_preceding_content = true;
            }
            Event::Start(Tag::Heading { .. }) => {
                if has_preceding_content {
                    rendered_line += 1; // blank separator before heading
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                has_preceding_content = true;
            }
            Event::Text(_) if !in_code_block => {
                // Text within paragraphs/headings — each line gets a rendered line.
                // We don't need exact accuracy here since we only care about code block
                // positions. But let's approximate: count newlines.
                rendered_line += 1;
            }
            Event::SoftBreak if !in_code_block => {
                // tui-markdown renders soft breaks as spaces within a line, not new lines.
            }
            Event::HardBreak if !in_code_block => {
                rendered_line += 1;
            }
            Event::Start(Tag::Item) => {
                // List items render on their own line.
            }
            Event::End(TagEnd::Item) => {
                has_preceding_content = true;
            }
            Event::Start(Tag::List(_)) => {
                if has_preceding_content {
                    rendered_line += 1; // separator before list
                }
            }
            Event::End(TagEnd::List(_)) => {
                has_preceding_content = true;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if has_preceding_content {
                    rendered_line += 1;
                }
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                has_preceding_content = true;
            }
            Event::Rule => {
                rendered_line += 1;
                has_preceding_content = true;
            }
            _ => {}
        }
    }

    blocks
}

/// Detect the programming language of a code snippet using simple heuristics.
/// Returns a syntect-compatible language token.
fn detect_language(code: &str) -> Option<&'static str> {
    let trimmed = code.trim();

    // Shebang detection.
    if let Some(first_line) = trimmed.lines().next() {
        if first_line.starts_with("#!") {
            if first_line.contains("python") {
                return Some("python");
            }
            if first_line.contains("node") || first_line.contains("deno") {
                return Some("javascript");
            }
            if first_line.contains("bash") || first_line.contains("/sh") {
                return Some("bash");
            }
            if first_line.contains("ruby") {
                return Some("ruby");
            }
            if first_line.contains("perl") {
                return Some("perl");
            }
        }
    }

    // Rust patterns.
    if trimmed.contains("fn main()")
        || (trimmed.contains("fn ") && trimmed.contains("->"))
        || trimmed.contains("let mut ")
        || trimmed.contains("impl ")
        || trimmed.contains("use std::")
        || trimmed.contains("pub fn ")
    {
        return Some("rust");
    }

    // Python patterns.
    if trimmed.contains("def ") && trimmed.contains(':')
        || trimmed.contains("import ") && !trimmed.contains('{')
        || trimmed.starts_with("class ") && trimmed.contains(':')
        || trimmed.contains("print(")
    {
        return Some("python");
    }

    // Go patterns.
    if trimmed.contains("func ") && trimmed.contains('{')
        || trimmed.contains("package main")
        || trimmed.contains("fmt.Println")
    {
        return Some("go");
    }

    // JavaScript/TypeScript patterns.
    if trimmed.contains("const ") && trimmed.contains(" = ")
        || trimmed.contains("function ") && trimmed.contains('{')
        || trimmed.contains("console.log")
        || trimmed.contains("=>")
    {
        // Distinguish TS from JS.
        if trimmed.contains(": string")
            || trimmed.contains(": number")
            || trimmed.contains("interface ")
        {
            return Some("typescript");
        }
        return Some("javascript");
    }

    // SQL patterns.
    let upper = trimmed.to_uppercase();
    if (upper.contains("SELECT ") && upper.contains("FROM "))
        || (upper.contains("INSERT INTO ") || upper.contains("CREATE TABLE "))
    {
        return Some("sql");
    }

    // Shell patterns.
    if trimmed.contains("echo ")
        || trimmed.contains("if [")
        || (trimmed.contains("export ") && trimmed.contains('='))
        || trimmed.starts_with("$ ")
    {
        return Some("bash");
    }

    // HTML/XML patterns.
    if trimmed.starts_with('<') && trimmed.contains('>') && trimmed.contains("</") {
        return Some("html");
    }

    // JSON detection (starts with { or [, looks like JSON).
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if trimmed.contains('"') && trimmed.contains(':') {
            return Some("json");
        }
    }

    // YAML detection.
    if trimmed.contains(": ") && !trimmed.contains('{') && trimmed.lines().count() > 1 {
        let yaml_lines = trimmed
            .lines()
            .filter(|l| {
                let t = l.trim();
                t.contains(": ") || t.starts_with("- ") || t.starts_with('#')
            })
            .count();
        if yaml_lines > trimmed.lines().count() / 2 {
            return Some("yaml");
        }
    }

    // TOML detection.
    if trimmed.contains('[') && trimmed.contains(']') && trimmed.contains(" = ") {
        let toml_lines = trimmed
            .lines()
            .filter(|l| {
                let t = l.trim();
                (t.starts_with('[') && t.ends_with(']')) || t.contains(" = ") || t.starts_with('#')
            })
            .count();
        if toml_lines > trimmed.lines().count() / 2 {
            return Some("toml");
        }
    }

    None
}

/// Pre-annotate unfenced code blocks (` ``` ` with no language) by detecting
/// the language from the code content and injecting it into the fence.
fn annotate_unfenced_blocks(content: &str) -> String {
    // Find code blocks with empty fences and collect their byte ranges.
    // We'll work on the source string directly.
    let mut replacements: Vec<(usize, &'static str)> = Vec::new();
    let mut code_content = String::new();
    #[allow(unused_assignments)]
    let mut fence_offset: usize = 0;
    // Track whether we're inside a fenced code block (one with a language tag).
    // We must skip all lines inside fenced blocks so we don't misinterpret
    // their closing ``` as an opening unfenced fence.
    let mut in_fenced_block = false;

    let mut i = 0;
    let bytes = content.as_bytes();
    let len = bytes.len();

    while i < len {
        let at_line_start = i == 0 || bytes[i - 1] == b'\n';
        if at_line_start && i + 3 <= len && &bytes[i..i + 3] == b"```" {
            let after_fence = i + 3;
            let is_bare =
                after_fence >= len || bytes[after_fence] == b'\n' || bytes[after_fence] == b'\r';

            if in_fenced_block {
                // We're inside a fenced code block (e.g. ```python ... ```).
                // A bare ``` closes it; anything else is content — skip.
                if is_bare {
                    in_fenced_block = false;
                }
                // Either way, skip this line.
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < len {
                    i += 1;
                }
                continue;
            }

            if is_bare {
                // Bare ``` outside any code block → opening unfenced fence.
                fence_offset = i;
                code_content.clear();
                // Skip past the newline.
                i = after_fence;
                if i < len && bytes[i] == b'\r' {
                    i += 1;
                }
                if i < len && bytes[i] == b'\n' {
                    i += 1;
                }
                // Collect content until closing ```.
                while i < len {
                    let line_start = i;
                    while i < len && bytes[i] != b'\n' {
                        i += 1;
                    }
                    let line = &content[line_start..i];
                    if line.trim_end() == "```" {
                        break;
                    }
                    code_content.push_str(line);
                    code_content.push('\n');
                    if i < len {
                        i += 1; // skip \n
                    }
                }
                // Try to detect language.
                if let Some(lang) = detect_language(&code_content) {
                    replacements.push((fence_offset, lang));
                }
                // Skip past the closing fence line.
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < len {
                    i += 1;
                }
                continue;
            }

            // ``` followed by non-whitespace → fenced opening (e.g. ```python).
            // Enter fenced mode so we skip until the matching closing ```.
            in_fenced_block = true;
        }

        // Advance to next line.
        while i < len && bytes[i] != b'\n' {
            i += 1;
        }
        if i < len {
            i += 1;
        }
    }

    if replacements.is_empty() {
        return content.to_string();
    }

    // Apply replacements in reverse order so offsets remain valid.
    let mut result = content.to_string();
    for (offset, lang) in replacements.into_iter().rev() {
        let insert_pos = offset + 3;
        result.insert_str(insert_pos, lang);
    }
    result
}

/// Convert loose lists to tight lists by removing blank lines between list items.
///
/// Loose lists (with blank lines between items) cause pulldown-cmark to wrap
/// item content in `<p>` tags, which tui-markdown renders as a separate line
/// after the bullet marker. Tight lists avoid this issue.
///
/// This handles both ordered (`1. `) and unordered (`- `, `* `, `+ `) lists,
/// including nested lists with indentation.
fn tighten_lists(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        result.push(Cow::Borrowed(lines[i]));

        // Check if this line is a list item
        if is_list_item(lines[i]) {
            // Skip blank lines between this item and the next list item
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j].trim().is_empty() {
                    // Blank line — check if followed by another list item or more blank lines
                    let mut k = j + 1;
                    while k < lines.len() && lines[k].trim().is_empty() {
                        k += 1;
                    }
                    if k < lines.len() && is_list_item(lines[k]) {
                        // Skip all blank lines between list items
                        j = k;
                        continue;
                    } else {
                        // Blank line not followed by list item — keep it
                        break;
                    }
                } else {
                    break;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    // Rebuild with newlines, preserving trailing newline if original had one
    let mut out = result.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Check if a line starts a list item (ordered or unordered), possibly indented.
fn is_list_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    // Unordered: starts with "- ", "* ", or "+ "
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    // Ordered: starts with digits followed by ". " or ") "
    if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tight_list_unchanged() {
        let input = "1. Item one\n2. Item two\n3. Item three\n";
        assert_eq!(tighten_lists(input), input);
    }

    #[test]
    fn loose_ordered_list_tightened() {
        let input = "1. Item one\n\n2. Item two\n\n3. Item three\n";
        let expected = "1. Item one\n2. Item two\n3. Item three\n";
        assert_eq!(tighten_lists(input), expected);
    }

    #[test]
    fn loose_unordered_list_tightened() {
        let input = "- Item one\n\n- Item two\n\n- Item three\n";
        let expected = "- Item one\n- Item two\n- Item three\n";
        assert_eq!(tighten_lists(input), expected);
    }

    #[test]
    fn paragraph_after_list_preserved() {
        let input = "1. Item one\n2. Item two\n\nSome paragraph after.\n";
        // The blank line before "Some paragraph" should be preserved
        // because "Some paragraph" is NOT a list item.
        assert_eq!(tighten_lists(input), input);
    }

    #[test]
    fn mixed_content() {
        let input = "# Title\n\n1. First\n\n2. Second\n\nEnd paragraph.\n";
        let expected = "# Title\n\n1. First\n2. Second\n\nEnd paragraph.\n";
        assert_eq!(tighten_lists(input), expected);
    }

    #[test]
    fn nested_list() {
        let input = "- Outer\n\n  - Inner one\n\n  - Inner two\n\n- Outer again\n";
        let expected = "- Outer\n  - Inner one\n  - Inner two\n- Outer again\n";
        assert_eq!(tighten_lists(input), expected);
    }

    #[test]
    fn star_and_plus_lists() {
        let input = "* Item one\n\n* Item two\n";
        let expected = "* Item one\n* Item two\n";
        assert_eq!(tighten_lists(input), expected);

        let input2 = "+ Item one\n\n+ Item two\n";
        let expected2 = "+ Item one\n+ Item two\n";
        assert_eq!(tighten_lists(input2), expected2);
    }

    // -- Code block extraction tests --

    #[test]
    fn extract_no_code_blocks() {
        let input = "Hello world\n\nSome paragraph.";
        let blocks = extract_code_blocks(input);
        assert!(blocks.is_empty());
    }

    #[test]
    fn extract_single_code_block() {
        let input = "```rust\nfn main() {}\n```";
        let blocks = extract_code_blocks(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].language.as_deref(), Some("rust"));
        assert_eq!(blocks[0].content, "fn main() {}");
        assert!(!blocks[0].detected);
    }

    #[test]
    fn extract_multiple_code_blocks() {
        let input =
            "# Title\n\n```python\nprint('hello')\n```\n\nSome text.\n\n```bash\necho hi\n```";
        let blocks = extract_code_blocks(input);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].language.as_deref(), Some("python"));
        assert_eq!(blocks[0].content, "print('hello')");
        assert_eq!(blocks[1].language.as_deref(), Some("bash"));
        assert_eq!(blocks[1].content, "echo hi");
        // Second block should start after the first.
        assert!(blocks[1].start_line > blocks[0].end_line);
    }

    #[test]
    fn extract_unfenced_code_block() {
        let input = "```\nfn main() {}\n```";
        let blocks = extract_code_blocks(input);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].language.is_none());
        assert_eq!(blocks[0].content, "fn main() {}");
    }

    #[test]
    fn extract_multiline_code_block() {
        let input = "```rust\nline 1\nline 2\nline 3\n```";
        let blocks = extract_code_blocks(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "line 1\nline 2\nline 3");
        // Should span: ```rust (1 line) + 3 code lines + ``` (1 line) = 5 lines
        assert_eq!(blocks[0].end_line - blocks[0].start_line, 5);
    }

    // -- Language detection tests --

    #[test]
    fn detect_rust() {
        assert_eq!(
            detect_language("fn main() {\n    println!(\"hello\");\n}"),
            Some("rust")
        );
        assert_eq!(detect_language("pub fn foo() -> u32 { 42 }"), Some("rust"));
    }

    #[test]
    fn detect_python() {
        assert_eq!(
            detect_language("def hello():\n    print('hi')"),
            Some("python")
        );
        assert_eq!(
            detect_language("#!/usr/bin/env python3\nprint('hi')"),
            Some("python")
        );
    }

    #[test]
    fn detect_bash() {
        assert_eq!(detect_language("#!/bin/bash\necho hello"), Some("bash"));
        assert_eq!(detect_language("echo hello world"), Some("bash"));
    }

    #[test]
    fn detect_javascript() {
        assert_eq!(detect_language("const x = () => 42;"), Some("javascript"));
        assert_eq!(
            detect_language("function foo() {\n  return 1;\n}"),
            Some("javascript")
        );
    }

    #[test]
    fn detect_sql() {
        assert_eq!(
            detect_language("SELECT * FROM users WHERE id = 1;"),
            Some("sql")
        );
    }

    #[test]
    fn detect_none_for_ambiguous() {
        assert_eq!(detect_language("hello world"), None);
        assert_eq!(detect_language(""), None);
    }

    // -- Annotation tests --

    #[test]
    fn annotate_adds_language_to_unfenced() {
        let input = "```\nfn main() {\n    println!(\"hello\");\n}\n```";
        let result = annotate_unfenced_blocks(input);
        assert!(result.starts_with("```rust\n"));
    }

    #[test]
    fn annotate_leaves_fenced_alone() {
        let input = "```python\nprint('hi')\n```";
        let result = annotate_unfenced_blocks(input);
        assert_eq!(result, input);
    }

    #[test]
    fn annotate_preserves_multiple_fenced_blocks() {
        // Regression: closing ``` of a fenced block was misidentified as an
        // opening unfenced fence, causing the next block's content to be
        // swallowed into a phantom unfenced block.
        let input = "\
**Python**
```python
print('hello')
```

**TypeScript**
```typescript
const x = 1;
```

**Rust**
```rust
fn main() {}
```";
        let result = annotate_unfenced_blocks(input);
        // All blocks are already fenced — output must be identical to input.
        assert_eq!(result, input);
    }

    #[test]
    fn annotate_unfenced_between_fenced_blocks() {
        // An unfenced block sandwiched between fenced blocks should get annotated
        // without disturbing the fenced blocks.
        let input = "\
```python
print('hello')
```

```
fn main() {}
```

```bash
echo hi
```";
        let result = annotate_unfenced_blocks(input);
        // The unfenced block should be annotated as rust.
        assert!(result.contains("```rust\nfn main() {}"));
        // The python and bash blocks must be unchanged.
        assert!(result.contains("```python\nprint('hello')"));
        assert!(result.contains("```bash\necho hi"));
    }

    // -- Line number tests --

    #[test]
    fn digit_count_values() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(1), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(99), 2);
        assert_eq!(digit_count(100), 3);
    }

    // -- Strip fences tests --

    #[test]
    fn strip_fences_removes_delimiter_lines() {
        // Build a minimal Text with fence lines matching what tui-markdown emits:
        // line 0: "Hello"
        // line 1: "" (blank separator)
        // line 2: "```rust" (opening fence)
        // line 3: "let x = 1;" (code)
        // line 4: "```" (closing fence)
        // line 5: "" (blank separator)
        // line 6: "World"
        let mut text = Text::from(vec![
            Line::from("Hello"),
            Line::from(""),
            Line::from("```rust"),
            Line::from("let x = 1;"),
            Line::from("```"),
            Line::from(""),
            Line::from("World"),
        ]);
        let mut blocks = vec![CodeBlock {
            language: Some("rust".to_string()),
            detected: false,
            content: "let x = 1;".to_string(),
            start_line: 2,
            end_line: 5,
        }];

        strip_fences(&mut text, &mut blocks);

        // Fence lines removed — 5 lines remain.
        assert_eq!(text.lines.len(), 5);
        // Code block now spans just the code line.
        assert_eq!(blocks[0].start_line, 2);
        assert_eq!(blocks[0].end_line, 3);
        // No fence content in the text.
        let all_text: String = text
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all_text.contains("```"));
    }

    #[test]
    fn strip_fences_multiple_blocks() {
        // Two code blocks.
        let mut text = Text::from(vec![
            Line::from("```python"),  // 0
            Line::from("print('a')"), // 1
            Line::from("```"),        // 2
            Line::from(""),           // 3
            Line::from("```bash"),    // 4
            Line::from("echo hi"),    // 5
            Line::from("```"),        // 6
        ]);
        let mut blocks = vec![
            CodeBlock {
                language: Some("python".to_string()),
                detected: false,
                content: "print('a')".to_string(),
                start_line: 0,
                end_line: 3,
            },
            CodeBlock {
                language: Some("bash".to_string()),
                detected: false,
                content: "echo hi".to_string(),
                start_line: 4,
                end_line: 7,
            },
        ];

        strip_fences(&mut text, &mut blocks);

        // 4 fence lines removed, 3 remain.
        assert_eq!(text.lines.len(), 3);
        // First block: just code line.
        assert_eq!(blocks[0].start_line, 0);
        assert_eq!(blocks[0].end_line, 1);
        // Second block: shifted.
        assert_eq!(blocks[1].start_line, 2);
        assert_eq!(blocks[1].end_line, 3);
    }

    // -- Render integration test --

    #[test]
    fn render_returns_code_blocks() {
        let input = "Hello\n\n```rust\nlet x = 1;\n```\n\nWorld";
        let result = render(input);
        assert_eq!(result.code_blocks.len(), 1);
        assert_eq!(result.code_blocks[0].language.as_deref(), Some("rust"));
        assert_eq!(result.code_blocks[0].content, "let x = 1;");
        // The text should have some lines (fences stripped, but code + prose remain).
        assert!(!result.text.lines.is_empty());
        // Fence lines should not appear in the rendered output.
        let all_text: String = result
            .text
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !all_text.contains("```"),
            "Rendered output should not contain fence delimiters, got:\n{all_text}"
        );
    }
}
