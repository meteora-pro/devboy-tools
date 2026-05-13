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

## See also

- [ADR-023 §3.4](../../docs/architecture/adr/ADR-023-secret-store-ux-layer.md)
  — UX layer architecture; the provision dialog lives here.
- `devboy-secrets-agent` — companion daemon owning the unlocked local-vault.
