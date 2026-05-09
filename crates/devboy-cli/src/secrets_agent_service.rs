//! launchd / systemd-user service generators for the
//! `devboy-secrets-agent` daemon per [ADR-023] §3.3 lifecycle.
//!
//! `secrets agent install` renders a per-platform service unit that
//! (a) starts the daemon at user login, (b) restarts it on failure,
//! and (c) survives a reboot, then loads it via the platform's
//! service manager:
//!
//! | Platform | Path                                                    | Loader                                              |
//! |----------|---------------------------------------------------------|-----------------------------------------------------|
//! | macOS    | `~/Library/LaunchAgents/dev.devboy.secrets.plist`       | `launchctl bootstrap gui/<uid> <path>`              |
//! | Linux    | `~/.config/systemd/user/devboy-secrets-agent.service`   | `systemctl --user daemon-reload && --user enable`   |
//!
//! `secrets agent uninstall` reverses both steps.
//!
//! ## Manual verification
//!
//! Once installed, the user can verify the daemon is running with:
//!
//! - macOS: `launchctl print gui/$(id -u)/dev.devboy.secrets`
//! - Linux: `systemctl --user status devboy-secrets-agent.service`
//!
//! And tear it down again with `devboy secrets agent uninstall`.
//!
//! ## Why no Windows
//!
//! ADR-023 §3.3 explicitly scopes the v1 daemon to Unix; the agent
//! socket itself uses UNIX-domain transport. A future named-pipe
//! port would surface as a Windows Service rather than a
//! `LaunchAgent`/`systemd --user` unit.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

// Both renderers (plist + systemd) compile on every platform so that
// the unit tests in this module exercise both code paths regardless
// of which OS CI runs on. On a release build the unused renderer for
// the *other* OS would otherwise trip `dead_code`; that's expected,
// not a defect, so we silence the lint at module scope.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// macOS launchd label / Linux systemd unit basename. Used both as
/// the plist's `Label` and as part of the on-disk filename so the
/// two stay in sync.
pub const SERVICE_LABEL_MACOS: &str = "dev.devboy.secrets";

/// Linux systemd unit name. Conventional kebab-case, matches the
/// agent binary so `journalctl --user-unit devboy-secrets-agent` is
/// the obvious thing to type.
pub const SERVICE_NAME_LINUX: &str = "devboy-secrets-agent";

// =============================================================================
// Layout — pure data
// =============================================================================

/// Inputs needed to render and install a user service unit.
///
/// Lives in its own struct (rather than a long argument list on the
/// install function) so tests can build a layout, render the unit
/// content, and assert on the result without touching the
/// filesystem or the platform service manager.
#[derive(Debug, Clone)]
pub struct UserServiceLayout {
    /// Absolute path to the `devboy-secrets-agent` binary.
    pub binary_path: PathBuf,
    /// Where the daemon should write its stdout/stderr.
    /// Defaults under `<config_dir>/devboy-tools/secrets/agent.log`.
    pub log_path: PathBuf,
    /// Optional override for `DEVBOY_AGENT_SOCKET`. Lets the daemon
    /// bind to a non-default path; usually left `None` so the agent
    /// uses its compiled-in default.
    pub socket_path: Option<PathBuf>,
    /// Optional override for `DEVBOY_VAULT_PATH`. Same caveat as
    /// `socket_path`.
    pub vault_path: Option<PathBuf>,
}

impl UserServiceLayout {
    /// Render the macOS plist content for this layout.
    ///
    /// Sets `RunAtLoad=true` and `KeepAlive` only on non-clean exit
    /// so a manual `launchctl unload` does not respawn the daemon.
    pub fn render_plist(&self) -> String {
        let env = self.render_plist_env_dict();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>ProcessType</key>
    <string>Background</string>{env}
</dict>
</plist>
"#,
            label = SERVICE_LABEL_MACOS,
            binary = xml_escape(&self.binary_path.display().to_string()),
            log = xml_escape(&self.log_path.display().to_string()),
            env = env,
        )
    }

    fn render_plist_env_dict(&self) -> String {
        if self.socket_path.is_none() && self.vault_path.is_none() {
            return String::new();
        }
        let mut entries = String::new();
        if let Some(p) = &self.socket_path {
            entries.push_str(&format!(
                "\n        <key>DEVBOY_AGENT_SOCKET</key>\n        <string>{}</string>",
                xml_escape(&p.display().to_string())
            ));
        }
        if let Some(p) = &self.vault_path {
            entries.push_str(&format!(
                "\n        <key>DEVBOY_VAULT_PATH</key>\n        <string>{}</string>",
                xml_escape(&p.display().to_string())
            ));
        }
        format!("\n    <key>EnvironmentVariables</key>\n    <dict>{entries}\n    </dict>")
    }

    /// Render the Linux systemd-user unit content for this layout.
    ///
    /// `Type=simple` keeps the unit file small; the agent does not
    /// daemonize itself in a way systemd would need to track. Logs
    /// are funneled through journal by default, so no separate
    /// log-redirect is necessary (`StandardOutput=journal`,
    /// `StandardError=journal`).
    pub fn render_systemd_unit(&self) -> String {
        let env_lines = self.render_systemd_env_lines();
        let env_block = if env_lines.is_empty() {
            String::new()
        } else {
            format!("\n{env_lines}")
        };
        format!(
            "[Unit]\n\
             Description=devboy secrets agent (ADR-023 §3.3)\n\
             Documentation=https://github.com/meteora-pro/devboy-tools\n\
             After=default.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={binary}\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             StandardOutput=journal\n\
             StandardError=journal{env_block}\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            binary = self.binary_path.display(),
            env_block = env_block,
        )
    }

    fn render_systemd_env_lines(&self) -> String {
        let mut lines = Vec::new();
        if let Some(p) = &self.socket_path {
            lines.push(format!("Environment=DEVBOY_AGENT_SOCKET={}", p.display()));
        }
        if let Some(p) = &self.vault_path {
            lines.push(format!("Environment=DEVBOY_VAULT_PATH={}", p.display()));
        }
        lines.join("\n")
    }
}

// =============================================================================
// Per-platform install paths
// =============================================================================

/// Where the unit file lives on disk. Returns an error on
/// unsupported platforms.
pub fn unit_install_path(home: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{SERVICE_LABEL_MACOS}.plist")))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(home
            .join(".config")
            .join("systemd")
            .join("user")
            .join(format!("{SERVICE_NAME_LINUX}.service")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = home;
        bail!(
            "service install is only supported on macOS and Linux; \
             run the daemon manually with `devboy secrets agent start` instead"
        )
    }
}

/// Render the unit content for the current platform.
pub fn render_unit_for_current_platform(layout: &UserServiceLayout) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        Ok(layout.render_plist())
    }
    #[cfg(target_os = "linux")]
    {
        Ok(layout.render_systemd_unit())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = layout;
        bail!(
            "service unit generation is only supported on macOS and Linux; \
             run the daemon manually with `devboy secrets agent start` instead"
        )
    }
}

// =============================================================================
// Install / uninstall
// =============================================================================

/// Knobs for [`install_user_service`] and [`uninstall_user_service`].
///
/// `load` toggles whether to actually invoke `launchctl bootstrap`
/// (or `systemctl --user enable --now`). Tests pass `false` so the
/// install just writes the file and asserts on its content; the
/// real CLI passes `true`.
#[derive(Debug, Clone, Copy)]
pub struct ServiceOptions {
    /// Whether to call the platform service manager after writing
    /// (or before removing) the unit file.
    pub load: bool,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self { load: true }
    }
}

/// Install the user service for the current platform.
///
/// Writes the rendered unit to [`unit_install_path`]. When
/// `options.load` is true, also instructs the platform service
/// manager to load and start it. Returns the path the unit was
/// written to.
pub fn install_user_service(
    home: &Path,
    layout: &UserServiceLayout,
    options: &ServiceOptions,
) -> Result<PathBuf> {
    let path = unit_install_path(home)?;
    let content = render_unit_for_current_platform(layout)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write {}", path.display()))?;
    if options.load {
        invoke_loader(&path)?;
    }
    Ok(path)
}

/// Uninstall the user service for the current platform.
///
/// When `options.load` is true, asks the platform service manager
/// to stop and unload the unit *before* the file is removed (so the
/// service manager has a chance to find it). Returns `Ok(false)` if
/// the unit file was not present (idempotent).
pub fn uninstall_user_service(home: &Path, options: &ServiceOptions) -> Result<bool> {
    let path = unit_install_path(home)?;
    if !path.exists() {
        return Ok(false);
    }
    if options.load {
        // Best-effort unload — never fail the uninstall just because
        // the service manager refuses (the file removal below is
        // what users actually care about).
        if let Err(e) = invoke_unloader(&path) {
            eprintln!("warning: failed to unload service: {e}");
        }
    }
    std::fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn invoke_loader(path: &Path) -> Result<()> {
    let uid = current_uid_string();
    // `bootstrap` is the modern API on macOS 11+; `load` is
    // deprecated but kept as a fallback for older systems.
    let bootstrap = Command::new("launchctl")
        .arg("bootstrap")
        .arg(format!("gui/{uid}"))
        .arg(path)
        .status();
    if let Ok(s) = bootstrap
        && s.success()
    {
        return Ok(());
    }
    let status = Command::new("launchctl")
        .arg("load")
        .arg("-w")
        .arg(path)
        .status()
        .with_context(|| "failed to invoke launchctl")?;
    if !status.success() {
        bail!("launchctl load exited with {status}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn invoke_loader(path: &Path) -> Result<()> {
    let _ = path; // unit name is conventional, derived from path basename
    let reload = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .with_context(|| "failed to invoke systemctl --user daemon-reload")?;
    if !reload.success() {
        bail!("systemctl --user daemon-reload exited with {reload}");
    }
    let unit = format!("{SERVICE_NAME_LINUX}.service");
    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", &unit])
        .status()
        .with_context(|| "failed to invoke systemctl --user enable --now")?;
    if !enable.success() {
        bail!("systemctl --user enable --now {unit} exited with {enable}");
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn invoke_loader(_path: &Path) -> Result<()> {
    bail!("service loading is only supported on macOS and Linux")
}

#[cfg(target_os = "macos")]
fn invoke_unloader(path: &Path) -> Result<()> {
    let uid = current_uid_string();
    let bootout = Command::new("launchctl")
        .arg("bootout")
        .arg(format!("gui/{uid}/{SERVICE_LABEL_MACOS}"))
        .status();
    if let Ok(s) = bootout
        && s.success()
    {
        return Ok(());
    }
    let status = Command::new("launchctl")
        .arg("unload")
        .arg("-w")
        .arg(path)
        .status()
        .with_context(|| "failed to invoke launchctl")?;
    if !status.success() {
        bail!("launchctl unload exited with {status}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn invoke_unloader(_path: &Path) -> Result<()> {
    let unit = format!("{SERVICE_NAME_LINUX}.service");
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", &unit])
        .status();
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn invoke_unloader(_path: &Path) -> Result<()> {
    bail!("service unloading is only supported on macOS and Linux")
}

#[cfg(target_os = "macos")]
fn current_uid_string() -> String {
    // SAFETY: `getuid()` is a leaf libc call with no preconditions —
    // but we can't call libc directly here (workspace
    // `unsafe_code = "forbid"`). Use `nix` instead. The agent
    // crate already pulls in nix; for the CLI we read $UID from the
    // env (always set on macOS by login(1)). Falls back to "0" only
    // in the pathological case of a missing env var, which would
    // make the bootstrap step fail with a useful error.
    std::env::var("UID").unwrap_or_else(|_| "0".to_owned())
}

// =============================================================================
// Helpers
// =============================================================================

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture_layout() -> UserServiceLayout {
        UserServiceLayout {
            binary_path: PathBuf::from("/usr/local/bin/devboy-secrets-agent"),
            log_path: PathBuf::from("/Users/test/Library/Logs/devboy-secrets/agent.log"),
            socket_path: None,
            vault_path: None,
        }
    }

    // -- plist rendering ------------------------------------------------

    #[test]
    fn plist_contains_label_and_program_arguments() {
        let p = fixture_layout().render_plist();
        assert!(p.contains("<string>dev.devboy.secrets</string>"));
        assert!(p.contains("<string>/usr/local/bin/devboy-secrets-agent</string>"));
    }

    #[test]
    fn plist_keeps_alive_only_on_failure() {
        let p = fixture_layout().render_plist();
        assert!(p.contains("<key>KeepAlive</key>"));
        assert!(p.contains("<key>SuccessfulExit</key>"));
        assert!(p.contains("<false/>"));
    }

    #[test]
    fn plist_includes_env_dict_when_overrides_are_set() {
        let mut layout = fixture_layout();
        layout.socket_path = Some(PathBuf::from("/tmp/agent.sock"));
        layout.vault_path = Some(PathBuf::from("/tmp/vault.dvb"));
        let p = layout.render_plist();
        assert!(p.contains("<key>EnvironmentVariables</key>"));
        assert!(p.contains("<key>DEVBOY_AGENT_SOCKET</key>"));
        assert!(p.contains("<string>/tmp/agent.sock</string>"));
        assert!(p.contains("<key>DEVBOY_VAULT_PATH</key>"));
        assert!(p.contains("<string>/tmp/vault.dvb</string>"));
    }

    #[test]
    fn plist_omits_env_dict_when_no_overrides() {
        let p = fixture_layout().render_plist();
        assert!(!p.contains("EnvironmentVariables"));
    }

    #[test]
    fn plist_xml_escapes_path_components() {
        let mut layout = fixture_layout();
        layout.binary_path = PathBuf::from("/Users/Mr <Tester>/bin/agent");
        let p = layout.render_plist();
        assert!(p.contains("&lt;Tester&gt;"));
        assert!(!p.contains("<Tester>"));
    }

    // -- systemd rendering ---------------------------------------------

    #[test]
    fn systemd_unit_has_required_sections() {
        let s = fixture_layout().render_systemd_unit();
        assert!(s.contains("[Unit]"));
        assert!(s.contains("[Service]"));
        assert!(s.contains("[Install]"));
        assert!(s.contains("ExecStart=/usr/local/bin/devboy-secrets-agent"));
    }

    #[test]
    fn systemd_unit_restarts_on_failure() {
        let s = fixture_layout().render_systemd_unit();
        assert!(s.contains("Restart=on-failure"));
        assert!(s.contains("RestartSec=5"));
    }

    #[test]
    fn systemd_unit_wants_default_target() {
        let s = fixture_layout().render_systemd_unit();
        assert!(s.contains("WantedBy=default.target"));
    }

    #[test]
    fn systemd_unit_emits_environment_lines_for_overrides() {
        let mut layout = fixture_layout();
        layout.socket_path = Some(PathBuf::from("/tmp/agent.sock"));
        layout.vault_path = Some(PathBuf::from("/tmp/vault.dvb"));
        let s = layout.render_systemd_unit();
        assert!(s.contains("Environment=DEVBOY_AGENT_SOCKET=/tmp/agent.sock"));
        assert!(s.contains("Environment=DEVBOY_VAULT_PATH=/tmp/vault.dvb"));
    }

    #[test]
    fn systemd_unit_skips_environment_section_when_no_overrides() {
        let s = fixture_layout().render_systemd_unit();
        assert!(
            !s.contains("Environment="),
            "default layout should not write Environment= lines"
        );
    }

    // -- install path resolution ---------------------------------------

    #[cfg(target_os = "macos")]
    #[test]
    fn install_path_is_under_library_launch_agents_on_macos() {
        let home = Path::new("/Users/test");
        let p = unit_install_path(home).unwrap();
        assert_eq!(
            p,
            PathBuf::from("/Users/test/Library/LaunchAgents/dev.devboy.secrets.plist")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn install_path_is_under_config_systemd_user_on_linux() {
        let home = Path::new("/home/test");
        let p = unit_install_path(home).unwrap();
        assert_eq!(
            p,
            PathBuf::from("/home/test/.config/systemd/user/devboy-secrets-agent.service")
        );
    }

    // -- install / uninstall round-trip --------------------------------

    #[test]
    fn install_writes_unit_file_under_synthetic_home() {
        let home = TempDir::new().unwrap();
        let layout = fixture_layout();
        let path =
            install_user_service(home.path(), &layout, &ServiceOptions { load: false }).unwrap();
        assert!(path.exists(), "expected unit file to exist at {path:?}");

        // The on-disk content must match the renderer.
        let content = std::fs::read_to_string(&path).unwrap();
        let expected = render_unit_for_current_platform(&layout).unwrap();
        assert_eq!(content, expected);
    }

    #[test]
    fn install_creates_parent_directories() {
        // Synthetic HOME is a fresh tempdir — neither
        // ~/Library/LaunchAgents nor ~/.config/systemd/user exists
        // yet. Install must mkdir -p them.
        let home = TempDir::new().unwrap();
        let _ = install_user_service(
            home.path(),
            &fixture_layout(),
            &ServiceOptions { load: false },
        )
        .unwrap();
        let parent = unit_install_path(home.path()).unwrap();
        let parent = parent.parent().unwrap();
        assert!(parent.is_dir(), "parent dir not created: {parent:?}");
    }

    #[test]
    fn uninstall_removes_the_unit_file() {
        let home = TempDir::new().unwrap();
        let path = install_user_service(
            home.path(),
            &fixture_layout(),
            &ServiceOptions { load: false },
        )
        .unwrap();
        assert!(path.exists());

        let removed = uninstall_user_service(home.path(), &ServiceOptions { load: false }).unwrap();
        assert!(
            removed,
            "uninstall should report `true` when a unit was removed"
        );
        assert!(!path.exists(), "unit file should be gone after uninstall");
    }

    #[test]
    fn uninstall_is_idempotent_when_unit_is_absent() {
        let home = TempDir::new().unwrap();
        let removed = uninstall_user_service(home.path(), &ServiceOptions { load: false }).unwrap();
        assert!(
            !removed,
            "uninstall on an absent unit should report `false`, not error"
        );
    }
}
