# Token catalog: how to author your own

The token catalog is a backend-driven UI feed: each provider lives in a JSON file, and `devboy secrets ui` renders the provision form by reading those files at runtime. Adding a new provider — or a new variant of an existing one (CN tier vs global tier vs subscription tier) — is a JSON edit, not a Rust patch.

This guide is for **catalog authors**: anyone shipping a provider entry, either upstream as a contribution to `devboy-tools` or downstream in their own team's repo.

> Schema reference: [`crates/devboy-token-catalog/schema/v1.json`](https://github.com/meteora-pro/devboy-tools/blob/main/crates/devboy-token-catalog/schema/v1.json) (JSON Schema 2020-12, point your IDE at it for autocomplete + inline validation).

## Why this exists

Most providers ship a single API key kind, and a single regex + retrieval URL is enough. Some don't:

- **Kimi** has CN (`api.moonshot.cn`), Global (`api.moonshot.ai`), and Coding (`api.kimi.com/coding`) tiers — separate consoles, separate billing, separate regex prefixes for the Coding tier.
- **AWS** has access-key pairs vs IAM role assumption vs SSO short-lived creds.
- **Hashicorp Cloud** has user tokens vs service-principal tokens vs Vault root tokens.
- Internal corporate providers often have prod / staging / sandbox endpoints with different procedures.

A single `pattern_id` per path can't capture this. The catalog formalises **per-provider variants** so the user picks the exact kind, sees the exact procedure for it, and the framework probes the exact endpoint.

## Discovery sources

`devboy-tools` walks three sources in order, **least-to-most specific**:

| Source | Path | Use |
|---|---|---|
| Bundled | compiled into the binary | Reference defaults shipped upstream (currently: Kimi). |
| User | `~/.devboy/secrets/catalog/*.json` | Per-machine entries the user maintains for themselves. |
| Project | `<project>/.devboy/secrets/catalog/*.json` | Team-shared entries, versioned with the project's manifest. |

Later sources **override** earlier ones on `provider_id` collision. So a project-scope `kimi.json` wins over the user one, which wins over the bundled default. This lets a team pin its own canonical Kimi procedure (e.g. with internal-tooling extras) without forking `devboy-tools`.

## File format

One file = one provider. The schema:

```json
{
  "$schema": "https://devboy-tools.dev/schemas/token-catalog/v1.json",
  "schema_version": 1,
  "provider_id": "<kebab-case-id>",
  "display_name": "Human-readable name",
  "description": "One-paragraph context shown above the variant list.",
  "variants": [
    {
      "id": "<provider>-<variant>",
      "display_name": "Variant name",
      "description": "What this variant is and when to pick it.",
      "format_regex": "^...$",
      "format_hint": "human-readable shape hint",
      "retrieval": {
        "console_url": "https://console.example.invalid/keys",
        "steps": [
          "Step one — clear, action-oriented.",
          "Step two."
        ],
        "notes": "Optional gotchas, scope requirements, billing caveats."
      },
      "liveness": {
        "kind": "http",
        "url": "https://api.example.invalid/v1/probe",
        "method": "GET",
        "auth": { "kind": "bearer" },
        "expect_status": 200
      },
      "rotation": {
        "method": "manual",
        "every_days": 90
      }
    }
  ]
}
```

### Required fields

- `schema_version` — pinned to `1`.
- `provider_id` — lowercase kebab-case, matches the filename without extension.
- `display_name` — non-empty.
- `variants` — at least one entry; each variant needs `id`, `display_name`, `description`, `retrieval`.
- `retrieval.console_url` + `retrieval.steps` (≥ 1 step).

### Optional fields

- `description` (provider-level)
- `format_regex` (Rust regex syntax, anchored)
- `format_hint` (the human-readable counterpart)
- `liveness` (HTTP probe — currently the only `kind`)
- `rotation` (cadence + method)
- `default_keychain_account` (overrides the default `account = path` convention)
- `retrieval.notes`

## Authoring a new provider

1. Pick a `provider_id` that doesn't collide with bundled defaults. Run `devboy secrets catalog list` (when the command lands) to see what's already loaded.
2. Drop `<provider>.json` into one of the three discovery dirs. For team-shared entries, choose `<repo>/.devboy/secrets/catalog/`.
3. Add the `$schema` reference at the top so your editor validates while you type:
   ```json
   { "$schema": "https://devboy-tools.dev/schemas/token-catalog/v1.json", ... }
   ```
4. Cover at least one variant. Most providers are single-variant; multi-variant is for those that genuinely have separate token kinds (region / tier / subscription).
5. Test with `devboy secrets catalog validate path/to/file.json` (when the command lands) — runs the same JSON-Schema validation the runtime does, plus URL liveness on `console_url` and `liveness.url`.

## Authoring a new variant for an existing provider

Pick the right discovery scope:

- **Upstream PR** — when the variant is a public, stable provider feature (e.g. Kimi adding a fourth subscription tier). Land it in `data/<provider>.json` of `crates/devboy-token-catalog/`.
- **User-scope** — when it's specific to your account (e.g. a beta program key your team got under NDA). Drop into `~/.devboy/secrets/catalog/<provider>.json`.
- **Project-scope** — when the team needs a custom shape (corporate-prefixed regex, internal proxy URL). Drop into `<repo>/.devboy/secrets/catalog/<provider>.json`.

Project-scope is the one most teams will touch. It's safe to commit alongside the manifest because the file contains no secret values — only metadata about *how* to obtain them.

## Worked example

`crates/devboy-token-catalog/data/kimi.json` ships three variants of the Moonshot AI provider (CN, Global, Coding). Each declares its own:

- `format_regex` — CN/Global share `^sk-[A-Za-z0-9]{32,}$`; Coding uses `^kc-[A-Za-z0-9]{40,}$`.
- `retrieval.console_url` — `platform.moonshot.cn` vs `platform.moonshot.ai` vs `kimi.com/dev`.
- `liveness.url` — `api.moonshot.cn` vs `api.moonshot.ai` vs `api.kimi.com/coding`.
- `retrieval.notes` — gotcha about not crossing keys between hosts (CN key against Global host = 401).

Read [the JSON file](https://github.com/meteora-pro/devboy-tools/blob/main/crates/devboy-token-catalog/data/kimi.json) end-to-end — it's the canonical reference for what a polished provider entry looks like.

## What the GUI does with it

1. Reads the catalog at startup (bundled + user + project, in that order).
2. When a user opens the provision dialog for a path whose `pattern_id` matches a variant id, the dialog uses the catalog's `format_regex` for live feedback and `liveness` for the on-Save HTTP probe.
3. The context card above the input renders `display_name`, `description`, `retrieval.console_url` as a hyperlink, and the `retrieval.steps` as a numbered list — straight out of the JSON, no per-provider Rust code.
4. When the catalog file at the project scope overrides the bundled one, the GUI shows a small chip indicating which source the variant came from. That keeps it auditable when a team pins its own version.

## Stability promise

`schema_version: 1` is stable. Future major bumps land as `v2.json` alongside, and the loader keeps reading both for at least one minor release before old files become an error. Authors can pin their files to a known schema by URL and get notified when they need to migrate.

## Sharing catalogs across a team or community

Once you have a working catalog file, the typical next step is sharing it with teammates so they don't have to re-author the same metadata. There are three layered options:

1. **Disk-share** — drop the JSON file into `~/.devboy/secrets/catalog/` on each developer machine (Ansible, dotfiles, `make install`). Simplest path; downside is keeping copies in sync after every edit.

2. **Project-scope override** — commit `<project>/.devboy/secrets/catalog/<provider>.json` to the project repo. Anyone who clones inherits the catalog automatically; the project-scope copy wins over user / bundled per the override precedence in [`token-catalog.md` §discovery-sources](#discovery-sources).

3. **URL source** — host the JSON on a Git provider's raw endpoint (or any HTTPS server) and let `devboy secrets catalog add-url` subscribe each developer machine. The `[[source]]` entry lands in `~/.devboy/secrets/catalog/sources.toml`; the loader fetches with sha-pin or TOFU, caches, and refreshes at the configured TTL. Right when more than one team uses the same provider knowledge.

### Recommended layout for a community / team catalog repo

If you're standing up a shared repo (think `devboy-catalog`, `<team>-secrets-catalog`, …), keep the layout simple so any consumer can `add-url` against any file without touching the repo:

```
your-org/devboy-catalog/
├── README.md          ← short pitch + the canonical raw URL pattern
├── LICENSE
├── anthropic.json
├── openai.json
├── gitlab.json
├── slack.json
├── …                  ← one file per provider, mirrors crates/devboy-token-catalog/data/
└── .github/workflows/
    └── validate.yml   ← runs `devboy secrets catalog validate <file>` on every PR
```

The `README.md` should give the consumer the exact subscribe command:

```bash
devboy secrets catalog add-url \
  https://raw.githubusercontent.com/your-org/devboy-catalog/main/anthropic.json \
  --pin <sha256>            # recommended for prod; or omit for TOFU
  --refresh-seconds 86400   # 24h is the default; lower for fast iteration
  --enable                  # only if this is the user's first URL source
```

Catalogs validated against the same `schema/v1.json` shipped here load cleanly into any `devboy` ≥ 0.27. Consumers see the URL-source entries as `url` origin in `devboy secrets catalog status`, alongside a `[pin:<sha8>…]` or `[tofu]` marker for the trust state.

**A separate canonical reference repo is tracked as issue #258** ([token-reference repo + crawler](https://github.com/meteora-pro/devboy-tools/issues/258)) — that effort owns the markdown-first procedure docs (where to obtain / rotate / revoke each token) plus a scheduled crawler to keep them fresh. The catalog JSONs documented here can live alongside or in their own repo; both are first-class.

## See also

- [`onboarding.md`](./onboarding.md) — first-run install + manifest setup.
- [`catalog-url-sources.md`](./catalog-url-sources.md) — serving the catalog over the network: opt-in flag, threat model, SHA pinning, TOFU, audit log.
- [`agent-protocol.md`](./agent-protocol.md) — MCP-side surface that consumes the catalog metadata.
- ADR-020 — secret manifest format (catalog is downstream of `pattern_id` declared there).
- ADR-023 §3.4 — UI provision dialog (where the catalog drives the form).
