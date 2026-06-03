//! Router configuration loader per [ADR-021] §2.
//!
//! The router maps an ADR-020 path to a `(source, reference)` pair.
//! Its configuration is global and lives at
//! `<config_dir>/devboy-tools/secrets/sources.toml` (the ADR text
//! abbreviates this to `~/.devboy/secrets/sources.toml`).
//!
//! This module parses and **validates** that file. The actual
//! resolution algorithm — "which source serves this path?" — is
//! the next phase (P5.3) and lives in `crate::router_resolve`.
//! Splitting parse from resolve keeps the loader testable in
//! isolation and lets the config be inspected by `doctor` without
//! committing to a runtime decision.
//!
//! ## File layout
//!
//! ```toml
//! # Source definitions — one per backend instance.
//! [[source]]
//! name = "keychain"
//! type = "keychain"
//!
//! [[source]]
//! name = "1p-personal"
//! type = "1password"
//! account = "personal.example.1password.com"
//!
//! [[source]]
//! name = "vault-team"
//! type = "vault"
//! addr  = "https://vault.example.internal/"
//! mount = "secret"
//!
//! # The default route — used when no [[route]] prefix matches.
//! [default]
//! source   = "keychain"
//! fallback = "local-vault"          # optional, see ADR-021 §8
//!
//! # Prefix routes — longest match wins.
//! [[route]]
//! prefix = "team/"
//! source = "vault-team"
//! mount  = "secret/data/team"        # source-specific extra
//!
//! # Per-secret override — explicit (source, reference) for one path.
//! [secret."client-acme/jira/api-key"]
//! source    = "1p-personal"
//! reference = "op://Work/Acme Jira/credential"
//! ```
//!
//! Per-source and per-route extra fields are kept verbatim as
//! `toml::Value`; concrete source plugins (P6) parse them into
//! their own typed config when they're constructed. The router
//! itself never inspects them.
//!
//! ## Validation
//!
//! [`RouterConfig::parse`] returns a typed config when:
//!
//! - source names are non-empty and `^[a-z0-9][a-z0-9_-]*$`,
//! - no two `[[source]]` blocks share a name,
//! - `default.source` and `default.fallback` (when set) reference
//!   defined sources,
//! - every `[[route]].source` references a defined source,
//! - every `[[route]].prefix` ends with `/`,
//! - no two `[[route]]` blocks share a prefix,
//! - every `[secret."<path>"]` key parses as a [`SecretPath`],
//! - every `[secret."<path>"].source` references a defined source.
//!
//! Anything else is left to P5.3 / P5.5 (e.g. the source-credential
//! recursion check).
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::index::SECRETS_SUBDIR;
use crate::secret_path::{PathError, SecretPath};
use crate::source::Capabilities;

/// Filename of the router config inside [`SECRETS_SUBDIR`].
pub const SOURCES_FILENAME: &str = "sources.toml";

// =============================================================================
// Errors
// =============================================================================

/// Failure modes when loading or validating a [`RouterConfig`].
#[derive(Debug, Error)]
pub enum RouterConfigError {
    /// `dirs::config_dir()` returned `None` — extremely unusual on
    /// supported platforms but worth a typed variant rather than a
    /// panic.
    #[error("could not resolve the user's config directory")]
    NoConfigDir,

    /// I/O error reading the config file.
    #[error("failed to read {}: {source}", path.display())]
    Read {
        /// Path the loader tried to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// TOML deserialization error.
    #[error("failed to parse {}: {source}", path.display())]
    Parse {
        /// Path the loader tried to parse.
        path: PathBuf,
        /// Underlying parse error.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// Source name failed the identifier regex (kebab-case allowed).
    #[error("invalid source name '{name}': {reason}")]
    BadSourceName {
        /// The offending name.
        name: String,
        /// Human-readable detail.
        reason: String,
    },

    /// Two `[[source]]` blocks share a name.
    #[error("source '{name}' is defined more than once")]
    DuplicateSource {
        /// The duplicated name.
        name: String,
    },

    /// `[default].source` references an undefined source.
    #[error("[default].source = '{name}' references an undefined source")]
    UndefinedDefaultSource {
        /// The unresolved source name.
        name: String,
    },

    /// `[default].fallback` references an undefined source.
    #[error("[default].fallback = '{name}' references an undefined source")]
    UndefinedFallbackSource {
        /// The unresolved source name.
        name: String,
    },

    /// `[[route]].source` references an undefined source.
    #[error("[[route]] for prefix '{prefix}' references undefined source '{name}'")]
    UndefinedRouteSource {
        /// Prefix of the route in question.
        prefix: String,
        /// The unresolved source name.
        name: String,
    },

    /// `[secret."<path>"].source` references an undefined source.
    #[error("[secret.\"{path}\"] references undefined source '{name}'")]
    UndefinedSecretSource {
        /// Path key of the override.
        path: String,
        /// The unresolved source name.
        name: String,
    },

    /// `[[route]].prefix` does not end with `/`.
    #[error("[[route]].prefix '{prefix}' must end with '/'")]
    BadRoutePrefix {
        /// The offending prefix.
        prefix: String,
    },

    /// Two `[[route]]` blocks share a prefix.
    #[error("[[route]].prefix '{prefix}' is declared more than once")]
    DuplicateRoutePrefix {
        /// The duplicated prefix.
        prefix: String,
    },

    /// `[secret."<path>"]` key is not a valid ADR-020 path.
    #[error("[secret.\"{path}\"] is not a valid secret path: {source}")]
    BadSecretPath {
        /// The offending key.
        path: String,
        /// Underlying path-validation error.
        #[source]
        source: PathError,
    },
}

// =============================================================================
// Public types
// =============================================================================

/// Parsed + validated router configuration.
///
/// Built by [`RouterConfig::load`] / [`RouterConfig::parse`]. The
/// router (P5.3) consumes the typed view; `doctor` (P7.2) reports
/// on it.
//
// Eq is intentionally NOT derived: `toml::Value` only implements
// `PartialEq` (its `Float` variant has NaN-style ambiguity), and
// `Eq` propagates through `BTreeMap<String, toml::Value>`.
// `PartialEq` is enough for tests that compare configs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouterConfig {
    /// All `[[source]]` blocks in declaration order.
    pub sources: Vec<SourceDefinition>,
    /// `[default]`, if present. Optional because a config may
    /// declare only routes / per-secret overrides and leave the
    /// default unset (in which case unmatched paths fail with a
    /// "no route" error at resolution time).
    pub default: Option<DefaultRoute>,
    /// All `[[route]]` blocks in declaration order. The router
    /// picks the longest matching prefix at resolution time.
    pub routes: Vec<RouteRule>,
    /// All `[secret."<path>"]` blocks. Ordered by path for stable
    /// iteration in `doctor` output and tests.
    pub secret_overrides: BTreeMap<SecretPath, SecretOverride>,
}

/// Access mode for one `[[source]]` — a capability mask
/// layered over whatever the source plugin declares.
///
/// A source plugin advertises a static [`Capabilities`] set
/// (`local-vault` is `READ | LIST | VALIDATE | WRITE | ROTATE
/// | …`). The `access` key in `[[source]]` lets an operator
/// *narrow* that per config without touching the plugin:
/// mount the team's shared vault `read` everywhere, leave
/// `readwrite` only on the box that owns rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceAccess {
    /// Read-only — the effective capability set is the
    /// plugin's declared set AND-ed with `READ | LIST |
    /// VALIDATE`. `WRITE` / `ROTATE` are masked off even when
    /// the plugin supports them.
    Read,
    /// Full access — the plugin's declared capability set is
    /// used unchanged. This is the default when `access` is
    /// omitted, so existing configs keep their behaviour.
    #[default]
    ReadWrite,
}

impl SourceAccess {
    /// Apply this access mode to a source plugin's `declared`
    /// capability set, returning the *effective* set the
    /// router / UI should honour.
    pub fn mask(self, declared: Capabilities) -> Capabilities {
        match self {
            SourceAccess::ReadWrite => declared,
            SourceAccess::Read => {
                declared & (Capabilities::READ | Capabilities::LIST | Capabilities::VALIDATE)
            }
        }
    }
}

/// One `[[source]]` block.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceDefinition {
    /// Stable name; used everywhere else in the config.
    pub name: String,
    /// `type` field. Renamed because `type` is a reserved word in
    /// Rust.
    pub source_type: String,
    /// Access mask — `read` or `readwrite` (default). Narrows
    /// the source plugin's declared capabilities; see
    /// [`SourceAccess`].
    pub access: SourceAccess,
    /// Any additional fields the source plugin understands
    /// (`account`, `addr`, `mount`, `file`, …). Stored verbatim;
    /// the router does not inspect them.
    pub settings: BTreeMap<String, toml::Value>,
}

impl SourceDefinition {
    /// The capability set the router / UI should honour for
    /// this source — the plugin's `declared` set narrowed by
    /// the configured [`SourceAccess`].
    pub fn effective_capabilities(&self, declared: Capabilities) -> Capabilities {
        self.access.mask(declared)
    }
}

/// `[default]` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultRoute {
    /// Source name to dispatch to when no `[[route]]` matches.
    pub source: String,
    /// Source name to fall back to when `default.source` reports
    /// `is_available() == NotInstalled`. Per ADR-021 §2 step 3.
    pub fallback: Option<String>,
}

/// One `[[route]]` block.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteRule {
    /// Path prefix. The router matches by longest prefix; ties go
    /// to declaration order (resolved in P5.3).
    pub prefix: String,
    /// Source name to dispatch to.
    pub source: String,
    /// Source-specific extras (e.g. `mount`, `vault`). Stored
    /// verbatim.
    pub settings: BTreeMap<String, toml::Value>,
}

/// `[secret."<path>"]` block — explicit override for one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretOverride {
    /// Source name to dispatch to.
    pub source: String,
    /// Backend-specific reference string handed to the source's
    /// `get` (e.g. `op://Work/Acme Jira/credential`).
    pub reference: String,
}

// =============================================================================
// Loading
// =============================================================================

impl RouterConfig {
    /// Resolve the canonical on-disk path
    /// (`<config_dir>/devboy-tools/secrets/sources.toml`).
    pub fn default_path() -> Result<PathBuf, RouterConfigError> {
        let dir = dirs::config_dir().ok_or(RouterConfigError::NoConfigDir)?;
        Ok(dir
            .join("devboy-tools")
            .join(SECRETS_SUBDIR)
            .join(SOURCES_FILENAME))
    }

    /// Load the config from the canonical default path. Returns an
    /// empty (no-sources) config if the file does not exist — the
    /// router config is opt-in.
    pub fn load() -> Result<Self, RouterConfigError> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Load from a specific path. Returns an empty config if the
    /// file does not exist; otherwise reads, parses, and validates.
    pub fn load_from(path: &Path) -> Result<Self, RouterConfigError> {
        if !path.exists() {
            debug!(path = ?path, "router config not present, using empty");
            return Ok(Self::default());
        }
        let body = fs::read_to_string(path).map_err(|e| RouterConfigError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::parse(&body).map_err(|e| match e {
            RouterConfigError::Parse { source, .. } => RouterConfigError::Parse {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })
    }

    /// Parse + validate from an in-memory TOML string. Path-aware
    /// errors that expose a [`PathBuf`] (`Read`, `Parse`) carry a
    /// placeholder `<inline>` from this entry point — use
    /// [`Self::load_from`] when you want them to point at the real
    /// file.
    pub fn parse(toml_body: &str) -> Result<Self, RouterConfigError> {
        let raw: RawConfig = toml::from_str(toml_body).map_err(|e| RouterConfigError::Parse {
            path: PathBuf::from("<inline>"),
            source: Box::new(e),
        })?;
        raw.into_validated()
    }
}

// =============================================================================
// Raw (serde) layer
// =============================================================================

#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    #[serde(default, rename = "source")]
    sources: Vec<RawSource>,
    #[serde(default)]
    default: Option<RawDefault>,
    #[serde(default, rename = "route")]
    routes: Vec<RawRoute>,
    #[serde(default)]
    secret: HashMap<String, RawSecret>,
}

#[derive(Debug, Deserialize)]
struct RawSource {
    name: String,
    #[serde(rename = "type")]
    source_type: String,
    /// `read` / `readwrite`; defaults to `readwrite` when
    /// omitted. Declared as an explicit field so the
    /// `#[serde(flatten)]` below does NOT sweep it into
    /// `settings`.
    #[serde(default)]
    access: SourceAccess,
    #[serde(flatten)]
    settings: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawDefault {
    source: String,
    fallback: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRoute {
    prefix: String,
    source: String,
    #[serde(flatten)]
    settings: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawSecret {
    source: String,
    reference: String,
}

impl RawConfig {
    fn into_validated(self) -> Result<RouterConfig, RouterConfigError> {
        // 1) Sources: validate names, reject duplicates.
        let mut seen_names = HashSet::new();
        let mut sources = Vec::with_capacity(self.sources.len());
        for raw in self.sources {
            validate_source_name(&raw.name)?;
            if !seen_names.insert(raw.name.clone()) {
                return Err(RouterConfigError::DuplicateSource { name: raw.name });
            }
            sources.push(SourceDefinition {
                name: raw.name,
                source_type: raw.source_type,
                access: raw.access,
                settings: raw.settings,
            });
        }

        // 2) Default.
        let default = self
            .default
            .map(|d| {
                if !seen_names.contains(&d.source) {
                    return Err(RouterConfigError::UndefinedDefaultSource {
                        name: d.source.clone(),
                    });
                }
                if let Some(f) = &d.fallback
                    && !seen_names.contains(f)
                {
                    return Err(RouterConfigError::UndefinedFallbackSource { name: f.clone() });
                }
                Ok(DefaultRoute {
                    source: d.source,
                    fallback: d.fallback,
                })
            })
            .transpose()?;

        // 3) Routes: prefixes must end with '/', no duplicates,
        //    sources must be defined.
        let mut seen_prefixes = HashSet::new();
        let mut routes = Vec::with_capacity(self.routes.len());
        for raw in self.routes {
            if !raw.prefix.ends_with('/') {
                return Err(RouterConfigError::BadRoutePrefix { prefix: raw.prefix });
            }
            if !seen_prefixes.insert(raw.prefix.clone()) {
                return Err(RouterConfigError::DuplicateRoutePrefix { prefix: raw.prefix });
            }
            if !seen_names.contains(&raw.source) {
                return Err(RouterConfigError::UndefinedRouteSource {
                    prefix: raw.prefix,
                    name: raw.source,
                });
            }
            routes.push(RouteRule {
                prefix: raw.prefix,
                source: raw.source,
                settings: raw.settings,
            });
        }

        // 4) Per-secret overrides: paths must parse, sources must
        //    be defined. `parse_internal` is used (not `parse`) so
        //    users can explicitly route source-credentials living
        //    under `__sources/` (per ADR-021 §5) — the recursion
        //    check (P5.5) ultimately enforces those land on a
        //    credential-free source.
        let mut secret_overrides = BTreeMap::new();
        for (path_str, raw) in self.secret {
            let parsed = SecretPath::parse_internal(&path_str).map_err(|source| {
                RouterConfigError::BadSecretPath {
                    path: path_str.clone(),
                    source,
                }
            })?;
            if !seen_names.contains(&raw.source) {
                return Err(RouterConfigError::UndefinedSecretSource {
                    path: path_str,
                    name: raw.source,
                });
            }
            secret_overrides.insert(
                parsed,
                SecretOverride {
                    source: raw.source,
                    reference: raw.reference,
                },
            );
        }

        Ok(RouterConfig {
            sources,
            default,
            routes,
            secret_overrides,
        })
    }
}

// =============================================================================
// Validators
// =============================================================================

fn validate_source_name(name: &str) -> Result<(), RouterConfigError> {
    if name.is_empty() {
        return Err(RouterConfigError::BadSourceName {
            name: name.to_owned(),
            reason: "must be non-empty".into(),
        });
    }
    let first = name.as_bytes()[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(RouterConfigError::BadSourceName {
            name: name.to_owned(),
            reason: "must start with a lowercase letter or a digit".into(),
        });
    }
    for c in name.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
        if !ok {
            return Err(RouterConfigError::BadSourceName {
                name: name.to_owned(),
                reason: format!(
                    "invalid character '{c}' (allowed: lowercase letters, digits, '-', '_')"
                ),
            });
        }
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- Parsing happy path ---------------------------------------------

    #[test]
    fn parse_minimal_config_with_only_sources() {
        let cfg = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"
            "#,
        )
        .unwrap();

        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.sources[0].name, "keychain");
        assert_eq!(cfg.sources[0].source_type, "keychain");
        assert!(cfg.sources[0].settings.is_empty());
        assert!(cfg.default.is_none());
        assert!(cfg.routes.is_empty());
        assert!(cfg.secret_overrides.is_empty());
    }

    // -- SourceAccess (W1) -------------------------------------

    #[test]
    fn source_access_defaults_to_readwrite_when_omitted() {
        let cfg = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.sources[0].access, SourceAccess::ReadWrite);
    }

    #[test]
    fn source_access_read_parses_and_does_not_leak_into_settings() {
        let cfg = RouterConfig::parse(
            r#"
            [[source]]
            name = "team-vault"
            type = "vault"
            access = "read"
            addr = "https://vault.example.invalid"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.sources[0].access, SourceAccess::Read);
        // `access` is an explicit field — it must NOT be swept
        // into the flattened settings map.
        assert!(!cfg.sources[0].settings.contains_key("access"));
        // …but other unknown keys still flow through to settings.
        assert!(cfg.sources[0].settings.contains_key("addr"));
    }

    #[test]
    fn source_access_readwrite_parses_explicitly() {
        let cfg = RouterConfig::parse(
            r#"
            [[source]]
            name = "rw"
            type = "local-vault"
            access = "readwrite"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.sources[0].access, SourceAccess::ReadWrite);
    }

    #[test]
    fn read_access_masks_off_write_and_rotate() {
        // local-vault declares the full set; `access = "read"`
        // must narrow it to READ | LIST | VALIDATE.
        let declared = Capabilities::READ
            | Capabilities::LIST
            | Capabilities::VALIDATE
            | Capabilities::WRITE
            | Capabilities::ROTATE;
        let masked = SourceAccess::Read.mask(declared);
        assert!(masked.contains(Capabilities::READ));
        assert!(masked.contains(Capabilities::LIST));
        assert!(masked.contains(Capabilities::VALIDATE));
        assert!(!masked.contains(Capabilities::WRITE));
        assert!(!masked.contains(Capabilities::ROTATE));
    }

    #[test]
    fn readwrite_access_passes_the_declared_set_through_unchanged() {
        let declared = Capabilities::READ | Capabilities::WRITE | Capabilities::ROTATE;
        assert_eq!(SourceAccess::ReadWrite.mask(declared), declared);
    }

    #[test]
    fn read_access_cannot_grant_a_capability_the_plugin_lacks() {
        // env-store only declares READ — masking with `read`
        // must not magically add LIST / VALIDATE.
        let declared = Capabilities::READ;
        let masked = SourceAccess::Read.mask(declared);
        assert_eq!(masked, Capabilities::READ);
        assert!(!masked.contains(Capabilities::LIST));
    }

    #[test]
    fn effective_capabilities_routes_through_the_access_mode() {
        let cfg = RouterConfig::parse(
            r#"
            [[source]]
            name = "ro"
            type = "vault"
            access = "read"
            "#,
        )
        .unwrap();
        let declared = Capabilities::READ | Capabilities::WRITE | Capabilities::ROTATE;
        let effective = cfg.sources[0].effective_capabilities(declared);
        assert!(effective.contains(Capabilities::READ));
        assert!(!effective.contains(Capabilities::WRITE));
    }

    #[test]
    fn bad_access_value_is_a_parse_error() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "x"
            type = "vault"
            access = "write-only"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, RouterConfigError::Parse { .. }));
    }

    #[test]
    fn parse_full_adr_021_example() {
        // Mirrors the example block in ADR-021 §2.
        let cfg = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [[source]]
            name = "local-vault"
            type = "local-vault"

            [[source]]
            name = "1p-personal"
            type = "1password"
            account = "personal.example.1password.com"

            [[source]]
            name = "vault-team"
            type = "vault"
            addr  = "https://vault.example.internal/"
            mount = "secret"

            [default]
            source   = "keychain"
            fallback = "local-vault"

            [[route]]
            prefix = "team/"
            source = "vault-team"
            mount  = "secret/data/team"

            [[route]]
            prefix = "personal/"
            source = "1p-personal"
            vault  = "Personal"

            [secret."client-acme/jira/api-key"]
            source    = "1p-personal"
            reference = "op://Work/Acme Jira/credential"
            "#,
        )
        .unwrap();

        assert_eq!(cfg.sources.len(), 4);
        let vault_team = cfg.sources.iter().find(|s| s.name == "vault-team").unwrap();
        assert_eq!(vault_team.source_type, "vault");
        assert_eq!(
            vault_team.settings.get("addr").unwrap().as_str().unwrap(),
            "https://vault.example.internal/"
        );

        let default = cfg.default.unwrap();
        assert_eq!(default.source, "keychain");
        assert_eq!(default.fallback.as_deref(), Some("local-vault"));

        assert_eq!(cfg.routes.len(), 2);
        assert_eq!(cfg.routes[0].prefix, "team/");
        assert_eq!(
            cfg.routes[0]
                .settings
                .get("mount")
                .unwrap()
                .as_str()
                .unwrap(),
            "secret/data/team"
        );

        let path = SecretPath::parse("client-acme/jira/api-key").unwrap();
        let ovr = cfg.secret_overrides.get(&path).unwrap();
        assert_eq!(ovr.source, "1p-personal");
        assert_eq!(ovr.reference, "op://Work/Acme Jira/credential");
    }

    // -- Source-name validation -----------------------------------------

    #[test]
    fn empty_source_name_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = ""
            type = "keychain"
            "#,
        )
        .unwrap_err();
        match err {
            RouterConfigError::BadSourceName { name, reason } => {
                assert_eq!(name, "");
                assert!(reason.contains("non-empty"));
            }
            other => panic!("expected BadSourceName, got {other:?}"),
        }
    }

    #[test]
    fn uppercase_source_name_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "Keychain"
            type = "keychain"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, RouterConfigError::BadSourceName { .. }));
    }

    #[test]
    fn source_name_starting_with_dash_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "-bad"
            type = "x"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, RouterConfigError::BadSourceName { .. }));
    }

    #[test]
    fn source_name_with_digit_first_accepted() {
        // The ADR uses `1p-personal` — make sure our validator
        // matches the spec.
        let cfg = RouterConfig::parse(
            r#"
            [[source]]
            name = "1p-personal"
            type = "1password"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.sources[0].name, "1p-personal");
    }

    #[test]
    fn duplicate_source_name_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "vault-x"
            type = "vault"

            [[source]]
            name = "vault-x"
            type = "vault"
            "#,
        )
        .unwrap_err();
        match err {
            RouterConfigError::DuplicateSource { name } => assert_eq!(name, "vault-x"),
            other => panic!("expected DuplicateSource, got {other:?}"),
        }
    }

    // -- Default reference checks --------------------------------------

    #[test]
    fn default_referencing_undefined_source_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "nope"
            "#,
        )
        .unwrap_err();
        match err {
            RouterConfigError::UndefinedDefaultSource { name } => assert_eq!(name, "nope"),
            other => panic!("expected UndefinedDefaultSource, got {other:?}"),
        }
    }

    #[test]
    fn default_fallback_referencing_undefined_source_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "keychain"
            fallback = "nope"
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RouterConfigError::UndefinedFallbackSource { .. }
        ));
    }

    // -- Route checks ----------------------------------------------------

    #[test]
    fn route_prefix_without_trailing_slash_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [[route]]
            prefix = "team"
            source = "vault-team"
            "#,
        )
        .unwrap_err();
        match err {
            RouterConfigError::BadRoutePrefix { prefix } => assert_eq!(prefix, "team"),
            other => panic!("expected BadRoutePrefix, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_route_prefix_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "vault-a"
            type = "vault"
            [[source]]
            name = "vault-b"
            type = "vault"

            [[route]]
            prefix = "team/"
            source = "vault-a"

            [[route]]
            prefix = "team/"
            source = "vault-b"
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RouterConfigError::DuplicateRoutePrefix { .. }
        ));
    }

    #[test]
    fn route_with_undefined_source_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [[route]]
            prefix = "team/"
            source = "vault-team"
            "#,
        )
        .unwrap_err();
        match err {
            RouterConfigError::UndefinedRouteSource { prefix, name } => {
                assert_eq!(prefix, "team/");
                assert_eq!(name, "vault-team");
            }
            other => panic!("expected UndefinedRouteSource, got {other:?}"),
        }
    }

    // -- Per-secret override checks --------------------------------------

    #[test]
    fn secret_override_with_invalid_path_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [secret."BAD"]
            source = "keychain"
            reference = "BAD"
            "#,
        )
        .unwrap_err();
        match err {
            RouterConfigError::BadSecretPath { path, .. } => assert_eq!(path, "BAD"),
            other => panic!("expected BadSecretPath, got {other:?}"),
        }
    }

    #[test]
    fn secret_override_with_undefined_source_rejected() {
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [secret."team/gitlab/token-deploy"]
            source = "nope"
            reference = "x"
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RouterConfigError::UndefinedSecretSource { .. }
        ));
    }

    // -- Loading ---------------------------------------------------------

    #[test]
    fn load_from_missing_file_returns_default_empty_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.toml");
        let cfg = RouterConfig::load_from(&path).unwrap();
        assert_eq!(cfg, RouterConfig::default());
    }

    #[test]
    fn load_from_invalid_toml_surfaces_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sources.toml");
        std::fs::write(&path, "[[ this is not toml").unwrap();
        let err = RouterConfig::load_from(&path).unwrap_err();
        match err {
            RouterConfigError::Parse { path: errpath, .. } => assert_eq!(errpath, path),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn load_from_valid_file_round_trips_through_parse() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sources.toml");
        std::fs::write(
            &path,
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "keychain"
            "#,
        )
        .unwrap();
        let cfg = RouterConfig::load_from(&path).unwrap();
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.default.as_ref().unwrap().source, "keychain");
    }

    // -- default_path ----------------------------------------------------

    #[test]
    fn default_path_lives_under_devboy_tools_secrets_sources_toml() {
        let p = RouterConfig::default_path().unwrap();
        let s = p.to_string_lossy();
        assert!(s.contains("devboy-tools"));
        assert!(
            s.ends_with(&format!("{SECRETS_SUBDIR}/{SOURCES_FILENAME}"))
                || s.ends_with(&format!("{SECRETS_SUBDIR}\\{SOURCES_FILENAME}"))
        );
    }
}
