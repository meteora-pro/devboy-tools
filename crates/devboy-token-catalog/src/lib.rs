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
    /// Env-var → variant patterns the `setup-secrets` proposer
    /// (devboy-cli) consults to map a scanned env-var name to
    /// an ADR-020 path with high accuracy. Optional and
    /// additive — catalogs that don't carry these fields
    /// still load and the proposer falls back to its
    /// hardcoded heuristics.
    ///
    /// Each pattern says "if the env-var name matches one of
    /// `matches`, propose `<scope>/<provider_id>/<variant>`".
    /// Globs use `*` as a single-segment wildcard
    /// (`OPENAI_*_KEY`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_var_patterns: Vec<EnvVarPattern>,
    /// Env-var names this provider's catalog asks the proposer
    /// to skip outright — typically configuration toggles
    /// shaped like `<PROVIDER>_BASE_URL`, `<PROVIDER>_MODEL`,
    /// `<PROVIDER>_TIMEOUT` that look like credentials at
    /// first glance but never carry a value the framework
    /// would store. Globs use `*` (`OPENAI_*_URL`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_var_skip: Vec<String>,
}

/// One env-var → variant mapping. The proposer treats
/// `matches` as a list of literal names plus glob patterns
/// (`*` = single underscore segment); the first entry that
/// matches wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvVarPattern {
    /// One or more env-var names the pattern fires on. A bare
    /// name is matched literally (`OPENAI_API_KEY`); a name
    /// with `*` is a glob (`OPENAI_*_KEY` matches
    /// `OPENAI_PROD_KEY`, `OPENAI_STAGE_KEY`, …).
    pub matches: Vec<String>,
    /// Variant id (must exist in the same catalog's
    /// `variants` list) the matched env-var maps to.
    pub variant: String,
    /// ADR-020 scope segment for the resulting path. Defaults
    /// to `team`. Catalogs override to `personal` for
    /// developer-account-style credentials.
    #[serde(default = "default_pattern_scope")]
    pub scope: String,
}

fn default_pattern_scope() -> String {
    "team".to_owned()
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
    /// target). This is the *creation* page — where the user
    /// clicks "Create key".
    pub console_url: String,
    /// Provider's official documentation for this credential —
    /// the page that explains scopes, security best practices,
    /// and the auth model. Distinct from `console_url`: that's
    /// where you *make* the key, this is where you *understand*
    /// it. Optional and additive (v1 catalogs without it still
    /// load); the provision dialog renders a "Provider docs"
    /// link when present.
    #[serde(default)]
    pub docs_url: Option<String>,
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
/// `devboy_secret_patterns::LivenessSpec` but is JSON-shaped
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
    /// Provider's rotation / key-hygiene guide. Optional and
    /// additive — many providers do not have a dedicated
    /// rotation page, in which case `notes` carries the
    /// procedure instead. When present the provision dialog
    /// renders a "Rotation guide" link.
    #[serde(default)]
    pub guide_url: Option<String>,
    /// Concrete rotation procedure / caveats — the "how", not
    /// the "when". Examples: "rotate the public+secret pair
    /// atomically, update both env vars in lockstep" /
    /// "reinstall the Slack app after a scope change, the old
    /// token does not auto-grant new scopes" / "the previous
    /// key keeps working for a 24h overlap window". Optional;
    /// rendered as a block in the dialog's rotation section.
    #[serde(default)]
    pub notes: Option<String>,
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
    #[error("URL source `{url}` failed to load: {source}")]
    Fetch {
        url: String,
        #[source]
        source: FetchError,
    },
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
            let path = Some(dir.join(format!("{}.json", c.provider_id)));
            override_or_push(&mut loaded, c, CatalogSource::User, path);
        }
        errors.extend(errs);
    }
    if let Some(dir) = project_dir {
        let (cats, errs) = load_dir(dir);
        for c in cats {
            let path = Some(dir.join(format!("{}.json", c.provider_id)));
            override_or_push(&mut loaded, c, CatalogSource::Project, path);
        }
        errors.extend(errs);
    }
    (loaded, errors)
}

/// Same as [`load_all`] but with a fourth tier — remote-URL
/// sources from `sources.toml` (P23). URL sources fall **after**
/// project-scope, i.e. they win on `provider_id` collision —
/// the assumption being that a team operating a shared remote
/// catalog wants it to override anything checked into the local
/// repo. The fetcher is opt-in: when
/// [`CatalogSourcesConfig::enable_url_catalogs`] is `false` (the
/// default), URL sources are silently skipped.
///
/// The 8-arg signature is the union of every URL-source policy
/// knob (sha-pin / cache / first-fetch / audit). A bundled
/// config struct would be tidier — left as a follow-up so the
/// `#247` epic can land without another API churn.
#[allow(clippy::too_many_arguments)]
pub fn load_all_with_urls(
    bundled: &[ProviderCatalog],
    user_dir: Option<&Path>,
    project_dir: Option<&Path>,
    url_config: Option<&CatalogSourcesConfig>,
    known_hashes_path: Option<&Path>,
    cache_dir: Option<&Path>,
    first_fetch: FirstFetchPolicy,
    audit_log_path: Option<&Path>,
) -> (Vec<LoadedCatalog>, Vec<CatalogError>) {
    let (mut loaded, mut errors) = load_all(bundled, user_dir, project_dir);
    if let Some(cfg) = url_config
        && cfg.enable_url_catalogs
    {
        for src in &cfg.sources {
            match fetch_url_source(
                src,
                known_hashes_path,
                cache_dir,
                first_fetch,
                audit_log_path,
            ) {
                Ok(catalog) => {
                    let url_source = CatalogSource::Url {
                        url: src.url.clone(),
                        sha256: src.sha256.clone(),
                    };
                    override_or_push(&mut loaded, catalog, url_source, None);
                }
                Err(source) => errors.push(CatalogError::Fetch {
                    url: src.url.clone(),
                    source,
                }),
            }
        }
    }
    (loaded, errors)
}

fn override_or_push(
    loaded: &mut Vec<LoadedCatalog>,
    catalog: ProviderCatalog,
    source: CatalogSource,
    path: Option<PathBuf>,
) {
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

// =============================================================================
// URL fetcher (P23.2)
// =============================================================================

/// Hard cap on the body of a fetched catalog. Defends against
/// memory exhaustion when a misconfigured (or hostile) server
/// streams gigabytes of garbage at the loader. 256 KB is plenty
/// for a real provider catalog — the bundled Kimi file is well
/// under 4 KB.
pub const MAX_CATALOG_BODY_BYTES: usize = 256 * 1024;

/// Per-request timeout for the URL fetcher. The TLS handshake
/// alone can chew several seconds on a cold network path, so
/// 10 s is the floor below which legitimate requests start
/// timing out.
pub const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Build a `reqwest::blocking::Client` that re-checks
/// [`check_ssrf_safe`] on every redirect target. Without this,
/// the original-URL SSRF check is trivially bypassable: a
/// public HTTPS endpoint can return a 30x to
/// `http://169.254.169.254/...` (cloud metadata),
/// `http://10.0.0.5/...` (RFC1918), or any other unsafe address
/// and the default `reqwest` policy will happily follow up to
/// 10 hops before delivering the response. That defeats the
/// whole P23 catalog-fetch + GUI liveness-probe threat model
/// — the user's freshly-typed secret would land on the wrong
/// host.
///
/// This helper is the canonical client constructor for every
/// catalog-fetcher and liveness-probe call site. Callers must
/// NOT build their own `reqwest::blocking::Client` for these
/// flows; the type system can't enforce that, but every
/// existing call site lives in this workspace and is grep-able.
///
/// The redirect callback uses `Action::error` so a refused hop
/// surfaces as a hard error in the calling fetcher, not as a
/// silent 3xx response.
pub fn ssrf_safe_blocking_client(
    timeout: std::time::Duration,
) -> Result<reqwest::blocking::Client, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // Refuse runaway redirect chains in addition to the
            // SSRF check — even when every hop is "safe" in
            // isolation, 50 chained ones is not.
            if attempt.previous().len() >= 10 {
                return attempt.error("too many redirects (10 hops cap)");
            }
            let url = attempt.url().to_string();
            match check_ssrf_safe(&url) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(format!(
                    "redirect target refused by SSRF guard ({url}): {e}"
                )),
            }
        }))
        .build()
}

/// Reasons the URL fetcher can refuse a request.
#[derive(Debug, Error)]
pub enum FetchError {
    #[error("URL must use https:// (got `{url}`)")]
    HttpsRequired { url: String },
    #[error("could not build the HTTP client: {source}")]
    Client {
        #[source]
        source: reqwest::Error,
    },
    #[error("HTTP request failed: {source}")]
    Request {
        #[source]
        source: reqwest::Error,
    },
    #[error("HTTP {status}")]
    Status { status: u16 },
    #[error("body too large: server reported {bytes} bytes, cap is {MAX_CATALOG_BODY_BYTES}")]
    BodyTooLarge { bytes: u64 },
    #[error("Content-Type must be application/json (got `{got}`)")]
    BadContentType { got: String },
    #[error("body did not parse as a ProviderCatalog: {source}")]
    Parse {
        #[source]
        source: serde_json::Error,
    },
    #[error("schema version mismatch: body declares {found}, this build supports {SCHEMA_VERSION}")]
    SchemaVersion { found: u32 },
    #[error(
        "pinned SHA256 mismatch: sources.toml declares `{expected}` but the body hashes to `{actual}`"
    )]
    ShaMismatch { expected: String, actual: String },
    #[error(
        "TOFU mismatch for {url}: known_hashes.toml records `{known}` but the body now hashes to `{actual}` — refusing to load. If the upstream changed legitimately, remove the URL from known_hashes.toml or pin the new sha256 in sources.toml."
    )]
    TofuMismatch {
        url: String,
        known: String,
        actual: String,
    },
    #[error("known_hashes.toml I/O failed at {path}: {source}")]
    KnownHashesIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("known_hashes.toml is malformed: {source}")]
    KnownHashesParse {
        #[source]
        source: toml::de::Error,
    },
    #[error("first fetch from `{url}` requires user confirmation (sha256 = {sha256})")]
    FirstFetchNeedsConfirmation { url: String, sha256: String },
    #[error("URL refused by SSRF guard: {source}")]
    Ssrf {
        #[source]
        source: SsrfError,
    },
}

/// What the loader does when a URL has neither a pinned sha256
/// nor an existing entry in `known_hashes.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FirstFetchPolicy {
    /// Record the hash silently on first successful fetch
    /// (TOFU happy path). Right for unattended CLI runs.
    #[default]
    AutoRecord,
    /// Surface
    /// [`FetchError::FirstFetchNeedsConfirmation`] so the
    /// caller (e.g. the GUI) can prompt the user before
    /// activating the catalog. The caller is then expected to
    /// call [`record_url_trust`] before re-running the loader.
    RequireConfirmation,
}

/// Fetch one [`UrlSource`] over HTTPS and decode the body as a
/// [`ProviderCatalog`]. Performs the full P23.2 guard chain —
/// HTTPS-only, 10 s timeout, 256 KB body cap, JSON-only
/// Content-Type, schema-version match — plus, when present, the
/// P23.3 SHA pin / TOFU and the P23.5 disk cache.
///
/// `cache_dir`: when `Some`, fetched bodies and ETag metadata
/// are persisted under it (one file pair per URL). On startup
/// the cache short-circuits the network when within the source's
/// `refresh_seconds`; outside the TTL it goes back to the wire
/// with `If-None-Match` so a 304 still avoids re-downloading the
/// body. When the network is unreachable AND a cached copy
/// exists, the cached copy serves as a graceful fallback. Pass
/// `None` to disable caching (e.g. one-shot CLI invocations).
pub fn fetch_url_source(
    source: &UrlSource,
    known_hashes_path: Option<&Path>,
    cache_dir: Option<&Path>,
    first_fetch: FirstFetchPolicy,
    audit_log_path: Option<&Path>,
) -> Result<ProviderCatalog, FetchError> {
    let audit = AuditCtx {
        log_path: audit_log_path,
        url: &source.url,
    };
    if !source.url.starts_with("https://") {
        let detail = format!("non-https URL: {}", source.url);
        audit.record(AuditOutcome::BlockedHttpsRequired, None, None, detail);
        return Err(FetchError::HttpsRequired {
            url: source.url.clone(),
        });
    }
    if let Err(e) = check_ssrf_safe(&source.url) {
        audit.record(AuditOutcome::BlockedSsrf, None, None, e.to_string());
        return Err(FetchError::Ssrf { source: e });
    }
    let client =
        ssrf_safe_blocking_client(FETCH_TIMEOUT).map_err(|source| FetchError::Client { source })?;
    fetch_inner_with_client(
        &client,
        source,
        known_hashes_path,
        cache_dir,
        first_fetch,
        unix_now(),
        audit_log_path,
    )
}

/// Internal entry point for [`fetch_url_source`]. Splits out
/// the client, time source, and cache plumbing so tests can
/// drive it against an `http://` mock server (httpmock does
/// not speak TLS) and against synthetic clock values without
/// sleeping. Audit events are emitted at every decision point.
fn fetch_inner_with_client(
    client: &reqwest::blocking::Client,
    source: &UrlSource,
    known_hashes_path: Option<&Path>,
    cache_dir: Option<&Path>,
    first_fetch: FirstFetchPolicy,
    now_secs: u64,
    audit_log_path: Option<&Path>,
) -> Result<ProviderCatalog, FetchError> {
    let audit = AuditCtx {
        log_path: audit_log_path,
        url: &source.url,
    };
    let cache_paths = cache_dir.map(|d| cache_paths_for(d, &source.url));

    // Cache hit within TTL — no network at all.
    if let Some((body_path, meta_path)) = cache_paths.as_ref()
        && let Some((meta, bytes)) = read_cache(meta_path, body_path)
        && now_secs.saturating_sub(meta.fetched_at) < source.refresh_seconds
    {
        let actual = sha256_hex(&bytes);
        match enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch) {
            Ok(()) => {
                audit.record(
                    AuditOutcome::LoadedFromCache,
                    None,
                    Some(actual.clone()),
                    "",
                );
                return parse_catalog_bytes(&bytes);
            }
            Err(e) => {
                audit_pin_or_tofu_error(&audit, &e, &actual);
                return Err(e);
            }
        }
    }

    // Outside TTL (or no cache) — go back to the wire. If we
    // do have a stored ETag, send `If-None-Match` so a server
    // that hasn't changed can give us 304 + zero body.
    let prev_etag = cache_paths
        .as_ref()
        .and_then(|(_, mp)| read_meta(mp))
        .and_then(|m| m.etag);
    let http_result = fetch_bytes_with_client(client, &source.url, prev_etag.as_deref());

    match http_result {
        Ok(HttpFetch::NotModified) => {
            // Server says nothing changed — bump fetched_at on
            // the existing meta and serve the cached body.
            let Some((body_path, meta_path)) = cache_paths.as_ref() else {
                audit.record(
                    AuditOutcome::BlockedHttpStatus,
                    Some(304),
                    None,
                    "server returned 304 but we have no cache",
                );
                return Err(FetchError::Status { status: 304 });
            };
            let Some((mut meta, bytes)) = read_cache(meta_path, body_path) else {
                // Body disappeared between read_meta and now —
                // unusual but recoverable: re-fetch unconditionally.
                let bytes = match fetch_bytes_with_client(client, &source.url, None)? {
                    HttpFetch::Body { bytes, etag } => {
                        let m = CacheMeta {
                            url: source.url.clone(),
                            sha256: sha256_hex(&bytes),
                            etag,
                            fetched_at: now_secs,
                        };
                        let _ = write_cache(body_path, meta_path, &m, &bytes);
                        bytes
                    }
                    HttpFetch::NotModified => {
                        return Err(FetchError::Status { status: 304 });
                    }
                };
                return finalize_after_fetch(
                    &audit,
                    source,
                    known_hashes_path,
                    first_fetch,
                    &bytes,
                    Some(200),
                );
            };
            meta.fetched_at = now_secs;
            let _ = write_meta(meta_path, &meta);
            let actual = sha256_hex(&bytes);
            match enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch) {
                Ok(()) => {
                    audit.record(AuditOutcome::LoadedFromCache, Some(304), Some(actual), "");
                    parse_catalog_bytes(&bytes)
                }
                Err(e) => {
                    audit_pin_or_tofu_error(&audit, &e, &actual);
                    Err(e)
                }
            }
        }
        Ok(HttpFetch::Body { bytes, etag }) => {
            // Fresh body — write through cache (best-effort: a
            // failed write does not block the load).
            if let Some((body_path, meta_path)) = cache_paths.as_ref() {
                let m = CacheMeta {
                    url: source.url.clone(),
                    sha256: sha256_hex(&bytes),
                    etag,
                    fetched_at: now_secs,
                };
                let _ = write_cache(body_path, meta_path, &m, &bytes);
            }
            finalize_after_fetch(
                &audit,
                source,
                known_hashes_path,
                first_fetch,
                &bytes,
                Some(200),
            )
        }
        Err(e) => {
            // Network failed. If we have a stale cache, serve
            // it as graceful degradation — better than nothing
            // when the user is on a flaky link or offline.
            if let Some((body_path, meta_path)) = cache_paths.as_ref()
                && let Some((_meta, bytes)) = read_cache(meta_path, body_path)
            {
                let actual = sha256_hex(&bytes);
                match enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch) {
                    Ok(()) => {
                        audit.record(
                            AuditOutcome::ServedStaleCache,
                            None,
                            Some(actual),
                            e.to_string(),
                        );
                        return parse_catalog_bytes(&bytes);
                    }
                    Err(pin_err) => {
                        audit_pin_or_tofu_error(&audit, &pin_err, &actual);
                        return Err(pin_err);
                    }
                }
            }
            audit_network_error(&audit, &e);
            Err(e)
        }
    }
}

/// Run pin/TOFU + parse, emit the right audit outcome.
fn finalize_after_fetch(
    audit: &AuditCtx<'_>,
    source: &UrlSource,
    known_hashes_path: Option<&Path>,
    first_fetch: FirstFetchPolicy,
    bytes: &[u8],
    status_code: Option<u16>,
) -> Result<ProviderCatalog, FetchError> {
    let actual = sha256_hex(bytes);
    if let Err(e) = enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch) {
        audit_pin_or_tofu_error(audit, &e, &actual);
        return Err(e);
    }
    match parse_catalog_bytes(bytes) {
        Ok(cat) => {
            audit.record(AuditOutcome::Loaded, status_code, Some(actual), "");
            Ok(cat)
        }
        Err(e) => {
            let outcome = match &e {
                FetchError::SchemaVersion { .. } => AuditOutcome::BlockedSchemaVersion,
                FetchError::Parse { .. } => AuditOutcome::BlockedParse,
                _ => AuditOutcome::BlockedParse,
            };
            audit.record(outcome, status_code, Some(actual), e.to_string());
            Err(e)
        }
    }
}

fn audit_pin_or_tofu_error(audit: &AuditCtx<'_>, err: &FetchError, actual_sha: &str) {
    let outcome = match err {
        FetchError::ShaMismatch { .. } => AuditOutcome::BlockedPin,
        FetchError::TofuMismatch { .. } => AuditOutcome::BlockedTofuMismatch,
        FetchError::FirstFetchNeedsConfirmation { .. } => AuditOutcome::FirstFetchPending,
        _ => AuditOutcome::BlockedParse,
    };
    audit.record(outcome, None, Some(actual_sha.to_owned()), err.to_string());
}

fn audit_network_error(audit: &AuditCtx<'_>, err: &FetchError) {
    let (outcome, status) = match err {
        FetchError::Status { status } => (AuditOutcome::BlockedHttpStatus, Some(*status)),
        FetchError::BadContentType { .. } => (AuditOutcome::BlockedContentType, None),
        FetchError::BodyTooLarge { .. } => (AuditOutcome::BlockedSize, None),
        _ => (AuditOutcome::NetworkError, None),
    };
    audit.record(outcome, status, None, err.to_string());
}

/// Outcome of one HTTP body fetch — either fresh body bytes or
/// a 304 short-circuit when the server confirms our ETag.
enum HttpFetch {
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
    NotModified,
}

/// Body fetch with optional `If-None-Match`. Runs the
/// HTTP / Content-Type / size guards on 200. Returns
/// [`HttpFetch::NotModified`] on 304 (the caller is expected to
/// have a cached body to fall back on).
fn fetch_bytes_with_client(
    client: &reqwest::blocking::Client,
    url: &str,
    if_none_match: Option<&str>,
) -> Result<HttpFetch, FetchError> {
    let mut req = client.get(url);
    if let Some(etag) = if_none_match {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = req
        .send()
        .map_err(|source| FetchError::Request { source })?;

    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(HttpFetch::NotModified);
    }
    if !resp.status().is_success() {
        return Err(FetchError::Status {
            status: resp.status().as_u16(),
        });
    }

    if let Some(ct) = resp.headers().get(reqwest::header::CONTENT_TYPE) {
        let got = ct.to_str().unwrap_or("<non-ascii>").to_owned();
        let main_type = got
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if main_type != "application/json" {
            return Err(FetchError::BadContentType { got });
        }
    }

    if let Some(cl) = resp.content_length()
        && (cl as usize) > MAX_CATALOG_BODY_BYTES
    {
        return Err(FetchError::BodyTooLarge { bytes: cl });
    }

    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = resp
        .bytes()
        .map_err(|source| FetchError::Request { source })?;
    if bytes.len() > MAX_CATALOG_BODY_BYTES {
        return Err(FetchError::BodyTooLarge {
            bytes: bytes.len() as u64,
        });
    }
    Ok(HttpFetch::Body {
        bytes: bytes.to_vec(),
        etag,
    })
}

/// Parse already-fetched body bytes as a [`ProviderCatalog`].
/// Public so the disk-cache (P23.5) can reuse it without
/// re-hitting the network.
pub fn parse_catalog_bytes(bytes: &[u8]) -> Result<ProviderCatalog, FetchError> {
    let cat: ProviderCatalog =
        serde_json::from_slice(bytes).map_err(|source| FetchError::Parse { source })?;
    if cat.schema_version != SCHEMA_VERSION {
        return Err(FetchError::SchemaVersion {
            found: cat.schema_version,
        });
    }
    Ok(cat)
}

/// Hex-encoded SHA256 of the input. Public so the rest of the
/// catalog stack (cache / audit) can compute the same digest.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Enforce the P23.3 trust policy:
///
/// 1. **Pin**: when `source.sha256` is set, compare and reject
///    on mismatch.
/// 2. **TOFU**: when no pin and a `known_hashes_path` is given,
///    look the URL up in `known_hashes.toml`. If known and
///    matching → ok. If known and mismatched → reject with a
///    `TofuMismatch`. If unknown → record on first successful
///    fetch.
/// 3. When neither pin nor known_hashes_path is configured →
///    no trust check (the caller has explicitly chosen the
///    permissive path).
fn enforce_pin_or_tofu(
    source: &UrlSource,
    known_hashes_path: Option<&Path>,
    actual_sha: &str,
    first_fetch: FirstFetchPolicy,
) -> Result<(), FetchError> {
    if let Some(expected) = source.sha256.as_deref() {
        if expected.eq_ignore_ascii_case(actual_sha) {
            return Ok(());
        }
        return Err(FetchError::ShaMismatch {
            expected: expected.to_owned(),
            actual: actual_sha.to_owned(),
        });
    }
    let Some(path) = known_hashes_path else {
        return Ok(());
    };
    let mut known = read_known_hashes(path)?;
    if let Some(stored) = known.url.get(&source.url) {
        if stored.eq_ignore_ascii_case(actual_sha) {
            return Ok(());
        }
        return Err(FetchError::TofuMismatch {
            url: source.url.clone(),
            known: stored.clone(),
            actual: actual_sha.to_owned(),
        });
    }
    match first_fetch {
        FirstFetchPolicy::AutoRecord => {
            // First-fetch: record and persist. A failed write
            // is fatal so the next run isn't lulled into
            // thinking nothing was recorded — better to
            // surface the I/O error now.
            known.url.insert(source.url.clone(), actual_sha.to_owned());
            write_known_hashes(path, &known)?;
            Ok(())
        }
        FirstFetchPolicy::RequireConfirmation => {
            // Defer: caller (GUI) must surface a confirm
            // dialog and call `record_url_trust` before
            // re-running the loader.
            Err(FetchError::FirstFetchNeedsConfirmation {
                url: source.url.clone(),
                sha256: actual_sha.to_owned(),
            })
        }
    }
}

/// Persist a TOFU entry into `known_hashes.toml` after the
/// user has confirmed they trust the URL. Idempotent —
/// overwrites any existing entry, which is also how the GUI
/// resolves a `TofuMismatch` warning when the user accepts
/// the new hash.
pub fn record_url_trust(
    known_hashes_path: &Path,
    url: &str,
    sha256: &str,
) -> Result<(), FetchError> {
    let mut known = read_known_hashes(known_hashes_path)?;
    known.url.insert(url.to_owned(), sha256.to_owned());
    write_known_hashes(known_hashes_path, &known)
}

// =============================================================================
// SSRF guard for liveness probes (P23.4)
// =============================================================================
//
// The catalog gets to declare *where* the GUI ships a freshly-typed
// secret to liveness-check it. A malicious or compromised catalog
// could point that URL at private infrastructure (`http://10.0.0.5/`,
// `http://169.254.169.254/latest/meta-data/`, `http://localhost:9090/`),
// turning the user's machine into an SSRF probe with the secret
// piggybacking. The guard below resolves the URL's hostname to every
// IP it would dial, refuses any blocked range, and refuses well-known
// cloud-metadata hostnames before DNS even runs.

/// Reasons [`check_ssrf_safe`] can refuse a URL.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SsrfError {
    #[error("URL is malformed: {0}")]
    InvalidUrl(String),
    #[error("DNS resolution failed for `{0}`")]
    DnsResolutionFailed(String),
    #[error(
        "host `{host}` resolves to {ip}, which is in a blocked range (private / loopback / link-local / multicast)"
    )]
    BlockedAddress { host: String, ip: std::net::IpAddr },
    #[error("hostname `{host}` matches a known cloud-metadata service — refused")]
    CloudMetadata { host: String },
}

/// Hostnames that name cloud-instance metadata services. Any of
/// these resolves to a link-local IP (`169.254.169.254` or
/// similar), but a hostile DNS server could resolve them to a
/// public address — so we reject by name *before* DNS, then
/// recheck the IP afterwards.
const CLOUD_METADATA_HOSTS: &[&str] = &[
    "metadata.google.internal",
    "metadata.aws.internal",
    "metadata.azure.com",
    "metadata",
    "169.254.169.254",
];

/// Resolve `url`'s hostname and refuse to dial it when the
/// resulting IP, or the hostname itself, hits a known
/// SSRF-relevant range. Production callers — both the rust-
/// catalogue and the catalog-driven liveness probe paths —
/// invoke this immediately before constructing a request.
pub fn check_ssrf_safe(url: &str) -> Result<(), SsrfError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| SsrfError::InvalidUrl(url.to_owned()))?;
    let Some(host) = parsed.host_str() else {
        return Err(SsrfError::InvalidUrl(url.to_owned()));
    };

    let host_lower = host.to_ascii_lowercase();
    if CLOUD_METADATA_HOSTS
        .iter()
        .any(|blocked| host_lower == *blocked)
    {
        return Err(SsrfError::CloudMetadata {
            host: host.to_owned(),
        });
    }

    // `Url::socket_addrs` walks the OS resolver and handles
    // IPv6 brackets / IDN / default ports in one call. We
    // refuse if *any* returned IP hits a blocked range —
    // partial defence against DNS rebinding (a hostile DNS
    // returning multiple IPs, only some safe).
    let addrs = parsed
        .socket_addrs(|| Some(443))
        .map_err(|_| SsrfError::DnsResolutionFailed(host.to_owned()))?;
    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(SsrfError::BlockedAddress {
                host: host.to_owned(),
                ip: addr.ip(),
            });
        }
    }
    Ok(())
}

/// Classify an IP as "must not dial". Covers:
///
/// - IPv4 loopback (`127.0.0.0/8`), private (`10/8`, `172.16/12`,
///   `192.168/16`), link-local (`169.254/16`), broadcast,
///   unspecified, multicast.
/// - IPv6 loopback (`::1`), unspecified (`::`), multicast,
///   ULA (`fc00::/7`), link-local (`fe80::/10`).
///
/// Conservative: anything not clearly a public host gets rejected.
fn is_blocked_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return true;
            }
            let segs = v6.segments();
            // ULA fc00::/7 — first byte 0xfc or 0xfd.
            let high_byte = (segs[0] >> 8) as u8;
            if high_byte == 0xfc || high_byte == 0xfd {
                return true;
            }
            // Link-local fe80::/10 — top 10 bits == 1111111010.
            if (segs[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            false
        }
    }
}

// =============================================================================
// known_hashes.toml — TOFU store for URL-loaded catalogs (P23.3)
// =============================================================================

/// Default `known_hashes.toml` path. Lives next to
/// `sources.toml` so all URL-source state is in one directory.
pub fn default_known_hashes_path() -> Option<PathBuf> {
    default_user_catalog_dir().map(|d| d.join("known_hashes.toml"))
}

/// Default cache directory for fetched URL-catalog bodies:
/// `~/.devboy/secrets/catalog/cache/`.
pub fn default_catalog_cache_dir() -> Option<PathBuf> {
    default_user_catalog_dir().map(|d| d.join("cache"))
}

/// Default append-only audit log for URL-catalog fetches:
/// `~/.devboy/secrets/catalog/audit.log`. One JSONL event per
/// fetch attempt (P23.7). Honoured by the loader when wired
/// through `load_all_with_urls` (CLI / GUI both pass it).
pub fn default_catalog_audit_log_path() -> Option<PathBuf> {
    default_user_catalog_dir().map(|d| d.join("audit.log"))
}

// =============================================================================
// Audit log for URL-loaded catalogs (P23.7)
// =============================================================================

/// One row of the audit log. Serialised to JSONL — one event
/// per line — so external tools can `tail -f` and grep without
/// any custom parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// RFC3339 timestamp with offset, e.g.
    /// `2026-05-10T12:34:56+00:00`.
    pub timestamp: String,
    /// Original `[[source]].url` from sources.toml.
    pub url: String,
    /// HTTP status the upstream returned, when there was a
    /// response. `None` for events that fail before the wire
    /// (https-only refusal, SSRF guard, etc.) and for cache
    /// hits that did not touch the network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// SHA256 of the body the loader saw, lowercase hex.
    /// `None` when no body was decided (early refusals, network
    /// errors with no cache fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Outcome class — one of the [`AuditOutcome`] variants.
    pub outcome: AuditOutcome,
    /// Free-form detail message — the underlying error string
    /// for blocked outcomes, empty for the happy paths.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Enumerated outcomes for the audit log. Designed so a `grep`
/// for any `kebab-case` token from this list pulls every
/// matching event out of the JSONL file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditOutcome {
    /// URL fetched, hash recorded/matched, catalog activated.
    Loaded,
    /// Cache hit within TTL — no network at all.
    LoadedFromCache,
    /// Network failed but a cached body was served.
    ServedStaleCache,
    /// First-fetch awaiting user confirmation
    /// ([`FirstFetchPolicy::RequireConfirmation`]).
    FirstFetchPending,
    /// Pinned `sources.toml` SHA256 mismatch.
    BlockedPin,
    /// `known_hashes.toml` mismatch — TOFU broken.
    BlockedTofuMismatch,
    /// SSRF guard refused the URL host / IP.
    BlockedSsrf,
    /// Body exceeded `MAX_CATALOG_BODY_BYTES`.
    BlockedSize,
    /// `https://` guard fired.
    BlockedHttpsRequired,
    /// Content-Type wasn't `application/json`.
    BlockedContentType,
    /// Server returned a non-2xx, non-304 status.
    BlockedHttpStatus,
    /// Body parsed but `schema_version` was wrong.
    BlockedSchemaVersion,
    /// Body did not parse as JSON / `ProviderCatalog`.
    BlockedParse,
    /// Network call itself failed (timeout, DNS, TCP).
    NetworkError,
}

/// Append a JSON-encoded event to the audit log. Best-effort:
/// any I/O failure on the audit path is swallowed silently —
/// losing an audit line is preferable to refusing to load the
/// catalog because the disk is full.
pub fn append_audit_event(audit_log_path: &Path, event: &AuditEvent) {
    use std::io::Write;
    if let Some(parent) = audit_log_path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_log_path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Bundles per-event audit context so the deep call chain
/// inside `fetch_inner_with_client` doesn't grow another arg.
struct AuditCtx<'a> {
    log_path: Option<&'a Path>,
    url: &'a str,
}

impl AuditCtx<'_> {
    fn record(
        &self,
        outcome: AuditOutcome,
        status_code: Option<u16>,
        sha256: Option<String>,
        detail: impl Into<String>,
    ) {
        let Some(path) = self.log_path else {
            return;
        };
        append_audit_event(
            path,
            &AuditEvent {
                timestamp: rfc3339_now(),
                url: self.url.to_owned(),
                status_code,
                sha256,
                outcome,
                detail: detail.into(),
            },
        );
    }
}

// =============================================================================
// Disk cache for URL-loaded catalogs (P23.5)
// =============================================================================

/// Sidecar metadata for a cached URL fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheMeta {
    /// Original URL the body came from.
    pub url: String,
    /// SHA256 of the cached body, lowercase hex. Verified on
    /// every read — a tampered file is treated as a cache miss.
    pub sha256: String,
    /// Server-supplied ETag, if any. Used to drive
    /// `If-None-Match` on the next refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Unix epoch seconds the body was last accepted (whether
    /// via 200 or via 304 confirming the cached body is still
    /// good).
    pub fetched_at: u64,
}

/// Compute the (body, meta) paths for a given URL inside a
/// cache directory. Key is `sha256(url)` so collisions are
/// effectively impossible and the user can `ls` the directory
/// to count what's cached.
fn cache_paths_for(cache_dir: &Path, url: &str) -> (PathBuf, PathBuf) {
    let key = sha256_hex(url.as_bytes());
    (
        cache_dir.join(format!("{key}.json")),
        cache_dir.join(format!("{key}.meta.toml")),
    )
}

fn read_meta(meta_path: &Path) -> Option<CacheMeta> {
    let body = fs::read_to_string(meta_path).ok()?;
    toml::from_str(&body).ok()
}

fn write_meta(meta_path: &Path, meta: &CacheMeta) -> Result<(), FetchError> {
    if let Some(parent) = meta_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| FetchError::KnownHashesIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let body = toml::to_string_pretty(meta).expect("CacheMeta always serializes");
    fs::write(meta_path, body).map_err(|source| FetchError::KnownHashesIo {
        path: meta_path.to_path_buf(),
        source,
    })
}

/// Read a (meta, body) pair from disk, re-verifying that the
/// body's SHA matches what the meta records. Returns `None` on
/// any I/O failure, schema mismatch, or SHA mismatch — the
/// caller treats `None` as a cache miss and refetches. This is
/// the integrity check from the P23.5 design: a tampered
/// cached file does not poison the loader.
fn read_cache(meta_path: &Path, body_path: &Path) -> Option<(CacheMeta, Vec<u8>)> {
    let meta = read_meta(meta_path)?;
    let bytes = fs::read(body_path).ok()?;
    if sha256_hex(&bytes) != meta.sha256 {
        return None;
    }
    Some((meta, bytes))
}

fn write_cache(
    body_path: &Path,
    meta_path: &Path,
    meta: &CacheMeta,
    bytes: &[u8],
) -> Result<(), FetchError> {
    if let Some(parent) = body_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| FetchError::KnownHashesIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(body_path, bytes).map_err(|source| FetchError::KnownHashesIo {
        path: body_path.to_path_buf(),
        source,
    })?;
    write_meta(meta_path, meta)?;
    Ok(())
}

fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// On-disk shape of `known_hashes.toml`:
///
/// ```toml
/// [url]
/// "https://example.invalid/catalog.json" = "abc123…"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnownHashes {
    /// Map of URL → lowercase-hex SHA256 of the most recent
    /// body the loader accepted at that URL.
    #[serde(default)]
    pub url: std::collections::BTreeMap<String, String>,
}

/// Read `known_hashes.toml`. A missing file is *not* an error —
/// it just means TOFU has not recorded anything yet.
pub fn read_known_hashes(path: &Path) -> Result<KnownHashes, FetchError> {
    let body = match fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(KnownHashes::default()),
        Err(source) => {
            return Err(FetchError::KnownHashesIo {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    toml::from_str(&body).map_err(|source| FetchError::KnownHashesParse { source })
}

/// Persist `known_hashes.toml`. Creates parent directories
/// when needed.
pub fn write_known_hashes(path: &Path, known: &KnownHashes) -> Result<(), FetchError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| FetchError::KnownHashesIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let body = toml::to_string_pretty(known).expect("KnownHashes always serializes");
    fs::write(path, body).map_err(|source| FetchError::KnownHashesIo {
        path: path.to_path_buf(),
        source,
    })
}

/// Bundled provider catalogs shipped in the binary itself.
///
/// Backed by [`rust_embed`] auto-discovery: every `*.json`
/// under `crates/devboy-token-catalog/data/` is included at
/// compile time. **Adding a new provider is one file drop —
/// no source change required**, which keeps PRs that add
/// different catalogs free of merge conflicts on a shared
/// registry.
///
/// Files that fail to parse against the schema are silently
/// dropped here; CI runs `devboy secrets catalog validate`
/// against every bundled file so a malformed JSON is caught
/// before merge, not at runtime. Output is sorted by
/// `provider_id` for deterministic ordering.
pub fn bundled_catalogs() -> Vec<ProviderCatalog> {
    let mut out: Vec<ProviderCatalog> = BundledCatalogAssets::iter()
        .filter_map(|name| {
            let asset = BundledCatalogAssets::get(name.as_ref())?;
            serde_json::from_slice::<ProviderCatalog>(&asset.data).ok()
        })
        .collect();
    out.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
    out
}

/// rust-embed handle for the on-disk `data/` tree. The folder
/// path is relative to the crate root; `include` filters down
/// to the JSON catalog files we ship.
#[derive(rust_embed::Embed)]
#[folder = "data/"]
#[include = "*.json"]
struct BundledCatalogAssets;

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
                    docs_url: None,
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
                    guide_url: None,
                    notes: None,
                }),
                default_keychain_account: None,
            }],
            env_var_patterns: Vec::new(),
            env_var_skip: Vec::new(),
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

    // -- env_var_patterns / env_var_skip (S1) --------------------

    #[test]
    fn env_var_patterns_round_trip_with_defaults() {
        // Matches without `scope` should default to "team".
        let body = r#"{
            "schema_version":1,
            "provider_id":"openai",
            "display_name":"OpenAI",
            "variants":[{
                "id":"openai-api-key",
                "display_name":"OpenAI API key",
                "description":"main",
                "retrieval":{"console_url":"https://example.invalid","steps":["x"]}
            }],
            "env_var_patterns":[
                {"matches":["OPENAI_API_KEY"],"variant":"openai-api-key"},
                {"matches":["E2E_OPENAI_API_KEY"],"variant":"openai-api-key","scope":"personal"}
            ],
            "env_var_skip":["OPENAI_API_BASE","OPENAI_*_URL","OPENAI_MODEL"]
        }"#;
        let cat: ProviderCatalog = serde_json::from_str(body).unwrap();
        assert_eq!(cat.env_var_patterns.len(), 2);
        assert_eq!(cat.env_var_patterns[0].scope, "team");
        assert_eq!(cat.env_var_patterns[1].scope, "personal");
        assert_eq!(cat.env_var_skip.len(), 3);
    }

    #[test]
    fn env_var_patterns_omitted_in_legacy_catalogs() {
        // Catalogs authored before S1 lack the new fields and
        // must still load; the proposer treats absence as
        // "no patterns".
        let body = r#"{
            "schema_version":1,
            "provider_id":"x",
            "display_name":"X",
            "variants":[{
                "id":"x-default",
                "display_name":"X",
                "description":"main",
                "retrieval":{"console_url":"https://example.invalid","steps":["x"]}
            }]
        }"#;
        let cat: ProviderCatalog = serde_json::from_str(body).unwrap();
        assert!(cat.env_var_patterns.is_empty());
        assert!(cat.env_var_skip.is_empty());
    }

    #[test]
    fn env_var_pattern_rejects_unknown_field() {
        let body = r#"{
            "schema_version":1,
            "provider_id":"x",
            "display_name":"X",
            "variants":[{
                "id":"x-default",
                "display_name":"X",
                "description":"main",
                "retrieval":{"console_url":"https://example.invalid","steps":["x"]}
            }],
            "env_var_patterns":[
                {"matches":["X_TOKEN"],"variant":"x-default","unknown":true}
            ]
        }"#;
        let r: Result<ProviderCatalog, _> = serde_json::from_str(body);
        assert!(
            r.is_err(),
            "deny_unknown_fields must reject extra keys on EnvVarPattern"
        );
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
    fn fetch_rejects_http_scheme_directly() {
        let src = UrlSource {
            url: "http://example.invalid/x.json".into(),
            sha256: None,
            refresh_seconds: 60,
        };
        let err =
            fetch_url_source(&src, None, None, FirstFetchPolicy::AutoRecord, None).unwrap_err();
        assert!(matches!(err, FetchError::HttpsRequired { .. }));
    }

    fn test_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap()
    }

    /// Convenience for the body+parse tests: combines the two
    /// post-P23.3 internal stages (`fetch_bytes_with_client` →
    /// `parse_catalog_bytes`) so existing test logic stays as-is.
    /// 200-only — these tests don't drive ETag.
    fn fetch_with_client(
        client: &reqwest::blocking::Client,
        url: &str,
    ) -> Result<ProviderCatalog, FetchError> {
        match fetch_bytes_with_client(client, url, None)? {
            HttpFetch::Body { bytes, .. } => parse_catalog_bytes(&bytes),
            HttpFetch::NotModified => Err(FetchError::Status { status: 304 }),
        }
    }

    fn fixture_json() -> String {
        serde_json::to_string(&fixture()).unwrap()
    }

    #[test]
    fn fetch_with_client_happy_path() {
        let server = httpmock::MockServer::start();
        let m = server.mock(|when, then| {
            when.method("GET").path("/kimi.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(fixture_json());
        });
        let url = format!("{}/kimi.json", server.base_url());
        let cat = fetch_with_client(&test_client(), &url).unwrap();
        m.assert();
        assert_eq!(cat.provider_id, "kimi");
    }

    #[test]
    fn fetch_with_client_rejects_non_2xx() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/x.json");
            then.status(404);
        });
        let url = format!("{}/x.json", server.base_url());
        let err = fetch_with_client(&test_client(), &url).unwrap_err();
        assert!(matches!(err, FetchError::Status { status: 404 }));
    }

    #[test]
    fn fetch_with_client_rejects_html_content_type() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/x.json");
            then.status(200)
                .header("content-type", "text/html; charset=utf-8")
                .body("<html>oops</html>");
        });
        let url = format!("{}/x.json", server.base_url());
        let err = fetch_with_client(&test_client(), &url).unwrap_err();
        assert!(matches!(err, FetchError::BadContentType { .. }));
    }

    #[test]
    fn fetch_with_client_rejects_oversize_body() {
        let server = httpmock::MockServer::start();
        // 257 KB of `x`s — one byte over the cap.
        let payload = "x".repeat(MAX_CATALOG_BODY_BYTES + 1);
        server.mock(|when, then| {
            when.method("GET").path("/big.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(payload);
        });
        let url = format!("{}/big.json", server.base_url());
        let err = fetch_with_client(&test_client(), &url).unwrap_err();
        assert!(matches!(err, FetchError::BodyTooLarge { .. }));
    }

    #[test]
    fn fetch_with_client_rejects_malformed_json() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/bad.json");
            then.status(200)
                .header("content-type", "application/json")
                .body("{ not json");
        });
        let url = format!("{}/bad.json", server.base_url());
        let err = fetch_with_client(&test_client(), &url).unwrap_err();
        assert!(matches!(err, FetchError::Parse { .. }));
    }

    #[test]
    fn fetch_with_client_rejects_schema_mismatch() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/v99.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"schema_version":99,"provider_id":"x","display_name":"X","variants":[]}"#,
                );
        });
        let url = format!("{}/v99.json", server.base_url());
        let err = fetch_with_client(&test_client(), &url).unwrap_err();
        assert!(matches!(err, FetchError::SchemaVersion { found: 99 }));
    }

    #[test]
    fn sha256_hex_known_vector() {
        // RFC 6234 vector #1: SHA256("abc") = ba7816bf...
        let h = sha256_hex(b"abc");
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn fixture_url_source(server_url: &str, path: &str, sha256: Option<String>) -> UrlSource {
        UrlSource {
            url: format!("{server_url}{path}"),
            sha256,
            refresh_seconds: 60,
        }
    }

    /// Like fetch_url_source but skips the https:// guard so
    /// httpmock can serve over http://. Re-implements the
    /// exact public flow (now via `fetch_inner_with_client`,
    /// which handles cache + ETag + pin/TOFU).
    fn fetch_no_https_guard(
        source: &UrlSource,
        known_hashes_path: Option<&Path>,
    ) -> Result<ProviderCatalog, FetchError> {
        fetch_inner_with_client(
            &test_client(),
            source,
            known_hashes_path,
            None,
            FirstFetchPolicy::AutoRecord,
            unix_now(),
            None,
        )
    }

    // -- F1: SSRF-safe client refuses redirects into unsafe space --

    /// Walk the std::error::Error::source chain and concatenate
    /// every layer's display string. Reqwest wraps custom
    /// redirect-policy errors deep inside a generic "error
    /// following redirect" message; the inner cause carries our
    /// SSRF text.
    fn collect_error_chain(err: &dyn std::error::Error) -> String {
        let mut out = err.to_string();
        let mut cause = err.source();
        while let Some(c) = cause {
            out.push_str(" :: ");
            out.push_str(&c.to_string());
            cause = c.source();
        }
        out
    }

    #[test]
    fn ssrf_safe_client_refuses_redirect_to_link_local_metadata() {
        // httpmock listens on 127.0.0.1 (loopback) but we ask the
        // ssrf-safe client to FOLLOW it. The first hop is not
        // checked by the redirect callback — only subsequent
        // Location targets are. Our 302 sends the client at
        // 169.254.169.254 (AWS / GCP cloud-metadata) which the
        // SSRF guard refuses → the policy returns
        // `Action::error(...)` → reqwest surfaces it.
        let server = httpmock::MockServer::start();
        let m = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/catalog.json");
            then.status(302)
                .header("Location", "http://169.254.169.254/latest/meta-data");
        });

        let client = ssrf_safe_blocking_client(std::time::Duration::from_secs(2)).unwrap();
        let res = client
            .get(format!("{}/catalog.json", server.base_url()))
            .send();
        m.assert();
        let err = res.expect_err("SSRF-safe client must refuse the 302 to 169.254.169.254");
        let chain = collect_error_chain(&err);
        assert!(
            chain.contains("SSRF guard") || chain.contains("169.254"),
            "error chain must blame the SSRF guard, got: {chain}"
        );
    }

    #[test]
    fn ssrf_safe_client_refuses_redirect_to_rfc1918() {
        let server = httpmock::MockServer::start();
        let m = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/catalog.json");
            then.status(301)
                .header("Location", "http://10.0.0.5/internal");
        });

        let client = ssrf_safe_blocking_client(std::time::Duration::from_secs(2)).unwrap();
        let res = client
            .get(format!("{}/catalog.json", server.base_url()))
            .send();
        m.assert();
        let err = res.expect_err("SSRF-safe client must refuse a 301 redirect into RFC1918 space");
        let chain = collect_error_chain(&err);
        assert!(
            chain.contains("SSRF guard") || chain.contains("10.0.0.5"),
            "error chain must blame the SSRF guard, got: {chain}"
        );
    }

    #[test]
    fn ssrf_safe_client_refuses_runaway_redirect_chain() {
        // Httpmock that redirects to itself indefinitely.
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/loop");
            // Note: the Location header points back at /loop on
            // the same server — every hop stays on 127.0.0.1
            // (loopback). check_ssrf_safe accepts the loopback
            // here (the callback isn't invoked for the very
            // first hop; only subsequent ones). After 10 hops
            // the `previous().len() >= 10` guard fires.
            then.status(302)
                .header("Location", format!("{}/loop", server.base_url()));
        });

        let client = ssrf_safe_blocking_client(std::time::Duration::from_secs(2)).unwrap();
        let res = client.get(format!("{}/loop", server.base_url())).send();
        let err = res.expect_err(
            "SSRF-safe client must refuse a redirect chain that exceeds the 10-hop cap",
        );
        // The error may come from EITHER our guard (loopback
        // refused) OR the 10-hop cap, depending on whether the
        // first internal-redirect hop's loopback IP gets a
        // pass. Both wordings are acceptable for this test.
        let chain = collect_error_chain(&err);
        assert!(
            chain.contains("too many redirects") || chain.contains("SSRF guard"),
            "error must blame the cap or the guard, got: {chain}"
        );
    }

    #[test]
    fn pinned_sha_match_succeeds() {
        let server = httpmock::MockServer::start();
        let body = fixture_json();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(body.clone());
        });
        let expected = sha256_hex(body.as_bytes());
        let src = fixture_url_source(&server.base_url(), "/k.json", Some(expected));
        let cat = fetch_no_https_guard(&src, None).unwrap();
        assert_eq!(cat.provider_id, "kimi");
    }

    #[test]
    fn pinned_sha_mismatch_rejects() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(fixture_json());
        });
        let bogus = "0".repeat(64);
        let src = fixture_url_source(&server.base_url(), "/k.json", Some(bogus));
        let err = fetch_no_https_guard(&src, None).unwrap_err();
        assert!(matches!(err, FetchError::ShaMismatch { .. }));
    }

    #[test]
    fn tofu_records_on_first_fetch_and_accepts_on_second() {
        let server = httpmock::MockServer::start();
        let body = fixture_json();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(body.clone());
        });
        let dir = TempDir::new().unwrap();
        let kh_path = dir.path().join("known_hashes.toml");
        let src = fixture_url_source(&server.base_url(), "/k.json", None);

        // First fetch — records the hash.
        fetch_no_https_guard(&src, Some(&kh_path)).unwrap();
        let known = read_known_hashes(&kh_path).unwrap();
        assert_eq!(
            known.url.get(&src.url).unwrap(),
            &sha256_hex(body.as_bytes())
        );

        // Second fetch — same hash, accepted silently.
        fetch_no_https_guard(&src, Some(&kh_path)).unwrap();
    }

    #[test]
    fn tofu_rejects_when_known_hash_changes() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(fixture_json());
        });
        let dir = TempDir::new().unwrap();
        let kh_path = dir.path().join("known_hashes.toml");
        let src = fixture_url_source(&server.base_url(), "/k.json", None);

        // Pre-record a different hash → next fetch must reject.
        let mut seeded = KnownHashes::default();
        seeded.url.insert(src.url.clone(), "0".repeat(64));
        write_known_hashes(&kh_path, &seeded).unwrap();

        let err = fetch_no_https_guard(&src, Some(&kh_path)).unwrap_err();
        assert!(matches!(err, FetchError::TofuMismatch { .. }));
    }

    #[test]
    fn known_hashes_roundtrip_through_disk() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("known_hashes.toml");
        let mut original = KnownHashes::default();
        original.url.insert(
            "https://example.invalid/x.json".into(),
            sha256_hex(b"some bytes"),
        );
        write_known_hashes(&path, &original).unwrap();
        let back = read_known_hashes(&path).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn ssrf_blocks_loopback_v4() {
        let err = check_ssrf_safe("https://127.0.0.1/v1/models").unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[test]
    fn ssrf_blocks_loopback_v6() {
        let err = check_ssrf_safe("https://[::1]/v1/models").unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[test]
    fn ssrf_blocks_rfc1918_10_dot() {
        let err = check_ssrf_safe("https://10.0.0.5/health").unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[test]
    fn ssrf_blocks_rfc1918_192_168() {
        let err = check_ssrf_safe("https://192.168.1.1/").unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[test]
    fn ssrf_blocks_rfc1918_172_16() {
        let err = check_ssrf_safe("https://172.16.5.10/").unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[test]
    fn ssrf_blocks_link_local_v4() {
        let err = check_ssrf_safe("https://169.254.169.254/latest/meta-data/").unwrap_err();
        // Either the hostname-list path (literal IP listed) or
        // the IP-classification path will fire. Both are
        // SsrfError variants — accept either.
        assert!(matches!(
            err,
            SsrfError::BlockedAddress { .. } | SsrfError::CloudMetadata { .. }
        ));
    }

    #[test]
    fn ssrf_blocks_cloud_metadata_hostnames() {
        for host in [
            "metadata.google.internal",
            "metadata.aws.internal",
            "metadata.azure.com",
            "Metadata.Google.Internal", // case-insensitive
        ] {
            let url = format!("https://{host}/computeMetadata/v1/instance/");
            let err = check_ssrf_safe(&url).unwrap_err();
            assert!(
                matches!(err, SsrfError::CloudMetadata { .. }),
                "expected CloudMetadata, got {err:?} for {host}"
            );
        }
    }

    #[test]
    fn ssrf_allows_public_ipv4() {
        // 1.1.1.1 — Cloudflare, public anycast. No DNS needed.
        check_ssrf_safe("https://1.1.1.1/").unwrap();
    }

    #[test]
    fn ssrf_rejects_malformed_url() {
        let err = check_ssrf_safe("not-a-url").unwrap_err();
        assert!(matches!(err, SsrfError::InvalidUrl(_)));
    }

    #[test]
    fn read_known_hashes_returns_empty_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.toml");
        let kh = read_known_hashes(&path).unwrap();
        assert!(kh.url.is_empty());
    }

    fn fetch_inner_at(
        source: &UrlSource,
        cache_dir: Option<&Path>,
        now_secs: u64,
    ) -> Result<ProviderCatalog, FetchError> {
        fetch_inner_with_client(
            &test_client(),
            source,
            None,
            cache_dir,
            FirstFetchPolicy::AutoRecord,
            now_secs,
            None,
        )
    }

    #[test]
    fn strict_first_fetch_returns_confirmation_error_then_record_lets_it_through() {
        let server = httpmock::MockServer::start();
        let body = fixture_json();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(body.clone());
        });
        let dir = TempDir::new().unwrap();
        let kh_path = dir.path().join("known_hashes.toml");
        let src = fixture_url_source(&server.base_url(), "/k.json", None);

        // Strict mode + unknown URL → confirmation needed.
        let err = fetch_inner_with_client(
            &test_client(),
            &src,
            Some(&kh_path),
            None,
            FirstFetchPolicy::RequireConfirmation,
            1_000,
            None,
        )
        .unwrap_err();
        let (url, sha) = match err {
            FetchError::FirstFetchNeedsConfirmation { url, sha256 } => (url, sha256),
            other => panic!("expected FirstFetchNeedsConfirmation, got {other:?}"),
        };
        assert_eq!(url, src.url);
        assert_eq!(sha, sha256_hex(body.as_bytes()));

        // GUI confirms → records the trust.
        record_url_trust(&kh_path, &url, &sha).unwrap();

        // Strict mode again — now URL is known, fetch succeeds.
        fetch_inner_with_client(
            &test_client(),
            &src,
            Some(&kh_path),
            None,
            FirstFetchPolicy::RequireConfirmation,
            1_000,
            None,
        )
        .unwrap();
    }

    #[test]
    fn cache_serves_within_ttl_without_network() {
        let server = httpmock::MockServer::start();
        let body = fixture_json();
        let m = server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .header("etag", "\"v1\"")
                .body(body.clone());
        });
        let dir = TempDir::new().unwrap();
        let src = fixture_url_source(&server.base_url(), "/k.json", None);

        // First fetch — fills cache.
        fetch_inner_at(&src, Some(dir.path()), 1_000).unwrap();
        m.assert_calls(1);

        // Second fetch within TTL — must NOT hit the server.
        fetch_inner_at(&src, Some(dir.path()), 1_000 + src.refresh_seconds - 5).unwrap();
        m.assert_calls(1);
    }

    #[test]
    fn cache_uses_etag_on_refresh_when_server_returns_304() {
        let server = httpmock::MockServer::start();
        let body = fixture_json();
        // Specific mock first — only matches when the client
        // sends `If-None-Match: "v1"`. httpmock evaluates mocks
        // in registration order and returns the first matching
        // one, so the request without the header falls through
        // to the 200 mock below.
        let three_oh_four = server.mock(|when, then| {
            when.method("GET")
                .path("/k.json")
                .header("if-none-match", "\"v1\"");
            then.status(304);
        });
        let two_hundred = server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .header("etag", "\"v1\"")
                .body(body.clone());
        });
        let dir = TempDir::new().unwrap();
        let src = fixture_url_source(&server.base_url(), "/k.json", None);

        // First fetch — server replies 200 with ETag.
        fetch_inner_at(&src, Some(dir.path()), 1_000).unwrap();
        two_hundred.assert_calls(1);

        // After TTL — server replies 304, body served from cache.
        let cat = fetch_inner_at(&src, Some(dir.path()), 1_000 + src.refresh_seconds + 1).unwrap();
        assert_eq!(cat.provider_id, "kimi");
        three_oh_four.assert_calls(1);
        two_hundred.assert_calls(1);
    }

    #[test]
    fn offline_falls_back_to_stale_cache() {
        let body = fixture_json();
        let dir = TempDir::new().unwrap();
        // URL points at a port no one is listening on — the
        // fetch fails. With a stale cache present, the loader
        // serves it as graceful degradation.
        let stale_src = UrlSource {
            url: "http://127.0.0.1:1/k.json".to_owned(),
            sha256: None,
            refresh_seconds: 1,
        };
        let (body_path, meta_path) = cache_paths_for(dir.path(), &stale_src.url);
        let meta = CacheMeta {
            url: stale_src.url.clone(),
            sha256: sha256_hex(body.as_bytes()),
            etag: None,
            fetched_at: 0,
        };
        write_cache(&body_path, &meta_path, &meta, body.as_bytes()).unwrap();

        let cat = fetch_inner_at(&stale_src, Some(dir.path()), 1_000_000).unwrap();
        assert_eq!(cat.provider_id, "kimi");
    }

    #[test]
    fn cache_tampering_is_treated_as_miss() {
        let server = httpmock::MockServer::start();
        let body = fixture_json();
        let m = server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(body.clone());
        });
        let dir = TempDir::new().unwrap();
        let src = fixture_url_source(&server.base_url(), "/k.json", None);
        fetch_inner_at(&src, Some(dir.path()), 1_000).unwrap();
        m.assert_calls(1);

        // Tamper: overwrite the cached body with junk.
        let (body_path, _meta_path) = cache_paths_for(dir.path(), &src.url);
        std::fs::write(&body_path, b"<tampered>").unwrap();

        // Within TTL — read_cache returns None on sha mismatch,
        // so the loader refetches over the wire.
        fetch_inner_at(&src, Some(dir.path()), 1_010).unwrap();
        m.assert_calls(2);
    }

    #[test]
    fn audit_log_records_loaded_event_on_success() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(fixture_json());
        });
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("audit.log");
        let src = fixture_url_source(&server.base_url(), "/k.json", None);

        fetch_inner_with_client(
            &test_client(),
            &src,
            None,
            None,
            FirstFetchPolicy::AutoRecord,
            1_000,
            Some(&log_path),
        )
        .unwrap();

        let body = std::fs::read_to_string(&log_path).unwrap();
        let line = body
            .lines()
            .next()
            .expect("audit log should have at least one line");
        let event: AuditEvent = serde_json::from_str(line).expect("each line is JSON");
        assert_eq!(event.url, src.url);
        assert_eq!(event.outcome, AuditOutcome::Loaded);
        assert!(event.sha256.is_some());
        assert_eq!(event.status_code, Some(200));
        // RFC3339 format check: starts with year + dash.
        assert!(
            event.timestamp.len() >= 19 && event.timestamp.chars().nth(4) == Some('-'),
            "timestamp not RFC3339-ish: {}",
            event.timestamp
        );
    }

    #[test]
    fn audit_log_records_blocked_size_event() {
        let server = httpmock::MockServer::start();
        let payload = "x".repeat(MAX_CATALOG_BODY_BYTES + 1);
        server.mock(|when, then| {
            when.method("GET").path("/big.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(payload);
        });
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("audit.log");
        let src = fixture_url_source(&server.base_url(), "/big.json", None);

        let _ = fetch_inner_with_client(
            &test_client(),
            &src,
            None,
            None,
            FirstFetchPolicy::AutoRecord,
            1_000,
            Some(&log_path),
        );

        let body = std::fs::read_to_string(&log_path).unwrap();
        let event: AuditEvent = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(event.outcome, AuditOutcome::BlockedSize);
    }

    #[test]
    fn audit_log_records_blocked_pin_event() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(fixture_json());
        });
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("audit.log");
        let bogus_pin = "0".repeat(64);
        let src = fixture_url_source(&server.base_url(), "/k.json", Some(bogus_pin));

        let _ = fetch_inner_with_client(
            &test_client(),
            &src,
            None,
            None,
            FirstFetchPolicy::AutoRecord,
            1_000,
            Some(&log_path),
        );

        let body = std::fs::read_to_string(&log_path).unwrap();
        let event: AuditEvent = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(event.outcome, AuditOutcome::BlockedPin);
    }

    #[test]
    fn audit_log_appends_each_call() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/k.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(fixture_json());
        });
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("audit.log");
        let src = fixture_url_source(&server.base_url(), "/k.json", None);

        for _ in 0..3 {
            fetch_inner_with_client(
                &test_client(),
                &src,
                None,
                None,
                FirstFetchPolicy::AutoRecord,
                1_000,
                Some(&log_path),
            )
            .unwrap();
        }

        let body = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            body.lines().count(),
            3,
            "expected 3 appended lines, got {body:?}"
        );
    }

    #[test]
    fn ssrf_blocks_catalog_url_at_fetch_time() {
        // Direct call to fetch_url_source — exercises the
        // public-API SSRF guard for catalog URLs (P23.7 added
        // it; previously SSRF only fired on liveness probes).
        let src = UrlSource {
            url: "https://127.0.0.1/x.json".to_owned(),
            sha256: None,
            refresh_seconds: 60,
        };
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("audit.log");
        let err = fetch_url_source(
            &src,
            None,
            None,
            FirstFetchPolicy::AutoRecord,
            Some(&log_path),
        )
        .unwrap_err();
        assert!(matches!(err, FetchError::Ssrf { .. }));
        let body = std::fs::read_to_string(&log_path).unwrap();
        let event: AuditEvent = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(event.outcome, AuditOutcome::BlockedSsrf);
    }

    #[test]
    fn load_all_with_urls_skips_when_disabled() {
        let cfg = CatalogSourcesConfig {
            enable_url_catalogs: false,
            sources: vec![UrlSource {
                url: "https://example.invalid/should-not-be-fetched.json".into(),
                sha256: None,
                refresh_seconds: 60,
            }],
        };
        let (loaded, errors) = load_all_with_urls(
            &[],
            None,
            None,
            Some(&cfg),
            None,
            None,
            FirstFetchPolicy::AutoRecord,
            None,
        );
        assert!(loaded.is_empty());
        assert!(errors.is_empty(), "fetch should be skipped, no errors");
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
        for asset_path in BundledCatalogAssets::iter() {
            let asset = BundledCatalogAssets::get(asset_path.as_ref())
                .expect("rust-embed asset must be retrievable");
            serde_json::from_slice::<ProviderCatalog>(&asset.data)
                .expect("bundled catalog must parse cleanly");
        }
    }

    /// G-series contract: every bundled variant must tell the
    /// user HOW to obtain the credential AND how to rotate it.
    /// "How to obtain" = a non-empty `retrieval.steps` list
    /// (already required by the schema). "How to rotate" = a
    /// `rotation` block that carries either a `guide_url` or a
    /// non-empty `notes` string — a bare `{method, every_days}`
    /// tells the user *when* to rotate but not *how*, which is
    /// the gap this test guards against regressing.
    #[test]
    fn every_bundled_variant_has_rotation_guidance() {
        for catalog in bundled_catalogs() {
            for variant in &catalog.variants {
                let rotation = variant.rotation.as_ref().unwrap_or_else(|| {
                    panic!(
                        "{}/{}: bundled variants must declare a `rotation` block",
                        catalog.provider_id, variant.id
                    )
                });
                let has_guide = rotation.guide_url.is_some();
                let has_notes = rotation
                    .notes
                    .as_deref()
                    .is_some_and(|n| !n.trim().is_empty());
                assert!(
                    has_guide || has_notes,
                    "{}/{}: rotation block must carry a `guide_url` or non-empty \
                     `notes` — `{{method, every_days}}` alone says when to rotate, \
                     not how",
                    catalog.provider_id,
                    variant.id
                );
            }
        }
    }

    /// Companion to the rotation check: every bundled variant
    /// must point at the provider's official documentation via
    /// `retrieval.docs_url`. `console_url` is where you *make*
    /// the key; `docs_url` is where you *understand* it
    /// (scopes, best practices). The G-series promise is that
    /// the provision dialog can always offer a "Provider docs"
    /// link.
    #[test]
    fn every_bundled_variant_has_a_docs_url() {
        for catalog in bundled_catalogs() {
            for variant in &catalog.variants {
                assert!(
                    variant
                        .retrieval
                        .docs_url
                        .as_deref()
                        .is_some_and(|u| u.starts_with("https://")),
                    "{}/{}: retrieval.docs_url must be a populated https URL",
                    catalog.provider_id,
                    variant.id
                );
            }
        }
    }
}
