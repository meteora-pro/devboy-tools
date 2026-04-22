---
id: ADR-016
title: Skills language adaptation
status: proposed
date: 2026-04-17
deciders: ["Andrei Mazniak"]
tags: ["skills", "localization", "future-work"]
supersedes: null
superseded_by: null
---

# ADR-016: Skills language adaptation

## Status

**proposed** — deliberately deferred. This ADR exists as a placeholder so the problem is tracked and the discussion has a home when we are ready to make a decision.

## Context

All OSS skills ship in English (ADR-012). In many teams the operators' day-to-day working language is not English, and the cost of constantly context-switching between Russian / German / Spanish / … user input and English skill output is real. A skill that ends with "Done — 3 issues created, 1 failed" is friendlier in the reader's native language; an explanation paragraph inside the SKILL.md body is easier to skim in their native language.

At the same time:

- Maintaining parallel localised SKILL.md files multiplies the ongoing work of keeping skills in sync with the tool bundle.
- Automatic translation at install time (via the agent's LLM) has quality and freshness trade-offs that we have not yet measured.
- Most users of `devboy-tools` today work in English-speaking environments or are fluent enough that the friction is manageable.

The decision is therefore **deferred until we have concrete data** — which teams hit the language wall, what they need localised (output only, body too, activation phrases?), and which adaptation model fits best.

## Decision

> **Decision:** Deferred. Do not localise skills today. Revisit this ADR when at least one of these triggers fires:
>
> 1. User feedback identifies language friction as a top adoption blocker.
> 2. An LLM translation path becomes cheap enough that round-tripping SKILL.md bodies at install time is a better experience than reading the English original.
> 3. A community contributor ships a compelling prototype we can build on.

Until then, skills ship in English, and users who want localised output can prompt the agent at invocation time ("reply in Russian") rather than having it baked into the skill.

## Likely future directions (for the record, not a commitment)

- **Output localisation only.** Skills stay in English; a wrapper at invocation time translates the final user-facing message. Smallest possible change.
- **Parallel SKILL.md per locale.** `SKILL.en.md`, `SKILL.ru.md`, … with a `language` field in frontmatter. Scales linearly with languages and requires ongoing translation work.
- **Translate-at-install.** The CLI asks the agent's LLM to translate SKILL.md on install to the user's preferred locale. Single source of truth, free localisation, quality depends on the LLM.
- **Localised activation only.** Keep bodies in English but localise the `activation:` phrases so agents that support trigger-based activation recognise native-language triggers.

## Consequences

### Positive

- ✅ We don't spend engineering on a problem we don't yet have data on
- ✅ Skill authoring stays a single-language exercise — contributors only write one version
- ✅ The placeholder gives the question a home instead of rediscovering it in every discussion

### Negative

- ❌ Non-English-speaking teams take a small UX hit until this is revisited
- ❌ Skills carry the implicit "English-only" expectation; anyone adding a skill today is making that assumption permanent unless this ADR supersedes it

## Alternatives Considered

At this stage we are deliberately not evaluating alternatives — the whole point of this ADR is to capture the deferral, not to pick a winner.

## Implementation

None until this ADR is superseded.

## References

- [ADR-012: Skills subsystem](./ADR-012-skills-subsystem.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-17 | Andrei Mazniak | Initial version — deferred placeholder |
