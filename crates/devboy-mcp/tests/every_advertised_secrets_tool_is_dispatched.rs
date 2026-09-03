//! Source gate: a `secrets_*` tool the server advertises must have
//! a dispatch arm, and vice versa.
//!
//! # The hole this fills
//!
//! This epic's signature defect is code that exists and is never
//! reached. `secrets_unlock` and `secrets_status` were implemented,
//! tested, and absent from `tools/list`, so no agent could call
//! them. `secrets_validate` had its reply types written and
//! covered by unit tests while nothing dispatched the name at all.
//! Both were found by reading, not by a failing build.
//!
//! The executor crate already checks one direction — a hand-kept
//! list of dispatched names must all be advertised. That list is
//! maintained by the same person making the mistake, and it says
//! nothing about the reverse: a tool advertised to agents with no
//! handler behind it, which fails at the worst possible moment,
//! after an agent has decided to use it.
//!
//! # How
//!
//! Reads the advertised names from `mcp_only_tools()` and the
//! dispatch arms out of the server's source. Textual, because the
//! dispatch is a `match` on string literals inside a private
//! method, and the honest way to see it from a test is to read it.
//!
//! A false pass is possible — a name could appear in the file
//! somewhere that is not a match arm. The arms are anchored to
//! `"name" =>` to make that unlikely, and the failure this guards
//! is a *missing* arm, which no amount of unrelated text creates.

use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Names the server's internal-tool `match` handles.
fn dispatched_names(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix('"')?;
            let (name, tail) = rest.split_once('"')?;
            tail.trim_start().starts_with("=>").then(|| name.to_owned())
        })
        .collect()
}

#[test]
fn every_advertised_secrets_tool_has_a_handler() {
    let source = std::fs::read_to_string(crate_root().join("src/server.rs"))
        .expect("the MCP server's source is readable from its own test");
    let dispatched = dispatched_names(&source);

    let advertised: Vec<String> = devboy_executor::tools::mcp_only_tools()
        .into_iter()
        .map(|t| t.name)
        .filter(|n| n.starts_with("secrets_"))
        .collect();

    assert!(
        !advertised.is_empty(),
        "the secrets family vanished from tools/list, which is a bigger problem than this test"
    );

    let orphans: Vec<&String> = advertised
        .iter()
        .filter(|name| !dispatched.contains(name))
        .collect();

    assert!(
        orphans.is_empty(),
        "advertised to agents with nothing behind them: {orphans:?}. An agent that picks one of \
         these gets an error after deciding to use it, which is worse than never offering it."
    );
}

/// The parser has to actually find arms, or the test above passes
/// by seeing an empty list on both sides of the comparison.
#[test]
fn the_arm_parser_finds_the_arms() {
    let source = std::fs::read_to_string(crate_root().join("src/server.rs")).unwrap();
    let dispatched = dispatched_names(&source);

    assert!(
        dispatched.iter().any(|n| n == "secrets_list"),
        "the parser found no `secrets_list` arm, so it is not reading dispatch at all: \
         {dispatched:?}"
    );
    assert!(
        dispatched.iter().any(|n| n == "secrets_validate"),
        "{dispatched:?}"
    );
}
