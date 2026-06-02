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
//! This module converts the small markdown subset our comments use — inline
//! code, bold, fenced code blocks, bullet/ordered lists, ATX headings, and
//! plain paragraphs — into that array. Anything it doesn't recognise is
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
    pub code: bool,
    /// Block-level code fence. ClickUp's shape is `{"code-block": "plain"}`.
    #[serde(rename = "code-block", skip_serializing_if = "Option::is_none")]
    pub code_block: Option<CodeBlockAttr>,
    /// List membership. ClickUp's shape is `{"list": "bullet" | "ordered"}`.
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
        !self.bold && !self.code && self.code_block.is_none() && self.list.is_none()
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
/// - `1. ` ordered items → content + an `ordered` list newline;
/// - ATX headings (`#`..`######`) → bold content (comments have no heading mark);
/// - inline `` `code` `` and `**bold**` within any non-code line;
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

    for line in body.split('\n') {
        // Fence toggles. A line whose trimmed start is ``` opens or closes a
        // fenced block; the fence line itself (and its info string) is dropped.
        if line.trim_start().starts_with("```") {
            in_code_fence = !in_code_fence;
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
            continue;
        }

        // ATX heading: 1–6 leading `#` then a space. Comments have no heading
        // attribute, so render the text bold to preserve emphasis.
        if let Some(rest) = strip_heading(line) {
            push_bold_run(&mut blocks, rest);
            blocks.push(newline(CommentAttributes::default()));
            continue;
        }

        // Plain paragraph line (may contain inline code / bold).
        push_inline_runs(&mut blocks, line);
        blocks.push(newline(CommentAttributes::default()));
    }

    // `split('\n')` yields a trailing empty segment when the body ends in a
    // newline, producing a redundant trailing separator. Trim a single
    // trailing plain newline so we don't emit a dangling blank line.
    if let Some(last) = blocks.last() {
        if last.text == "\n" && last.attributes.is_empty() && blocks.len() > 1 {
            blocks.pop();
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

/// Strip 1–6 leading `#` followed by a space, returning the heading text.
fn strip_heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if let Some(rest) = rest.strip_prefix(' ') {
            return Some(rest);
        }
    }
    None
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
        let blocks = markdown_to_comment_blocks("## Done");
        assert_eq!(blocks[0].text, "Done");
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
    fn empty_body_yields_no_blocks() {
        assert!(markdown_to_comment_blocks("").is_empty());
    }
}
