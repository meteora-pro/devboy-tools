//! `devboy secrets ui` — backend autodetection + launcher per
//! [ADR-023] §3.4.
//!
//! Selects between the ratatui (TUI) and egui (GUI) renderers
//! based on the runtime environment. The detection logic is
//! the explicit deliverable of P12.2:
//!
//! 1. `--tui` / `--gui` flags override everything.
//! 2. On Linux: `$DISPLAY` or `$WAYLAND_DISPLAY` set → GUI.
//! 3. On macOS / Windows: GUI by default (a windowing system
//!    is always available).
//! 4. Otherwise: TUI.
//!
//! ## Launcher behaviour
//!
//! - **TUI**: spins up a real ratatui event loop with the
//!   inventory view rendered into the alternate screen. `q` /
//!   `Esc` quits. Real keystrokes drive
//!   [`devboy_secrets_ui::InventoryState`]'s key handlers.
//! - **GUI**: spins up a real eframe-backed native window
//!   that hosts the egui inventory view from
//!   [`devboy_secrets_ui::gui::inventory`]. The window opens
//!   at 1024×640 with the title `devboy — secrets inventory`
//!   and pumps frames until the user closes it.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::io::{self, IsTerminal, Stdout};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use devboy_secrets_ui::{InventoryState, render_inventory};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

// =============================================================================
// GUI subprocess discovery
// =============================================================================

/// Env-var override for the `devboy-secrets-ui` binary path.
///
/// An absolute path here overrides the sibling-of-current_exe and
/// PATH lookup. Use for development (build the bin into a custom
/// target dir and point this at it) and for tests (point at a
/// stub binary that records argv).
pub const UI_BIN_ENV: &str = "DEVBOY_UI_BIN";

/// Filename the discovery walk looks for.
pub const UI_BIN_NAME: &str = "devboy-secrets-ui";

/// Locate the `devboy-secrets-ui` binary on disk.
///
/// Resolution order — same shape as [`crate::secrets_agent::find_agent_binary`]:
///
/// 1. The [`UI_BIN_ENV`] env var (absolute path).
/// 2. Sibling of the current executable. This is the "installed
///    alongside the CLI" case (npm package, `cargo install`,
///    release tarball).
/// 3. `PATH` walk — supports `cargo run` / development workflows
///    where `current_exe()` resolves to `target/debug/deps/...`
///    and the sibling check would miss the colocated binary in
///    `target/debug`.
pub fn find_ui_binary() -> Result<PathBuf> {
    if let Ok(s) = std::env::var(UI_BIN_ENV)
        && !s.is_empty()
    {
        return Ok(PathBuf::from(s));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(UI_BIN_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let with_ext = dir.join(format!("{UI_BIN_NAME}.exe"));
            if with_ext.is_file() {
                return Ok(with_ext);
            }
        }
    }
    if let Ok(path_value) = std::env::var("PATH") {
        for entry in std::env::split_paths(&path_value) {
            let candidate = entry.join(UI_BIN_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!(
        "could not locate the `{UI_BIN_NAME}` binary; \
         set {UI_BIN_ENV} to its absolute path or install it alongside `devboy` \
         (it ships as a separate binary so the CLI does not link eframe/egui)"
    );
}

// =============================================================================
// Backend selection
// =============================================================================

/// The renderer the user will see. Returned by [`resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Tui,
    Gui,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Gui => "gui",
        }
    }
}

/// User intent — the CLI flags map onto this enum, then
/// [`resolve`] folds it together with the environment to pick
/// a concrete [`Backend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendChoice {
    /// No flag — autodetect.
    #[default]
    Auto,
    /// `--tui` — force the ratatui backend regardless of
    /// environment.
    ForceTui,
    /// `--gui` — force the egui backend regardless of
    /// environment.
    ForceGui,
}

/// Snapshot of the environment relevant to backend detection.
/// Pulled out as a struct so the detection logic stays a pure
/// function and tests can poke at any combination of values
/// without going through `std::env`.
#[derive(Debug, Clone)]
pub struct DetectionEnv {
    /// Linux: `$DISPLAY` (X11). `Some` non-empty → GUI.
    pub display: Option<String>,
    /// Linux: `$WAYLAND_DISPLAY`. `Some` non-empty → GUI.
    pub wayland_display: Option<String>,
    /// `cfg!(target_os)` snapshot. Pinned to the build target
    /// on real runs; tests can override.
    pub target_os: TargetOs,
    /// Whether stdout is a TTY. Required for the TUI backend —
    /// piping to a file with no terminal makes ratatui useless,
    /// so even when otherwise selected we'll error rather than
    /// scramble the user's pipe.
    pub stdout_is_tty: bool,
}

/// Coarse OS classification. We don't need the full
/// `std::env::consts::OS` matrix here — just "is a windowing
/// system definitely available?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOs {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl TargetOs {
    /// Snapshot the build target's OS. Tests build their own
    /// `DetectionEnv` and skip this.
    pub fn from_cfg() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

impl DetectionEnv {
    /// Snapshot the real environment. Reads `$DISPLAY` /
    /// `$WAYLAND_DISPLAY` from `std::env` (these are
    /// process-globals, but autodetection happens once at
    /// startup so the race window is theoretical).
    pub fn from_real_env() -> Self {
        Self {
            display: std::env::var("DISPLAY").ok().filter(|s| !s.is_empty()),
            wayland_display: std::env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|s| !s.is_empty()),
            target_os: TargetOs::from_cfg(),
            stdout_is_tty: io::stdout().is_terminal(),
        }
    }
}

/// Detect the preferred backend from the environment alone —
/// no override applied. Pure function over [`DetectionEnv`].
pub fn detect_preferred(env: &DetectionEnv) -> Backend {
    match env.target_os {
        TargetOs::Linux => {
            if env.display.is_some() || env.wayland_display.is_some() {
                Backend::Gui
            } else {
                Backend::Tui
            }
        }
        // macOS and Windows always have a windowing system —
        // GUI is the natural default.
        TargetOs::MacOs | TargetOs::Windows => Backend::Gui,
        // Anything else (BSD without X, exotic targets) — TUI is
        // the safe fallback.
        TargetOs::Other => Backend::Tui,
    }
}

/// Resolve user intent + environment into a concrete backend.
/// Errors when the user forced TUI in a non-TTY pipe, since
/// rendering ratatui into a pipe just scrambles the output.
pub fn resolve(choice: BackendChoice, env: &DetectionEnv) -> Result<Backend> {
    let chosen = match choice {
        BackendChoice::ForceTui => Backend::Tui,
        BackendChoice::ForceGui => Backend::Gui,
        BackendChoice::Auto => detect_preferred(env),
    };
    if chosen == Backend::Tui && !env.stdout_is_tty {
        anyhow::bail!(
            "TUI backend selected, but stdout is not a terminal. \
             Run interactively or pass `--gui` if a windowing system is available."
        );
    }
    Ok(chosen)
}

// =============================================================================
// CLI surface
// =============================================================================

/// Flags for `devboy secrets ui`.
#[derive(Args, Debug, Default)]
pub struct UiArgs {
    /// Force the terminal renderer (ratatui).
    #[arg(long, conflicts_with = "gui")]
    pub tui: bool,
    /// Force the windowed renderer (egui). Opens a native
    /// window via eframe; runs until the user closes it.
    #[arg(long)]
    pub gui: bool,
    /// Open the provision dialog focused on the given path.
    /// The window still opens with the full inventory in the
    /// background, but the dialog overlay is armed at startup
    /// — useful when the AI agent (or a script) wants to put
    /// the user one click away from filling a known-missing
    /// secret. Path must be valid ADR-020.
    #[arg(long, value_name = "PATH")]
    pub provision: Option<String>,
}

impl UiArgs {
    pub fn choice(&self) -> BackendChoice {
        if self.tui {
            BackendChoice::ForceTui
        } else if self.gui {
            BackendChoice::ForceGui
        } else {
            BackendChoice::Auto
        }
    }
}

/// Top-level handler. Resolves the backend and dispatches to
/// the matching launcher.
pub async fn handle(args: UiArgs) -> Result<()> {
    let env = DetectionEnv::from_real_env();
    let backend = resolve(args.choice(), &env)?;
    eprintln!("devboy secrets ui: backend = {}", backend.label());
    match backend {
        Backend::Tui => launch_tui(),
        Backend::Gui => launch_gui(args.provision.as_deref()),
    }
}

// =============================================================================
// Launchers
// =============================================================================

fn launch_tui() -> Result<()> {
    let mut terminal = setup_terminal().context("could not enter raw mode / alternate screen")?;
    let result = run_tui_loop(&mut terminal);
    restore_terminal(&mut terminal).context("could not restore terminal")?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable_raw_mode failed")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("EnterAlternateScreen failed")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("ratatui Terminal::new failed")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("LeaveAlternateScreen failed")?;
    disable_raw_mode().context("disable_raw_mode failed")?;
    terminal.show_cursor().context("show_cursor failed")?;
    Ok(())
}

fn run_tui_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    // P12.2's deliverable is the launcher — the rows themselves
    // come from the orchestration layer wired up in P14+
    // (`secrets.list` MCP + the daemon's manifest snapshot). For
    // now the inventory view starts empty and the user can
    // confirm the renderer is alive.
    let mut state = InventoryState::new(Vec::new());

    loop {
        terminal.draw(|f| {
            let area = f.area();
            render_inventory(f, &state, area);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                    KeyCode::Tab => state.cycle_focus_forward(),
                    KeyCode::BackTab => state.cycle_focus_backward(),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Launch the GUI by spawning the standalone `devboy-secrets-ui`
/// binary as a subprocess.
///
/// The eframe/egui rendering stack lives in a separate crate
/// (`devboy-secrets-ui-bin`) so the CLI doesn't link 4-5 MiB of
/// GUI code that CI users never touch. This launcher discovers
/// the binary (via [`find_ui_binary`]), passes `--provision <PATH>`
/// through when set, inherits stdio so the user can see the
/// child's diagnostics, and waits for the window to close.
///
/// A non-zero exit from the child is propagated as an error.
/// "Binary not found" is the most common failure mode — the
/// error message mentions the [`UI_BIN_ENV`] override so the
/// user can wire up a custom build path.
fn launch_gui(provision_path: Option<&str>) -> Result<()> {
    let binary = find_ui_binary()?;
    let mut cmd = Command::new(&binary);
    if let Some(p) = provision_path {
        cmd.arg("--provision").arg(p);
    }
    let status = cmd
        .status()
        .with_context(|| format!("failed to spawn `{}`", binary.display()))?;
    if !status.success() {
        anyhow::bail!("`{}` exited with {}", UI_BIN_NAME, status);
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn env_linux(display: Option<&str>, wayland: Option<&str>, tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: display.map(str::to_string),
            wayland_display: wayland.map(str::to_string),
            target_os: TargetOs::Linux,
            stdout_is_tty: tty,
        }
    }

    fn env_macos(tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: None,
            wayland_display: None,
            target_os: TargetOs::MacOs,
            stdout_is_tty: tty,
        }
    }

    fn env_windows(tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: None,
            wayland_display: None,
            target_os: TargetOs::Windows,
            stdout_is_tty: tty,
        }
    }

    fn env_other(tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: None,
            wayland_display: None,
            target_os: TargetOs::Other,
            stdout_is_tty: tty,
        }
    }

    // -- detect_preferred --------------------------------------

    #[test]
    fn linux_with_display_picks_gui() {
        assert_eq!(
            detect_preferred(&env_linux(Some(":0"), None, true)),
            Backend::Gui
        );
    }

    #[test]
    fn linux_with_wayland_display_picks_gui() {
        assert_eq!(
            detect_preferred(&env_linux(None, Some("wayland-0"), true)),
            Backend::Gui
        );
    }

    #[test]
    fn linux_without_either_display_var_picks_tui() {
        assert_eq!(detect_preferred(&env_linux(None, None, true)), Backend::Tui);
    }

    #[test]
    fn linux_treats_empty_display_string_as_unset() {
        // `DISPLAY=` (set but empty) is what some sshd configs
        // leak through — that's not a usable display.
        let env = DetectionEnv {
            display: Some(String::new()).filter(|s| !s.is_empty()),
            wayland_display: None,
            target_os: TargetOs::Linux,
            stdout_is_tty: true,
        };
        assert_eq!(detect_preferred(&env), Backend::Tui);
    }

    #[test]
    fn macos_picks_gui_unconditionally() {
        assert_eq!(detect_preferred(&env_macos(true)), Backend::Gui);
    }

    #[test]
    fn windows_picks_gui_unconditionally() {
        assert_eq!(detect_preferred(&env_windows(true)), Backend::Gui);
    }

    #[test]
    fn unknown_os_falls_back_to_tui() {
        assert_eq!(detect_preferred(&env_other(true)), Backend::Tui);
    }

    // -- resolve overrides -------------------------------------

    #[test]
    fn force_tui_overrides_gui_environment() {
        let backend = resolve(BackendChoice::ForceTui, &env_macos(true)).unwrap();
        assert_eq!(backend, Backend::Tui);
    }

    #[test]
    fn force_gui_overrides_tui_environment() {
        let backend = resolve(BackendChoice::ForceGui, &env_linux(None, None, true)).unwrap();
        assert_eq!(backend, Backend::Gui);
    }

    #[test]
    fn auto_picks_the_detected_backend() {
        let backend = resolve(BackendChoice::Auto, &env_macos(true)).unwrap();
        assert_eq!(backend, Backend::Gui);
        let backend = resolve(BackendChoice::Auto, &env_linux(None, None, true)).unwrap();
        assert_eq!(backend, Backend::Tui);
    }

    // -- TTY guard ---------------------------------------------

    #[test]
    fn tui_in_a_pipe_errors_loudly_rather_than_scrambling_output() {
        // Force TUI but stdin/stdout aren't a terminal — must
        // refuse, not write raw escape codes into the pipe.
        let err = resolve(BackendChoice::ForceTui, &env_macos(false)).unwrap_err();
        assert!(err.to_string().contains("not a terminal"));
    }

    #[test]
    fn auto_on_linux_without_display_in_a_pipe_also_errors() {
        // No display vars → would pick TUI → no TTY → must fail
        // cleanly so the user knows to attach a terminal.
        let err = resolve(BackendChoice::Auto, &env_linux(None, None, false)).unwrap_err();
        assert!(err.to_string().contains("not a terminal"));
    }

    #[test]
    fn force_gui_in_a_pipe_succeeds_because_gui_does_not_use_stdout() {
        // The GUI launcher renders into its own window; a piped
        // stdout doesn't matter.
        let backend = resolve(BackendChoice::ForceGui, &env_macos(false)).unwrap();
        assert_eq!(backend, Backend::Gui);
    }

    // -- UiArgs.choice -----------------------------------------

    #[test]
    fn ui_args_default_is_auto() {
        assert_eq!(UiArgs::default().choice(), BackendChoice::Auto);
    }

    #[test]
    fn ui_args_tui_flag_forces_tui() {
        let args = UiArgs {
            tui: true,
            gui: false,
            provision: None,
        };
        assert_eq!(args.choice(), BackendChoice::ForceTui);
    }

    #[test]
    fn ui_args_gui_flag_forces_gui() {
        let args = UiArgs {
            tui: false,
            gui: true,
            provision: None,
        };
        assert_eq!(args.choice(), BackendChoice::ForceGui);
    }
}
