# devboy-secret-kdbx

[KDBX 4](https://keepass.info/help/kb/kdbx_4.html) (KeePass database) `SecretSource`
plugin for `devboy-tools`. Read entries from an existing `.kdbx` file using a
user-provided passphrase (+ optional keyfile).

## Why KDBX

Single encrypted file, industry-standard format, broad ecosystem interop:
KeePass, KeePassXC, KeeWeb, Strongbox, KeePass2Android, KeeWeb-cloud, etc. The
file can ride in Git / Syncthing / Dropbox without exposing values. For users
who already keep their tokens in a KeePass DB this avoids a migration.

## File format

KDBX 4 uses:

- Argon2id KDF (default)
- ChaCha20 or AES-256-CBC for the body
- HMAC-SHA256 for authenticated integrity
- Optional gzip compression of the inner XML
- Master key = passphrase (+ optional keyfile)

Architecturally a sibling of devboy's own `local-vault.dvb` format
(ADR-021 §8): single encrypted file, Argon2id KDF, ChaCha20-family AEAD, opens
into a hash map of secret paths → values.

## Configuration

```toml
# ~/.devboy/secrets/sources.toml (or platform config dir)
[[source]]
name = "personal-keepass"
type = "kdbx"
file = "~/Documents/secrets.kdbx"
keyfile = "~/Documents/secrets.keyx"  # optional
```

Passphrase is **not** stored in config — the user is prompted on first read of
the session (UI modal in the GUI; stdin prompt in the CLI). Subsequent reads
inside the same `devboy-secrets-ui` window reuse the cached unlock.

## Agent-blindness guarantee

KDBX entries' Password fields land **only** inside the UI process address
space. The `devboy-secrets-agent` daemon never opens the KDBX file — same
boundary as the existing local-vault flow (ADR-023 §3.7). Agent-side tools
(`secrets list`, `secrets describe`, MCP `secrets_*`) see entry titles + URLs
+ user-names + the routed-source label, but never the Password field.

## Path mapping

KeePass uses a Group / Entry hierarchy + free-form custom string fields. The
ADR-020 path convention is `scope/provider/purpose` (≥3 slash-separated
segments). The plugin maps KDBX entries by joining their Group breadcrumb
with the Entry title:

```
KeePass:   Root / team / openai / "api-key" (Password field)
ADR-020:   team/openai/api-key
```

The Password field is the value. Other fields (UserName, URL, Notes, custom
strings) surface as metadata on the inventory row.

## Read-only first

The MVP only reads. Writing back to the KDBX file (and the concurrent-write
safety questions that come with it — KeePass GUI users typically have the
same file open) lands as a follow-up. The `WRITE` capability bit stays off
until then.

## Smoke tests

### Header probe (no passphrase needed)

Verify a `.kdbx` file is structurally a KDBX 4 database without
decrypting anything:

```sh
cargo run -p devboy-secret-kdbx --example probe_user_file -- ~/path/to/your.kdbx
```

Prints file size + version. Useful as a pre-flight before
investing in the GUI flow.

### Full end-to-end (passphrase required)

```sh
DEVBOY_KDBX_FILE=~/path/to/your.kdbx \
  devboy secrets ui --gui
```

On launch:

1. The UI window appears with the unlock modal already armed
   ("Unlock KeePass database").
2. Type the passphrase, click Unlock.
3. The inventory populates with one row per entry; the row's
   path follows the convention `kdbx/<group-path>/<title>`.
4. The provision-dialog context card surfaces Title / UserName /
   URL when you click a row.

The decrypted snapshot stays inside the `devboy-secrets-ui`
process only. The `devboy-secrets-agent` daemon never opens the
KDBX file; the CLI's MCP-side tools (`secrets list` /
`secrets_describe`) never see a Password field. This matches
the agent-blindness rule ADR-023 §3.7 specifies for the
`local-vault` backend.

## See also

- [ADR-021](../../../../docs/architecture/adr/ADR-021-external-secret-sources.md)
  — `SecretSource` trait + router contract.
- [ADR-023 §3.7](../../../../docs/architecture/adr/ADR-023-secret-store-ux-layer.md)
  — agent-blindness boundary.
- `devboy-secret-local-vault` — sibling source plugin with the same
  single-file-encrypted shape, devboy-native format.
