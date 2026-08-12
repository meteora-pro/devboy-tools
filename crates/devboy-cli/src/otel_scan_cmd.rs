//! `devboy otel scan` command family.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use devboy_otel_scan::{Finding, ScanReport, Scanner, scan_jsonl};
use devboy_secret_patterns::Catalogue;
use serde::Serialize;

/// `devboy otel <subcommand>`.
#[derive(Subcommand)]
pub enum OtelCommands {
    /// Scan a JSONL OpenTelemetry artifact for leaked secrets.
    Scan(ScanArgs),
}

/// Arguments for `devboy otel scan`.
#[derive(Args)]
pub struct ScanArgs {
    /// JSONL file or directory to scan. Use `jsonl:<path>` to force JSONL,
    /// or `-` to read JSONL from standard input.
    #[arg(long)]
    input: String,

    /// Input format. `auto` detects `.jsonl` files and directories of them.
    #[arg(long, value_enum, default_value_t = ScanFormat::Auto)]
    format: ScanFormat,

    /// Report format.
    #[arg(long, value_enum, default_value_t = ScanOutput::Text)]
    output: ScanOutput,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ScanFormat {
    Auto,
    Jsonl,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ScanOutput {
    Text,
    Json,
}

/// Executes an OTEL command and returns its documented process exit code.
pub fn handle(command: OtelCommands) -> Result<i32> {
    match command {
        OtelCommands::Scan(args) => scan(args),
    }
}

fn scan(args: ScanArgs) -> Result<i32> {
    let catalogue = Catalogue::builtins_only();
    let scanner = Scanner::new(&catalogue);
    let report = match scan_input(&scanner, &args.input, args.format) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("scan error: {error}");
            return Ok(3);
        }
    };

    match args.output {
        ScanOutput::Text => print_text(&args.input, &report),
        ScanOutput::Json => print_json(&report)?,
    }
    Ok(exit_code(&report))
}

fn scan_input(
    scanner: &Scanner<'_>,
    input: &str,
    format: ScanFormat,
) -> Result<ScanReport, String> {
    if input == "-" {
        return scan_jsonl(scanner, "stdin", BufReader::new(io::stdin().lock()))
            .map_err(|error| error.to_string());
    }

    let (forced_jsonl, raw_path) = input
        .strip_prefix("jsonl:")
        .map_or((false, input), |path| (true, path));
    let path = Path::new(raw_path);
    let metadata = fs::metadata(path).map_err(|_| format!("could not open input '{raw_path}'"))?;
    if metadata.is_file() {
        if forced_jsonl || matches!(format, ScanFormat::Jsonl) || is_jsonl(path) {
            return scan_jsonl_file(scanner, path);
        }
        return Err(format!(
            "could not detect a supported format for '{raw_path}'"
        ));
    }
    if metadata.is_dir() {
        let mut report = ScanReport::default();
        let files = collect_jsonl_files(path)?;
        if files.is_empty() {
            return Err(format!("no .jsonl files found under '{raw_path}'"));
        }
        for file in files {
            report.extend(scan_jsonl_file(scanner, &file)?);
        }
        return Ok(report);
    }
    Err(format!(
        "input '{raw_path}' is not a regular file or directory"
    ))
}

fn scan_jsonl_file(scanner: &Scanner<'_>, path: &Path) -> Result<ScanReport, String> {
    let file =
        File::open(path).map_err(|_| format!("could not open input '{}'", path.display()))?;
    scan_jsonl(scanner, path.display().to_string(), BufReader::new(file))
        .map_err(|error| error.to_string())
}

fn collect_jsonl_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_jsonl_files_at(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_jsonl_files_at(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|_| format!("could not read directory '{}'", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|_| format!("could not read directory '{}'", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|_| format!("could not inspect '{}'", path.display()))?;
        if file_type.is_dir() {
            collect_jsonl_files_at(&path, files)?;
        } else if file_type.is_file() && is_jsonl(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
}

fn print_text(input: &str, report: &ScanReport) {
    println!("Scanning: {input}");
    println!("{}", "=".repeat(32));
    for finding in &report.findings {
        let line = finding
            .line
            .map_or_else(|| "?".to_owned(), |line| line.to_string());
        println!(
            "[{}] line {}, attribute={}: {} ({})",
            severity_label(finding),
            line,
            finding.attribute_path,
            finding.display_name,
            finding.match_redacted,
        );
    }
    println!("\nSummary:");
    println!("  Records:  {}", report.summary.records);
    println!(
        "  Findings: {} HIGH, {} MEDIUM, {} LOW",
        report.summary.high, report.summary.medium, report.summary.low
    );
}

fn print_json(report: &ScanReport) -> Result<()> {
    #[derive(Serialize)]
    struct JsonReport<'a> {
        scan_summary: &'a devboy_otel_scan::ScanSummary,
        findings: &'a [Finding],
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&JsonReport {
            scan_summary: &report.summary,
            findings: &report.findings
        })?
    );
    Ok(())
}

fn severity_label(finding: &Finding) -> &'static str {
    match finding.severity {
        devboy_secret_patterns::Severity::High => "HIGH",
        devboy_secret_patterns::Severity::Medium => "MEDIUM",
        devboy_secret_patterns::Severity::Low => "LOW",
    }
}

fn exit_code(report: &ScanReport) -> i32 {
    if report.summary.high > 0 || report.summary.medium > 0 {
        1
    } else if report.summary.low > 0 {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn exit_code_prioritizes_high_and_medium_findings() {
        let mut report = ScanReport::default();
        assert_eq!(exit_code(&report), 0);
        report.summary.low = 1;
        assert_eq!(exit_code(&report), 2);
        report.summary.medium = 1;
        assert_eq!(exit_code(&report), 1);
    }

    #[test]
    fn directory_input_scans_jsonl_files_recursively_in_order() {
        let dir = TempDir::new().expect("temp directory");
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        fs::write(dir.path().join("ignored.txt"), "not JSONL").expect("ignored fixture");
        fs::write(nested.join("a.jsonl"), "{\"body\":\"safe\"}\n").expect("safe fixture");
        fs::write(
            dir.path().join("b.jsonl"),
            "{\"body\":\"Bearer ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ\"}\n",
        )
        .expect("leak fixture");

        let catalogue = Catalogue::builtins_only();
        let scanner = Scanner::new(&catalogue);
        let report = scan_input(
            &scanner,
            &format!("jsonl:{}", dir.path().display()),
            ScanFormat::Auto,
        )
        .expect("directory scan");

        assert_eq!(report.summary.records, 2);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.category == "github-pat")
        );
    }
}
