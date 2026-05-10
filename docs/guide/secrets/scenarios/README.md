# Secret-framework BDD scenarios

Five Gherkin `.feature` files covering the user-facing behaviour of the secret framework — onboarding, approve-on-use, catalog URL lifecycle, the agent trust boundary, and the proposer noise-reduction series.

These are **executable specifications** in the BDD sense — every scenario states a concrete observable outcome a user (developer or AI agent) can verify by running the documented commands. They are not (yet) wired into a `cucumber-rs` test harness; the `.feature` files act as the written contract that the existing unit / integration / end-to-end test suites already cover (look for the named functions and reasons in `crates/devboy-cli/src/secrets_setup.rs`, `crates/devboy-token-catalog/src/lib.rs`, `crates/devboy-mcp/src/secrets_*.rs`).

## Files

| File | Covers |
|---|---|
| [`onboarding-wizard.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/onboarding-wizard.feature) | `devboy secrets setup --scan-only / --write-manifest / --resume` — the happy path, the resume contract, the catalog-driven proposer accuracy on a real project. |
| [`approve-on-use.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/approve-on-use.feature) | `approve_on_use = never / session / per-call` policy, dialog flow, `SessionApprovalCache` semantics, threat-model alignment (agent cannot escalate a deny). |
| [`catalog-url-source.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/catalog-url-source.feature) | `catalog add-url / status / refresh / forget / pin` — the full TOFU recovery + pin promotion flow. |
| [`agent-trust-boundary.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/agent-trust-boundary.feature) | "Agent never sees the value" enforced by `AgentSafeReply` marker + CI grep gate + negative test; covers every `secrets_*` MCP tool. |
| [`proposer-noise-reduction.feature`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/scenarios/proposer-noise-reduction.feature) | The five-step skip-list expansion (P1-P5) plus the catalog-driven precision (S2 + bundled catalogs) that took the proposer from 236 to 161 paths on the canonical demo project. |

## Why Gherkin

The BDD shape forces every behaviour into a `Given / When / Then` triple, which is invaluable when the surface spans CLI, MCP wire-format, GUI dialog, and a daemon over a UNIX socket. A reviewer who reads the `.feature` files can sanity-check that:

- the documented user actions cover every "happy path" the implementation pretends to support,
- the failure modes have explicit scenarios and aren't just "the test asserts an error",
- the trust boundary is stated as a positive contract ("agent receives only the verdict") rather than left implicit.

## When to add a new scenario

Add a `.feature` block any time:

- a CLI command grows a new flag whose semantics differ from the default (e.g. `add-url --pin` vs `add-url` alone),
- the MCP wire format gains a new field that the agent should understand,
- a new policy value lands on the manifest schema (e.g. extending `approve_on_use` with a `Project` or `Org` scope in a future epic).

Keep scenarios concrete — name actual env vars, paths, error reasons. The Examples table is the right place for breadth (P1-P5 outlines).
