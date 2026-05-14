# devboy-secrets-ui-bin

Native GUI window for the `devboy-tools` secrets inventory, shipped as the
`devboy-secrets-ui` binary.

This binary is split out of the main `devboy` CLI so the eframe / egui /
winit / glow / wayland / x11 / font-decoder stack is **not** linked into
the CLI itself. Users who only need `secrets list`, `secrets validate`,
the MCP server, or the format pipeline (a large share of the user base —
CI runners, headless servers, scripts) get a leaner `devboy` binary.

## Invocation

```sh
devboy-secrets-ui [--provision <PATH>]
```

`--provision <PATH>` arms the provision dialog at startup. Normally users
do not call this directly — they run `devboy secrets ui --gui` and the
CLI spawns this binary as a subprocess. The launcher discovers it via
(in order):

1. `DEVBOY_UI_BIN` environment variable (absolute path)
2. A sibling of `devboy` in the same directory
3. `PATH`

## Dev screenshot mode

Built with `--features dev-screenshot`, the binary also accepts:

```sh
devboy-secrets-ui --screenshot <PATH> [--screenshot-view <VIEW>]
```

This renders **one** UI frame to a PNG at `<PATH>` through an offscreen
`egui_kittest` wgpu harness, then exits — no window, no event loop. It
draws the exact same `gui::*::render` the live app calls, so the PNG is
a faithful preview.

`--screenshot-view` selects which view (default `provision`):

| Value        | Renders |
|--------------|---------|
| `provision`  | the provision dialog, catalog-matched (`team/openai/api-key`) |
| `unlock`     | the vault unlock modal, with a sample wrong-passphrase error |
| `create`     | the vault create modal (passphrase + confirm fields) |
| `onboarding` | the first-run onboarding wizard, all three providers selected |

Purpose: lets an automated agent (or a CI visual-diff job) inspect GUI
changes without a human looking at the native window. The feature is
**off by default** so the shipped binary does not link a second (wgpu)
rendering backend on top of `glow`.

```sh
cargo run -p devboy-secrets-ui-bin --features dev-screenshot -- \
  --screenshot /tmp/provision-dialog.png
```

## See also

- [ADR-023 §3.4](../../docs/architecture/adr/ADR-023-secret-store-ux-layer.md)
  — UX layer architecture; the provision dialog lives here.
- `devboy-secrets-agent` — companion daemon owning the unlocked local-vault.
