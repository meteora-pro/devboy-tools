---
name: devboy-setup
description: Walk the user through initial devboy configuration from scratch.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "setup devboy"
  - "configure devboy"
  - "initialise devboy"
tools:
  - doctor
  - config
---

# devboy-setup

> Placeholder content. Fleshed out in PR-B (#159). The frontmatter above is
> the real schema; the body below is a skeleton so the embedded source has
> a valid file to enumerate during PR-A.

## What this skill does

Walks a user through configuring `devboy-tools` from scratch: detecting
the environment, running `devboy init`, storing tokens in the OS keychain
(or the environment-variable fallback chain), and verifying with
`devboy test <provider>` and `devboy doctor`.

## Commands the skill relies on

- `devboy init --yes` — non-interactive bootstrap
- `devboy config set <key> <value>` — set a configuration key
- `devboy config set-secret <key>` — store a secret in the keychain
- `devboy doctor --format json` — diagnose the current state
- `devboy test <provider>` — verify provider connectivity
