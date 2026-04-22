---
id: ADR-XXX
title: Decision title
status: proposed          # proposed | accepted | deprecated | superseded
date: YYYY-MM-DD
deciders: ["Name1", "Name2"]
tags: ["tag1", "tag2"]
supersedes: null          # ADR-XXX if this one replaces a previous decision
superseded_by: null       # ADR-XXX if this one is replaced by a newer decision
---

# ADR-XXX: Decision title

## Status

**{proposed | accepted | deprecated | superseded}** (if `superseded`, follow with "by [ADR-XXX](./ADR-XXX-...md)"; keep frontmatter `status:` to one of the canonical values only)

## Context

What problem are we solving? Why was this decision needed?

- Background and motivation
- Constraints
- Requirements

## Decision

> **Decision:** {one-sentence summary of the decision}

Detailed description of the decision:

- Point 1
- Point 2

## Consequences

### Positive

- ✅ Benefit 1
- ✅ Benefit 2

### Negative

- ❌ Cost 1

### Risks

- ⚠️ Risk 1 — mitigation: ...

## Alternatives Considered

### Alternative 1: Name

**Description:** Brief description of the alternative.

**Why rejected:** Reason.

### Alternative 2: Name

**Description:** Brief description of the alternative.

**Why rejected:** Reason.

## Implementation

- **Issues:** #NNN, #MMM
- **PR:** #NNN
- **Code:** `path/to/implementation`

## References

- [Link 1](https://example.com)
- [Link 2](https://example.com)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| YYYY-MM-DD | Name | Initial version |
