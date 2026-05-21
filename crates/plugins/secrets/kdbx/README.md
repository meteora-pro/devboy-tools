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
with the Entry title, then normalising every segment to lowercase +
`[a-z0-9_-]`:

```text
KeePass:   Personal / Cloud / "AWS Access Key" (Password field)
ADR-020:   kdbx/personal/cloud/aws-access-key
```

Rules:

- The synthetic Root group is **never** part of the path (KeePass names it
  after the file).
- Each segment is lowercased + dash-coalesced from runs of non-`[a-z0-9_]`.
- Entries that collapse to fewer than 2 user-derived segments get padded
  with `imported` to keep the result valid (e.g. a root-level "Token"
  entry → `kdbx/imported/token`).
- The `kdbx/` prefix is added unconditionally so the rows are easy to tell
  apart from manifest-declared paths in the inventory.

## Value field auto-detect

Most KeePass entries put the token in the standard `Password` field. The
plugin handles two other patterns:

| Where the value lives | Plugin behaviour |
|---|---|
| **Standard `Password` field** (most common) | Used directly. `value_field = Password`. |
| **Single Protected custom string** + empty Password (e.g. `api_token` field) | Auto-promoted to the value slot. `value_field = CustomField { name: "api_token" }`. The promoted field is hidden from the custom-fields list so the UI doesn't render it twice. |
| **Multiple Protected custom strings** + empty Password | Ambiguous — nothing is promoted. `password = None`. All candidates remain in `custom_fields` so the UI can let the user pick. |

The active value-field is surfaced in the inventory's provision dialog
context card so the user can see why a non-Password field won.

## Metadata round-trip

Every KeePass field the plugin reads is preserved in the in-process
snapshot WITHOUT decryption to disk:

| KeePass field | KdbxEntry slot |
|---|---|
| `Title` | `title` (plus path component) |
| `UserName` | `username` (mapped to `IndexEntry.env_var` for context-card display) |
| `Password` | `password` (the value) |
| `URL` | `url` (mapped to `IndexEntry.retrieval_url`) |
| `Notes` | `notes` (multiline block in `IndexEntry.description`) |
| `tags` | `tags: Vec<String>` |
| `times.creation` | `created_at` (ISO 8601) |
| `times.last_modification` | `modified_at` → `IndexEntry.last_rotated_at` (rotation-age heuristic) |
| `times.expiry` + `times.expires == Some(true)` | `expires_at` → `IndexEntry.expires_at`. Ghost expiry (Expires=false) is suppressed. |
| `Entry.id` (UUID) | `uuid: String` (hyphenated hex) |
| `Entry.get_raw_otp_value()` | `otp` (TOTP marker chip in UI) |
| Custom string fields | `custom_fields: BTreeMap<String, String>` (alphabetical) |
| Attachments | `attachments: Vec<KdbxAttachmentMeta { name, size_bytes }>` — names + sizes only, bytes never read |

## Read-only first, then metadata-only writes

The MVP only reads, and the standard `SecretSource::store` surface still
refuses — value rotation through this plugin would need a much larger
concurrency / merge story (KeePass GUI users typically have the same file
open).

What landed in K14–K17 is a strictly-narrower write surface: **metadata
only**. Two new functions, plus matching CLI + MCP wrappers, let agents
rotate documentation around a secret — Notes / Tags / URL / Title /
UserName / expiry timestamp — without ever touching the value-bearing
Password or any Protected custom string. The ADR-023 §3.7 agent-blindness
boundary is enforced at three layers:

1. `MetadataPatch` has no field for Password / Protected custom strings —
   there is literally no API surface to mutate them through this flow.
2. `describe_metadata` filters Password and Protected fields out of the
   response.
3. The MCP tool wrappers refuse to read the passphrase from tool
   arguments; they only honour the `DEVBOY_KDBX_PASSPHRASE` env var (set
   in the user's shell, agent can't see env).

Write-side safety: callers MUST pass a working-copy path
(`derive_working_copy_path` + `prepare_working_copy`); `edit_metadata`
writes verbatim to the given path. The CLI + MCP wrappers wire the
working-copy step in for you so the user's original `.kdbx` is never
overwritten — sync-back to the original is left to the caller.

```rust
use devboy_secret_kdbx::{edit_metadata, MetadataPatch};
use secrecy::SecretString;

let patch = MetadataPatch {
    notes: Some("rotation runbook: ops-wiki/rotations#42".into()),
    tags: Some(vec!["api".into(), "prod".into(), "rotated-q1".into()]),
    expires_at: Some(Some("2027-01-15T00:00:00Z".into())),
    ..Default::default()
};
edit_metadata(
    &working_copy_path,
    &SecretString::from("…"),
    None,
    "12345678-90ab-cdef-1234-567890abcdef",
    &patch,
)?;
```

CLI usage:

```sh
# Read-only projection (no Password / Protected fields)
devboy secrets kdbx describe-metadata \
  --file ~/path/to/your.kdbx \
  --uuid 12345678-90ab-cdef-1234-567890abcdef \
  --json

# Patch — title / username / url / notes are scalar (empty clears),
# --tag is repeatable (or --clear-tags), --expires-at sets, --no-expiry
# clears. Writes to a derived working-copy path that's printed on
# success.
devboy secrets kdbx edit-metadata \
  --file ~/path/to/your.kdbx \
  --uuid 12345678-90ab-cdef-1234-567890abcdef \
  --notes "rotation runbook: ops-wiki/rotations#42" \
  --tag api --tag prod --tag rotated-q1 \
  --expires-at 2027-01-15T00:00:00Z
```

MCP tools (`kdbx_describe_metadata`, `kdbx_edit_metadata`) take the same
arguments as the CLI minus the passphrase prompt; the passphrase comes
from `DEVBOY_KDBX_PASSPHRASE` (refused if missing or empty).

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
