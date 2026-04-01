//! E2E tests for format pipeline using GitHub fixtures.
//!
//! Demonstrates the pipeline's ability to:
//! - Transform JSON data to TOON (token savings)
//! - Truncate large outputs with pagination hints
//! - Provide agent-friendly output with context

use std::path::PathBuf;

use devboy_core::{FileDiff, Issue, MergeRequest};
use devboy_format_pipeline::{OutputFormat, Pipeline, PipelineConfig};

/// Load GitHub issues from fixtures.
fn load_github_issues() -> Vec<Issue> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("github")
        .join("issues.json");

    let content = std::fs::read_to_string(&path).expect("Failed to load issues fixture");
    serde_json::from_str(&content).expect("Failed to parse issues JSON")
}

/// Load GitHub pull requests from fixtures.
fn load_github_prs() -> Vec<MergeRequest> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("github")
        .join("pull_requests.json");

    let content = std::fs::read_to_string(&path).expect("Failed to load PRs fixture");
    serde_json::from_str(&content).expect("Failed to parse PRs JSON")
}

/// Create sample file diffs for testing.
fn sample_diffs() -> Vec<FileDiff> {
    vec![
        FileDiff {
            file_path: "src/main.rs".to_string(),
            old_path: None,
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            diff: r#"@@ -1,5 +1,7 @@
 fn main() {
-    println!("Hello");
+    // Improved greeting
+    println!("Hello, World!");
+    println!("Welcome to devboy-tools");
 }
"#
            .to_string(),
            additions: Some(3),
            deletions: Some(1),
        },
        FileDiff {
            file_path: "README.md".to_string(),
            old_path: None,
            new_file: true,
            deleted_file: false,
            renamed_file: false,
            diff: r#"@@ -0,0 +1,10 @@
+# DevBoy tools
+
+LLM-optimized developer tools.
"#
            .to_string(),
            additions: Some(10),
            deletions: Some(0),
        },
        FileDiff {
            file_path: "old_file.txt".to_string(),
            old_path: None,
            new_file: false,
            deleted_file: true,
            renamed_file: false,
            diff: r#"@@ -1,3 +0,0 @@
-This file is no longer needed.
-Goodbye!
"#
            .to_string(),
            additions: Some(0),
            deletions: Some(3),
        },
    ]
}

// ============================================================================
// Token Savings Tests
// ============================================================================

#[test]
fn test_json_vs_toon_token_savings() {
    let issues = load_github_issues();
    let json_output = serde_json::to_string_pretty(&issues).unwrap();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,
        max_chars: 100_000,
        ..Default::default()
    });

    let toon_output = pipeline.transform_issues(issues).unwrap();

    let json_len = json_output.len();
    let toon_len = toon_output.content.len();

    println!("=== Token Savings: Issues ===");
    println!("JSON: {} chars", json_len);
    println!("TOON: {} chars", toon_len);
    if toon_len < json_len {
        let savings = ((json_len - toon_len) as f64 / json_len as f64 * 100.0) as i32;
        println!("Savings: {}%", savings);
    }

    // TOON should be smaller than JSON
    assert!(toon_len < json_len, "TOON should be smaller than JSON");
}

#[test]
fn test_pull_requests_toon_output() {
    let prs = load_github_prs();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,
        max_chars: 100_000,
        ..Default::default()
    });

    let output = pipeline.transform_merge_requests(prs).unwrap();

    println!("=== Pull Requests TOON ===");
    println!("{}", output.content);

    // Verify key data is present
    assert!(output.content.contains("pr#5"));
}

// ============================================================================
// Truncation & Pagination Tests
// ============================================================================

#[test]
fn test_truncation_with_pagination_hints() {
    let issues = load_github_issues();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,
        max_chars: 300,
        include_hints: true,
        ..Default::default()
    });

    let output = pipeline.transform_issues(issues).unwrap();

    assert!(output.truncated, "Output should be marked as truncated");
    assert_eq!(output.total_count, Some(5));
    assert!(output.included_count < 5);

    let hint = output.agent_hint.as_ref().expect("Should have agent hint");
    assert!(hint.contains("trimmed by budget"));
}

#[test]
fn test_no_truncation_when_under_limit() {
    let issues = load_github_issues(); // 5 issues

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,
        max_chars: 100_000,
        include_hints: true,
        ..Default::default()
    });

    let output = pipeline.transform_issues(issues).unwrap();

    assert!(!output.truncated);
    assert!(output.agent_hint.is_none());
}

#[test]
fn test_character_limit_truncation() {
    let issues = load_github_issues();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,

        max_chars: 500,
        include_hints: true,
        ..Default::default()
    });

    let output = pipeline.transform_issues(issues).unwrap();

    assert!(output.content.len() <= 500);
    assert!(output.truncated);
}

// ============================================================================
// File Diffs Tests
// ============================================================================

#[test]
fn test_diffs_toon_output() {
    let diffs = sample_diffs();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,
        max_chars: 100_000,
        max_chars_per_item: 1000,
        ..Default::default()
    });

    let output = pipeline.transform_diffs(diffs).unwrap();

    println!("=== File Diffs TOON ===");
    println!("{}", output.content);

    assert!(output.content.contains("src/main.rs"));
    assert!(output.content.contains("README.md"));
    assert!(output.content.contains("old_file.txt"));
}

#[test]
fn test_diff_content_truncation() {
    let long_diff = (1..=100)
        .map(|i| format!("+Line {} with some content that makes it longer", i))
        .collect::<Vec<_>>()
        .join("\n");

    let diffs = vec![FileDiff {
        file_path: "large_file.rs".to_string(),
        old_path: None,
        new_file: false,
        deleted_file: false,
        renamed_file: false,
        diff: long_diff,
        additions: Some(100),
        deletions: Some(0),
    }];

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,
        max_chars: 100_000,
        max_chars_per_item: 200,
        ..Default::default()
    });

    let output = pipeline.transform_diffs(diffs).unwrap();
    assert!(
        output.content.contains("..."),
        "Long diff should be truncated"
    );
}

// ============================================================================
// Format Comparison Demo
// ============================================================================

#[test]
fn test_format_comparison_demo() {
    let issues = load_github_issues();

    println!("=== Format Pipeline Comparison Demo ===");
    println!();

    // JSON
    let json = serde_json::to_string_pretty(&issues).unwrap();
    println!("JSON: {} chars", json.len());

    // TOON
    let toon_pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Toon,

        max_chars: 100_000,
        ..Default::default()
    });
    let toon = toon_pipeline.transform_issues(issues).unwrap();
    println!("TOON: {} chars", toon.content.len());

    if toon.content.len() < json.len() {
        println!(
            "Savings: {:.0}%",
            (1.0 - toon.content.len() as f64 / json.len() as f64) * 100.0
        );
    }

    println!();
    println!("--- TOON Output ---");
    println!("{}", &toon.content[..500.min(toon.content.len())]);
}
