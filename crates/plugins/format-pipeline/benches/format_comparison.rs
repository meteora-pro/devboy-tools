//! Benchmark: JSON vs TOON format comparison.
//!
//! Measures token savings, encoding latency, and pagination efficiency
//! across different data types and sizes.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use devboy_core::{Comment, Discussion, FileDiff, Issue, User};
use devboy_format_pipeline::token_counter::estimate_tokens;
use devboy_format_pipeline::toon::{self, TrimLevel};
use devboy_format_pipeline::{OutputFormat, Pipeline, PipelineConfig};

// ============================================================================
// Data generators
// ============================================================================

fn generate_issues(n: usize) -> Vec<Issue> {
    (0..n)
        .map(|i| Issue {
            key: format!("gh#{}", i + 1),
            title: format!("Issue {}: implement feature for module {}", i + 1, i % 10),
            description: Some(format!(
                "This issue tracks the implementation of feature {} for module {}. \
                 Requirements include: proper error handling, unit tests, \
                 documentation updates, and backward compatibility. \
                 Priority is {} and affects {} components.",
                i + 1,
                i % 10,
                ["high", "medium", "low"][i % 3],
                i % 5 + 1
            )),
            state: ["open", "closed"][i % 2].into(),
            source: "github".into(),
            priority: Some(["high", "medium", "low"][i % 3].into()),
            labels: vec![
                ["bug", "feature", "enhancement"][i % 3].into(),
                ["backend", "frontend", "infra"][i % 3].into(),
            ],
            author: Some(User {
                id: format!("{}", i % 5),
                username: format!("user{}", i % 5),
                name: Some(format!("User {}", i % 5)),
                email: Some(format!("user{}@example.com", i % 5)),
                avatar_url: Some("https://example.com/avatar.png".into()),
            }),
            assignees: vec![User {
                id: format!("{}", (i + 1) % 5),
                username: format!("user{}", (i + 1) % 5),
                name: None,
                email: None,
                avatar_url: None,
            }],
            url: Some(format!("https://github.com/org/repo/issues/{}", i + 1)),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-06-15T12:00:00Z".into()),
            attachments_count: None,
            parent: None,
            subtasks: vec![],
            custom_fields: std::collections::HashMap::new(),
            ..Default::default()
        })
        .collect()
}

fn generate_diffs(n: usize) -> Vec<FileDiff> {
    let file_types = [
        ("src/main.rs", false, false),
        ("Cargo.lock", false, false),
        ("tests/test_main.rs", false, false),
        ("README.md", true, false),
        ("old_module.rs", false, true),
    ];

    (0..n)
        .map(|i| {
            let (path, new_file, deleted) = file_types[i % file_types.len()];
            FileDiff {
                file_path: format!(
                    "{}{}",
                    if i >= file_types.len() {
                        format!("pkg{}/", i / file_types.len())
                    } else {
                        String::new()
                    },
                    path
                ),
                old_path: None,
                new_file,
                deleted_file: deleted,
                renamed_file: false,
                diff: format!(
                    "@@ -{},{} +{},{} @@\n{}\n{}",
                    i * 10 + 1,
                    5 + i % 3,
                    i * 10 + 1,
                    7 + i % 3,
                    (0..5)
                        .map(|j| format!("-old line {} in file {}", j, i))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    (0..7)
                        .map(|j| format!("+new line {} in file {}", j, i))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                additions: Some(7 + i as u32 % 3),
                deletions: Some(5 + i as u32 % 3),
            }
        })
        .collect()
}

fn generate_discussions(n: usize) -> Vec<Discussion> {
    (0..n)
        .map(|i| Discussion {
            id: format!("d{}", i),
            resolved: i % 3 == 0,
            resolved_by: None,
            comments: (0..3 + i % 4)
                .map(|j| Comment {
                    id: format!("c{}_{}", i, j),
                    body: format!(
                        "Comment {} on discussion {}: {}",
                        j,
                        i,
                        if j == 0 {
                            "Initial review comment with detailed feedback about the implementation."
                        } else {
                            "Reply addressing the feedback."
                        }
                    ),
                    author: Some(User {
                        id: format!("{}", j % 3),
                        username: format!("reviewer{}", j % 3),
                        name: None,
                        email: None,
                        avatar_url: None,
                    }),
                    created_at: Some("2024-03-15T10:00:00Z".into()),
                    updated_at: None,
                    position: None,
                })
                .collect(),
            position: None,
        })
        .collect()
}

// ============================================================================
// Benchmarks
// ============================================================================

fn bench_encode_issues(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode_issues");

    for size in [5, 20, 50, 100] {
        let issues = generate_issues(size);

        group.bench_with_input(BenchmarkId::new("json", size), &issues, |b, issues| {
            b.iter(|| serde_json::to_string_pretty(black_box(issues)).unwrap())
        });

        group.bench_with_input(BenchmarkId::new("toon_full", size), &issues, |b, issues| {
            b.iter(|| toon::encode_issues(black_box(issues), TrimLevel::Full).unwrap())
        });

        group.bench_with_input(
            BenchmarkId::new("toon_minimal", size),
            &issues,
            |b, issues| {
                b.iter(|| toon::encode_issues(black_box(issues), TrimLevel::Minimal).unwrap())
            },
        );
    }

    group.finish();
}

fn bench_encode_diffs(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode_diffs");

    for size in [5, 20, 50] {
        let diffs = generate_diffs(size);

        group.bench_with_input(BenchmarkId::new("json", size), &diffs, |b, diffs| {
            b.iter(|| serde_json::to_string_pretty(black_box(diffs)).unwrap())
        });

        group.bench_with_input(BenchmarkId::new("toon", size), &diffs, |b, diffs| {
            b.iter(|| toon::encode_diffs(black_box(diffs)).unwrap())
        });
    }

    group.finish();
}

fn bench_pipeline_transform(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline_transform");

    for size in [5, 25, 100] {
        let issues = generate_issues(size);

        group.bench_with_input(
            BenchmarkId::new("json_pipeline", size),
            &issues,
            |b, issues| {
                let pipeline = Pipeline::with_config(PipelineConfig {
                    format: OutputFormat::Json,
                    max_chars: 1_000_000,
                    ..Default::default()
                });
                b.iter(|| {
                    pipeline
                        .transform_issues(black_box(issues.clone()))
                        .unwrap()
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("toon_pipeline", size),
            &issues,
            |b, issues| {
                let pipeline = Pipeline::with_config(PipelineConfig {
                    format: OutputFormat::Toon,
                    max_chars: 1_000_000,
                    ..Default::default()
                });
                b.iter(|| {
                    pipeline
                        .transform_issues(black_box(issues.clone()))
                        .unwrap()
                })
            },
        );
    }

    group.finish();
}

fn bench_token_savings_report(c: &mut Criterion) {
    // Not a real benchmark — prints a comparison report
    c.bench_function("token_savings_report", |b| {
        b.iter(|| {
            println!("\n{}", "=".repeat(60));
            println!("Format Comparison Report (budget=8000 tokens)");
            println!("{}\n", "=".repeat(60));

            let configs: Vec<(&str, usize)> = vec![
                ("Issues (5)", 5),
                ("Issues (25)", 25),
                ("Issues (100)", 100),
            ];

            for (label, size) in &configs {
                let issues = generate_issues(*size);
                let json = serde_json::to_string_pretty(&issues).unwrap();
                let toon_full = toon::encode_issues(&issues, TrimLevel::Full).unwrap();
                let toon_min = toon::encode_issues(&issues, TrimLevel::Minimal).unwrap();

                let json_tokens = estimate_tokens(&json);
                let toon_tokens = estimate_tokens(&toon_full);
                let min_tokens = estimate_tokens(&toon_min);

                let savings = if json_tokens > 0 {
                    (1.0 - toon_tokens as f64 / json_tokens as f64) * 100.0
                } else {
                    0.0
                };

                let budget = 8000usize;
                let json_pages = json_tokens.div_ceil(budget);
                let toon_pages = toon_tokens.div_ceil(budget);

                println!(
                    "{:<15} JSON: {:>6} tok ({} pgs) | TOON: {:>6} tok ({} pgs) | Min: {:>6} tok | Savings: {:.0}%",
                    label, json_tokens, json_pages, toon_tokens, toon_pages, min_tokens, savings
                );
            }

            // Diffs
            for size in [10, 50] {
                let diffs = generate_diffs(size);
                let json = serde_json::to_string_pretty(&diffs).unwrap();
                let toon_out = toon::encode_diffs(&diffs).unwrap();
                let json_tokens = estimate_tokens(&json);
                let toon_tokens = estimate_tokens(&toon_out);
                let savings = if json_tokens > 0 {
                    (1.0 - toon_tokens as f64 / json_tokens as f64) * 100.0
                } else {
                    0.0
                };
                println!(
                    "Diffs ({:<3})     JSON: {:>6} tok | TOON: {:>6} tok | Savings: {:.0}%",
                    size, json_tokens, toon_tokens, savings
                );
            }

            // Discussions
            for size in [10, 30] {
                let discussions = generate_discussions(size);
                let json = serde_json::to_string_pretty(&discussions).unwrap();
                let toon_out = toon::encode_discussions(&discussions).unwrap();
                let json_tokens = estimate_tokens(&json);
                let toon_tokens = estimate_tokens(&toon_out);
                let savings = if json_tokens > 0 {
                    (1.0 - toon_tokens as f64 / json_tokens as f64) * 100.0
                } else {
                    0.0
                };
                println!(
                    "Discussions ({:<2}) JSON: {:>6} tok | TOON: {:>6} tok | Savings: {:.0}%",
                    size, json_tokens, toon_tokens, savings
                );
            }

            println!();
        })
    });
}

criterion_group!(
    benches,
    bench_encode_issues,
    bench_encode_diffs,
    bench_pipeline_transform,
    bench_token_savings_report,
);
criterion_main!(benches);
