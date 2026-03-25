use crate::doctor::{CheckDescriptor, CheckResult, CheckStatus};
use crate::update_check::VersionStatus;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, Write};

pub fn print_report(version: &VersionStatus, results: &[CheckResult], verbose: bool) {
    let mut stdout = io::stdout();
    write_report(&mut stdout, version, results, verbose)
        .expect("writing doctor report to stdout should succeed");
}

pub fn print_check_list(checks: &[CheckDescriptor]) {
    let mut stdout = io::stdout();
    write_check_list(&mut stdout, checks)
        .expect("writing doctor check list to stdout should succeed");
}

fn write_report<W: Write>(
    writer: &mut W,
    version: &VersionStatus,
    results: &[CheckResult],
    verbose: bool,
) -> io::Result<()> {
    writeln!(writer, "DevBoy Doctor - Diagnostic Report")?;
    writeln!(writer, "=================================")?;
    writeln!(writer)?;
    writeln!(writer, "Version")?;
    writeln!(writer, "  Current: {}", version.current_version)?;

    if version.update_available {
        if let Some(latest_version) = &version.latest_version {
            writeln!(writer, "  Latest: {}", latest_version)?;
        }
        writeln!(writer, "  Status: Update available")?;
        writeln!(writer, "  Update with: {}", version.update_command)?;
    } else if let Some(latest_version) = &version.latest_version {
        writeln!(writer, "  Latest: {}", latest_version)?;
        writeln!(writer, "  Status: Up to date")?;
    } else {
        writeln!(writer, "  Latest: unavailable")?;
        writeln!(writer, "  Status: Unable to check")?;
    }

    let mut result_groups: BTreeMap<&str, Vec<&CheckResult>> = BTreeMap::new();
    for result in results {
        result_groups
            .entry(result.category.as_str())
            .or_default()
            .push(result);
    }

    for category in ["Environment", "Configuration"] {
        if let Some(checks) = result_groups.remove(category) {
            writeln!(writer)?;
            writeln!(writer, "{category}")?;
            for result in checks {
                writeln!(
                    writer,
                    "  {} {}",
                    status_label(result.status),
                    result.message
                )?;

                if let Some(fix_command) = &result.fix_command {
                    writeln!(writer, "     Run: {fix_command}")?;
                }

                if verbose {
                    writeln!(writer, "     Check: {}", result.id)?;
                    if let Some(details) = &result.details {
                        write_details(writer, details, 5)?;
                    }
                }
            }
        }
    }

    for (category, checks) in result_groups {
        writeln!(writer)?;
        writeln!(writer, "{category}")?;
        for result in checks {
            writeln!(
                writer,
                "  {} {}",
                status_label(result.status),
                result.message
            )?;

            if let Some(fix_command) = &result.fix_command {
                writeln!(writer, "     Run: {fix_command}")?;
            }

            if verbose {
                writeln!(writer, "     Check: {}", result.id)?;
                if let Some(details) = &result.details {
                    write_details(writer, details, 5)?;
                }
            }
        }
    }

    let summary = summarize(results);
    writeln!(writer)?;
    writeln!(
        writer,
        "Summary: {} error(s), {} warning(s), {} passed, {} skipped",
        summary.errors, summary.warnings, summary.passed, summary.skipped
    )?;

    Ok(())
}

fn write_check_list<W: Write>(writer: &mut W, checks: &[CheckDescriptor]) -> io::Result<()> {
    writeln!(writer, "Available doctor checks")?;
    writeln!(writer, "=======================")?;

    let mut grouped: BTreeMap<&str, Vec<&CheckDescriptor>> = BTreeMap::new();
    for check in checks {
        grouped
            .entry(check.category.as_str())
            .or_default()
            .push(check);
    }

    for (category, items) in grouped {
        writeln!(writer)?;
        writeln!(writer, "{category}")?;
        for check in items {
            writeln!(writer, "  {} - {}", check.id, check.name)?;
        }
    }

    Ok(())
}

fn write_details<W: Write>(writer: &mut W, value: &Value, indent: usize) -> io::Result<()> {
    let prefix = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                match value {
                    Value::Null => {}
                    Value::Object(_) | Value::Array(_) => {
                        writeln!(writer, "{prefix}{key}:")?;
                        write_details(writer, value, indent + 2)?;
                    }
                    _ => writeln!(writer, "{prefix}{key}: {}", scalar_to_string(value))?,
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        writeln!(writer, "{prefix}-")?;
                        write_details(writer, item, indent + 2)?;
                    }
                    _ => writeln!(writer, "{prefix}- {}", scalar_to_string(item))?,
                }
            }
        }
        _ => writeln!(writer, "{prefix}{}", scalar_to_string(value))?,
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_check::VersionStatus;
    use serde_json::json;

    fn sample_result(status: CheckStatus) -> CheckResult {
        CheckResult {
            id: "config.exists".to_string(),
            category: "Configuration".to_string(),
            name: "Config file exists".to_string(),
            status,
            message: "Config file found".to_string(),
            details: Some(json!({
                "path": ".devboy.toml",
                "items": ["one", {"nested": true}],
            })),
            fix_command: Some("devboy init".to_string()),
            fix_url: None,
        }
    }

    fn sample_version(update_available: bool, latest_version: Option<&str>) -> VersionStatus {
        VersionStatus {
            current_version: "0.10.0".to_string(),
            latest_version: latest_version.map(ToString::to_string),
            update_available,
            install_method: "standalone".to_string(),
            update_command: "devboy upgrade".to_string(),
        }
    }

    #[test]
    fn write_report_renders_verbose_output() {
        let mut buffer = Vec::new();
        let version = sample_version(true, Some("0.11.0"));
        let results = vec![
            CheckResult {
                category: "Environment".to_string(),
                ..sample_result(CheckStatus::Pass)
            },
            sample_result(CheckStatus::Warning),
        ];

        write_report(&mut buffer, &version, &results, true).unwrap();
        let output = String::from_utf8(buffer).unwrap();

        assert!(output.contains("DevBoy Doctor - Diagnostic Report"));
        assert!(output.contains("Version"));
        assert!(output.contains("Current: 0.10.0"));
        assert!(output.contains("Latest: 0.11.0"));
        assert!(output.contains("Status: Update available"));
        assert!(output.contains("Update with: devboy upgrade"));
        assert!(output.contains("Environment"));
        assert!(output.contains("Configuration"));
        assert!(output.contains("[PASS] Config file found"));
        assert!(output.contains("[WARN] Config file found"));
        assert!(output.contains("Run: devboy init"));
        assert!(output.contains("Check: config.exists"));
        assert!(output.contains("nested: true"));
        assert!(output.contains("Summary: 0 error(s), 1 warning(s), 1 passed, 0 skipped"));
    }

    #[test]
    fn write_report_shows_unavailable_latest_version() {
        let mut buffer = Vec::new();
        let version = sample_version(false, None);

        write_report(
            &mut buffer,
            &version,
            &[sample_result(CheckStatus::Pass)],
            false,
        )
        .unwrap();
        let output = String::from_utf8(buffer).unwrap();

        assert!(output.contains("Current: 0.10.0"));
        assert!(output.contains("Latest: unavailable"));
        assert!(output.contains("Status: Unable to check"));
    }

    #[test]
    fn write_check_list_groups_checks() {
        let mut buffer = Vec::new();
        let checks = vec![
            CheckDescriptor {
                id: "config.exists".to_string(),
                category: "Configuration".to_string(),
                name: "Config file exists".to_string(),
            },
            CheckDescriptor {
                id: "environment.os_support".to_string(),
                category: "Environment".to_string(),
                name: "Operating system supported".to_string(),
            },
        ];

        write_check_list(&mut buffer, &checks).unwrap();
        let output = String::from_utf8(buffer).unwrap();

        assert!(output.contains("Available doctor checks"));
        assert!(output.contains("Configuration"));
        assert!(output.contains("Environment"));
        assert!(output.contains("config.exists - Config file exists"));
        assert!(output.contains("environment.os_support - Operating system supported"));
    }

    #[test]
    fn write_details_handles_scalars_and_arrays() {
        let mut buffer = Vec::new();
        write_details(&mut buffer, &json!(["text", 2, {"ok": false}]), 2).unwrap();
        let output = String::from_utf8(buffer).unwrap();

        assert!(output.contains("  - text"));
        assert!(output.contains("  - 2"));
        assert!(output.contains("  -"));
        assert!(output.contains("    ok: false"));
    }

    #[test]
    fn scalar_to_string_formats_strings_and_non_strings() {
        assert_eq!(scalar_to_string(&json!("text")), "text");
        assert_eq!(scalar_to_string(&json!(42)), "42");
    }

    #[test]
    fn status_label_maps_all_statuses() {
        assert_eq!(status_label(CheckStatus::Pass), "[PASS]");
        assert_eq!(status_label(CheckStatus::Warning), "[WARN]");
        assert_eq!(status_label(CheckStatus::Error), "[ERR]");
        assert_eq!(status_label(CheckStatus::Skipped), "[SKIP]");
    }

    #[test]
    fn summarize_counts_each_status() {
        let summary = summarize(&[
            sample_result(CheckStatus::Pass),
            sample_result(CheckStatus::Warning),
            sample_result(CheckStatus::Error),
            sample_result(CheckStatus::Skipped),
        ]);

        assert_eq!(summary.passed, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.skipped, 1);
    }
}
