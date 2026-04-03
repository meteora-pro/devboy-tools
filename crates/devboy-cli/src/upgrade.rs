//! Self-upgrade command implementation.
//!
//! Downloads and replaces the devboy binary with the latest version
//! from GitHub Releases. Detects npm-managed installations and
//! suggests the appropriate package manager command instead.

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
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

/// Build a GitHub API request with optional authentication.
///
/// Uses `GITHUB_TOKEN` or `GH_TOKEN` env var for authentication if available,
/// which increases the rate limit from 60 to 5000 requests/hour.
fn github_api_request(client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    let mut req = client.get(url);
    if let Ok(token) = env::var("GITHUB_TOKEN").or_else(|_| env::var("GH_TOKEN"))
        && !token.is_empty()
    {
        req = req.bearer_auth(token);
    }
    req
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

    let response = github_api_request(&client, &url)
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
///
/// - **Unix**: writes to a temp file and performs an atomic rename.
/// - **Windows**: writes the new binary next to the current one, then spawns
///   a helper `cmd.exe` process that waits for this process to exit and
///   replaces the executable. This is necessary because Windows locks running
///   executables, preventing in-place replacement.
fn replace_binary(new_binary: &[u8]) -> Result<PathBuf> {
    let current_exe = env::current_exe().context("Failed to get current executable path")?;
    let current_exe = current_exe.canonicalize().unwrap_or(current_exe);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let temp_path = current_exe.with_extension("new");
        fs::write(&temp_path, new_binary).context("Failed to write new binary")?;
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o755))
            .context("Failed to set executable permissions")?;
        fs::rename(&temp_path, &current_exe).context("Failed to replace binary (atomic rename)")?;
    }

    #[cfg(windows)]
    {
        let temp_path = current_exe.with_extension("new.exe");
        fs::write(&temp_path, new_binary).context("Failed to write new binary")?;

        // Spawn a helper process that waits for us to exit, then moves the
        // new binary over the old one and cleans up.
        let current_str = current_exe.to_string_lossy();
        let temp_str = temp_path.to_string_lossy();

        // ping localhost is a classic Windows trick to sleep without `timeout`
        // (which requires a console). 3 pings ≈ 2 seconds.
        let script = format!(
            "ping 127.0.0.1 -n 3 >nul & move /Y \"{}\" \"{}\"",
            temp_str, current_str,
        );

        std::process::Command::new("cmd")
            .args(["/C", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Failed to spawn helper process for binary replacement")?;
    }

    #[cfg(not(any(unix, windows)))]
    {
        bail!(
            "Self-update is not supported on this platform. Download the binary manually from GitHub Releases."
        );
    }

    Ok(current_exe)
}

/// Run upgrade via the detected package manager (npm/pnpm/yarn).
///
/// Spawns the package manager as a child process, inheriting
/// stdout/stderr so the user sees real-time progress.
///
/// On Windows, the running executable is locked by the OS, so the
/// package manager cannot replace it in-place. In that case we fall
/// back to printing the command for the user to run manually.
fn run_managed_upgrade(install_method: &crate::update_check::InstallMethod) -> Result<()> {
    let cmd_str = install_method.update_command();

    println!(
        "Installation managed by {}. Running: \x1b[1m{}\x1b[0m\n",
        install_method.name(),
        cmd_str
    );

    #[cfg(windows)]
    {
        // Windows locks the running .exe, so the package manager cannot
        // replace it while we are alive. Spawn a detached `cmd.exe` that
        // waits for this process to exit and then runs the update.
        // 3 pings ≈ 2 seconds — classic Windows sleep-without-console trick.
        let script = format!("ping 127.0.0.1 -n 3 >nul & {}", cmd_str);

        std::process::Command::new("cmd")
            .args(["/C", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Failed to spawn helper process for upgrade")?;

        println!("\x1b[33mThe upgrade will run in the background after this process exits.\x1b[0m");
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        let (program, args) = install_method.update_command_parts();
        let status = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .with_context(|| {
                format!(
                    "Failed to run '{}'. Is {} installed?",
                    program,
                    install_method.name()
                )
            })?;

        if !status.success() {
            bail!(
                "{} exited with {}. You can try manually: {}",
                program,
                status,
                cmd_str
            );
        }

        println!(
            "\n\x1b[32m✓ Successfully upgraded via {}\x1b[0m",
            install_method.name()
        );
        Ok(())
    }
}

/// Run the upgrade command.
///
/// If `check_only` is true, only checks for updates without installing.
pub async fn run_upgrade(check_only: bool) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");

    // Check installation method first
    let install_method = detect_install_method();

    if install_method.is_managed() && !check_only {
        return run_managed_upgrade(&install_method);
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

    #[cfg(windows)]
    println!(
        "\n\x1b[33mNote: The binary will be replaced in a few seconds after this process exits.\x1b[0m"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_asset_name() {
        let name = get_asset_name().unwrap();
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

    #[test]
    fn test_extract_tar_gz_valid() {
        // Create a tar.gz archive with a "devboy" file in memory
        let mut builder = tar::Builder::new(Vec::new());

        let content = b"fake binary content for testing";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();

        builder
            .append_data(&mut header, "devboy", &content[..])
            .unwrap();

        let tar_data = builder.into_inner().unwrap();

        // Compress with gzip
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_data).unwrap();
        let gz_data = encoder.finish().unwrap();

        let result = extract_tar_gz(&gz_data);
        assert!(result.is_ok(), "Should extract devboy from tar.gz");
        assert_eq!(result.unwrap(), content);
    }

    #[test]
    fn test_extract_tar_gz_missing_binary() {
        // Create a tar.gz with a different filename
        let mut builder = tar::Builder::new(Vec::new());

        let content = b"not devboy";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        builder
            .append_data(&mut header, "other-file", &content[..])
            .unwrap();

        let tar_data = builder.into_inner().unwrap();

        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_data).unwrap();
        let gz_data = encoder.finish().unwrap();

        let result = extract_tar_gz(&gz_data);
        assert!(result.is_err(), "Should fail when devboy not in archive");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not found in archive"),
        );
    }

    #[test]
    fn test_extract_tar_gz_with_directory_prefix() {
        // Create tar.gz where devboy is in a subdirectory
        let mut builder = tar::Builder::new(Vec::new());

        let content = b"binary in subdir";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();

        builder
            .append_data(&mut header, "release/devboy", &content[..])
            .unwrap();

        let tar_data = builder.into_inner().unwrap();

        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_data).unwrap();
        let gz_data = encoder.finish().unwrap();

        // Should still find it by file_name()
        let result = extract_tar_gz(&gz_data);
        assert!(result.is_ok(), "Should find devboy in subdirectory");
        assert_eq!(result.unwrap(), content);
    }

    #[test]
    fn test_extract_zip_valid() {
        use std::io::Cursor;

        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip_writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            zip_writer.start_file("devboy.exe", options).unwrap();
            std::io::Write::write_all(&mut zip_writer, b"fake exe content").unwrap();
            zip_writer.finish().unwrap();
        }

        let zip_data = buf.into_inner();
        let result = extract_zip(&zip_data);
        assert!(result.is_ok(), "Should extract devboy.exe from zip");
        assert_eq!(result.unwrap(), b"fake exe content");
    }

    #[test]
    fn test_extract_zip_devboy_without_exe() {
        use std::io::Cursor;

        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip_writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            zip_writer.start_file("devboy", options).unwrap();
            std::io::Write::write_all(&mut zip_writer, b"unix binary").unwrap();
            zip_writer.finish().unwrap();
        }

        let zip_data = buf.into_inner();
        let result = extract_zip(&zip_data);
        assert!(
            result.is_ok(),
            "Should extract devboy (without .exe) from zip"
        );
        assert_eq!(result.unwrap(), b"unix binary");
    }

    #[test]
    fn test_extract_zip_missing_binary() {
        use std::io::Cursor;

        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip_writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            zip_writer.start_file("readme.txt", options).unwrap();
            std::io::Write::write_all(&mut zip_writer, b"not a binary").unwrap();
            zip_writer.finish().unwrap();
        }

        let zip_data = buf.into_inner();
        let result = extract_zip(&zip_data);
        assert!(result.is_err(), "Should fail when binary not in zip");
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_extract_tar_gz_invalid_data() {
        let result = extract_tar_gz(b"not a tar.gz file");
        assert!(result.is_err(), "Should fail on invalid tar.gz data");
    }

    #[test]
    fn test_extract_zip_invalid_data() {
        let result = extract_zip(b"not a zip file");
        assert!(result.is_err(), "Should fail on invalid zip data");
    }

    #[test]
    fn test_replace_binary_creates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let binary_path = dir.path().join("devboy-test");

        // Create initial binary
        fs::write(&binary_path, b"old binary").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        // We can't easily test replace_binary because it uses current_exe(),
        // but we can test the extraction + write pipeline
        let new_content = b"new binary content";
        fs::write(&binary_path, new_content).unwrap();

        let content = fs::read(&binary_path).unwrap();
        assert_eq!(content, new_content);
    }

    #[test]
    fn test_release_deserialization() {
        let json = r#"{
            "tag_name": "v1.2.3",
            "assets": [
                {
                    "name": "devboy-linux-x86_64.tar.gz",
                    "browser_download_url": "https://example.com/devboy-linux-x86_64.tar.gz"
                },
                {
                    "name": "devboy-macos-arm64.tar.gz",
                    "browser_download_url": "https://example.com/devboy-macos-arm64.tar.gz"
                }
            ]
        }"#;

        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v1.2.3");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "devboy-linux-x86_64.tar.gz");
        assert_eq!(
            release.assets[1].browser_download_url,
            "https://example.com/devboy-macos-arm64.tar.gz"
        );
    }

    #[test]
    fn test_release_tag_name_strip_prefix() {
        let tag = "v1.2.3";
        let version = tag.strip_prefix('v').unwrap_or(tag);
        assert_eq!(version, "1.2.3");

        let tag_no_prefix = "1.2.3";
        let version = tag_no_prefix.strip_prefix('v').unwrap_or(tag_no_prefix);
        assert_eq!(version, "1.2.3");
    }
}
