---
id: ADR-001
title: Apache 2.0 license for the project
status: accepted
date: 2026-01-12
deciders: ["Andrei Mazniak", "Mikhail Kitaev"]
tags: ["open-source", "legal"]
supersedes: null
superseded_by: null
---

# ADR-001: Apache 2.0 license for the project

## Status

**accepted**

## Context

`devboy-tools` is distributed as an Open Source project. Goals of open-sourcing:

- Increase trust in the code by making it fully auditable
- Transparent development
- Enable community contributions
- Attract users through adoption

The license must:

1. Allow commercial use (companies are a meaningful segment of future contributors)
2. Provide explicit patent protection
3. Be a recognised choice in enterprise environments
4. Not discourage contributors

## Decision

> **Decision:** License the project under **Apache License 2.0** across all source, docs, and build scripts.

Apache 2.0 was chosen because it provides:

- **Explicit patent grant** — Section 3 covers this, unlike MIT
- **Enterprise acceptance** — de-facto standard for infrastructure/backend projects
- **Permissive commercial use** — companies can ship it inside their products; this is what actually drives community upstream contributions
- **Track record** — Kubernetes, Docker, TensorFlow, Android and similar successful projects use Apache 2.0

## Consequences

### Positive

- ✅ Enterprises can adopt the code without legal review friction
- ✅ Community contributions are not blocked by copyleft constraints
- ✅ Patent retaliation clause protects downstream users
- ✅ Compatible with most other open-source licenses
- ✅ Well-known wording — reviewers recognise it immediately

### Negative

- ❌ A competitor can fork and sell a derivative without contributing back
- ❌ Not copyleft — downstream is not required to open-source their improvements

### Risks

- ⚠️ **Fork by a competitor** — mitigation: compete on brand, community, and execution speed rather than licensing
- ⚠️ **Patent trolls** — mitigation: Apache 2.0 revokes the patent grant to anyone who sues over patents in the covered code

## Alternatives Considered

### Alternative 1: MIT License

**Description:** A short, permissive license (~170 words).

**Why rejected:** No explicit patent grant — ambiguous for enterprise legal review. Apache 2.0 is effectively "MIT plus patent protection" with no meaningful downside.

### Alternative 2: GPL v3

**Description:** Copyleft license — derivatives must also be GPL-licensed.

**Why rejected:** Large cloud providers and many enterprise users avoid GPL dependencies outright, which would kill adoption. A license that forbids commercial use practically guarantees that commercial teams will not contribute improvements back.

### Alternative 3: AGPL v3

**Description:** GPL plus a network-use clause.

**Why rejected:** Even more restrictive. Many companies have blanket policies against AGPL dependencies, which would kill adoption.

### Alternative 4: SSPL (Server Side Public License)

**Description:** MongoDB's license — forbids third-party SaaS use of the software.

**Why rejected:** Not recognised by OSI as an open-source license. Cannot honestly call the project "Open Source" under this license.

### Alternative 5: Dual license (GPL + commercial)

**Description:** GPL for everyone, separate commercial license for purchase.

**Why rejected:** Requires a legal operation to maintain. Confusing for users. Adds friction to contribution (CLAs, copyright assignment).

## Implementation

- **Files:**
  - `LICENSE` at the repository root
  - Badge in `README.md`

**Source header (optional, not required by Apache 2.0):**

```
Copyright 2026 Meteora Pro

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```

## References

- [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
- [Choose a License: Apache 2.0](https://choosealicense.com/licenses/apache-2.0/)
- [Open Source Initiative](https://opensource.org/licenses/Apache-2.0)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-01-12 | Claude Code | Initial version |
| 2026-04-17 | Claude Code | Translated to English and brought into this repository |
