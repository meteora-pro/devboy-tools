//! E2E tests for pipeline transformations using GitHub fixtures.
//!
//! These tests demonstrate the pipeline's ability to:
//! - Transform JSON data to Markdown (token savings)
//! - Truncate large outputs with pagination hints
//! - Provide agent-friendly output with context
//!
//! # Example Output Comparison
//!
//! ```text
//! JSON (5 issues):  2847 chars
//! Markdown:          892 chars (69% reduction)
//! Compact:           312 chars (89% reduction)
//! ```

use std::path::PathBuf;

use devboy_core::{FileDiff, Issue, MergeRequest};
use devboy_pipeline::{OutputFormat, Pipeline, PipelineConfig};

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
+# DevBoy Tools
+
+LLM-optimized developer tools.
+
+## Features
+
+- Issue tracking
+- MR/PR management
+- Pipeline transforms
+"#
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
-It was used for testing.
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
fn test_json_vs_markdown_token_savings() {
    let issues = load_github_issues();
    let json_output = serde_json::to_string_pretty(&issues).unwrap();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Markdown,
        max_items: 100,
        max_chars: 100000,
        ..Default::default()
    });

    let markdown_output = pipeline.transform_issues(issues.clone()).unwrap();

    let json_len = json_output.len();
    let md_len = markdown_output.content.len();
    let savings = ((json_len - md_len) as f64 / json_len as f64 * 100.0) as i32;

    println!("=== Token Savings: Issues ===");
    println!("JSON:     {} chars", json_len);
    println!("Markdown: {} chars", md_len);
    println!("Savings:  {}%", savings);
    println!();
    println!("--- Markdown Output ---");
    println!("{}", markdown_output.content);

    // Markdown should be at least 30% smaller than JSON
    assert!(md_len < json_len, "Markdown should be smaller than JSON");
    assert!(
        savings >= 30,
        "Expected at least 30% savings, got {}%",
        savings
    );
}

#[test]
fn test_compact_format_maximum_savings() {
    let issues = load_github_issues();
    let json_output = serde_json::to_string_pretty(&issues).unwrap();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Compact,
        max_items: 100,
        max_chars: 100000,
        ..Default::default()
    });

    let compact_output = pipeline.transform_issues(issues).unwrap();

    let json_len = json_output.len();
    let compact_len = compact_output.content.len();
    let savings = ((json_len - compact_len) as f64 / json_len as f64 * 100.0) as i32;

    println!("=== Maximum Savings: Compact Format ===");
    println!("JSON:    {} chars", json_len);
    println!("Compact: {} chars", compact_len);
    println!("Savings: {}%", savings);
    println!();
    println!("--- Compact Output ---");
    println!("{}", compact_output.content);

    // Compact should be at least 70% smaller than JSON
    assert!(
        savings >= 70,
        "Expected at least 70% savings, got {}%",
        savings
    );
}

#[test]
fn test_pull_requests_markdown_output() {
    let prs = load_github_prs();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Markdown,
        max_items: 100,
        max_chars: 100000,
        ..Default::default()
    });

    let output = pipeline.transform_merge_requests(prs).unwrap();

    println!("=== Pull Requests Markdown ===");
    println!("{}", output.content);

    // Verify structure
    assert!(output.content.contains("# Merge Requests"));
    assert!(output.content.contains("## pr#5"));
    assert!(output.content.contains("**Branch:**"));
    assert!(output
        .content
        .contains("`andreymaznyakthailand-bot-patch-1` → `main`"));
}

// ============================================================================
// Truncation & Pagination Tests
// ============================================================================

#[test]
fn test_truncation_with_pagination_hints() {
    let issues = load_github_issues();

    // Limit to 2 items
    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Markdown,
        max_items: 2,
        max_chars: 100000,
        include_hints: true,
        ..Default::default()
    });

    let output = pipeline.transform_issues(issues).unwrap();

    println!("=== Truncation with Pagination Hints ===");
    println!("{}", output.to_string_with_hints());

    assert!(output.truncated, "Output should be marked as truncated");
    assert_eq!(
        output.total_count,
        Some(5),
        "Should report total of 5 issues"
    );
    assert_eq!(output.included_count, 2, "Should include only 2 issues");

    // Check agent hint
    let hint = output.agent_hint.as_ref().expect("Should have agent hint");
    assert!(hint.contains("2/5"), "Hint should show 2/5");
    assert!(
        hint.contains("3 more"),
        "Hint should mention 3 more available"
    );
    assert!(
        hint.contains("offset"),
        "Hint should mention offset parameter"
    );
}

#[test]
fn test_no_truncation_when_under_limit() {
    let issues = load_github_issues(); // 5 issues

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Markdown,
        max_items: 10, // More than we have
        max_chars: 100000,
        include_hints: true,
        ..Default::default()
    });

    let output = pipeline.transform_issues(issues).unwrap();

    assert!(!output.truncated, "Output should not be truncated");
    assert!(output.agent_hint.is_none(), "Should not have agent hint");
}

#[test]
fn test_character_limit_truncation() {
    let issues = load_github_issues();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Markdown,
        max_items: 100,
        max_chars: 500, // Very small limit
        include_hints: true,
        ..Default::default()
    });

    let output = pipeline.transform_issues(issues).unwrap();

    println!("=== Character Limit Truncation ===");
    println!("Output length: {} chars", output.content.len());
    println!("{}", output.content);

    assert!(
        output.content.len() <= 500,
        "Output should respect character limit"
    );
    assert!(output.truncated, "Should be marked as truncated");
}

// ============================================================================
// File Diffs Tests
// ============================================================================

#[test]
fn test_diffs_markdown_output() {
    let diffs = sample_diffs();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Markdown,
        max_items: 100,
        max_chars: 100000,
        max_chars_per_item: 1000,
        ..Default::default()
    });

    let output = pipeline.transform_diffs(diffs).unwrap();

    println!("=== File Diffs Markdown ===");
    println!("{}", output.content);

    // Verify structure
    assert!(output.content.contains("# Changed Files"));
    assert!(output.content.contains("✏️ src/main.rs")); // Modified
    assert!(output.content.contains("➕ README.md")); // New file
    assert!(output.content.contains("➖ old_file.txt")); // Deleted
    assert!(output.content.contains("```diff")); // Code blocks
    assert!(output.content.contains("+3 -1")); // Stats
}

#[test]
fn test_diffs_compact_output() {
    let diffs = sample_diffs();

    let pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Compact,
        ..Default::default()
    });

    let output = pipeline.transform_diffs(diffs).unwrap();

    println!("=== File Diffs Compact ===");
    println!("{}", output.content);

    // Compact should be one line per file
    let lines: Vec<&str> = output.content.lines().collect();
    assert_eq!(lines.len(), 3, "Should have 3 lines for 3 diffs");
    assert!(lines[0].contains("[M]")); // Modified
    assert!(lines[1].contains("[A]")); // Added
    assert!(lines[2].contains("[D]")); // Deleted
}

#[test]
fn test_diff_content_truncation() {
    // Create a diff with very long content
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
        format: OutputFormat::Markdown,
        max_items: 100,
        max_chars: 100000,
        max_chars_per_item: 200, // Limit per diff
        ..Default::default()
    });

    let output = pipeline.transform_diffs(diffs).unwrap();

    println!("=== Diff Content Truncation ===");
    println!("{}", output.content);

    // The diff content should be truncated
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

    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║           Pipeline Output Format Comparison Demo                  ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();

    // JSON
    let json = serde_json::to_string_pretty(&issues).unwrap();
    println!("━━━ JSON Format ({} chars) ━━━", json.len());
    println!("{}", &json[..500.min(json.len())]);
    if json.len() > 500 {
        println!("... [truncated for demo]");
    }
    println!();

    // Markdown
    let md_pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Markdown,
        max_items: 3,
        ..Default::default()
    });
    let md = md_pipeline.transform_issues(issues.clone()).unwrap();
    println!("━━━ Markdown Format ({} chars) ━━━", md.content.len());
    println!("{}", md.to_string_with_hints());
    println!();

    // Compact
    let compact_pipeline = Pipeline::with_config(PipelineConfig {
        format: OutputFormat::Compact,
        max_items: 5,
        ..Default::default()
    });
    let compact = compact_pipeline.transform_issues(issues.clone()).unwrap();
    println!("━━━ Compact Format ({} chars) ━━━", compact.content.len());
    println!("{}", compact.content);
    println!();

    // Summary
    println!("━━━ Summary ━━━");
    println!("JSON:     {} chars (baseline)", json.len());
    println!(
        "Markdown: {} chars ({:.0}% of JSON)",
        md.content.len(),
        md.content.len() as f64 / json.len() as f64 * 100.0
    );
    println!(
        "Compact:  {} chars ({:.0}% of JSON)",
        compact.content.len(),
        compact.content.len() as f64 / json.len() as f64 * 100.0
    );
}

// ============================================================================
// Agent Hints Demo
// ============================================================================

#[test]
fn test_agent_pagination_hints_demo() {
    let issues = load_github_issues();

    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║                Agent Pagination Hints Demo                        ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();

    // Simulate paginated requests
    for (offset, limit) in [(0, 2), (2, 2), (4, 2)] {
        let page_issues: Vec<Issue> = issues.iter().skip(offset).take(limit).cloned().collect();
        let total = issues.len();

        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Compact,
            max_items: limit,
            include_hints: true,
            ..Default::default()
        });

        let mut output = pipeline.transform_issues(page_issues).unwrap();

        // Manually set the total for demo (normally this would come from API)
        if output.included_count < total {
            let remaining = total - offset - output.included_count;
            if remaining > 0 {
                output.truncated = true;
                output.total_count = Some(total);
                output.agent_hint = Some(format!(
                    "📊 Showing {}-{} of {} issues. {} more available. Use `offset={}` for next page.",
                    offset + 1,
                    offset + output.included_count,
                    total,
                    remaining,
                    offset + output.included_count
                ));
            }
        }

        println!("━━━ Page: offset={}, limit={} ━━━", offset, limit);
        println!("{}", output.to_string_with_hints());
        println!();
    }
}
