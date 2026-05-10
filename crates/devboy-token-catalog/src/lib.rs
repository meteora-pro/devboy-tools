//! Backend-driven token catalog. Loads per-provider JSON files
//! and exposes typed variants the `secrets ui` form binds to.
//!
//! ## Why JSON?
//!
//! The Rust pattern catalogue (`devboy-secret-patterns`) was
//! built for code-side classifiers — regex matchers, severity
//! tiers, leak scanners. It's compiled-in, single-valued per
//! pattern, and changing it is a code patch.
//!
//! The user-facing question — **"how do I get a Kimi key"** —
//! is multi-valued: Kimi alone has CN / global / coding
//! variants, each with its own console URL, API host, billing
//! flow, regex shape. The same provider keeps adding tiers
//! faster than we want to ship Rust patches.
//!
//! Hence the split: stay with `devboy-secret-patterns` for the
//! security classifiers; move *user-facing* data into JSON
//! files this crate loads at runtime.
//!
//! ## File layout
//!
//! ```text
//! ~/.devboy/secrets/catalog/
//!   kimi.json
//!   openai.json
//!   github.json
//!   ...
//! ```
//!
//! Each file is one [`ProviderCatalog`] with N [`TokenVariant`]s.
//! See `data/kimi.json` (shipped in this crate's repo) for the
//! canonical example.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// =============================================================================
// Schema
// =============================================================================

pub const SCHEMA_VERSION: u32 = 1;

/// One provider's catalog file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCatalog {
    /// JSON-Schema reference. Optional, ignored at load time —
    /// only there so editors that honour `$schema` can give
    /// authors autocomplete and inline validation.
    #[serde(default, rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Pinned to [`SCHEMA_VERSION`]. Future major bumps fail
    /// to load; minor changes go through `#[serde(default)]`.
    pub schema_version: u32,
    /// Stable identifier (`kimi`, `openai`, …). Matches the
    /// filename without extension.
    pub provider_id: String,
    /// Human-readable name shown in the variant picker.
    pub display_name: String,
    /// Short description shown above the variant list.
    #[serde(default)]
    pub description: Option<String>,
    /// All token variants this provider supports. One element
    /// = one form the GUI will render.
    pub variants: Vec<TokenVariant>,
}

/// One token kind — region, tier, subscription level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenVariant {
    /// Stable identifier scoped to the provider
    /// (`kimi-cn`, `kimi-global`, `kimi-coding`).
    pub id: String,
    /// Label rendered in the variant picker.
    pub display_name: String,
    /// One-line summary shown right under the picker.
    pub description: String,
    /// JS-flavored regex (Rust regex crate compatible) the
    /// value must match. Optional — some variants have no
    /// stable shape.
    #[serde(default)]
    pub format_regex: Option<String>,
    /// One-line shape hint shown above the input ("`sk-` +
    /// 32 alnum"). Independent of `format_regex` so the user
    /// sees something readable even when the regex is gnarly.
    #[serde(default)]
    pub format_hint: Option<String>,
    /// Retrieval / rotation procedure.
    pub retrieval: RetrievalSpec,
    /// Liveness probe to run on save (optional).
    #[serde(default)]
    pub liveness: Option<LivenessSpec>,
    /// Rotation cadence + method.
    #[serde(default)]
    pub rotation: Option<RotationSpec>,
    /// Default OS keychain entry name when the user opts to
    /// store via keychain (most variants use the ADR-020 path
    /// verbatim; some legacy providers want a different
    /// account). Optional.
    #[serde(default)]
    pub default_keychain_account: Option<String>,
}

/// How to obtain / rotate this token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalSpec {
    /// Provider's console / settings URL (`Open URL` button
    /// target).
    pub console_url: String,
    /// Numbered steps the user follows on the console UI.
    /// Rendered as a Markdown-ish ordered list in the
    /// provision dialog.
    pub steps: Vec<String>,
    /// Free-form notes shown below the steps (gotchas, scope
    /// requirements, billing caveats).
    #[serde(default)]
    pub notes: Option<String>,
}

/// HTTP probe definition. Mirrors
/// [`devboy_secret_patterns::LivenessSpec`] but is JSON-shaped
/// so it can ride in the catalog file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LivenessSpec {
    /// Currently always `"http"`. Reserved tag so future probe
    /// kinds (subprocess, socket) can be added without a
    /// breaking change.
    pub kind: String,
    pub url: String,
    /// HTTP verb — `GET`, `POST`, `HEAD`.
    #[serde(default = "default_method")]
    pub method: String,
    pub auth: AuthSpec,
    /// Status that means "secret valid".
    pub expect_status: u16,
}

fn default_method() -> String {
    "GET".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AuthSpec {
    /// `Authorization: Bearer <secret>`.
    Bearer,
    /// `Authorization: Basic base64(<secret>:)`.
    BasicUser,
    /// `Authorization: Basic base64(:<secret>)`.
    BasicPassword,
    /// Custom header carrying the raw secret.
    Header { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationSpec {
    /// `"manual"`, `"provider-ui"`, `"provider-api"`. Free
    /// string so future methods don't break the schema.
    pub method: String,
    pub every_days: u32,
}

// =============================================================================
// Loader
// =============================================================================

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("could not read catalog file at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed JSON in catalog file at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "schema version mismatch in {path}: file declares {found}, this build supports {SCHEMA_VERSION}"
    )]
    SchemaVersion { path: PathBuf, found: u32 },
}

/// Default user-scope catalog directory:
/// `$HOME/.devboy/secrets/catalog/`. Personal-machine entries
/// land here.
pub fn default_user_catalog_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".devboy").join("secrets").join("catalog"))
}

/// Default project-scope catalog directory, relative to a
/// repo root: `<root>/.devboy/secrets/catalog/`. Team-shared
/// catalogs live here so they get versioned alongside the
/// project's manifest.
pub fn default_project_catalog_dir(project_root: &Path) -> PathBuf {
    project_root.join(".devboy").join("secrets").join("catalog")
}

/// Where one catalog came from. Surfaced so the GUI can show
/// the origin next to each variant ("from your manifest" vs
/// "from the bundled defaults") and so a `validate` command
/// can group errors by source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CatalogSource {
    /// Bundled defaults shipped in the `devboy-tools` binary.
    Bundled,
    /// User-scope: `~/.devboy/secrets/catalog/`.
    User,
    /// Project-scope: `<project>/.devboy/secrets/catalog/`.
    Project,
    /// Remote-fetched via `sources.toml`. Carries the URL it
    /// was loaded from and the optional pinned SHA256 (when the
    /// user wrote one in the config). The fetcher (P23.2) and
    /// the GUI source-chip (P23.6) both consume these fields.
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
}

/// One loaded catalog plus its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCatalog {
    pub catalog: ProviderCatalog,
    pub source: CatalogSource,
    pub path: Option<PathBuf>,
}

/// Walk all configured catalog sources (bundled → user →
/// project), in **least-to-most specific** order. Later
/// sources win on `provider_id` collision — a project-scope
/// `kimi.json` overrides the user one, which overrides the
/// bundled default. Errors are isolated per file.
pub fn load_all(
    bundled: &[ProviderCatalog],
    user_dir: Option<&Path>,
    project_dir: Option<&Path>,
) -> (Vec<LoadedCatalog>, Vec<CatalogError>) {
    let mut loaded: Vec<LoadedCatalog> = bundled
        .iter()
        .cloned()
        .map(|c| LoadedCatalog {
            catalog: c,
            source: CatalogSource::Bundled,
            path: None,
        })
        .collect();
    let mut errors = Vec::new();

    if let Some(dir) = user_dir {
        let (cats, errs) = load_dir(dir);
        for c in cats {
            override_or_push(&mut loaded, c, CatalogSource::User, dir.to_path_buf());
        }
        errors.extend(errs);
    }
    if let Some(dir) = project_dir {
        let (cats, errs) = load_dir(dir);
        for c in cats {
            override_or_push(&mut loaded, c, CatalogSource::Project, dir.to_path_buf());
        }
        errors.extend(errs);
    }
    (loaded, errors)
}

fn override_or_push(
    loaded: &mut Vec<LoadedCatalog>,
    catalog: ProviderCatalog,
    source: CatalogSource,
    dir: PathBuf,
) {
    let path = Some(dir.join(format!("{}.json", catalog.provider_id)));
    if let Some(slot) = loaded
        .iter_mut()
        .find(|l| l.catalog.provider_id == catalog.provider_id)
    {
        slot.catalog = catalog;
        slot.source = source;
        slot.path = path;
    } else {
        loaded.push(LoadedCatalog {
            catalog,
            source,
            path,
        });
    }
}

/// Bundled provider catalogs shipped in the binary itself.
/// New providers land here as their canonical references
/// mature; downstream catalogs at user / project scope can
/// override any of these by `provider_id`.
pub fn bundled_catalogs() -> Vec<ProviderCatalog> {
    let mut out = Vec::new();
    for body in BUNDLED_SOURCES {
        if let Ok(c) = serde_json::from_str::<ProviderCatalog>(body) {
            out.push(c);
        }
    }
    out
}

const BUNDLED_KIMI: &str = include_str!("../data/kimi.json");
const BUNDLED_OPENAI: &str = include_str!("../data/openai.json");
const BUNDLED_GITHUB: &str = include_str!("../data/github.json");

/// Every bundled JSON catalog shipped in the binary. Order is
/// not load-bearing — `load_all` resolves overrides by source
/// scope (bundled < user < project), and within a scope
/// duplicate `provider_id`s would surface as a load error.
const BUNDLED_SOURCES: &[&str] = &[BUNDLED_KIMI, BUNDLED_OPENAI, BUNDLED_GITHUB];

/// Load every `*.json` file under `dir` as a [`ProviderCatalog`].
/// Errors are isolated per-file: one bad JSON file doesn't hide
/// the others. Returns `(loaded, errors)`.
pub fn load_dir(dir: &Path) -> (Vec<ProviderCatalog>, Vec<CatalogError>) {
    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    let Ok(read) = fs::read_dir(dir) else {
        return (loaded, errors);
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match load_file(&path) {
            Ok(c) => loaded.push(c),
            Err(e) => errors.push(e),
        }
    }
    (loaded, errors)
}

/// Load a single catalog file.
pub fn load_file(path: &Path) -> Result<ProviderCatalog, CatalogError> {
    let body = fs::read_to_string(path).map_err(|e| CatalogError::Read {
        path: path.to_path_buf(),
        source: e,
    })?;
    let cat: ProviderCatalog = serde_json::from_str(&body).map_err(|e| CatalogError::Parse {
        path: path.to_path_buf(),
        source: e,
    })?;
    if cat.schema_version != SCHEMA_VERSION {
        return Err(CatalogError::SchemaVersion {
            path: path.to_path_buf(),
            found: cat.schema_version,
        });
    }
    Ok(cat)
}

/// Find a variant by `<provider>.<variant>` pair. Returns
/// `None` when neither the provider nor the variant exists.
pub fn find_variant<'a>(
    catalogs: &'a [ProviderCatalog],
    provider_id: &str,
    variant_id: &str,
) -> Option<&'a TokenVariant> {
    catalogs
        .iter()
        .find(|c| c.provider_id == provider_id)?
        .variants
        .iter()
        .find(|v| v.id == variant_id)
}

/// Find every variant whose id matches — useful when the
/// caller has only the variant id (e.g. from `pattern_id` in
/// the manifest) and wants to discover the provider.
pub fn find_variant_by_id<'a>(
    catalogs: &'a [ProviderCatalog],
    variant_id: &str,
) -> Option<(&'a ProviderCatalog, &'a TokenVariant)> {
    for c in catalogs {
        if let Some(v) = c.variants.iter().find(|v| v.id == variant_id) {
            return Some((c, v));
        }
    }
    None
}

/// All variants from all loaded catalogs flattened. Useful for
/// the variant picker when the user hasn't pinned a specific
/// provider yet.
pub fn all_variants(catalogs: &[ProviderCatalog]) -> Vec<(&ProviderCatalog, &TokenVariant)> {
    catalogs
        .iter()
        .flat_map(|c| c.variants.iter().map(move |v| (c, v)))
        .collect()
}

// =============================================================================
// sources.toml — config for remote-URL catalog sources (P23)
// =============================================================================

/// Default refresh window for an unpinned URL source. 24h —
/// enough to catch upstream provider changes within a day, not
/// so eager that the loader hammers the server on every
/// startup.
fn default_refresh_seconds() -> u64 {
    86_400
}

/// Top-level config parsed from
/// `~/.devboy/secrets/catalog/sources.toml`. The fetcher (P23.2)
/// is **opt-in** — when [`enable_url_catalogs`] is `false`
/// (the default), URL sources are silently skipped even if the
/// `[[source]]` blocks are present, so a careless paste of a
/// malicious config in the user's home doesn't auto-activate
/// network fetches.
///
/// [`enable_url_catalogs`]: Self::enable_url_catalogs
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSourcesConfig {
    /// Master kill-switch. Defaults to `false` — URL sources
    /// require explicit user opt-in.
    #[serde(default)]
    pub enable_url_catalogs: bool,
    /// Each `[[source]]` block in the TOML maps to one entry.
    #[serde(default, rename = "source")]
    pub sources: Vec<UrlSource>,
}

/// One `[[source]]` block — a URL the fetcher should pull.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlSource {
    /// HTTPS URL of the JSON catalog. The fetcher (P23.2)
    /// rejects `http://`.
    pub url: String,
    /// Pinned SHA256 of the expected body, hex-encoded
    /// lowercase, no `sha256:` prefix. When set, the fetcher
    /// compares against this exact value and refuses any
    /// mismatch. When unset, the loader uses TOFU
    /// (`known_hashes.toml`, P23.3) instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// How long a cached body stays fresh before the loader
    /// re-fetches. Defaults to 24h.
    #[serde(default = "default_refresh_seconds")]
    pub refresh_seconds: u64,
}

/// Default `sources.toml` location. Same directory as user-
/// scope catalog JSONs so there's only one place to look.
pub fn default_sources_toml_path() -> Option<PathBuf> {
    default_user_catalog_dir().map(|d| d.join("sources.toml"))
}

/// Parse a `sources.toml` body. Pure — no I/O. Validates each
/// URL starts with `https://` and that pinned SHA256 strings
/// are 64 hex chars; returns a structured error per-source on
/// shape violations so the fetcher can surface them in the
/// audit log without aborting the whole load.
pub fn parse_sources_toml(body: &str) -> Result<CatalogSourcesConfig, SourcesConfigError> {
    let cfg: CatalogSourcesConfig =
        toml::from_str(body).map_err(|source| SourcesConfigError::Parse { source })?;
    for (idx, src) in cfg.sources.iter().enumerate() {
        if !src.url.starts_with("https://") {
            return Err(SourcesConfigError::HttpsRequired {
                index: idx,
                url: src.url.clone(),
            });
        }
        if let Some(sha) = src.sha256.as_deref()
            && !is_valid_sha256_hex(sha)
        {
            return Err(SourcesConfigError::InvalidSha256 {
                index: idx,
                value: sha.to_owned(),
            });
        }
    }
    Ok(cfg)
}

/// Errors surfaced by [`parse_sources_toml`].
#[derive(Debug, Error)]
pub enum SourcesConfigError {
    #[error("malformed sources.toml: {source}")]
    Parse {
        #[source]
        source: toml::de::Error,
    },
    #[error("source #{index} URL must start with https:// (got `{url}`)")]
    HttpsRequired { index: usize, url: String },
    #[error("source #{index} sha256 must be exactly 64 lowercase hex chars (got `{value}`)")]
    InvalidSha256 { index: usize, value: String },
}

fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> ProviderCatalog {
        ProviderCatalog {
            schema: None,
            schema_version: SCHEMA_VERSION,
            provider_id: "kimi".into(),
            display_name: "Kimi (Moonshot AI)".into(),
            description: Some("LLM API with CN, global, and coding tiers.".into()),
            variants: vec![TokenVariant {
                id: "kimi-cn".into(),
                display_name: "Kimi (CN)".into(),
                description: "Mainland-China tier".into(),
                format_regex: Some(r"^sk-[A-Za-z0-9]{32,}$".into()),
                format_hint: Some("sk- + 32 alnum".into()),
                retrieval: RetrievalSpec {
                    console_url: "https://platform.moonshot.cn/console/api-keys".into(),
                    steps: vec!["Sign in".into(), "Create key".into()],
                    notes: None,
                },
                liveness: Some(LivenessSpec {
                    kind: "http".into(),
                    url: "https://api.moonshot.cn/v1/models".into(),
                    method: "GET".into(),
                    auth: AuthSpec::Bearer,
                    expect_status: 200,
                }),
                rotation: Some(RotationSpec {
                    method: "manual".into(),
                    every_days: 90,
                }),
                default_keychain_account: None,
            }],
        }
    }

    #[test]
    fn round_trip_through_json() {
        let cat = fixture();
        let body = serde_json::to_string_pretty(&cat).unwrap();
        let back: ProviderCatalog = serde_json::from_str(&body).unwrap();
        assert_eq!(cat, back);
    }

    #[test]
    fn rejects_unknown_fields() {
        let body = r#"{"schema_version":1,"provider_id":"x","display_name":"X","variants":[],"unknown":true}"#;
        let r: Result<ProviderCatalog, _> = serde_json::from_str(body);
        assert!(r.is_err());
    }

    #[test]
    fn load_dir_isolates_per_file_errors() {
        let dir = TempDir::new().unwrap();
        // Good file.
        let good = serde_json::to_string(&fixture()).unwrap();
        std::fs::write(dir.path().join("kimi.json"), good).unwrap();
        // Bad file.
        std::fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
        // Non-JSON file (ignored).
        std::fs::write(dir.path().join("README.md"), "# unrelated").unwrap();

        let (loaded, errors) = load_dir(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].provider_id, "kimi");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn schema_version_mismatch_is_caught() {
        let dir = TempDir::new().unwrap();
        let body = r#"{"schema_version":99,"provider_id":"x","display_name":"X","variants":[]}"#;
        let path = dir.path().join("x.json");
        std::fs::write(&path, body).unwrap();
        let err = load_file(&path).unwrap_err();
        assert!(matches!(err, CatalogError::SchemaVersion { found: 99, .. }));
    }

    #[test]
    fn find_variant_by_id_walks_all_catalogs() {
        let cats = vec![fixture()];
        let hit = find_variant_by_id(&cats, "kimi-cn");
        assert!(hit.is_some());
        let miss = find_variant_by_id(&cats, "doesnt-exist");
        assert!(miss.is_none());
    }

    #[test]
    fn sources_toml_empty_body_yields_defaults() {
        let cfg = parse_sources_toml("").unwrap();
        assert!(!cfg.enable_url_catalogs);
        assert!(cfg.sources.is_empty());
    }

    #[test]
    fn sources_toml_full_block_parses() {
        let body = r#"
            enable_url_catalogs = true

            [[source]]
            url = "https://example.invalid/catalog.json"
            sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            refresh_seconds = 3600
        "#;
        let cfg = parse_sources_toml(body).unwrap();
        assert!(cfg.enable_url_catalogs);
        assert_eq!(cfg.sources.len(), 1);
        let s = &cfg.sources[0];
        assert_eq!(s.url, "https://example.invalid/catalog.json");
        assert_eq!(s.refresh_seconds, 3600);
        assert!(s.sha256.is_some());
    }

    #[test]
    fn sources_toml_url_only_uses_default_refresh() {
        let body = r#"
            [[source]]
            url = "https://example.invalid/x.json"
        "#;
        let cfg = parse_sources_toml(body).unwrap();
        assert_eq!(cfg.sources[0].refresh_seconds, 86_400);
        assert!(cfg.sources[0].sha256.is_none());
    }

    #[test]
    fn sources_toml_rejects_http_scheme() {
        let body = r#"
            [[source]]
            url = "http://example.invalid/x.json"
        "#;
        let err = parse_sources_toml(body).unwrap_err();
        assert!(matches!(err, SourcesConfigError::HttpsRequired { .. }));
    }

    #[test]
    fn sources_toml_rejects_malformed_sha256() {
        let body = r#"
            [[source]]
            url = "https://example.invalid/x.json"
            sha256 = "tooshort"
        "#;
        let err = parse_sources_toml(body).unwrap_err();
        assert!(matches!(err, SourcesConfigError::InvalidSha256 { .. }));
    }

    #[test]
    fn sources_toml_rejects_unknown_fields() {
        let body = r#"
            [[source]]
            url = "https://example.invalid/x.json"
            mystery = true
        "#;
        let err = parse_sources_toml(body).unwrap_err();
        assert!(matches!(err, SourcesConfigError::Parse { .. }));
    }

    #[test]
    fn every_bundled_catalog_parses() {
        let cats = bundled_catalogs();
        let ids: Vec<&str> = cats.iter().map(|c| c.provider_id.as_str()).collect();
        for expected in ["kimi", "openai", "github"] {
            assert!(
                ids.contains(&expected),
                "expected `{expected}` bundled, got {ids:?}"
            );
        }
        for body in BUNDLED_SOURCES {
            serde_json::from_str::<ProviderCatalog>(body)
                .expect("bundled catalog must parse cleanly");
        }
    }
}
