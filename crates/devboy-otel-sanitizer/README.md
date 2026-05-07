# devboy-otel-sanitizer

Generic, vendor-neutral OpenTelemetry sanitizer library.

Pluggable redaction middleware for OTLP traces, metrics, and logs.
Works with any OTLP backend — Honeycomb, Jaeger, Datadog, Langfuse,
self-hosted Collector, or local SQLite/JSONL sinks.

## Status

**Scaffold (issue #240)** — public API shape only. Rule engine,
validity filters, and default rule set arrive in subsequent commits.

## Public API

```rust
use devboy_otel_sanitizer::{Sanitizer, Rule, Strategy, Severity, load_default_rules};

let rules = load_default_rules()?;
let sanitizer = Sanitizer::new(rules)?;

// Inline redaction
let safe = sanitizer.sanitize_string("export AWS_SECRET=AKIA1234567890ABCDEF");

// Audit scan
let findings = sanitizer.scan(span_attribute_value);
```

## Design

See `docs/architecture/sanitizer-research.md` in the repo root for the
full design rationale, comparative analysis of prior art (gitleaks,
trufflehog, Microsoft Presidio, Datadog SDS, Sentry Relay), academic
foundations (Meli NDSS 2019), and algorithm choices.

Eight redaction strategies cover the practical space:

| Strategy | Use case |
|---|---|
| `allow` | Explicit allowlist (override broader deny rules) |
| `drop` | Remove the entire span / attribute |
| `hash` | Replace with `sha256(value)[..16]` for correlation without leak |
| `mask` | Replace with fixed string (e.g. `[REDACTED]`) |
| `regex_redact` | Replace only matched groups, keep surrounding context |
| `truncate` | Limit value length (path / large blob protection) |
| `scope_to_workspace` | `/Users/alice/...` → `<workspace>/...` |
| `entropy_filter` | Drop only if Shannon entropy > threshold |

## ML detection

Optional, **off by default**. Build with `--features=ml`, download a
model with `devboy models pull <name>`, and run with `devboy otel scan
--use-ml`. Three-layer opt-in keeps the default install lightweight
(<15 MB) and keeps the inline sanitization path deterministic.

## License

Apache-2.0.
