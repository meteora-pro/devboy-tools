//! Every `devboy …` command named in the source must exist.
//!
//! # Why this is a test and not a review habit
//!
//! An error message that tells the user to run a command is only
//! useful if the command is real. Five were not: `devboy secrets
//! lock`, `devboy secrets unlock`, `devboy secrets refresh`, `devboy
//! secrets vault add-totp` and `devboy tune analyze`. Two of those
//! reached the user at runtime — one from the daemon when TOTP was
//! unavailable, one from config validation — and the person who
//! followed either got "unrecognized subcommand" and no idea what to
//! do instead.
//!
//! Nothing catches this by reading. The command tree moves,
//! `secrets vault add-totp` becomes `secrets add-totp`, and the
//! prose that named it stays behind looking correct. So the check
//! belongs where the tree itself can be asked.
//!
//! # What it checks
//!
//! Every backtick-quoted `devboy …` string in the workspace's Rust
//! sources is resolved against the real clap tree. Flags, operands
//! and `<placeholders>` are dropped; only the leading run of
//! subcommand words is resolved, which is the part that either
//! exists or does not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Phrases that begin `devboy …` but are prose, not invocations.
///
/// Kept tiny and specific on purpose: a generous allow-list would
/// turn this test back into the review habit it replaces.
const NOT_COMMANDS: &[&str] = &[
    // "…hand devboy to the agent…" and similar.
    "to",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate> sits two levels below the workspace root")
        .to_path_buf()
}

/// Every `.rs` file under `crates/`.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Pull the subcommand words out of one backtick-quoted invocation.
///
/// `devboy secrets keyfile add --path x` → `["secrets", "keyfile",
/// "add"]`. Stops at the first token that cannot be a subcommand
/// name, since everything after it is arguments.
fn subcommand_path(quoted: &str) -> Vec<String> {
    quoted
        .split_whitespace()
        .skip(1) // "devboy"
        .take_while(|word| {
            !word.starts_with('-')
                && !word.starts_with('<')
                && !word.starts_with('[')
                && !word.contains('/')
                && !word.contains('=')
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        .map(str::to_owned)
        .collect()
}

/// Ask the real binary whether this command path resolves.
///
/// The shipped binary rather than an in-process `clap::Command`,
/// because `Cli` lives in `main.rs` and because what matters is what
/// the user actually gets when they type it. `--help` is answered by
/// clap before any command body runs, so this is side-effect free;
/// an unknown subcommand exits non-zero.
fn resolves(words: &[String]) -> bool {
    Command::new(env!("CARGO_BIN_EXE_devboy"))
        .args(words)
        .arg("--help")
        .output()
        .expect("the devboy binary is built for this test")
        .status
        .success()
}

/// The first word of `words` that does not resolve.
///
/// Walks left to right so the report names the word that broke,
/// not just the whole invocation. Answers are cached because the
/// same prefixes repeat across the workspace.
fn first_unknown_word(words: &[String], cache: &mut HashMap<Vec<String>, bool>) -> Option<String> {
    for depth in 1..=words.len() {
        let prefix = words[..depth].to_vec();
        let ok = match cache.get(&prefix) {
            Some(known) => *known,
            None => {
                let answer = resolves(&prefix);
                cache.insert(prefix, answer);
                answer
            }
        };
        if !ok {
            return Some(words[depth - 1].clone());
        }
    }
    None
}

#[test]
fn every_devboy_command_named_in_the_source_exists() {
    let mut cache: HashMap<Vec<String>, bool> = HashMap::new();

    let mut files = Vec::new();
    rust_sources(&workspace_root().join("crates"), &mut files);
    assert!(
        files.len() > 100,
        "expected to find the workspace sources, found {} files",
        files.len()
    );

    // `\x60` rather than a literal backtick so this file's own
    // pattern is not itself a quoted command.
    let quoted = regex::Regex::new(r"\x60devboy ([^\x60]{1,80})\x60").expect("valid pattern");

    let mut problems: Vec<String> = Vec::new();

    for file in &files {
        // This file quotes the broken commands on purpose, to say
        // what it exists to prevent.
        if file.ends_with("quoted_commands_exist.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };

        for capture in quoted.captures_iter(&text) {
            let whole = format!("devboy {}", &capture[1]);
            let words = subcommand_path(&whole);
            if words.is_empty() || NOT_COMMANDS.contains(&words[0].as_str()) {
                continue;
            }
            if let Some(unknown) = first_unknown_word(&words, &mut cache) {
                let shown = file
                    .strip_prefix(workspace_root())
                    .unwrap_or(file)
                    .display();
                problems.push(format!(
                    "  {shown}: `{whole}` — `{unknown}` is not a command there"
                ));
            }
        }
    }

    problems.sort();
    problems.dedup();
    assert!(
        problems.is_empty(),
        "the source names {} command(s) the CLI does not have. A user who follows one of these \
         gets `unrecognized subcommand` and nothing to do next:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// The gate has to be able to fail, or it proves nothing.
#[test]
fn the_check_rejects_a_command_that_does_not_exist() {
    let mut cache: HashMap<Vec<String>, bool> = HashMap::new();

    assert_eq!(
        first_unknown_word(
            &subcommand_path("devboy secrets vault add-totp"),
            &mut cache
        ),
        Some("vault".to_owned()),
        "this is the shape that shipped in a runtime error message"
    );
    assert_eq!(
        first_unknown_word(&subcommand_path("devboy secrets lock"), &mut cache),
        Some("lock".to_owned())
    );
    assert_eq!(
        first_unknown_word(&subcommand_path("devboy tune analyze"), &mut cache),
        Some("tune".to_owned())
    );

    // And accepts real ones, including with arguments attached.
    assert_eq!(
        first_unknown_word(&subcommand_path("devboy secrets add-totp"), &mut cache),
        None
    );
    assert_eq!(
        first_unknown_word(
            &subcommand_path("devboy secrets keyfile add --path /tmp/k"),
            &mut cache
        ),
        None
    );
    assert_eq!(
        first_unknown_word(&subcommand_path("devboy secrets agent unlock"), &mut cache),
        None
    );
}
