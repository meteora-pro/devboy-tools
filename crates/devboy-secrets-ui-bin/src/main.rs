//! `devboy-secrets-ui` — native GUI window for the
//! devboy-tools secrets inventory.
//!
//! This binary is split out of the main `devboy` CLI so the
//! eframe/egui rendering stack (eframe + egui + winit +
//! glow/wayland/x11 + skrifa fonts + image decoders) is not
//! linked into the CLI on machines that never open the GUI
//! (CI runners doing `secrets list` / `secrets validate`,
//! mostly). See ADR-023 §3.4.
//!
//! Invocation contract (the `devboy` CLI launches us as a
//! subprocess; humans normally don't call this directly):
//!
//! ```text
//! devboy-secrets-ui [--provision <PATH>]
//! ```
//!
//! `--provision <PATH>` opens the inventory window with the
//! provision dialog pre-armed on `<PATH>` — same UX as
//! `devboy secrets ui --gui --provision <PATH>` was before
//! the split.
//!
//! The actual eframe::run_native call + InventoryApp shell
//! lands in U2; this revision is the crate skeleton only so
//! the workspace + CI changes can be tested in isolation
//! before code moves.

use anyhow::Result;
use clap::Parser;

/// Command-line surface mirrored from the original
/// `devboy secrets ui` clap struct so existing users see
/// the same flags when they call the subprocess directly.
#[derive(Parser, Debug)]
#[command(
    name = "devboy-secrets-ui",
    version,
    about = "Native GUI window for the devboy-tools secrets inventory.",
    long_about = "Launches an eframe/egui window that renders the merged \
                  secrets inventory and offers a provision dialog. Normally \
                  spawned by `devboy secrets ui --gui`; called directly only \
                  for debugging the GUI in isolation."
)]
struct Cli {
    /// ADR-020 path the provision dialog should focus on at
    /// startup. When omitted the window opens on the
    /// inventory list with no dialog armed.
    #[arg(long, value_name = "PATH")]
    provision: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    println!(
        "devboy-secrets-ui v{} (skeleton — GUI lands in U2)",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(path) = cli.provision.as_deref() {
        println!("provision path: {path}");
    }
    Ok(())
}
