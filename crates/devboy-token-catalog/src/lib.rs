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
pub fn load_all_with_urls(
    bundled: &[ProviderCatalog],
    user_dir: Option<&Path>,
    project_dir: Option<&Path>,
    url_config: Option<&CatalogSourcesConfig>,
    known_hashes_path: Option<&Path>,
    cache_dir: Option<&Path>,
    first_fetch: FirstFetchPolicy,
) -> (Vec<LoadedCatalog>, Vec<CatalogError>) {
    let (mut loaded, mut errors) = load_all(bundled, user_dir, project_dir);
    if let Some(cfg) = url_config
        && cfg.enable_url_catalogs
    {
        for src in &cfg.sources {
            match fetch_url_source(src, known_hashes_path, cache_dir, first_fetch) {
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
) -> Result<ProviderCatalog, FetchError> {
    if !source.url.starts_with("https://") {
        return Err(FetchError::HttpsRequired {
            url: source.url.clone(),
        });
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|source| FetchError::Client { source })?;
    fetch_inner_with_client(
        &client,
        source,
        known_hashes_path,
        cache_dir,
        first_fetch,
        unix_now(),
    )
}

/// Internal entry point for [`fetch_url_source`]. Splits out
/// the client, time source, and cache plumbing so tests can
/// drive it against an `http://` mock server (httpmock does
/// not speak TLS) and against synthetic clock values without
/// sleeping.
fn fetch_inner_with_client(
    client: &reqwest::blocking::Client,
    source: &UrlSource,
    known_hashes_path: Option<&Path>,
    cache_dir: Option<&Path>,
    first_fetch: FirstFetchPolicy,
    now_secs: u64,
) -> Result<ProviderCatalog, FetchError> {
    let cache_paths = cache_dir.map(|d| cache_paths_for(d, &source.url));

    // Cache hit within TTL — no network at all.
    if let Some((body_path, meta_path)) = cache_paths.as_ref()
        && let Some((meta, bytes)) = read_cache(meta_path, body_path)
        && now_secs.saturating_sub(meta.fetched_at) < source.refresh_seconds
    {
        let actual = sha256_hex(&bytes);
        enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch)?;
        return parse_catalog_bytes(&bytes);
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
                // Without a cache directory, 304 is unexpected — we
                // never sent If-None-Match. Treat as protocol error.
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
                let actual = sha256_hex(&bytes);
                enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch)?;
                return parse_catalog_bytes(&bytes);
            };
            meta.fetched_at = now_secs;
            let _ = write_meta(meta_path, &meta);
            let actual = sha256_hex(&bytes);
            enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch)?;
            parse_catalog_bytes(&bytes)
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
            let actual = sha256_hex(&bytes);
            enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch)?;
            parse_catalog_bytes(&bytes)
        }
        Err(e) => {
            // Network failed. If we have a stale cache, serve
            // it as graceful degradation — better than nothing
            // when the user is on a flaky link or offline.
            if let Some((body_path, meta_path)) = cache_paths.as_ref()
                && let Some((_meta, bytes)) = read_cache(meta_path, body_path)
            {
                let actual = sha256_hex(&bytes);
                enforce_pin_or_tofu(source, known_hashes_path, &actual, first_fetch)?;
                return parse_catalog_bytes(&bytes);
            }
            Err(e)
        }
    }
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
    fn fetch_rejects_http_scheme_directly() {
        let src = UrlSource {
            url: "http://example.invalid/x.json".into(),
            sha256: None,
            refresh_seconds: 60,
        };
        let err = fetch_url_source(&src, None, None, FirstFetchPolicy::AutoRecord).unwrap_err();
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
        )
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
        for body in BUNDLED_SOURCES {
            serde_json::from_str::<ProviderCatalog>(body)
                .expect("bundled catalog must parse cleanly");
        }
    }
}
