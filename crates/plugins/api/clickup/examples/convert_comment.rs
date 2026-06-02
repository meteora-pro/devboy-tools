//! Convert a markdown comment body (read from stdin) into ClickUp's
//! structured `comment` rich-text array (printed as JSON to stdout).
//!
//! Mirrors exactly what `add_comment` sends, so it can be used to repair an
//! existing comment via `PUT /comment/{id}` with the same rendering logic.
//!
//! Usage: `cat body.md | cargo run -p devboy-clickup --example convert_comment`

use std::io::Read;

fn main() {
    let mut body = String::new();
    std::io::stdin()
        .read_to_string(&mut body)
        .expect("read stdin");
    let blocks = devboy_clickup::comment_format::markdown_to_comment_blocks(&body);
    let json = serde_json::to_string(&blocks).expect("serialize comment blocks");
    println!("{json}");
}
