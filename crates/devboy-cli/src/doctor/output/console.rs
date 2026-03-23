use crate::doctor::{CheckResult, CheckStatus};
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
                        println!("     Details: {}", details);
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
                    println!("     Details: {}", details);
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
