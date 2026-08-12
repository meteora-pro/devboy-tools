//! `devboy otel scan` command family.

use std::fs::File;
use std::io::{self, BufReader};

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
    /// JSONL file to scan. Use `-` to read JSONL from standard input.
    #[arg(long)]
    input: String,

    /// Input format. Only JSONL is implemented currently.
    #[arg(long, value_enum, default_value_t = ScanFormat::Jsonl)]
    format: ScanFormat,

    /// Report format.
    #[arg(long, value_enum, default_value_t = ScanOutput::Text)]
    output: ScanOutput,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ScanFormat {
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
    let report = match args.input.as_str() {
        "-" => scan_jsonl(&scanner, "stdin", BufReader::new(io::stdin().lock())),
        path => match File::open(path) {
            Ok(file) => scan_jsonl(&scanner, path, BufReader::new(file)),
            Err(_) => {
                eprintln!("scan error: could not open input '{path}'");
                return Ok(3);
            }
        },
    };
    let report = match report {
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

    #[test]
    fn exit_code_prioritizes_high_and_medium_findings() {
        let mut report = ScanReport::default();
        assert_eq!(exit_code(&report), 0);
        report.summary.low = 1;
        assert_eq!(exit_code(&report), 2);
        report.summary.medium = 1;
        assert_eq!(exit_code(&report), 1);
    }
}
