//! Builder that fuses an [`IndexEntry`] (manifest-side
//! metadata) with the active [`LoadedCatalog`] set (the token
//! catalog framework — ADR-023 §3.4) into a single
//! [`DialogMetadata`] for the provision / rotation modal.
//!
//! Why this lives here:
//!
//! - The UI crate (`devboy-secrets-ui`) deliberately knows
//!   nothing about `devboy_token_catalog` so the egui /
//!   ratatui renderers can stay stack-independent.
//! - The binary crate (`devboy-secrets-ui-bin`) owns the
//!   loaded catalogs at startup and is the only place that
//!   sees both halves of the metadata at once.
//!
//! Fallback order per field (catalog wins when it has a
//! match, manifest fills the gaps, hardcoded defaults are the
//! last resort):
//!
//! - `provider`           — second path segment (e.g. `openai`)
//! - `rotation_method`    — variant.rotation.method → entry.rotation_method → `"manual"`
//! - `provisioning_url`   — variant.retrieval.console_url → entry.retrieval_url → None
//! - `format_hint`        — variant.format_hint → None
//! - `description`        — variant.description → None
//! - `retrieval_steps`    — variant.retrieval.steps → []
//! - `retrieval_notes`    — variant.retrieval.notes → None
//! - `docs_url`           — variant.retrieval.docs_url → None
//! - `rotation_guide_url` — variant.rotation.guide_url → None
//! - `rotation_notes`     — variant.rotation.notes → None
//!
//! When **no catalog matches** (the provider segment maps to
//! nothing in the loaded set), every catalog-derived field
//! collapses to its empty value and the dialog falls back to
//! showing only what the manifest carried. This is the
//! pre-S2 behaviour the wizard had before P26.

use devboy_secrets_ui::DialogMetadata;
use devboy_storage::IndexEntry;
use devboy_token_catalog::{LoadedCatalog, RetrievalSpec, TokenVariant};

/// Build dialog metadata for one provision / rotation modal.
///
/// `variant_id` lets the caller pre-select a specific variant
/// (the user picked it in the inventory's variant chip). When
/// `None` the function picks the catalog's first variant —
/// the convention is that catalog authors put the most
/// common variant first.
pub fn metadata_from_catalog_and_entry(
    path: &str,
    entry: &IndexEntry,
    catalogs: &[LoadedCatalog],
    variant_id: Option<&str>,
) -> DialogMetadata {
    let provider = path.split('/').nth(1).unwrap_or("unknown").to_owned();

    // Find the catalog whose `provider_id` matches the path's
    // provider segment, then pick the variant.
    let variant: Option<&TokenVariant> = catalogs
        .iter()
        .find(|c| c.catalog.provider_id == provider)
        .and_then(|c| pick_variant(&c.catalog.variants, variant_id));

    let manifest_rotation = entry
        .rotation_method
        .as_ref()
        .map(|m| format!("{m:?}").to_lowercase());

    DialogMetadata {
        path: path.to_owned(),
        provider,
        rotation_method: variant
            .and_then(|v| v.rotation.as_ref())
            .map(|r| r.method.clone())
            .or(manifest_rotation)
            .unwrap_or_else(|| "manual".to_owned()),
        provisioning_url: variant
            .map(|v| v.retrieval.console_url.clone())
            .or_else(|| entry.retrieval_url.clone()),
        format_hint: variant.and_then(|v| v.format_hint.clone()),
        description: variant.map(|v| v.description.clone()),
        retrieval_steps: variant.map(retrieval_steps).unwrap_or_default(),
        retrieval_notes: variant.and_then(|v| v.retrieval.notes.clone()),
        docs_url: variant.and_then(|v| v.retrieval.docs_url.clone()),
        rotation_guide_url: variant
            .and_then(|v| v.rotation.as_ref())
            .and_then(|r| r.guide_url.clone()),
        rotation_notes: variant
            .and_then(|v| v.rotation.as_ref())
            .and_then(|r| r.notes.clone()),
    }
}

fn pick_variant<'a>(
    variants: &'a [TokenVariant],
    variant_id: Option<&str>,
) -> Option<&'a TokenVariant> {
    match variant_id {
        Some(id) => variants
            .iter()
            .find(|v| v.id == id)
            .or_else(|| variants.first()),
        None => variants.first(),
    }
}

fn retrieval_steps(variant: &TokenVariant) -> Vec<String> {
    let RetrievalSpec { steps, .. } = &variant.retrieval;
    steps.clone()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_storage::RotationMethod;
    use devboy_token_catalog::{
        AuthSpec, CatalogSource, LivenessSpec, ProviderCatalog, RetrievalSpec, RotationSpec,
        TokenVariant,
    };

    fn catalog(provider_id: &str, variants: Vec<TokenVariant>) -> LoadedCatalog {
        LoadedCatalog {
            catalog: ProviderCatalog {
                schema: None,
                schema_version: 1,
                provider_id: provider_id.to_owned(),
                display_name: provider_id.to_owned(),
                description: None,
                variants,
                env_var_patterns: Vec::new(),
                env_var_skip: Vec::new(),
            },
            source: CatalogSource::Bundled,
            path: None,
        }
    }

    fn variant_min(id: &str, description: &str) -> TokenVariant {
        TokenVariant {
            id: id.to_owned(),
            display_name: id.to_owned(),
            description: description.to_owned(),
            format_regex: None,
            format_hint: None,
            retrieval: RetrievalSpec {
                console_url: format!("https://example.invalid/{id}"),
                docs_url: None,
                steps: Vec::new(),
                notes: None,
            },
            liveness: None,
            rotation: None,
            default_keychain_account: None,
        }
    }

    #[test]
    fn no_catalog_match_falls_back_to_manifest_only() {
        let entry = IndexEntry {
            retrieval_url: Some("https://manifest.invalid/get".into()),
            rotation_method: Some(RotationMethod::Manual),
            ..IndexEntry::default()
        };
        let meta = metadata_from_catalog_and_entry("personal/unknown/key", &entry, &[], None);
        assert_eq!(meta.path, "personal/unknown/key");
        assert_eq!(meta.provider, "unknown");
        assert_eq!(meta.rotation_method, "manual");
        // Manifest URL leaks through when no catalog matched.
        assert_eq!(
            meta.provisioning_url.as_deref(),
            Some("https://manifest.invalid/get")
        );
        // Catalog-only fields are empty.
        assert!(meta.format_hint.is_none());
        assert!(meta.description.is_none());
        assert!(meta.retrieval_steps.is_empty());
        assert!(meta.retrieval_notes.is_none());
    }

    #[test]
    fn catalog_match_populates_every_field() {
        let v = TokenVariant {
            id: "openai-api-key".into(),
            display_name: "OpenAI API key".into(),
            description: "Workspace-scoped key".into(),
            format_regex: Some(r"^sk-(proj-)?[A-Za-z0-9_-]{20,}$".into()),
            format_hint: Some("starts with sk-, 20+ chars".into()),
            retrieval: RetrievalSpec {
                console_url: "https://platform.openai.com/api-keys".into(),
                docs_url: Some("https://platform.openai.com/docs/api-reference/authentication".into()),
                steps: vec![
                    "Open platform.openai.com/api-keys".into(),
                    "Click Create new secret key".into(),
                    "Copy the value — it is shown once".into(),
                ],
                notes: Some("Prefer sk-proj-... for server-to-server".into()),
            },
            liveness: Some(LivenessSpec {
                kind: "http".into(),
                url: "https://api.openai.com/v1/models".into(),
                method: "GET".into(),
                auth: AuthSpec::Bearer,
                expect_status: 200,
            }),
            rotation: Some(RotationSpec {
                method: "manual".into(),
                every_days: 90,
                guide_url: Some("https://platform.openai.com/docs/guides/production-best-practices".into()),
                notes: Some("Create the new key first, deploy it, then revoke the old one — there is no overlap constraint.".into()),
            }),
            default_keychain_account: None,
        };
        let catalogs = vec![catalog("openai", vec![v])];
        let meta = metadata_from_catalog_and_entry(
            "team/openai/api-key",
            &IndexEntry::default(),
            &catalogs,
            None,
        );
        assert_eq!(meta.provider, "openai");
        // Catalog console_url wins over the manifest's empty url.
        assert_eq!(
            meta.provisioning_url.as_deref(),
            Some("https://platform.openai.com/api-keys")
        );
        assert_eq!(
            meta.format_hint.as_deref(),
            Some("starts with sk-, 20+ chars")
        );
        assert_eq!(meta.description.as_deref(), Some("Workspace-scoped key"));
        assert_eq!(meta.retrieval_steps.len(), 3);
        assert!(meta.retrieval_steps[1].contains("Create new secret key"));
        assert_eq!(
            meta.retrieval_notes.as_deref(),
            Some("Prefer sk-proj-... for server-to-server")
        );
        assert_eq!(meta.rotation_method, "manual");
        // Guide fields (G-series): docs_url + rotation
        // guidance must thread through from the catalog.
        assert_eq!(
            meta.docs_url.as_deref(),
            Some("https://platform.openai.com/docs/api-reference/authentication")
        );
        assert_eq!(
            meta.rotation_guide_url.as_deref(),
            Some("https://platform.openai.com/docs/guides/production-best-practices")
        );
        assert!(
            meta.rotation_notes
                .as_deref()
                .unwrap()
                .contains("revoke the old one"),
            "rotation_notes must carry the concrete procedure"
        );
    }

    #[test]
    fn guide_fields_collapse_to_none_when_catalog_omits_them() {
        // variant_min builds a variant with no docs_url and no
        // rotation block at all — every guide field must land
        // as None rather than panicking or fabricating a URL.
        let catalogs = vec![catalog("plain", vec![variant_min("v", "minimal variant")])];
        let meta = metadata_from_catalog_and_entry(
            "team/plain/key",
            &IndexEntry::default(),
            &catalogs,
            None,
        );
        assert!(meta.docs_url.is_none());
        assert!(meta.rotation_guide_url.is_none());
        assert!(meta.rotation_notes.is_none());
    }

    #[test]
    fn variant_id_argument_selects_named_variant_over_default() {
        let catalogs = vec![catalog(
            "kimi",
            vec![
                variant_min("kimi-cn", "Mainland China endpoint"),
                variant_min("kimi-global", "Global endpoint"),
            ],
        )];
        let meta = metadata_from_catalog_and_entry(
            "personal/kimi/api-key",
            &IndexEntry::default(),
            &catalogs,
            Some("kimi-global"),
        );
        assert_eq!(meta.description.as_deref(), Some("Global endpoint"));
        assert_eq!(
            meta.provisioning_url.as_deref(),
            Some("https://example.invalid/kimi-global")
        );
    }

    #[test]
    fn variant_id_falls_back_to_first_when_id_not_found() {
        let catalogs = vec![catalog(
            "kimi",
            vec![
                variant_min("kimi-cn", "Mainland China endpoint"),
                variant_min("kimi-global", "Global endpoint"),
            ],
        )];
        let meta = metadata_from_catalog_and_entry(
            "personal/kimi/api-key",
            &IndexEntry::default(),
            &catalogs,
            Some("kimi-typo-not-found"),
        );
        // First variant wins when the named one is missing —
        // surfacing *something* is better than rendering an
        // empty dialog.
        assert_eq!(meta.description.as_deref(), Some("Mainland China endpoint"));
    }

    #[test]
    fn no_variants_in_catalog_yields_manifest_only_metadata() {
        let catalogs = vec![catalog("empty-provider", vec![])];
        let entry = IndexEntry {
            retrieval_url: Some("https://manifest.invalid/x".into()),
            ..IndexEntry::default()
        };
        let meta =
            metadata_from_catalog_and_entry("team/empty-provider/key", &entry, &catalogs, None);
        // Catalog matched the provider but has no variants —
        // we still pick up provider segment correctly.
        assert_eq!(meta.provider, "empty-provider");
        assert!(meta.description.is_none());
        assert_eq!(
            meta.provisioning_url.as_deref(),
            Some("https://manifest.invalid/x")
        );
    }

    #[test]
    fn rotation_method_from_catalog_wins_over_manifest() {
        let v = TokenVariant {
            rotation: Some(RotationSpec {
                method: "provider-ui".into(),
                every_days: 30,
                guide_url: None,
                notes: None,
            }),
            ..variant_min("v", "x")
        };
        let catalogs = vec![catalog("p", vec![v])];
        let entry = IndexEntry {
            rotation_method: Some(RotationMethod::Manual),
            ..IndexEntry::default()
        };
        let meta = metadata_from_catalog_and_entry("team/p/k", &entry, &catalogs, None);
        // Catalog has the most authoritative rotation
        // procedure for this token kind; manifest is a fallback.
        assert_eq!(meta.rotation_method, "provider-ui");
    }

    #[test]
    fn path_with_too_few_segments_yields_unknown_provider() {
        let meta =
            metadata_from_catalog_and_entry("just-one-segment", &IndexEntry::default(), &[], None);
        assert_eq!(meta.provider, "unknown");
    }

    #[test]
    fn empty_catalog_list_collapses_every_catalog_field() {
        let entry = IndexEntry {
            retrieval_url: Some("https://manifest.invalid/x".into()),
            rotation_method: Some(RotationMethod::Manual),
            ..IndexEntry::default()
        };
        let meta = metadata_from_catalog_and_entry("team/openai/api-key", &entry, &[], None);
        assert!(meta.format_hint.is_none());
        assert!(meta.description.is_none());
        assert!(meta.retrieval_steps.is_empty());
        assert!(meta.retrieval_notes.is_none());
        assert_eq!(meta.rotation_method, "manual");
    }
}
