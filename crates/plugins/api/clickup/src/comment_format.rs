//! Markdown → ClickUp comment rich-text conversion.
//!
//! ClickUp's Comments API does **not** render markdown. The `comment_text`
//! field is run through a lossy auto-formatter that turns every backtick span
//! into a fragmented inline-code chip and drops everything else; the
//! `markdown_content` field (which works for task descriptions) is silently
//! ignored on comments. See <https://developer.clickup.com/docs/comments>.
//!
//! The only way to get clean rendering is the structured `comment` array — a
//! [Quill Delta](https://quilljs.com/docs/delta/)-style list of `{text,
//! attributes}` runs. Inline marks (`code`, `bold`) attach to the content run;
//! block marks (`code-block`, `list`) attach to the trailing `"\n"` separator
//! that closes the line. See <https://developer.clickup.com/docs/comment-formatting>.
//!
//! This module converts the markdown subset our comments use — inline code,
//! bold, italic, links, fenced code blocks, bullet/ordered/task lists, ATX
//! headings, blockquotes, horizontal rules, GFM tables, and plain paragraphs —
//! into that array. ClickUp comments have no table block, so GFM tables render
//! as an aligned monospace `code-block`. Anything it doesn't recognise is
//! emitted as plain text, so output is never worse than the old behaviour.

use serde::Serialize;

/// One run in a ClickUp comment's `comment` array.
///
/// `attributes` is omitted from the wire payload when empty so the request
/// mirrors what the ClickUp UI itself sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommentBlock {
    pub text: String,
    #[serde(skip_serializing_if = "CommentAttributes::is_empty")]
    pub attributes: CommentAttributes,
}

/// Formatting marks for a single run. Inline marks (`code`, `bold`) sit on a
/// content run; block marks (`code_block`, `list`) sit on a `"\n"` separator.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CommentAttributes {
    #[serde(skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub code: bool,
    /// Hyperlink target URL for this run's text (inline `[text](url)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Block-level code fence. ClickUp's shape is `{"code-block": "plain"}`.
    #[serde(rename = "code-block", skip_serializing_if = "Option::is_none")]
    pub code_block: Option<CodeBlockAttr>,
    /// List membership. ClickUp's shape is
    /// `{"list": "bullet" | "ordered" | "checked" | "unchecked"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list: Option<ListAttr>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeBlockAttr {
    #[serde(rename = "code-block")]
    pub code_block: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListAttr {
    pub list: String,
}

impl CommentAttributes {
    fn is_empty(&self) -> bool {
        !self.bold
            && !self.italic
            && !self.code
            && self.link.is_none()
            && self.code_block.is_none()
            && self.list.is_none()
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // signature required by serde's skip_serializing_if
fn is_false(b: &bool) -> bool {
    !*b
}

/// A line separator carrying a block-level attribute (or none).
fn newline(attributes: CommentAttributes) -> CommentBlock {
    CommentBlock {
        text: "\n".to_string(),
        attributes,
    }
}

/// Convert a markdown comment body into ClickUp's `comment` rich-text array.
///
/// The supported subset:
/// - fenced code blocks (```` ``` ````) → each line + a `code-block` newline;
/// - `- ` / `* ` / `+ ` bullets → content + a `bullet` list newline;
/// - `- [ ]` / `- [x]` task items → content + an `unchecked`/`checked` newline;
/// - `1. ` ordered items → content + an `ordered` list newline;
/// - ATX headings (`#`..`######`) → bold content + a leading glyph (no heading mark);
/// - GFM tables → an aligned monospace `code-block` (comments have no table mark);
/// - `> ` blockquotes and `---` horizontal rules;
/// - inline `` `code` ``, `**bold**`, `*italic*`, and `[text](url)` links;
/// - everything else → plain text.
pub fn markdown_to_comment_blocks(body: &str) -> Vec<CommentBlock> {
    // An empty body has no structure to express; returning no blocks lets the
    // request fall back to plain `comment_text` (the `comment` array is skipped
    // when empty).
    if body.is_empty() {
        return Vec::new();
    }

    let mut blocks: Vec<CommentBlock> = Vec::new();
    let mut in_code_fence = false;

    // Indexed walk (not a plain `for`) so table detection can look ahead at the
    // separator row and consume the contiguous table block.
    let lines: Vec<&str> = body.split('\n').collect();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];

        // Fence toggles. A line whose trimmed start is ``` opens or closes a
        // fenced block; the fence line itself (and its info string) is dropped.
        if line.trim_start().starts_with("```") {
            in_code_fence = !in_code_fence;
            idx += 1;
            continue;
        }

        if in_code_fence {
            // Inside a fence everything is literal; no inline parsing.
            if !line.is_empty() {
                blocks.push(CommentBlock {
                    text: line.to_string(),
                    attributes: CommentAttributes::default(),
                });
            }
            blocks.push(newline(CommentAttributes {
                code_block: Some(CodeBlockAttr {
                    code_block: "plain".to_string(),
                }),
                ..Default::default()
            }));
            idx += 1;
            continue;
        }

        // GFM table: a `|...|` row immediately followed by a separator row.
        // Rendered as an aligned monospace code-block (no table mark exists).
        if is_table_row(line)
            && idx + 1 < lines.len()
            && is_separator_row(&split_table_row(lines[idx + 1]))
        {
            let mut rows = vec![line.to_string()];
            let mut j = idx + 1;
            while j < lines.len() && is_table_row(lines[j]) {
                rows.push(lines[j].to_string());
                j += 1;
            }
            for rendered in render_table(&rows) {
                blocks.push(CommentBlock {
                    text: rendered,
                    attributes: CommentAttributes::default(),
                });
                blocks.push(newline(CommentAttributes {
                    code_block: Some(CodeBlockAttr {
                        code_block: "plain".to_string(),
                    }),
                    ..Default::default()
                }));
            }
            idx = j;
            continue;
        }

        // Task list item: `- [ ]` / `- [x]` (checked/unchecked list).
        if let Some((checked, rest)) = strip_task_item(line) {
            push_inline_runs(&mut blocks, rest);
            blocks.push(newline(CommentAttributes {
                list: Some(ListAttr {
                    list: if checked { "checked" } else { "unchecked" }.to_string(),
                }),
                ..Default::default()
            }));
            idx += 1;
            continue;
        }

        // Bullet list item: `-`, `*`, or `+` followed by a space.
        if let Some(rest) = strip_bullet(line) {
            push_inline_runs(&mut blocks, rest);
            blocks.push(newline(CommentAttributes {
                list: Some(ListAttr {
                    list: "bullet".to_string(),
                }),
                ..Default::default()
            }));
            idx += 1;
            continue;
        }

        // Ordered list item: `<n>.` or `<n>)` followed by a space.
        if let Some(rest) = strip_ordered(line) {
            push_inline_runs(&mut blocks, rest);
            blocks.push(newline(CommentAttributes {
                list: Some(ListAttr {
                    list: "ordered".to_string(),
                }),
                ..Default::default()
            }));
            idx += 1;
            continue;
        }

        // ATX heading: 1–6 leading `#` then a space. Comments have no heading
        // attribute, so render the text bold with a leading glyph (◆ for h1,
        // ▸ for h2) to preserve section structure.
        if let Some((level, rest)) = strip_heading(line) {
            let prefix = heading_prefix(level);
            if !prefix.is_empty() {
                blocks.push(CommentBlock {
                    text: prefix.to_string(),
                    attributes: CommentAttributes {
                        bold: true,
                        ..Default::default()
                    },
                });
            }
            push_bold_run(&mut blocks, rest);
            blocks.push(newline(CommentAttributes::default()));
            idx += 1;
            continue;
        }

        // Horizontal rule: `---`, `***`, or `___` on its own line.
        if matches!(line.trim(), "---" | "***" | "___") {
            blocks.push(CommentBlock {
                text: "\u{2500}".repeat(10),
                attributes: CommentAttributes::default(),
            });
            blocks.push(newline(CommentAttributes::default()));
            idx += 1;
            continue;
        }

        // Blockquote: `> ...` rendered as an italic line with a `| ` gutter
        // (comments have no quote mark).
        if let Some(rest) = strip_blockquote(line) {
            blocks.push(CommentBlock {
                text: "| ".to_string(),
                attributes: CommentAttributes::default(),
            });
            for mut run in parse_inline(rest) {
                run.attributes.italic = true;
                blocks.push(run);
            }
            blocks.push(newline(CommentAttributes::default()));
            idx += 1;
            continue;
        }

        // Plain paragraph line (may contain inline code / bold / italic / link).
        push_inline_runs(&mut blocks, line);
        blocks.push(newline(CommentAttributes::default()));
        idx += 1;
    }

    // `split('\n')` yields a trailing empty segment for every trailing newline
    // in the body, each producing a redundant plain separator. Trim all
    // trailing *plain* newlines (never the last remaining block, and never a
    // separator carrying a block attribute like code-block/list — those are
    // structurally significant) so we don't emit dangling blank lines.
    while blocks.len() > 1 {
        let last = &blocks[blocks.len() - 1];
        if last.text == "\n" && last.attributes.is_empty() {
            blocks.pop();
        } else {
            break;
        }
    }

    blocks
}

/// Strip a `- ` / `* ` / `+ ` bullet marker (allowing leading indent),
/// returning the item text. `None` if the line isn't a bullet.
fn strip_bullet(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ['-', '*', '+'] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            if let Some(rest) = rest.strip_prefix(' ') {
                return Some(rest);
            }
        }
    }
    None
}

/// Strip a `<n>. ` / `<n>) ` ordered marker, returning the item text.
fn strip_ordered(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let digits_end = trimmed.find(|c: char| !c.is_ascii_digit())?;
    if digits_end == 0 {
        return None;
    }
    let after = &trimmed[digits_end..];
    for sep in ['.', ')'] {
        if let Some(rest) = after.strip_prefix(sep) {
            if let Some(rest) = rest.strip_prefix(' ') {
                return Some(rest);
            }
        }
    }
    None
}

/// Strip 1–6 leading `#` followed by a space, returning `(level, heading text)`.
fn strip_heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if let Some(rest) = rest.strip_prefix(' ') {
            return Some((hashes, rest));
        }
    }
    None
}

/// Leading glyph for a heading level (comments have no heading mark). Neutral
/// geometric glyphs keep it locale-neutral; deeper levels get plain bold.
fn heading_prefix(level: usize) -> &'static str {
    match level {
        1 => "\u{25C6} ", // ◆
        2 => "\u{25B8} ", // ▸
        _ => "",
    }
}

/// Strip a `- [ ] ` / `- [x] ` task-list marker, returning `(checked, text)`.
fn strip_task_item(line: &str) -> Option<(bool, &str)> {
    let rest = strip_bullet(line)?;
    if let Some(rest) = rest.strip_prefix("[ ] ") {
        Some((false, rest))
    } else if let Some(rest) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        Some((true, rest))
    } else {
        None
    }
}

/// Strip a `> ` (or `>`) blockquote marker, returning the quoted text.
fn strip_blockquote(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// Push `text` as a single bold run (used for headings).
fn push_bold_run(blocks: &mut Vec<CommentBlock>, text: &str) {
    if text.is_empty() {
        return;
    }
    blocks.push(CommentBlock {
        text: text.to_string(),
        attributes: CommentAttributes {
            bold: true,
            ..Default::default()
        },
    });
}

/// Split a single line into runs, honouring inline `` `code` `` and `**bold**`,
/// and push them onto `blocks`. Inline code takes precedence over bold (so a
/// backtick span is never re-parsed for `**`).
fn push_inline_runs(blocks: &mut Vec<CommentBlock>, line: &str) {
    for run in parse_inline(line) {
        blocks.push(run);
    }
}

/// Parse inline marks in a single line into a sequence of runs.
fn parse_inline(line: &str) -> Vec<CommentBlock> {
    let mut runs: Vec<CommentBlock> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut plain = String::new();

    let flush_plain = |plain: &mut String, runs: &mut Vec<CommentBlock>| {
        if !plain.is_empty() {
            runs.push(CommentBlock {
                text: std::mem::take(plain),
                attributes: CommentAttributes::default(),
            });
        }
    };

    while i < chars.len() {
        let c = chars[i];

        // Inline code: `...` (single backtick, no nesting).
        if c == '`' {
            if let Some(close) = find_char(&chars, i + 1, '`') {
                flush_plain(&mut plain, &mut runs);
                let text: String = chars[i + 1..close].iter().collect();
                runs.push(CommentBlock {
                    text,
                    attributes: CommentAttributes {
                        code: true,
                        ..Default::default()
                    },
                });
                i = close + 1;
                continue;
            }
        }

        // Link: [text](url). The link text may contain inline marks, so parse
        // it recursively and attach the URL to every resulting run.
        if c == '[' {
            if let Some((text_end, url_start, url_end)) = find_link(&chars, i) {
                flush_plain(&mut plain, &mut runs);
                let text: String = chars[i + 1..text_end].iter().collect();
                let url: String = chars[url_start..url_end].iter().collect();
                for mut run in parse_inline(&text) {
                    run.attributes.link = Some(url.clone());
                    runs.push(run);
                }
                i = url_end + 1;
                continue;
            }
        }

        // Bold: **...**. The inner content may itself contain inline code, so
        // parse it recursively and mark every resulting run bold (a run can
        // carry both `bold` and `code` where they overlap, e.g. **`x`**).
        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(close) = find_double_star(&chars, i + 2) {
                flush_plain(&mut plain, &mut runs);
                let inner: String = chars[i + 2..close].iter().collect();
                for mut run in parse_inline(&inner) {
                    run.attributes.bold = true;
                    runs.push(run);
                }
                i = close + 2;
                continue;
            }
        }

        // Italic: *...* or _..._ (single delimiter, not part of a `**` pair —
        // bold is handled above so any remaining lone `*` is italic).
        if c == '*' || c == '_' {
            if let Some(close) = find_char(&chars, i + 1, c) {
                if close > i + 1 {
                    flush_plain(&mut plain, &mut runs);
                    let inner: String = chars[i + 1..close].iter().collect();
                    for mut run in parse_inline(&inner) {
                        run.attributes.italic = true;
                        runs.push(run);
                    }
                    i = close + 1;
                    continue;
                }
            }
        }

        plain.push(c);
        i += 1;
    }

    flush_plain(&mut plain, &mut runs);
    runs
}

/// Find the next index of `needle` in `chars` at or after `from`.
fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == needle)
}

/// Find the next `**` (start index) in `chars` at or after `from`.
fn find_double_star(chars: &[char], from: usize) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == '*' && chars[j + 1] == '*' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Parse a `[text](url)` link starting at `open` (the `[`). Returns
/// `(text_end, url_start, url_end)` as char indices, where `text_end` is the
/// `]`, `url_start` is the first URL char, and `url_end` is the `)`.
fn find_link(chars: &[char], open: usize) -> Option<(usize, usize, usize)> {
    let close_br = find_char(chars, open + 1, ']')?;
    if chars.get(close_br + 1) != Some(&'(') {
        return None;
    }
    let url_start = close_br + 2;
    let close_paren = find_char(chars, url_start, ')')?;
    Some((close_br, url_start, close_paren))
}

// =============================================================================
// GFM tables → aligned monospace code-block (ClickUp comments have no table
// mark). Columns size to content; cell data is never truncated.
// =============================================================================

/// True when a line looks like a GFM table row (`| ... |`).
fn is_table_row(line: &str) -> bool {
    let l = line.trim();
    l.starts_with('|') && l.matches('|').count() >= 2
}

/// Split a `| a | b |` row into trimmed cells.
fn split_table_row(line: &str) -> Vec<String> {
    let l = line.trim();
    let l = l.strip_prefix('|').unwrap_or(l);
    let l = l.strip_suffix('|').unwrap_or(l);
    l.split('|').map(|c| c.trim().to_string()).collect()
}

/// True when every cell is a separator (`---`, `:--`, `--:`, `:-:`).
fn is_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let t = c.trim();
            !t.is_empty() && t.contains('-') && t.chars().all(|ch| ch == '-' || ch == ':')
        })
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
    Center,
}

fn parse_align(cell: &str) -> Align {
    let t = cell.trim();
    match (t.starts_with(':'), t.ends_with(':')) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    }
}

/// Display width: most chars = 1, CJK/fullwidth/emoji = 2. Cyrillic counts as
/// 1, so Russian table columns align correctly. No `unicode-width` dep.
fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn char_width(ch: char) -> usize {
    let c = ch as u32;
    let double = (0x1100..=0x115F).contains(&c)
        || (0x2E80..=0xA4CF).contains(&c)
        || (0xAC00..=0xD7A3).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFF00..=0xFF60).contains(&c)
        || (0xFFE0..=0xFFE6).contains(&c)
        || (0x1F300..=0x1FAFF).contains(&c)
        || (0x20000..=0x3FFFD).contains(&c);
    if double { 2 } else { 1 }
}

/// Pad a cell to `width` display columns per alignment. Never truncates: the
/// width is always the column's content width, so cell data is preserved
/// verbatim (read-back stays a valid GFM table for downstream LLMs).
fn pad_cell(cell: &str, width: usize, align: Align) -> String {
    let w = display_width(cell);
    let pad = width.saturating_sub(w);
    match align {
        Align::Left => format!("{}{}", cell, " ".repeat(pad)),
        Align::Right => format!("{}{}", " ".repeat(pad), cell),
        Align::Center => {
            let left = pad / 2;
            format!("{}{}{}", " ".repeat(left), cell, " ".repeat(pad - left))
        }
    }
}

/// Render GFM table rows to aligned monospace lines (one string per line).
fn render_table(rows: &[String]) -> Vec<String> {
    let parsed: Vec<Vec<String>> = rows.iter().map(|r| split_table_row(r)).collect();
    let ncols = parsed.iter().map(|c| c.len()).max().unwrap_or(0);
    if ncols == 0 {
        return Vec::new();
    }

    let mut aligns = vec![Align::Left; ncols];
    let mut data: Vec<Vec<String>> = Vec::new();
    for cells in &parsed {
        if is_separator_row(cells) {
            for (i, c) in cells.iter().enumerate().take(ncols) {
                aligns[i] = parse_align(c);
            }
        } else {
            let mut row = cells.clone();
            row.resize(ncols, String::new());
            data.push(row);
        }
    }
    if data.is_empty() {
        return Vec::new();
    }

    // Column widths sized to content (never below, so no truncation).
    let mut width = vec![0usize; ncols];
    for row in &data {
        for (i, cell) in row.iter().enumerate() {
            width[i] = width[i].max(display_width(cell));
        }
    }

    let mut out: Vec<String> = Vec::new();
    for (ri, row) in data.iter().enumerate() {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| pad_cell(cell, width[i], aligns[i]))
            .collect();
        out.push(format!("| {} |", cells.join(" | ")));
        if ri == 0 {
            let dividers: Vec<String> = width.iter().map(|w| "-".repeat(*w)).collect();
            out.push(format!("|-{}-|", dividers.join("-|-")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> CommentBlock {
        CommentBlock {
            text: text.to_string(),
            attributes: CommentAttributes::default(),
        }
    }

    #[test]
    fn plain_paragraph() {
        let blocks = markdown_to_comment_blocks("hello world");
        assert_eq!(blocks, vec![plain("hello world")]);
    }

    #[test]
    fn inline_code_splits_runs() {
        let blocks = markdown_to_comment_blocks("the `SecretBackend` trait");
        assert_eq!(
            blocks,
            vec![
                plain("the "),
                CommentBlock {
                    text: "SecretBackend".to_string(),
                    attributes: CommentAttributes {
                        code: true,
                        ..Default::default()
                    },
                },
                plain(" trait"),
            ]
        );
    }

    #[test]
    fn inline_code_does_not_fragment_surrounding_prose() {
        // Regression for the issue: many backtick tokens must stay as
        // contiguous prose runs, not each become an isolated chip with the
        // text shattered around them.
        let blocks = markdown_to_comment_blocks("a `x` b `y` c");
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["a ", "x", " b ", "y", " c"]);
        assert!(blocks[1].attributes.code);
        assert!(blocks[3].attributes.code);
    }

    #[test]
    fn bold_run() {
        let blocks = markdown_to_comment_blocks("a **bold** b");
        assert_eq!(blocks[1].text, "bold");
        assert!(blocks[1].attributes.bold);
    }

    #[test]
    fn bold_with_nested_inline_code() {
        // **`SecretBackend`** must yield a single run that is BOTH bold and
        // code, with the backticks consumed — not a bold run with literal
        // backticks in the text.
        let blocks = markdown_to_comment_blocks("**`SecretBackend`**");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "SecretBackend");
        assert!(blocks[0].attributes.bold);
        assert!(blocks[0].attributes.code);
    }

    #[test]
    fn bold_with_mixed_inner_content() {
        // **`backend.rs`** (new) — leading bold+code run, then plain tail.
        let blocks = markdown_to_comment_blocks("**`backend.rs`** (new)");
        assert_eq!(blocks[0].text, "backend.rs");
        assert!(blocks[0].attributes.bold && blocks[0].attributes.code);
        assert_eq!(blocks[1].text, " (new)");
        assert!(blocks[1].attributes.is_empty());
    }

    #[test]
    fn fenced_code_block() {
        let body = "```rust\nlet x = 1;\nlet y = 2;\n```";
        let blocks = markdown_to_comment_blocks(body);
        // Two content lines, each followed by a code-block newline. The fence
        // markers themselves are dropped.
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].text, "let x = 1;");
        assert!(blocks[1].attributes.code_block.is_some());
        assert_eq!(blocks[1].text, "\n");
        assert_eq!(blocks[2].text, "let y = 2;");
        assert!(blocks[3].attributes.code_block.is_some());
    }

    #[test]
    fn fenced_code_block_does_not_parse_inline() {
        // Backticks/stars inside a fence are literal.
        let body = "```\na `b` **c**\n```";
        let blocks = markdown_to_comment_blocks(body);
        assert_eq!(blocks[0].text, "a `b` **c**");
        assert!(blocks[0].attributes.is_empty());
    }

    #[test]
    fn bullet_list() {
        let body = "- one\n- two";
        let blocks = markdown_to_comment_blocks(body);
        assert_eq!(blocks[0].text, "one");
        assert_eq!(
            blocks[1].attributes.list.as_ref().map(|l| l.list.as_str()),
            Some("bullet")
        );
        assert_eq!(blocks[2].text, "two");
        assert_eq!(
            blocks[3].attributes.list.as_ref().map(|l| l.list.as_str()),
            Some("bullet")
        );
    }

    #[test]
    fn bullet_list_item_keeps_inline_code() {
        let blocks = markdown_to_comment_blocks("- first with `code`");
        assert_eq!(blocks[0].text, "first with ");
        assert!(blocks[1].attributes.code);
        assert_eq!(blocks[1].text, "code");
        assert!(blocks[2].attributes.list.is_some());
    }

    #[test]
    fn ordered_list() {
        let body = "1. one\n2. two";
        let blocks = markdown_to_comment_blocks(body);
        assert_eq!(blocks[0].text, "one");
        assert_eq!(
            blocks[1].attributes.list.as_ref().map(|l| l.list.as_str()),
            Some("ordered")
        );
    }

    #[test]
    fn heading_becomes_bold() {
        // h2 → leading ▸ glyph (bold) + bold heading text.
        let blocks = markdown_to_comment_blocks("## Done");
        assert_eq!(blocks[0].text, "\u{25B8} ");
        assert!(blocks[0].attributes.bold);
        assert_eq!(blocks[1].text, "Done");
        assert!(blocks[1].attributes.bold);
    }

    #[test]
    fn h3_heading_has_no_glyph() {
        let blocks = markdown_to_comment_blocks("### Sub");
        assert_eq!(blocks[0].text, "Sub");
        assert!(blocks[0].attributes.bold);
    }

    #[test]
    fn unterminated_inline_code_is_literal() {
        let blocks = markdown_to_comment_blocks("a `b c");
        assert_eq!(blocks, vec![plain("a `b c")]);
    }

    #[test]
    fn serializes_attributes_with_clickup_shape() {
        let body = "- item\n```\ncode\n```\n`x`";
        let blocks = markdown_to_comment_blocks(body);
        let json = serde_json::to_string(&blocks).unwrap();
        // List newline shape.
        assert!(json.contains(r#""list":{"list":"bullet"}"#));
        // Code-block newline shape.
        assert!(json.contains(r#""code-block":{"code-block":"plain"}"#));
        // Inline code mark.
        assert!(json.contains(r#""code":true"#));
        // Plain runs omit the attributes object entirely.
        assert!(json.contains(r#"{"text":"item"}"#));
    }

    #[test]
    fn trailing_blank_lines_are_trimmed() {
        // A body ending in one or more blank lines must not leave dangling
        // plain newline separators (regression for PR #294 review feedback).
        // All trailing plain newlines are trimmed, leaving just the content.
        for body in ["a", "a\n", "a\n\n", "a\n\n\n"] {
            let blocks = markdown_to_comment_blocks(body);
            assert_eq!(
                blocks,
                vec![plain("a")],
                "body {body:?} should trim every trailing plain newline"
            );
        }
    }

    #[test]
    fn trailing_block_separator_is_preserved() {
        // A trailing newline that carries a block attribute (list/code-block)
        // is structurally significant and must NOT be trimmed.
        let blocks = markdown_to_comment_blocks("- item\n");
        let last = blocks.last().unwrap();
        assert!(last.attributes.list.is_some());
    }

    #[test]
    fn empty_body_yields_no_blocks() {
        assert!(markdown_to_comment_blocks("").is_empty());
    }

    // --- additions: italic, links, task lists, blockquote, hr, tables ---

    #[test]
    fn italic_run() {
        let blocks = markdown_to_comment_blocks("a *b* c");
        assert_eq!(blocks[0], plain("a "));
        assert_eq!(blocks[1].text, "b");
        assert!(blocks[1].attributes.italic && !blocks[1].attributes.bold);
        assert_eq!(blocks[2], plain(" c"));
    }

    #[test]
    fn italic_underscore() {
        let blocks = markdown_to_comment_blocks("_x_");
        assert_eq!(blocks[0].text, "x");
        assert!(blocks[0].attributes.italic);
    }

    #[test]
    fn bold_not_swallowed_by_italic() {
        // `**b**` must stay bold, not be parsed as two italic `*` pairs.
        let blocks = markdown_to_comment_blocks("**b**");
        assert_eq!(blocks[0].text, "b");
        assert!(blocks[0].attributes.bold && !blocks[0].attributes.italic);
    }

    #[test]
    fn link_run() {
        let blocks = markdown_to_comment_blocks("see [docs](https://x.io) now");
        assert_eq!(blocks[0], plain("see "));
        assert_eq!(blocks[1].text, "docs");
        assert_eq!(blocks[1].attributes.link.as_deref(), Some("https://x.io"));
        assert_eq!(blocks[2], plain(" now"));
    }

    #[test]
    fn task_list_checked_unchecked() {
        let blocks = markdown_to_comment_blocks("- [ ] todo\n- [x] done");
        assert_eq!(
            blocks[1].attributes.list.as_ref().map(|l| l.list.as_str()),
            Some("unchecked")
        );
        assert_eq!(
            blocks[3].attributes.list.as_ref().map(|l| l.list.as_str()),
            Some("checked")
        );
    }

    #[test]
    fn blockquote_is_italic_with_gutter() {
        let blocks = markdown_to_comment_blocks("> quoted");
        assert_eq!(blocks[0].text, "| ");
        assert_eq!(blocks[1].text, "quoted");
        assert!(blocks[1].attributes.italic);
    }

    #[test]
    fn horizontal_rule() {
        let blocks = markdown_to_comment_blocks("---");
        assert_eq!(blocks[0].text, "\u{2500}".repeat(10));
    }

    #[test]
    fn table_cyrillic_aligns() {
        let md = "| Проверка | Результат |\n|---|---|\n| meet | OK |";
        let blocks = markdown_to_comment_blocks(md);
        let lines: Vec<&str> = blocks
            .iter()
            .filter(|b| b.text != "\n")
            .map(|b| b.text.as_str())
            .collect();
        assert_eq!(lines.len(), 3); // header, divider, one data row
        assert_eq!(lines[0], "| Проверка | Результат |");
        assert!(lines[1].starts_with("|-"));
        assert_eq!(lines[2], "| meet     | OK        |");
        // table lines carry the code-block block attribute (on the newline runs)
        assert!(
            blocks
                .iter()
                .any(|b| b.text == "\n" && b.attributes.code_block.is_some())
        );
    }

    #[test]
    fn wide_table_preserves_content() {
        let wide = "x".repeat(200);
        let md = format!("| {wide} | b |\n|---|---|\n| y | z |");
        let rendered: String = markdown_to_comment_blocks(&md)
            .iter()
            .map(|b| b.text.as_str())
            .collect();
        assert!(rendered.contains(&wide), "wide cell preserved");
        assert!(!rendered.contains('\u{2026}'), "no truncation ellipsis");
    }

    #[test]
    fn cyrillic_bold_no_panic() {
        // char-based parser must not panic on multi-byte UTF-8 next to a mark.
        let blocks = markdown_to_comment_blocks("жирный **текст** конец");
        assert!(
            blocks
                .iter()
                .any(|b| b.attributes.bold && b.text == "текст")
        );
    }
}
