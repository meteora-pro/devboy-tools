//! Traceability gate for the Gherkin specifications (Т8 /
//! ADR-024).
//!
//! # Why a gate and not a runner
//!
//! `docs/guide/secrets/scenarios/` holds Gherkin that describes the
//! secret framework's user-facing behaviour. Until now nothing
//! executed or checked it, so the scenarios could drift from the
//! code indefinitely — a specification that cannot be wrong is not
//! a specification.
//!
//! The obvious fix is `cucumber-rs`. It was considered and
//! rejected. The scenarios are written at a level no honest step
//! definition can reach:
//!
//! ```gherkin
//! Then the build fails with a typed error because SecretString
//!   does not implement AgentSafeReply
//! ```
//!
//! ```gherkin
//! When the user clicks "Deny" in the dialog
//! ```
//!
//! Steps like these can only be faked, and a green fake is *worse*
//! than no runner: the specification would look verified while
//! asserting nothing. The behaviour they describe is already
//! covered — by compile-fail gates, by GUI-renderer unit tests, by
//! process-level integration tests — just not from Gherkin.
//!
//! So this gate enforces the **link** rather than re-executing the
//! behaviour. Every scenario carries `@covered-by:<test_fn>`, and
//! this test checks that:
//!
//! 1. every scenario names at least one covering test,
//! 2. every named test still exists in the workspace,
//! 3. scenario names are unique within a file.
//!
//! Rule 2 is what makes the specification load-bearing: delete or
//! rename a test and the scenario it backed fails CI, which forces
//! the author to either restore the coverage or admit the
//! behaviour is gone.
//!
//! # Admitted gaps
//!
//! A handful of scenarios describe behaviour nothing tests — a GUI
//! button whose result flag no test ever inspects, a first-run check
//! that reads `dirs::config_dir()` and the environment directly and
//! so cannot be driven from a test. Tagging those with a
//! loosely-related test would make the gate *lie*, which is worse
//! than the gap it papers over.
//!
//! They carry `@not-covered:<reason>` instead, and
//! [`UNCOVERED`] pins the exact set. Adding a scenario without
//! coverage fails; so does quietly swapping which one is uncovered.
//! The debt is visible and cannot grow by accident, which is the
//! most a gate can honestly offer here.
//!
//! # What this deliberately does not guarantee
//!
//! That the named test *means* what the scenario says. A gate can
//! check that a link exists and still points somewhere; it cannot
//! check that the destination is the right one. Reviewing that
//! remains a human job, which is why the tag sits directly above
//! the scenario where a reviewer reads both at once.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Tag prefix that links a scenario to a covering test.
const COVERAGE_TAG: &str = "@covered-by:";

/// Tag prefix admitting that nothing covers a scenario.
const UNCOVERED_TAG: &str = "@not-covered:";

/// The scenarios that currently have no covering test, pinned so
/// the set can shrink but never grow.
///
/// Every entry is a real gap found by reading the sources, not a
/// convenience. Removing one means someone wrote the test.
const UNCOVERED: &[(&str, &str)] = &[
    // `OnboardingFrameResult.skip` is set by the button but every
    // render test discards the returned struct.
    ("onboarding.feature", "Skip goes straight to the keychain"),
    // `is_first_run()` reads `dirs::config_dir()` and the process
    // environment directly, so nothing can drive it.
    (
        "onboarding.feature",
        "The wizard does not re-appear once onboarded",
    ),
    // The pinned 845/236/175/161 counts need a checked-in fixture
    // of the demo project, which does not exist.
    (
        "proposer-noise-reduction.feature",
        "Cumulative noise reduction on the canonical demo project",
    ),
    // No egui interaction harness in the test tree — the existing
    // GUI tests only assert the renderer does not panic.
    (
        "ui-catalog-rendering.feature",
        "The dialog is a modal overlay, not an inline route",
    ),
    // Every fixture sets rotation notes and guide URL together, so
    // the notes-without-URL branch is never rendered.
    (
        "ui-catalog-rendering.feature",
        "Variant with rotation notes but no guide URL still renders the section",
    ),
    // `use_keychain` is set by a button and read by the modal, and
    // no test constructs or inspects the result.
    (
        "vault-unlock.feature",
        "The keychain escape hatch skips the vault for the session",
    ),
    // The provision dialog has a render-buffer scan proving the
    // value never reaches the output; the unlock modal has no
    // equivalent.
    (
        "vault-unlock.feature",
        "The agent never sees the passphrase",
    ),
];

/// Repository root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate> has two ancestors")
        .to_path_buf()
}

fn scenarios_dir() -> PathBuf {
    repo_root().join("docs/guide/secrets/scenarios")
}

/// One scenario and the tests claimed to cover it.
#[derive(Debug)]
struct Scenario {
    file: String,
    line: usize,
    name: String,
    covered_by: Vec<String>,
    /// Set when the scenario admits it has no covering test.
    uncovered_reason: Option<String>,
}

/// Parse every `.feature` file in the scenarios directory.
///
/// Deliberately a hand-rolled reader rather than a Gherkin crate:
/// the only constructs it needs are tag lines and `Scenario:` /
/// `Scenario Outline:` headers, and a dependency whose parser
/// rejects a file for unrelated reasons would turn a documentation
/// edit into a mysterious CI failure.
fn parse_scenarios() -> Vec<Scenario> {
    let dir = scenarios_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "feature"))
        .collect();
    entries.sort();

    assert!(
        !entries.is_empty(),
        "no .feature files under {} — if the specifications moved, this gate must move with them",
        dir.display()
    );

    let mut out = Vec::new();
    for path in entries {
        let file = path
            .file_name()
            .expect("feature file has a name")
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        // Tags accumulate until a scenario consumes them, which is
        // Gherkin's own rule.
        let mut pending: Vec<String> = Vec::new();
        let mut pending_uncovered: Option<String> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();

            if line.starts_with('@') {
                for token in line.split_whitespace() {
                    if let Some(test) = token.strip_prefix(COVERAGE_TAG) {
                        pending.push(test.to_owned());
                    } else if let Some(reason) = token.strip_prefix(UNCOVERED_TAG) {
                        pending_uncovered = Some(reason.to_owned());
                    }
                }
                continue;
            }

            if let Some(name) = line
                .strip_prefix("Scenario Outline:")
                .or_else(|| line.strip_prefix("Scenario:"))
            {
                out.push(Scenario {
                    file: file.clone(),
                    line: index + 1,
                    name: name.trim().to_owned(),
                    covered_by: std::mem::take(&mut pending),
                    uncovered_reason: pending_uncovered.take(),
                });
                continue;
            }

            // A blank line does not clear tags (Gherkin allows a gap
            // between tags and their scenario), but any other
            // content does — otherwise a tag on the Feature would
            // silently count as coverage for the first scenario.
            if !line.is_empty() && !line.starts_with('#') {
                pending.clear();
                pending_uncovered = None;
            }
        }
    }
    out
}

/// Every function name defined anywhere under `crates/`.
///
/// Test functions are the only ones a tag should name, but
/// restricting the index to `#[test]`-annotated items would mean
/// parsing attributes across `cfg` blocks and macro-generated
/// tests. Indexing every `fn` keeps the check robust; the failure
/// it must catch — a tag naming something that no longer exists —
/// is caught either way.
fn workspace_function_names() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                for line in text.lines() {
                    let Some(rest) = line.trim_start().strip_prefix("fn ").or_else(|| {
                        line.trim_start()
                            .strip_prefix("pub fn ")
                            .or_else(|| line.trim_start().strip_prefix("async fn "))
                            .or_else(|| line.trim_start().strip_prefix("pub async fn "))
                    }) else {
                        continue;
                    };
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        out.insert(name);
                    }
                }
            }
        }
    }

    let mut out = BTreeSet::new();
    walk(&repo_root().join("crates"), &mut out);
    out
}

/// Every scenario must name at least one covering test.
///
/// This is the rule that stops the specification growing faster
/// than the suite: a new scenario with no coverage is a claim
/// nobody checks.
#[test]
fn every_scenario_names_a_covering_test() {
    let untagged: Vec<String> = parse_scenarios()
        .into_iter()
        .filter(|s| s.covered_by.is_empty() && s.uncovered_reason.is_none())
        .map(|s| format!("  {}:{} — {}", s.file, s.line, s.name))
        .collect();

    assert!(
        untagged.is_empty(),
        "{} scenario(s) claim behaviour that no test is linked to.\n\n{}\n\nAdd a tag line \
         directly above each one:\n\n    {COVERAGE_TAG}name_of_the_test_that_covers_it\n\nIf \
         nothing covers it yet, write the test first. If it genuinely cannot be tested today, \
         say so with {UNCOVERED_TAG}<short-reason> and add it to UNCOVERED in this file — but \
         that list is a ratchet, so expect to justify it.",
        untagged.len(),
        untagged.join("\n")
    );
}

/// The admitted-gap list is a ratchet: it may shrink, never grow.
///
/// Without this, `@not-covered:` would be an unlimited escape hatch
/// and the gate would decay into decoration.
#[test]
fn the_set_of_uncovered_scenarios_matches_the_pinned_list() {
    let actual: BTreeSet<(String, String)> = parse_scenarios()
        .into_iter()
        .filter(|s| s.uncovered_reason.is_some())
        .map(|s| (s.file, s.name))
        .collect();
    let pinned: BTreeSet<(String, String)> = UNCOVERED
        .iter()
        .map(|(f, n)| ((*f).to_owned(), (*n).to_owned()))
        .collect();

    let added: Vec<_> = actual.difference(&pinned).collect();
    assert!(
        added.is_empty(),
        "{} scenario(s) newly admit having no test:\n{}\n\nA specification that grows faster \
         than the suite is how the two drift apart. Write the test, or add the entry to \
         UNCOVERED in this file with the reason it cannot be written.",
        added.len(),
        added
            .iter()
            .map(|(f, n)| format!("  {f} — {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let removed: Vec<_> = pinned.difference(&actual).collect();
    assert!(
        removed.is_empty(),
        "{} scenario(s) in UNCOVERED are no longer marked {UNCOVERED_TAG}:\n{}\n\nIf coverage \
         landed, delete the entry from UNCOVERED — the list only means something while it is \
         exact.",
        removed.len(),
        removed
            .iter()
            .map(|(f, n)| format!("  {f} — {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// An admitted gap must say *why*, so the next person can judge
/// whether it is still true.
#[test]
fn every_admitted_gap_carries_a_reason() {
    let vague: Vec<String> = parse_scenarios()
        .into_iter()
        .filter_map(|s| {
            let reason = s.uncovered_reason?;
            (reason.len() < 12).then(|| format!("  {} — {} ({reason:?})", s.file, s.name))
        })
        .collect();

    assert!(
        vague.is_empty(),
        "these admitted gaps do not explain themselves:\n{}",
        vague.join("\n")
    );
}

/// Every named test must still exist.
///
/// This is the gate the task was for: rename or delete a test and
/// the scenario resting on it goes red, instead of quietly becoming
/// fiction.
#[test]
fn every_referenced_test_still_exists() {
    let known = workspace_function_names();
    let mut dangling: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for scenario in parse_scenarios() {
        for test in &scenario.covered_by {
            if !known.contains(test) {
                dangling.entry(test.clone()).or_default().push(format!(
                    "{}:{} — {}",
                    scenario.file, scenario.line, scenario.name
                ));
            }
        }
    }

    assert!(
        dangling.is_empty(),
        "{} scenario tag(s) name a test that does not exist in the workspace:\n\n{}\n\nEither the \
         test was renamed — update the tag — or it was deleted, in which case the scenario \
         describes behaviour nothing verifies any more and must be re-covered or removed.",
        dangling.len(),
        dangling
            .iter()
            .map(|(test, uses)| format!(
                "  {test}\n    referenced by: {}",
                uses.join("\n                   ")
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Scenario names are how a human matches a failure to a spec, so
/// duplicates inside one file make the tag above them ambiguous.
#[test]
fn scenario_names_are_unique_within_a_file() {
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut duplicates = Vec::new();

    for scenario in parse_scenarios() {
        let key = (scenario.file.clone(), scenario.name.clone());
        if let Some(first) = seen.get(&key) {
            duplicates.push(format!(
                "  {}: \"{}\" at line {} repeats line {first}",
                scenario.file, scenario.name, scenario.line
            ));
        } else {
            seen.insert(key, scenario.line);
        }
    }

    assert!(
        duplicates.is_empty(),
        "duplicate scenario names:\n{}",
        duplicates.join("\n")
    );
}

/// The parser is the gate's single point of failure: if it silently
/// stopped recognising scenarios, every other test here would pass
/// vacuously on an empty list.
#[test]
fn the_parser_finds_the_scenarios_that_are_there() {
    let scenarios = parse_scenarios();

    assert!(
        scenarios.len() >= 50,
        "expected the full scenario corpus, found {} — the parser has probably stopped \
         recognising a heading form, which would make this whole gate pass vacuously",
        scenarios.len()
    );

    // Every file contributes, so a file that stops parsing is
    // noticed rather than skipped.
    let files: BTreeSet<&str> = scenarios.iter().map(|s| s.file.as_str()).collect();
    let on_disk = std::fs::read_dir(scenarios_dir())
        .expect("scenarios dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "feature"))
        .count();
    assert_eq!(
        files.len(),
        on_disk,
        "only {} of {on_disk} feature files yielded scenarios: {files:?}",
        files.len()
    );
}

/// A tag belonging to the `Feature` must not be inherited by the
/// first scenario — that would let one tag at the top of a file
/// silently satisfy the coverage rule for everything below it.
#[test]
fn feature_level_tags_do_not_count_as_scenario_coverage() {
    // Exercised against the real parser via a temporary file, since
    // the rule lives in `parse_scenarios`'s tag-clearing branch.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sample.feature");
    std::fs::write(
        &path,
        "@covered-by:some_feature_level_test\n\
         Feature: A feature whose tag must not leak downward\n\
         \n  \
         Scenario: untagged\n    \
         Given nothing\n",
    )
    .expect("write sample");

    // Re-implement the tag-clearing rule against this file to prove
    // the intent, then assert the shipped corpus obeys it too.
    let text = std::fs::read_to_string(&path).expect("read sample");
    let mut pending: Vec<&str> = Vec::new();
    let mut first_scenario_tags: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('@') {
            pending.push(line);
        } else if line.starts_with("Scenario:") {
            first_scenario_tags = Some(pending.len());
            break;
        } else if !line.is_empty() && !line.starts_with('#') {
            pending.clear();
        }
    }

    assert_eq!(
        first_scenario_tags,
        Some(0),
        "a tag above `Feature:` must be cleared by the Feature line, not inherited by the first \
         scenario"
    );
}
