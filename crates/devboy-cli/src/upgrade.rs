//! Self-upgrade command implementation.
//!
//! Downloads and replaces the devboy binary with the latest version
//! from GitHub Releases. Detects npm-managed installations and
//! suggests the appropriate package manager command instead.

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::update_check::{detect_install_method, is_newer_version};

/// GitHub repository owner.
const GITHUB_OWNER: &str = "meteora-pro";

/// GitHub repository name.
const GITHUB_REPO: &str = "devboy-tools";

/// HTTP request timeout for download.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// GitHub Release API response.
#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

/// GitHub Release asset.
#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Get the expected asset name for the current platform.
fn get_asset_name() -> Result<String> {
    let name = match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => "devboy-linux-x86_64.tar.gz",
        ("linux", "aarch64") => "devboy-linux-arm64.tar.gz",
        ("macos", "x86_64") => "devboy-macos-x86_64.tar.gz",
        ("macos", "aarch64") => "devboy-macos-arm64.tar.gz",
        ("windows", "x86_64") => "devboy-windows-x86_64.exe.zip",
        (os, arch) => bail!("Unsupported platform: {os}/{arch}"),
    };
    Ok(name.to_string())
}

/// Fetch the latest release info from GitHub.
async fn fetch_latest_release() -> Result<Release> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        GITHUB_OWNER, GITHUB_REPO
    );

    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(format!("devboy/{}", env!("CARGO_PKG_VERSION")))
        .build()?;

    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to fetch release info from GitHub")?;

    if !response.status().is_success() {
        bail!(
            "GitHub API returned status {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }

    response
        .json()
        .await
        .context("Failed to parse GitHub release response")
}

/// Download an asset from GitHub and return its bytes.
async fn download_asset(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(format!("devboy/{}", env!("CARGO_PKG_VERSION")))
        .build()?;

    let response = client
        .get(url)
        .send()
        .await
        .context("Failed to download release asset")?;

    if !response.status().is_success() {
        bail!("Failed to download asset: HTTP {}", response.status());
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read asset bytes")?;

    Ok(bytes.to_vec())
}

/// Extract the devboy binary from a tar.gz archive.
fn extract_tar_gz(data: &[u8]) -> Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(data);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().context("Failed to read tar entries")? {
        let mut entry = entry.context("Failed to read tar entry")?;
        let path = entry.path().context("Failed to read entry path")?;

        if path.file_name().and_then(|n| n.to_str()) == Some("devboy") {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf)?;
            return Ok(buf);
        }
    }

    bail!("Binary 'devboy' not found in archive")
}

/// Extract the devboy binary from a zip archive.
fn extract_zip(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Cursor;
    let reader = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(reader).context("Failed to read zip archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        let name = file.name().to_string();

        if name == "devboy.exe" || name == "devboy" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut buf)?;
            return Ok(buf);
        }
    }

    bail!("Binary not found in zip archive")
}

/// Replace the current binary with new content.
fn replace_binary(new_binary: &[u8]) -> Result<PathBuf> {
    let current_exe = env::current_exe().context("Failed to get current executable path")?;
    let current_exe = current_exe.canonicalize().unwrap_or(current_exe);

    if cfg!(unix) {
        // On Unix: write to temp file, then atomic rename
        let temp_path = current_exe.with_extension("new");

        fs::write(&temp_path, new_binary).context("Failed to write new binary")?;

        // Set executable permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o755))
                .context("Failed to set executable permissions")?;
        }

        fs::rename(&temp_path, &current_exe).context("Failed to replace binary (atomic rename)")?;
    } else {
        // On Windows: rename current to .old, write new, schedule cleanup
        let old_path = current_exe.with_extension("old.exe");
        let _ = fs::remove_file(&old_path); // Clean up previous .old if exists
        fs::rename(&current_exe, &old_path).context("Failed to move current binary aside")?;

        if let Err(e) = fs::write(&current_exe, new_binary) {
            // Try to restore the original
            let _ = fs::rename(&old_path, &current_exe);
            return Err(e).context("Failed to write new binary");
        }
    }

    Ok(current_exe)
}

/// Run the upgrade command.
///
/// If `check_only` is true, only checks for updates without installing.
pub async fn run_upgrade(check_only: bool) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");

    // Check installation method first
    let install_method = detect_install_method();

    if install_method.is_managed() && !check_only {
        println!(
            "This installation is managed by {}.\n\
             Run: \x1b[1m{}\x1b[0m",
            install_method.name(),
            install_method.update_command()
        );
        return Ok(());
    }

    println!("Current version: {}", current_version);
    println!("Checking for updates...");

    let release = fetch_latest_release().await?;
    let latest_version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);

    if !is_newer_version(current_version, latest_version) {
        println!(
            "You are already running the latest version ({}).",
            current_version
        );
        return Ok(());
    }

    println!(
        "New version available: {} → {}",
        current_version, latest_version
    );

    if check_only {
        let update_cmd = install_method.update_command();
        println!("Update with: \x1b[1m{}\x1b[0m", update_cmd);
        return Ok(());
    }

    // Find the right asset for this platform
    let asset_name = get_asset_name()?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .with_context(|| {
            format!(
                "Release asset '{}' not found. Available assets: {}",
                asset_name,
                release
                    .assets
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;

    print!("Downloading {}...", asset_name);
    std::io::stdout().flush()?;

    let data = download_asset(&asset.browser_download_url).await?;
    println!(" done ({:.1} MB)", data.len() as f64 / 1_048_576.0);

    print!("Extracting binary...");
    std::io::stdout().flush()?;

    let binary = if asset_name.ends_with(".tar.gz") {
        extract_tar_gz(&data)?
    } else if asset_name.ends_with(".zip") {
        extract_zip(&data)?
    } else {
        bail!("Unknown archive format: {}", asset_name);
    };
    println!(" done");

    print!("Replacing binary...");
    std::io::stdout().flush()?;

    let path = replace_binary(&binary)?;
    println!(" done");

    println!(
        "\n\x1b[32m✓ Successfully upgraded devboy {} → {}\x1b[0m\n  \
         Binary: {}",
        current_version,
        latest_version,
        path.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_asset_name() {
        let name = get_asset_name().unwrap();
        // Should return a valid asset name for the current platform
        assert!(
            name.starts_with("devboy-"),
            "Asset name should start with 'devboy-': {}",
            name
        );
        assert!(
            name.ends_with(".tar.gz") || name.ends_with(".zip"),
            "Asset name should end with .tar.gz or .zip: {}",
            name
        );
    }
}
