//! JSON-RPC over stdio wire protocol for `SecretSource`
//! plugins per [ADR-021] §10 (subprocess plugin extension).
//!
//! The host (`devboy-tools`) and the plugin exchange one
//! request and one response per line — the same newline-delimited
//! framing the secrets-agent daemon uses. Methods correspond
//! 1:1 to the [`crate::source::SecretSource`] trait so an
//! out-of-tree plugin can implement any backend the framework
//! doesn't ship natively (Doppler, AWS Secrets Manager, custom
//! HSMs, …) without touching the core.
//!
//! ## Method names
//!
//! | RPC method                    | Trait method                           |
//! |-------------------------------|----------------------------------------|
//! | `secret_source.init`          | (no equivalent — capability handshake) |
//! | `secret_source.is_available`  | [`SecretSource::is_available`]         |
//! | `secret_source.get`           | [`SecretSource::get`]                  |
//! | `secret_source.list`          | [`SecretSource::list`]                 |
//! | `secret_source.validate`      | [`SecretSource::validate`]             |
//! | `secret_source.put`           | [`SecretSource::put`]                  |
//! | `secret_source.delete`        | [`SecretSource::delete`]               |
//! | `secret_source.key_material`  | [`SecretSource::key_material`]         |
//!
//! `init` is the first call — the host sends config and gets
//! back the plugin's name + capability bitset. Subsequent
//! calls happen against an initialised session.
//!
//! ## Wire format
//!
//! Each line is a complete JSON-RPC 2.0 frame
//! (id + method or id + result/error). The `params` and
//! `result` payloads are typed via the enums below.
//!
//! ## What this module does **not** do
//!
//! Spawn the subprocess, manage its lifetime, or implement
//! retry / restart semantics. Those concerns live in the
//! plugin client (P15.2) which builds on top of these wire
//! types.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md
//! [`SecretSource::is_available`]: crate::source::SecretSource::is_available
//! [`SecretSource::get`]: crate::source::SecretSource::get
//! [`SecretSource::list`]: crate::source::SecretSource::list
//! [`SecretSource::validate`]: crate::source::SecretSource::validate
//! [`SecretSource::put`]: crate::source::SecretSource::put
//! [`SecretSource::delete`]: crate::source::SecretSource::delete
//! [`SecretSource::key_material`]: crate::source::SecretSource::key_material

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// =============================================================================
// JSON-RPC 2.0 framing
// =============================================================================

/// Pinned protocol version. Bumped on a breaking change so
/// host + plugin can refuse to talk if the major versions
/// don't match.
pub const PROTOCOL_VERSION: &str = "1.1";

/// Wrapper for a single request line. Always includes
/// `jsonrpc = "2.0"`, an integer `id`, and a method name with
/// typed params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRpcRequest {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    #[serde(flatten)]
    pub call: PluginRequest,
}

/// Wrapper for a single response line. Carries either `result`
/// or `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRpcResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: u64,
    #[serde(flatten)]
    pub outcome: RpcOutcome,
}

/// `result` xor `error` — one of, never both. Tagged
/// internally so a malformed response that includes both
/// fields fails to deserialise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcOutcome {
    Result(PluginResponse),
    Error(PluginError),
}

/// Newtype around the literal `"2.0"` so a malformed frame
/// fails fast at parse time instead of at the dispatcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcVersion(String);

impl JsonRpcVersion {
    pub fn current() -> Self {
        Self("2.0".into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_supported(&self) -> bool {
        self.0 == "2.0"
    }
}

impl Default for JsonRpcVersion {
    fn default() -> Self {
        Self::current()
    }
}

// =============================================================================
// Requests
// =============================================================================

/// One request the host sends. Tagged by `method` for clean
/// JSON-RPC 2.0 framing and so a future method addition can be
/// added as a new variant without touching dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum PluginRequest {
    /// First call after spawn. Hands the plugin its name +
    /// per-instance config (the `[[source]]` block from
    /// `sources.toml`) and receives the plugin's name +
    /// capability bitset back.
    #[serde(rename = "secret_source.init")]
    Init(InitParams),
    /// Probe — returns the plugin's current readiness.
    #[serde(rename = "secret_source.is_available")]
    IsAvailable,
    /// Fetch the value at `reference`. Plugin returns
    /// [`GetResult`] or an error.
    #[serde(rename = "secret_source.get")]
    Get(GetParams),
    /// Enumerate the plugin's inventory.
    #[serde(rename = "secret_source.list")]
    List,
    /// Confirm a reference is well-formed without round-trip.
    #[serde(rename = "secret_source.validate")]
    Validate(ValidateParams),
    #[serde(rename = "secret_source.put")]
    Put(PutParams),
    #[serde(rename = "secret_source.delete")]
    Delete(DeleteParams),
    #[serde(rename = "secret_source.key_material")]
    KeyMaterial(KeyMaterialParams),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitParams {
    /// Source name as configured in `sources.toml` (e.g.
    /// `"prod-vault"`). Lets the plugin distinguish multiple
    /// instances of itself.
    pub source_name: String,
    /// Per-instance config from `sources.toml` (free-form
    /// table — the plugin parses what it needs).
    pub config: BTreeMap<String, serde_json::Value>,
    /// Pinned protocol version. Plugin compares to its own and
    /// errors on mismatch.
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetParams {
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateParams {
    pub reference: String,
}

/// Params for `secret_source.put`.
///
/// `Debug` is hand-written: the derived one would print the value,
/// and a request struct is exactly the sort of thing that ends up
/// inside a `{:?}` while someone is debugging transport.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutParams {
    pub reference: String,
    /// The value as plaintext. Same rule as [`GetResult::value`] —
    /// the wire is the only place a raw `String` is acceptable.
    pub value: String,
}

impl std::fmt::Debug for PutParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PutParams")
            .field("reference", &self.reference)
            .field("value", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteParams {
    pub reference: String,
}

/// Params for `secret_source.key_material`.
///
/// The purpose is part of the contract: one plugin may hold the
/// wrapping key for the vault's keyfile envelope and another for a
/// file-backed token store, and returning the wrong bytes fails at
/// decryption time, far from the cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMaterialParams {
    pub purpose: String,
}

// =============================================================================
// Responses
// =============================================================================

/// Successful reply payload. Variants mirror the request set
/// (minus `IsAvailable`/`Validate`/`List` which have their own
/// shapes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginResponse {
    Init(InitResult),
    IsAvailable(IsAvailableResult),
    Get(GetResult),
    List(ListResult),
    Validate(ValidateResult),
    Put(PutResult),
    Delete(DeleteResult),
    KeyMaterial(KeyMaterialResult),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitResult {
    /// Echoed source name — lets the host detect a plugin
    /// that's been pointed at the wrong config.
    pub source_name: String,
    /// Capability bitset (raw bits — see
    /// [`crate::source::Capabilities`] for the layout).
    pub capabilities_bits: u32,
    /// Plugin's self-reported version (advisory; logged for
    /// support).
    pub plugin_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsAvailableResult {
    pub status: IsAvailableStatus,
    /// Optional human-readable detail (e.g. "Vault token
    /// expired in 12s"). The host surfaces this in `doctor`.
    pub detail: Option<String>,
}

/// Ground states the plugin can report. Mirrors
/// [`crate::source::SourceStatus`] but kept independent so the
/// wire protocol can evolve separately from the in-process
/// trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IsAvailableStatus {
    Available,
    Unavailable,
    /// Plugin reachable but its credential is missing /
    /// expired (e.g. `op signin` needed). The host shows the
    /// detail string and may try a fallback source.
    NeedsCredential,
}

/// The answer to `secret_source.get`.
///
/// `Debug` is hand-written for the same reason as [`PutParams`] —
/// and this is the type that carries the user's actual secret on
/// the most-travelled path. It had the derived one, while both of
/// its neighbours ([`PutParams`], [`KeyMaterialResult`]) were
/// given redacting impls, so a `{:?}` anywhere in the response
/// path printed the plaintext. One did: the plugin client's
/// init-handshake mismatch put the whole reply into an error
/// string.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetResult {
    /// The secret value as plaintext. The plugin client wraps
    /// it in [`secrecy::SecretString`] before returning to the
    /// rest of the framework — the wire is the only place a
    /// raw `String` is acceptable.
    pub value: String,
    /// Upstream-reported lease duration, in seconds. `None`
    /// means "no lease" → cache uses default TTL.
    pub lease_seconds: Option<u64>,
}

impl std::fmt::Debug for GetResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No length, unlike `KeyMaterialResult`: that one holds
        // key bytes of a size fixed by the algorithm, where here
        // the length is a property of the user's own secret.
        f.debug_struct("GetResult")
            .field("value", &"<redacted>")
            .field("lease_seconds", &self.lease_seconds)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListResult {
    pub entries: Vec<RemoteRefDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRefDto {
    pub reference: String,
    pub display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateResult {
    /// `true` when the reference parses cleanly. `false` is
    /// reported via [`PluginError::BadReference`] instead so
    /// success is unambiguous.
    pub ok: bool,
}

/// Reply to `secret_source.put`.
///
/// Carries a flag rather than being empty so the plugin can say
/// whether it replaced something. A caller that believes it
/// created a new entry and actually overwrote one is a class of
/// surprise worth naming on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutResult {
    /// `true` when a value already existed at that reference.
    #[serde(default)]
    pub replaced: bool,
}

/// Reply to `secret_source.delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteResult {
    /// `false` when there was nothing there. Not an error:
    /// deletion is an end state, and it already held.
    #[serde(default)]
    pub existed: bool,
}

/// Reply to `secret_source.key_material`.
///
/// Base64 because JSON has no byte string. `Debug` is hand-written
/// so the material cannot reach a log through a derived one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMaterialResult {
    /// Standard base64 of the raw bytes.
    pub material_b64: String,
}

impl std::fmt::Debug for KeyMaterialResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "KeyMaterialResult({} base64 chars, redacted)",
            self.material_b64.len()
        )
    }
}

// =============================================================================
// Errors
// =============================================================================

/// Error variants the plugin can return. The codes mirror
/// JSON-RPC 2.0 reserved ranges; payload is structured so the
/// host can map back to [`crate::source::SourceError`] without
/// regex-parsing strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PluginError {
    /// `secret_source.is_available` returned non-Available.
    #[error("source unavailable: {detail}")]
    Unavailable { detail: String },
    /// Plugin doesn't implement the requested capability
    /// (e.g. `list` on a write-only backend).
    #[error("source does not support capability: {capability}")]
    UnsupportedCapability { capability: String },
    /// Reference string didn't parse for this backend.
    #[error("source rejected reference `{reference}`: {reason}")]
    BadReference { reference: String, reason: String },
    /// Plugin needs a credential it doesn't have (e.g. `op
    /// signin` not yet completed).
    #[error("source needs credential: {detail}")]
    NeedsCredential { detail: String },
    /// Anything else — wire-format parse error, transport
    /// failure, etc. The host treats this as opaque and surfaces
    /// it in logs.
    #[error("source error: {detail}")]
    Other { detail: String },
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(call: PluginRequest, id: u64) -> PluginRpcRequest {
        PluginRpcRequest {
            jsonrpc: JsonRpcVersion::current(),
            id,
            call,
        }
    }

    fn ok(id: u64, resp: PluginResponse) -> PluginRpcResponse {
        PluginRpcResponse {
            jsonrpc: JsonRpcVersion::current(),
            id,
            outcome: RpcOutcome::Result(resp),
        }
    }

    // -- редакция секретов в Debug ---------------------------

    /// A request struct is exactly the sort of thing that ends up
    /// inside a `{:?}` while someone debugs transport, so the
    /// derived `Debug` would have printed the value.
    #[test]
    fn put_params_never_print_the_value() {
        let p = PutParams {
            reference: "personal/github/token".into(),
            value: "ghp_actual_secret_value".into(),
        };
        let shown = format!("{p:?}");

        assert!(!shown.contains("ghp_actual_secret_value"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
        assert!(
            shown.contains("personal/github/token"),
            "the reference is not a secret and is what makes the line useful: {shown}"
        );
    }

    #[test]
    fn key_material_result_never_prints_the_material() {
        let r = KeyMaterialResult {
            material_b64: "c3VwZXItc2VjcmV0LWtleS1tYXRlcmlhbA==".into(),
        };
        let shown = format!("{r:?}");

        assert!(!shown.contains("c3VwZXI"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
    }

    // -- новые методы на проводе -----------------------------

    #[test]
    fn put_round_trips_and_carries_the_value_under_its_own_method_name() {
        let r = req(
            PluginRequest::Put(PutParams {
                reference: "personal/github/token".into(),
                value: "ghp_x".into(),
            }),
            7,
        );
        let json = serde_json::to_value(&r).unwrap();

        assert_eq!(json["method"], "secret_source.put");
        assert_eq!(json["params"]["value"], "ghp_x");

        let back: PluginRpcRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn delete_and_key_material_round_trip() {
        let d = req(
            PluginRequest::Delete(DeleteParams {
                reference: "personal/github/token".into(),
            }),
            8,
        );
        assert_eq!(
            serde_json::to_value(&d).unwrap()["method"],
            "secret_source.delete"
        );
        assert_eq!(
            serde_json::from_value::<PluginRpcRequest>(serde_json::to_value(&d).unwrap()).unwrap(),
            d
        );

        let k = req(
            PluginRequest::KeyMaterial(KeyMaterialParams {
                purpose: "vault-keyfile".into(),
            }),
            9,
        );
        let json = serde_json::to_value(&k).unwrap();
        assert_eq!(json["method"], "secret_source.key_material");
        assert_eq!(json["params"]["purpose"], "vault-keyfile");
    }

    /// `existed: false` is a successful delete, not a failure —
    /// deletion asks for an end state and that state already
    /// held. Defaulting the field keeps a terse plugin honest.
    #[test]
    fn a_delete_reply_defaults_to_nothing_having_been_there() {
        let r: DeleteResult = serde_json::from_str("{}").unwrap();
        assert!(!r.existed);

        let r: PutResult = serde_json::from_str("{}").unwrap();
        assert!(!r.replaced);
    }

    /// The version is pinned so host and plugin can refuse to
    /// talk across a breaking change. Adding methods is additive,
    /// so this moved to 1.1 rather than 2.0 — and a plugin that
    /// only knows the old set still answers everything it did
    /// before.
    #[test]
    fn the_protocol_version_records_the_additive_bump() {
        assert_eq!(PROTOCOL_VERSION, "1.1");
    }

    // -- JSON-RPC framing ------------------------------------

    #[test]
    fn jsonrpc_version_constant_is_v2() {
        assert_eq!(JsonRpcVersion::current().as_str(), "2.0");
        assert!(JsonRpcVersion::current().is_supported());
    }

    #[test]
    fn jsonrpc_version_rejects_anything_other_than_v2() {
        let v = JsonRpcVersion("1.0".into());
        assert!(!v.is_supported());
    }

    // -- Request framing -------------------------------------

    #[test]
    fn init_request_round_trips_through_json() {
        let mut config = BTreeMap::new();
        config.insert("address".into(), json!("https://vault.example.invalid"));
        let r = req(
            PluginRequest::Init(InitParams {
                source_name: "prod-vault".into(),
                config,
                protocol_version: PROTOCOL_VERSION.into(),
            }),
            1,
        );
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains("\"method\":\"secret_source.init\""));
        assert!(line.contains("\"prod-vault\""));
        let back: PluginRpcRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn is_available_request_has_no_params() {
        let r = req(PluginRequest::IsAvailable, 7);
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains("\"method\":\"secret_source.is_available\""));
        let back: PluginRpcRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn get_request_carries_reference_only() {
        let r = req(
            PluginRequest::Get(GetParams {
                reference: "secret/data/team/jira".into(),
            }),
            42,
        );
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains("\"method\":\"secret_source.get\""));
        assert!(line.contains("\"reference\":\"secret/data/team/jira\""));
        let back: PluginRpcRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn list_request_has_no_params() {
        let r = req(PluginRequest::List, 99);
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains("\"method\":\"secret_source.list\""));
        let back: PluginRpcRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn validate_request_round_trips() {
        let r = req(
            PluginRequest::Validate(ValidateParams {
                reference: "op://Private/jira".into(),
            }),
            123,
        );
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains("\"method\":\"secret_source.validate\""));
        let back: PluginRpcRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(back, r);
    }

    // -- Response framing -------------------------------------

    #[test]
    fn init_result_round_trips() {
        let resp = ok(
            1,
            PluginResponse::Init(InitResult {
                source_name: "prod-vault".into(),
                capabilities_bits: 0b0000_0011,
                plugin_version: "0.1.0".into(),
            }),
        );
        let line = serde_json::to_string(&resp).unwrap();
        let back: PluginRpcResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn get_result_round_trips_with_lease() {
        let resp = ok(
            42,
            PluginResponse::Get(GetResult {
                value: "test-value-not-secret".into(),
                lease_seconds: Some(3600),
            }),
        );
        let line = serde_json::to_string(&resp).unwrap();
        let back: PluginRpcResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn list_result_round_trips_empty_and_populated() {
        let resp = ok(99, PluginResponse::List(ListResult { entries: vec![] }));
        let line = serde_json::to_string(&resp).unwrap();
        let back: PluginRpcResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(back, resp);

        let resp2 = ok(
            100,
            PluginResponse::List(ListResult {
                entries: vec![RemoteRefDto {
                    reference: "secret/data/team/jira".into(),
                    display: Some("Jira API token".into()),
                }],
            }),
        );
        let line2 = serde_json::to_string(&resp2).unwrap();
        let back2: PluginRpcResponse = serde_json::from_str(&line2).unwrap();
        assert_eq!(back2, resp2);
    }

    #[test]
    fn is_available_status_strings_are_pinned() {
        assert_eq!(
            serde_json::to_value(IsAvailableStatus::Available).unwrap(),
            json!("available")
        );
        assert_eq!(
            serde_json::to_value(IsAvailableStatus::NeedsCredential).unwrap(),
            json!("needs-credential")
        );
        assert_eq!(
            serde_json::to_value(IsAvailableStatus::Unavailable).unwrap(),
            json!("unavailable")
        );
    }

    // -- Error framing ----------------------------------------

    #[test]
    fn error_response_round_trips_each_kind() {
        for err in [
            PluginError::Unavailable {
                detail: "vault sealed".into(),
            },
            PluginError::UnsupportedCapability {
                capability: "list".into(),
            },
            PluginError::BadReference {
                reference: "garbage".into(),
                reason: "not a vault path".into(),
            },
            PluginError::NeedsCredential {
                detail: "op signin required".into(),
            },
            PluginError::Other {
                detail: "transport timeout".into(),
            },
        ] {
            let envelope = PluginRpcResponse {
                jsonrpc: JsonRpcVersion::current(),
                id: 1,
                outcome: RpcOutcome::Error(err),
            };
            let line = serde_json::to_string(&envelope).unwrap();
            let back: PluginRpcResponse = serde_json::from_str(&line).unwrap();
            assert_eq!(back, envelope);
        }
    }

    #[test]
    fn rpc_outcome_distinguishes_result_from_error_at_parse_time() {
        let line_ok = r#"{"jsonrpc":"2.0","id":1,"result":{"value":"v","lease_seconds":null}}"#;
        let parsed: PluginRpcResponse = serde_json::from_str(line_ok).unwrap();
        assert!(matches!(parsed.outcome, RpcOutcome::Result(_)));

        let line_err = r#"{"jsonrpc":"2.0","id":1,"error":{"kind":"unavailable","detail":"x"}}"#;
        let parsed: PluginRpcResponse = serde_json::from_str(line_err).unwrap();
        assert!(matches!(parsed.outcome, RpcOutcome::Error(_)));
    }
}
