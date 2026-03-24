use crate::doctor::{CheckResult, CheckStatus};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn print_report(results: &[CheckResult], verbose: bool) {
    println!("DevBoy Doctor - Diagnostic Report");
    println!("=================================");

    let mut grouped: BTreeMap<&str, Vec<&CheckResult>> = BTreeMap::new();
    for result in results {
        grouped
            .entry(result.category.as_str())
            .or_default()
            .push(result);
    }

    for category in ["Environment", "Configuration"] {
        if let Some(checks) = grouped.remove(category) {
            println!();
            println!("{category}");
            for result in checks {
                println!("  {} {}", status_label(result.status), result.message);

                if let Some(fix_command) = &result.fix_command {
                    println!("     Run: {fix_command}");
                }

                if verbose {
                    println!("     Check: {}", result.id);
                    if let Some(details) = &result.details {
                        print_details(details, 5);
                    }
                }
            }
        }
    }

    for (category, checks) in grouped {
        println!();
        println!("{category}");
        for result in checks {
            println!("  {} {}", status_label(result.status), result.message);

            if let Some(fix_command) = &result.fix_command {
                println!("     Run: {fix_command}");
            }

            if verbose {
                println!("     Check: {}", result.id);
                if let Some(details) = &result.details {
                    print_details(details, 5);
                }
            }
        }
    }

    let summary = summarize(results);
    println!();
    println!(
        "Summary: {} error(s), {} warning(s), {} passed, {} skipped",
        summary.errors, summary.warnings, summary.passed, summary.skipped
    );
}

fn print_details(value: &Value, indent: usize) {
    let prefix = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                match value {
                    Value::Null => {}
                    Value::Object(_) | Value::Array(_) => {
                        println!("{prefix}{key}:");
                        print_details(value, indent + 2);
                    }
                    _ => println!("{prefix}{key}: {}", scalar_to_string(value)),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        println!("{prefix}-");
                        print_details(item, indent + 2);
                    }
                    _ => println!("{prefix}- {}", scalar_to_string(item)),
                }
            }
        }
        _ => println!("{prefix}{}", scalar_to_string(value)),
    }
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

fn status_label(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "[PASS]",
        CheckStatus::Warning => "[WARN]",
        CheckStatus::Error => "[ERR]",
        CheckStatus::Skipped => "[SKIP]",
    }
}

pub struct Summary {
    pub passed: usize,
    pub warnings: usize,
    pub errors: usize,
    pub skipped: usize,
}

pub fn summarize(results: &[CheckResult]) -> Summary {
    let mut summary = Summary {
        passed: 0,
        warnings: 0,
        errors: 0,
        skipped: 0,
    };

    for result in results {
        match result.status {
            CheckStatus::Pass => summary.passed += 1,
            CheckStatus::Warning => summary.warnings += 1,
            CheckStatus::Error => summary.errors += 1,
            CheckStatus::Skipped => summary.skipped += 1,
        }
    }

    summary
}
