# Local Vault: file format, recovery, and backup

Local vault is `devboy-tools`'s own encrypted local secret store. One file, three unlock paths, a recovery phrase for emergencies. This document explains the format, when to pick local-vault over the OS keychain or external sources, and how to back it up safely.

See [ADR-023](../architecture/adr/ADR-023-secret-store-ux-layer.md) §3.1–§3.3 and [ADR-021](../architecture/adr/ADR-021-secret-source-router.md) §4 for the full specification.

> Russian translation: [`ru/local-vault.md`](./ru/local-vault.md).

---

## When to use local-vault

| Scenario | Recommended source | Why |
|---|---|---|
| One developer, one machine, macOS / Win / Linux | **OS keychain** | Already there, nothing to install, biometric unlock out of the box. |
| A team with shared tokens (CI, deploy) | **1Password / Vault** | Centralised access, audit, rotation. |
| Server-side CI without an interactive login | **env-store** (env vars) or **vault** (HTTP KV v2) | No UI, no interactivity. |
| Local playground without keychain access (containers, devcontainers, bare Linux without gnome-keyring) | **local-vault** | One file — easy to mount into a container, no system-service dependency. |
| Developer juggling several identities (work / personal / sandbox) | **local-vault** + several files | One vault per context, switch without rewriting keychain entries. |
| Auto-agent in a headless environment where the OS keychain wants the screen unlocked | **local-vault** + passphrase from a safe env-var | No UI prompt, scriptable unlock. |

> **Main criterion**: local-vault is for cases where there is *no* good system store. If the OS keychain or 1Password is available, prefer them. Local-vault gives you portability and file-level control at the cost of owning backups and key rotation yourself.

## File format

The file lives at `~/.devboy/secrets/local-vault.dvb` by default. Binary layout (see `crates/devboy-vault-crypto/src/format.rs`):

```text
HEADER (53 bytes, fixed)
  MAGIC       [4]   = b"DVB1"          // sanity check + format version
  VERSION     [1]   = 0x01
  KDF_PARAMS [16]   = m_cost u32 LE,    // Argon2id memory cost (KiB)
                      t_cost u32 LE,    // iterations
                      p_cost u32 LE,    // parallelism
                      salt_len u32 LE
  SALT       [32]   = random 32 bytes (KDF salt for the passphrase envelope)

UNLOCK_ENVELOPES (length-prefixed TOML)
  [envelopes_len: u32 LE][envelopes_bytes...]
  // One envelope per unlock method. Each envelope independently
  // stores an encrypted copy of the vault-key.

ENTRIES_INDEX (length-prefixed TOML, metadata)
  [entries_len: u32 LE][entries_bytes...]
  // path, description, retrieval_url, expires_at, pattern_id, …
  // No values in the index — it is read without unlock.

AEAD_BLOBS (length-prefixed binary stream)
  [blobs_len: u64 LE][blob_bytes...]
  // Encrypted values, one per entry. AEAD: XChaCha20-Poly1305.
  // AAD is bound to the envelope kind so a blob cannot be moved
  // between incompatible keys.
```

The `DVB1` magic is enough for `file local-vault.dvb` to tell a vault from random bytes.

### Metadata is readable without unlock

`description`, `retrieval_url`, `expires_at`, `rotation_method`, and `pattern_id` are stored in the clear. This is intentional — `secrets list`, `doctor`, and discovery flows must work without a PIN prompt. Threat model: a local process with read access to the file already sees what `ls -la` exposes. Encrypting the metadata would add a prompt with no real protection.

## Unlock paths (envelopes)

One vault can support several unlock methods at once. Each method is a separate envelope that independently stores an encrypted copy of the `vault-key`. Any envelope unlocks the same blobs.

### 1. Passphrase envelope (default)

```text
unlock-key  = HKDF(Argon2id(passphrase, salt, KDF_PARAMS), info="devboy-vault-envelope:passphrase:v1")
wrapped_key = XChaCha20-Poly1305(unlock-key).encrypt(vault-key, aad="devboy-vault-envelope:passphrase:v1")
```

Default Argon2id parameters (`KdfParams::DEFAULT`): m=64 MiB, t=3, p=1, salt 32 B. On 2024-class hardware one unlock takes ~250 ms. Change parameters via `devboy secrets vault rekey` — the old salt is preserved so other envelopes keep working.

### 2. Keychain envelope (optional)

The OS keychain stores a fresh random 32-byte key; the envelope's `wrapped_key` is `XChaCha20-Poly1305(keychain_key).encrypt(vault-key)`. Convenient: the vault unlocks via a single fingerprint / PIN prompt from the OS, no passphrase typing.

Available on macOS / Windows / Linux with gnome-keyring; other platforms — passphrase + recovery phrase only.

### 3. Recovery phrase envelope (BIP39)

12-word BIP39 mnemonic, generated when the vault is created. The envelope uses `HKDF(BIP39_seed)` without Argon2id (the phrase is high-entropy on its own). The recovery phrase is the spare key for cases where:

- The passphrase is forgotten.
- The keychain entry is wiped (new OS, reinstall).
- The vault file moves to a machine without a keychain.

The recovery phrase is **never** stored automatically. The CLI prints it once when the vault is created; you decide where to put it (paper list, password manager, sealed envelope).

> **⚠ Without the recovery phrase: potentially permanent loss**: if the passphrase is forgotten *and* the keychain is unavailable *and* the recovery phrase was never saved — the vault cannot be unlocked. There is no key escrow in the system; brute-forcing Argon2id with default parameters takes years. Back up the recovery phrase separately from the vault file.

## Recovery: what to do when something breaks

### Scenario A — passphrase forgotten

1. If a keychain envelope exists → unlock via keychain:

   ```bash
   devboy secrets agent start --use-keychain
   devboy secrets vault rekey --new-passphrase
   ```

   `rekey` recreates the passphrase envelope with a new phrase; the keychain envelope stays valid.

2. If the keychain is also gone → recovery phrase:

   ```bash
   devboy secrets vault unlock --recovery
   # Type the 12 BIP39 words
   devboy secrets vault rekey --new-passphrase
   ```

3. If neither keychain nor recovery phrase is available → restore from a vault-file backup taken *before* the key was lost.

### Scenario B — vault file is corrupt

The magic is wrong / the TOML doesn't parse / a blob fails to decrypt:

1. Don't panic — do not write anything over the corrupt file.
2. Copy `~/.devboy/secrets/local-vault.dvb` somewhere safe (`local-vault.dvb.broken`).
3. Restore from the latest backup.
4. Run `devboy secrets validate`. Green → continue.
5. Investigate what corrupted the file: full disk, `kill -9` mid-write, manual editing. Atomic writes (`tmpfile + rename`) rule out interrupt-based corruption but not external tampering.

### Scenario C — migrate the vault to a new machine

The vault is one file, so migration is trivial:

```bash
# On the old machine
cp ~/.devboy/secrets/local-vault.dvb /tmp/vault-export.dvb

# Move over a secure channel (not email, not Slack)
scp /tmp/vault-export.dvb new-host:/tmp/

# On the new machine
mkdir -p ~/.devboy/secrets
mv /tmp/vault-export.dvb ~/.devboy/secrets/local-vault.dvb
chmod 600 ~/.devboy/secrets/local-vault.dvb
devboy secrets vault unlock --passphrase   # if you know the passphrase
# or
devboy secrets vault unlock --recovery     # recovery only
```

After unlocking on the new machine, add a fresh keychain envelope:

```bash
devboy secrets vault add-envelope --kind keychain
```

## Backups

The vault is one file, so backup = regular copy + recovery phrase stored separately.

### Minimum policy (one developer)

1. Once a day, cron / `systemd --user` copies `~/.devboy/secrets/local-vault.dvb` to encrypted storage (Time Machine, Backblaze, a Restic repo with its own password).
2. Recovery phrase on paper in a physically secure place *and* in a personal password manager (e.g. 1Password Personal). Never with the vault file.
3. Once a quarter — restore drill: deploy the backup to a clean machine/container and try unlocking through the recovery phrase.

### What **not** to do

- ❌ Put the vault and the recovery phrase in the same backup directory. If the backup leaks, everything leaks.
- ❌ Store the recovery phrase as a plaintext file in a repo / on a Git server. BIP39 is high-entropy, but a public leak turns it into a one-step unlock key.
- ❌ Send the vault file over email / messengers. AEAD protects values, but the KDF salt in the header leaks, which speeds up offline passphrase brute-force. Use a secure channel (`scp`, `magic-wormhole`, S3 with KMS).
- ❌ Use a weak passphrase. Argon2id slows brute-force, but `password123` still falls. Minimum: 4 random `diceware`/`xkpasswd` words or 16+ characters from a password manager.

### A good workflow

```bash
# Backup (Restic for transparent dedup)
restic -r s3:s3.amazonaws.com/<bucket>/<prefix> backup ~/.devboy/secrets/local-vault.dvb

# Quarterly drill
restic -r s3:... restore latest --target /tmp/restore
DEVBOY_VAULT_PATH=/tmp/restore/local-vault.dvb devboy secrets vault unlock --recovery
DEVBOY_VAULT_PATH=/tmp/restore/local-vault.dvb devboy secrets list
rm -rf /tmp/restore
```

## Security boundary

- **AEAD choice**: XChaCha20-Poly1305. The long nonce (24 bytes) lets us pick it randomly without a counter. Throughput is on par with ChaCha20.
- **AAD**: every ciphertext is bound to its envelope kind (`passphrase` / `keychain` / `recovery`). Moving a blob between incompatible envelopes is detected at decrypt time.
- **Zeroize**: `vault-key` and `unlock-key` are wrapped in `secrecy::SecretBox`, which calls `zeroize` on drop. Hand-off through FFI to keychain backends uses minimal lifetimes.
- **Lock posture**: after `idle_timeout` (default 60 s with no requests) the daemon zeroizes the in-memory keys. Unlock is required again.
- **Does NOT protect against**:
  - A root/admin local process that can read the vault file *and* scrape envelope unlock-keys from the daemon's memory.
  - CPU side-channel attacks (Spectre-class).
  - Social engineering (phishing the recovery phrase).

## See also

- [`onboarding.md`](./onboarding.md) — first-run install + source setup.
- [`token-catalog.md`](./token-catalog.md) — author per-provider procedure files (`kimi.json`, `openai.json`) the GUI binds to.
- [`catalog-url-sources.md`](./catalog-url-sources.md) — serve catalogs over the network with sha-pinning + audit log.
- [`agent-protocol.md`](./agent-protocol.md) — how AI agents talk to the vault through MCP.
- ADR-023 §3.1 (format) and §3.2–§3.3 (envelopes) — the formal spec.
- `crates/devboy-vault-crypto/` — source for the format, AEAD, and KDF.
