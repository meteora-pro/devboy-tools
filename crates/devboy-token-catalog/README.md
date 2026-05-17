# devboy-token-catalog

Backend-driven token catalog for `devboy-tools`. Each provider lives in a JSON file under `~/.devboy/secrets/catalog/<provider>.json` (or shipped seeds in this crate's `data/` directory). One file declares N variants — a single token kind per region / tier / subscription — each with its own retrieval URL, regex, liveness probe, and step-by-step guide.

The `secrets ui` provision form reads this catalog and renders the form dynamically, so adding a new variant (or a new provider) is a JSON edit, not a Rust patch.

See `data/kimi.json` for a worked example with the CN, global, and coding variants of Moonshot.
