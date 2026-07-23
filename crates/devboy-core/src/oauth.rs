//! OAuth 2.1 client for proxy MCP upstream authentication (issue #306).
//!
//! Implements the client half of the MCP authorization spec against any
//! OAuth-2.1-compliant server, using only the standard RFCs the server already
//! advertises — no vendor-specific logic:
//!
//! - **Discovery** ([`discover`]): RFC 9728 protected-resource metadata (located
//!   via the upstream's `WWW-Authenticate` challenge) → RFC 8414 authorization-
//!   server metadata.
//! - Dynamic registration (RFC 7591), the device grant (RFC 8628) and refresh
//!   (RFC 6749 §6) build on the [`AuthServerMetadata`] discovered here.

use chrono::{DateTime, Duration, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// RFC 9728 §3.2 — OAuth Protected Resource Metadata (subset we consume).
#[derive(Debug, Clone, Deserialize)]
pub struct ProtectedResourceMetadata {
    /// Authorization servers that can issue tokens for this resource.
    #[serde(default)]
    pub authorization_servers: Vec<String>,
}

/// RFC 8414 §2 — Authorization Server Metadata (subset we consume).
#[derive(Debug, Clone, Deserialize)]
pub struct AuthServerMetadata {
    /// The AS's issuer identifier.
    pub issuer: String,
    /// RFC 6749 token endpoint (device_code + refresh_token grants).
    pub token_endpoint: String,
    /// RFC 8628 §4 device authorization endpoint.
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
    /// RFC 7591 dynamic client registration endpoint.
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    /// Scopes the AS supports; used as a default when the config omits `scopes`.
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

/// Failures across the OAuth client flow.
#[derive(Debug, Error)]
pub enum OAuthError {
    /// The `WWW-Authenticate` challenge lacked a `resource_metadata` parameter.
    #[error("no `resource_metadata` in WWW-Authenticate challenge")]
    NoResourceMetadata,
    /// The protected-resource metadata advertised no authorization server.
    #[error("no authorization server advertised by the resource")]
    NoAuthorizationServer,
    /// The AS metadata omits a device_authorization_endpoint (RFC 8628 §4).
    #[error("authorization server does not advertise a device_authorization_endpoint")]
    NoDeviceEndpoint,
    /// Network/HTTP failure talking to a discovery or token endpoint.
    #[error("HTTP error: {0}")]
    Http(String),
    /// A metadata or token response could not be parsed.
    #[error("malformed response: {0}")]
    Malformed(String),
    /// The token endpoint returned an OAuth `error` code (RFC 6749 §5.2 /
    /// RFC 8628 §3.5: `authorization_pending`, `slow_down`, `access_denied`,
    /// `expired_token`, `invalid_grant`, …).
    #[error("oauth error: {0}")]
    Oauth(String),
}

/// Extract the `resource_metadata` URL from an RFC 9728 `WWW-Authenticate`
/// challenge value, e.g. `Bearer resource_metadata="https://…/.well-known/…"`.
/// Handles quoted and bare forms and ignores surrounding params (`realm`, …).
pub(crate) fn parse_www_authenticate(value: &str) -> Option<String> {
    let idx = value.find("resource_metadata")?;
    let rest = value[idx + "resource_metadata".len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let url = if let Some(quoted) = rest.strip_prefix('"') {
        quoted.split('"').next()?
    } else {
        rest.split([',', ' ']).next()?
    };
    (!url.is_empty()).then(|| url.to_string())
}

/// Guard an outbound discovery/endpoint URL before we fetch it or POST secrets
/// to it. Metadata and token endpoints must be reached over `https` (`http` is
/// tolerated only for a genuine loopback host). A malicious upstream can put
/// anything in its `WWW-Authenticate` challenge or its advertised metadata, so
/// this parses the URL properly and rejects: credentials in the authority
/// (`http://localhost@evil/`), non-http(s) schemes (`file://`, …), and — for
/// https — IP literals in private/link-local/loopback ranges. That keeps a
/// crafted value from steering the client at an internal address or a plaintext
/// endpoint (e.g. exfiltrating a rotating refresh token).
///
/// Not caught here: a *hostname* that resolves to a private address (DNS
/// rebinding). Discovery clients additionally disable redirect-following (so a
/// single crafted hop can't downgrade the target); connect-time IP pinning is a
/// documented follow-up.
pub(crate) fn require_web_url(raw: &str) -> Result<(), OAuthError> {
    let parsed = url::Url::parse(raw)
        .map_err(|e| OAuthError::Oauth(format!("invalid discovery URL {raw:?}: {e}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(OAuthError::Oauth(format!(
            "discovery URL must not carry credentials: {raw}"
        )));
    }
    let host = parsed
        .host()
        .ok_or_else(|| OAuthError::Oauth(format!("discovery URL has no host: {raw}")))?;
    if parsed.host_str().unwrap_or("").is_empty() {
        return Err(OAuthError::Oauth(format!(
            "discovery URL has no host: {raw}"
        )));
    }
    match parsed.scheme() {
        "https" => {
            if host_is_private(&host) {
                return Err(OAuthError::Oauth(format!(
                    "refusing discovery URL at a private/link-local/loopback address: {raw}"
                )));
            }
            Ok(())
        }
        "http" if host_is_loopback(&host) => Ok(()),
        "http" => Err(OAuthError::Oauth(format!(
            "refusing non-loopback http discovery URL (use https): {raw}"
        ))),
        other => Err(OAuthError::Oauth(format!(
            "refusing discovery URL with non-http(s) scheme {other:?}: {raw}"
        ))),
    }
}

/// A host that is unambiguously loopback: exact `localhost`, `127.0.0.0/8`, `::1`.
fn host_is_loopback(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// An IP-literal host in a private, link-local, loopback, or unspecified range.
/// Domain names return `false` (see the rebinding note on [`require_web_url`]).
fn host_is_private(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(_) => false,
        url::Host::Ipv4(ip) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
        }
        url::Host::Ipv6(ip) => {
            let seg0 = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// Discover authorization-server metadata for an upstream, starting from its
/// `WWW-Authenticate` challenge (RFC 9728 → RFC 8414).
pub async fn discover(
    http: &reqwest::Client,
    www_authenticate: &str,
) -> Result<AuthServerMetadata, OAuthError> {
    let resource_meta_url =
        parse_www_authenticate(www_authenticate).ok_or(OAuthError::NoResourceMetadata)?;
    require_web_url(&resource_meta_url)?;
    let prm: ProtectedResourceMetadata = http
        .get(&resource_meta_url)
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .error_for_status()
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| OAuthError::Malformed(e.to_string()))?;
    let as_base = prm
        .authorization_servers
        .into_iter()
        .next()
        .ok_or(OAuthError::NoAuthorizationServer)?;
    fetch_as_metadata(http, &as_base).await
}

/// Fetch RFC 8414 authorization-server metadata. Per RFC 8414 §3.1 / RFC 8615
/// the well-known segment goes *after* the authority and *before* any issuer
/// path (`https://host/tenant` → `https://host/.well-known/oauth-authorization-server/tenant`),
/// not naively appended. The advertised `issuer` is verified to match (§3.3),
/// and every endpoint the AS advertises is scheme/host-guarded before we POST
/// secrets to it.
pub async fn fetch_as_metadata(
    http: &reqwest::Client,
    issuer: &str,
) -> Result<AuthServerMetadata, OAuthError> {
    require_web_url(issuer)?;
    let well_known = well_known_url(issuer)?;
    let meta: AuthServerMetadata = http
        .get(&well_known)
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .error_for_status()
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| OAuthError::Malformed(e.to_string()))?;
    // RFC 8414 §3.3: the returned issuer MUST match the one we asked about.
    if meta.issuer.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(OAuthError::Malformed(format!(
            "issuer mismatch: requested {issuer:?}, metadata declares {:?}",
            meta.issuer
        )));
    }
    // Guard every endpoint we may POST secrets to before trusting it.
    require_web_url(&meta.token_endpoint)?;
    if let Some(ep) = &meta.device_authorization_endpoint {
        require_web_url(ep)?;
    }
    if let Some(ep) = &meta.registration_endpoint {
        require_web_url(ep)?;
    }
    Ok(meta)
}

/// Build the RFC 8414 well-known metadata URL for an issuer, inserting the
/// well-known segment after the authority and preserving any issuer path.
fn well_known_url(issuer: &str) -> Result<String, OAuthError> {
    let parsed = url::Url::parse(issuer)
        .map_err(|e| OAuthError::Oauth(format!("invalid issuer {issuer:?}: {e}")))?;
    let authority = parsed
        .host_str()
        .ok_or_else(|| OAuthError::Oauth(format!("issuer has no host: {issuer}")))?;
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    let path = parsed.path().trim_end_matches('/'); // "" or "/tenant"
    Ok(format!(
        "{}://{}{}/.well-known/oauth-authorization-server{}",
        parsed.scheme(),
        authority,
        port,
        path
    ))
}

/// Device + refresh grant type identifiers this client registers for.
pub(crate) const GRANT_DEVICE_CODE: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub(crate) const GRANT_REFRESH_TOKEN: &str = "refresh_token";

/// RFC 7591 §2 dynamic client registration request. We register a **public**
/// client (no secret, `token_endpoint_auth_method = "none"`) that uses the
/// device-code and refresh-token grants — exactly what a CLI needs.
#[derive(Debug, Serialize)]
struct ClientRegistrationRequest {
    client_name: String,
    grant_types: Vec<String>,
    token_endpoint_auth_method: String,
}

/// RFC 7591 §3.2.1 registration response (subset — we only need `client_id`).
#[derive(Debug, Clone, Deserialize)]
pub struct ClientRegistrationResponse {
    pub client_id: String,
}

/// Register a public device-flow client via RFC 7591 and return its `client_id`.
/// Callers persist the id into `ProxyOAuthConfig.client_id` so re-login reuses it.
pub async fn register_client(
    http: &reqwest::Client,
    registration_endpoint: &str,
    client_name: &str,
) -> Result<String, OAuthError> {
    let req = ClientRegistrationRequest {
        client_name: client_name.to_string(),
        grant_types: vec![
            GRANT_DEVICE_CODE.to_string(),
            GRANT_REFRESH_TOKEN.to_string(),
        ],
        token_endpoint_auth_method: "none".to_string(),
    };
    let resp: ClientRegistrationResponse = http
        .post(registration_endpoint)
        .json(&req)
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .error_for_status()
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| OAuthError::Malformed(e.to_string()))?;
    Ok(resp.client_id)
}

/// RFC 8628 §3.2 device authorization response.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: i64,
    /// Minimum seconds between polls. RFC 8628 §3.2 default is 5.
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// RFC 6749 §5.1 successful token response (device_code + refresh grants).
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: SecretString,
    #[serde(default)]
    pub refresh_token: Option<SecretString>,
    /// Access-token lifetime in seconds (used to compute `expires_at`).
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub token_type: Option<String>,
}

/// One device-token poll outcome (RFC 8628 §3.5).
#[derive(Debug)]
pub enum DevicePollOutcome {
    /// Not yet approved — keep polling at the current interval.
    Pending,
    /// Polled too fast — the client must lengthen the interval by 5s.
    SlowDown,
    /// User approved — tokens issued.
    Granted(TokenResponse),
}

/// POST the RFC 8628 §3.1 device authorization request (form-encoded).
pub async fn request_device_authorization(
    http: &reqwest::Client,
    device_endpoint: &str,
    client_id: &str,
    scope: Option<&str>,
) -> Result<DeviceAuthResponse, OAuthError> {
    let mut form: Vec<(&str, &str)> = vec![("client_id", client_id)];
    if let Some(s) = scope {
        form.push(("scope", s));
    }
    http.post(device_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .error_for_status()
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| OAuthError::Malformed(e.to_string()))
}

/// Poll the token endpoint once with the device_code grant (RFC 8628 §3.4/§3.5).
/// `authorization_pending`/`slow_down` map to non-terminal outcomes; other error
/// codes (`access_denied`, `expired_token`, …) surface as [`OAuthError::Oauth`].
pub async fn poll_device_token_once(
    http: &reqwest::Client,
    token_endpoint: &str,
    device_code: &str,
    client_id: &str,
) -> Result<DevicePollOutcome, OAuthError> {
    let form = [
        ("grant_type", GRANT_DEVICE_CODE),
        ("device_code", device_code),
        ("client_id", client_id),
    ];
    let resp = http
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;
    if status.is_success() {
        let tokens: TokenResponse = serde_json::from_str(&body).map_err(|_| {
            // Don't fold the raw body into the error — it carries the tokens.
            OAuthError::Malformed("authorization server returned a malformed token response".into())
        })?;
        return Ok(DevicePollOutcome::Granted(tokens));
    }
    match parse_oauth_error(&body).as_deref() {
        Some("authorization_pending") => Ok(DevicePollOutcome::Pending),
        Some("slow_down") => Ok(DevicePollOutcome::SlowDown),
        Some(other) => Err(OAuthError::Oauth(other.to_string())),
        None => Err(OAuthError::Oauth(format!("HTTP {status}: {body}"))),
    }
}

/// RFC 6749 §6 refresh-token grant — exchange a refresh_token for a fresh set.
///
/// The DevBoy AS **rotates** the refresh_token: the response carries a new one
/// and the old is deactivated. Callers therefore MUST (1) persist the returned
/// pair immediately and (2) serialize refreshes (single-flight) so two requests
/// never race on the same — now dead — refresh_token. An `invalid_grant` error
/// means the refresh_token is spent/revoked → the caller should re-run login.
pub async fn refresh(
    http: &reqwest::Client,
    token_endpoint: &str,
    refresh_token: &str,
    client_id: &str,
) -> Result<TokenResponse, OAuthError> {
    let form = [
        ("grant_type", GRANT_REFRESH_TOKEN),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    let resp = http
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;
    if status.is_success() {
        // Don't fold the raw body into the error — it carries the tokens.
        return serde_json::from_str(&body).map_err(|_| {
            OAuthError::Malformed("authorization server returned a malformed token response".into())
        });
    }
    match parse_oauth_error(&body) {
        Some(code) => Err(OAuthError::Oauth(code)),
        None => Err(OAuthError::Oauth(format!("HTTP {status}: {body}"))),
    }
}

/// serde helpers for `SecretString` fields that must round-trip through the
/// (keychain-protected) JSON blob. Exposing a secret to serialize it is done
/// explicitly here rather than via a blanket `SerializableSecret` impl, so the
/// only place tokens become plaintext is this well-scoped boundary.
mod secret_serde {
    use secrecy::{ExposeSecret, SecretString};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(s: &SecretString, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(s.expose_secret())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<SecretString, D::Error> {
        Ok(SecretString::from(String::deserialize(de)?))
    }
}

/// A persisted OAuth token set for one proxy upstream. Serialized as JSON into
/// the credential store under a per-server key (e.g. `proxy.<name>.oauth`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    #[serde(with = "secret_serde")]
    pub access_token: SecretString,
    #[serde(with = "secret_serde")]
    pub refresh_token: SecretString,
    /// Absolute UTC instant the access token expires.
    pub expires_at: DateTime<Utc>,
}

impl OAuthTokens {
    /// Build from a token response, computing `expires_at` from `expires_in`
    /// relative to `now`. Falls back to `prev_refresh` when the response omits
    /// `refresh_token` (defensive — the DevBoy AS always rotates and re-sends it).
    pub fn from_response(
        resp: TokenResponse,
        now: DateTime<Utc>,
        prev_refresh: Option<&str>,
    ) -> Result<Self, OAuthError> {
        let refresh_token = resp
            .refresh_token
            .or_else(|| prev_refresh.map(|s| SecretString::from(s.to_string())))
            .ok_or_else(|| OAuthError::Malformed("token response has no refresh_token".into()))?;
        // Clamp the advertised lifetime. A malicious/buggy AS can send
        // `expires_in = i64::MAX` (a valid JSON integer), and `now + Duration`
        // panics on overflow — which in `OAuthAuth::refresh` would abort the
        // running MCP proxy on a live tool call. 400 days is far beyond any real
        // access-token lifetime; capping only means we refresh a little sooner.
        const MAX_TTL_SECS: i64 = 400 * 24 * 60 * 60;
        let ttl = resp.expires_in.unwrap_or(3600).clamp(0, MAX_TTL_SECS);
        Ok(Self {
            access_token: resp.access_token,
            refresh_token,
            expires_at: now + Duration::seconds(ttl),
        })
    }

    /// True when the access token is expired or within `skew` of expiry — the
    /// signal for a pre-flight refresh.
    pub fn is_near_expiry(&self, now: DateTime<Utc>, skew: Duration) -> bool {
        self.expires_at <= now + skew
    }
}

/// Extract the `error` code from an RFC 6749 §5.2 error response body.
pub(crate) fn parse_oauth_error(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrBody {
        error: String,
    }
    serde_json::from_str::<ErrBody>(body).ok().map(|e| e.error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn parse_www_authenticate_quoted() {
        let v = r#"Bearer resource_metadata="https://app.devboy.pro/.well-known/oauth-protected-resource""#;
        assert_eq!(
            parse_www_authenticate(v).as_deref(),
            Some("https://app.devboy.pro/.well-known/oauth-protected-resource")
        );
    }

    #[test]
    fn parse_www_authenticate_ignores_other_params() {
        let v = r#"Bearer realm="mcp", resource_metadata="https://x/y", error="invalid_token""#;
        assert_eq!(parse_www_authenticate(v).as_deref(), Some("https://x/y"));
    }

    #[test]
    fn parse_www_authenticate_bare_value() {
        let v = "Bearer resource_metadata=https://x/y error=foo";
        assert_eq!(parse_www_authenticate(v).as_deref(), Some("https://x/y"));
    }

    #[test]
    fn parse_www_authenticate_absent() {
        assert!(parse_www_authenticate("Bearer realm=\"x\"").is_none());
    }

    #[test]
    fn require_web_url_enforces_https_and_loopback() {
        // https to a public host is fine
        assert!(require_web_url("https://as.example.com/.well-known/x").is_ok());
        // http tolerated only for genuine loopback hosts
        assert!(require_web_url("http://localhost:8080/meta").is_ok());
        assert!(require_web_url("http://127.0.0.1/meta").is_ok());
        assert!(require_web_url("http://[::1]:9000/meta").is_ok());
        // non-loopback http is refused (must be https)
        assert!(require_web_url("http://evil.internal/meta").is_err());
        assert!(require_web_url("http://169.254.169.254/latest/meta-data").is_err());
        // non-web schemes are refused outright (SSRF surface)
        assert!(require_web_url("file:///etc/passwd").is_err());
        assert!(require_web_url("ftp://host/x").is_err());
        assert!(require_web_url("gopher://host").is_err());
        // unparsable input is rejected
        assert!(require_web_url("not a url").is_err());
    }

    #[test]
    fn require_web_url_blocks_the_reviewed_bypasses() {
        // https to a private/link-local/loopback IP literal (the guard's whole
        // point — previously only the scheme was checked).
        assert!(require_web_url("https://169.254.169.254/latest/meta-data").is_err());
        assert!(require_web_url("https://10.0.0.5/token").is_err());
        assert!(require_web_url("https://192.168.1.1/x").is_err());
        assert!(require_web_url("https://172.16.0.1/x").is_err());
        assert!(require_web_url("https://127.0.0.1/x").is_err());
        assert!(require_web_url("https://[::1]/x").is_err());
        assert!(require_web_url("https://[fd00::1]/x").is_err()); // ULA
        assert!(require_web_url("https://[fe80::1]/x").is_err()); // link-local
        // userinfo authority-confusion: real host is evil.com, not localhost.
        assert!(require_web_url("http://localhost:80@evil.com/x").is_err());
        assert!(require_web_url("http://localhost@evil.com/x").is_err());
        assert!(require_web_url("https://as.example.com@evil.com/x").is_err());
        // look-alike domain that merely starts with "127." is NOT loopback.
        assert!(require_web_url("http://127.0.0.1.evil.com/x").is_err());
        assert!(require_web_url("http://localhost.evil.com/x").is_err());
    }

    #[test]
    fn well_known_url_is_rfc8414_path_aware() {
        // hostless issuer → segment right after the authority
        assert_eq!(
            well_known_url("https://as.example.com").unwrap(),
            "https://as.example.com/.well-known/oauth-authorization-server"
        );
        // trailing slash normalized
        assert_eq!(
            well_known_url("https://as.example.com/").unwrap(),
            "https://as.example.com/.well-known/oauth-authorization-server"
        );
        // path-bearing issuer → well-known BEFORE the path (RFC 8414 §3.1)
        assert_eq!(
            well_known_url("https://ex.com/tenant1").unwrap(),
            "https://ex.com/.well-known/oauth-authorization-server/tenant1"
        );
        // port preserved
        assert_eq!(
            well_known_url("https://ex.com:8443/t").unwrap(),
            "https://ex.com:8443/.well-known/oauth-authorization-server/t"
        );
    }

    #[test]
    fn as_metadata_deserializes() {
        let json = r#"{
            "issuer": "https://as.example.com",
            "token_endpoint": "https://as.example.com/token",
            "device_authorization_endpoint": "https://as.example.com/device",
            "registration_endpoint": "https://as.example.com/register",
            "scopes_supported": ["mcp:read", "mcp:write"]
        }"#;
        let m: AuthServerMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(m.token_endpoint, "https://as.example.com/token");
        assert_eq!(
            m.device_authorization_endpoint.as_deref(),
            Some("https://as.example.com/device")
        );
        assert_eq!(m.scopes_supported.len(), 2);
    }

    #[test]
    fn as_metadata_minimal_optional_fields_default() {
        let json = r#"{"issuer":"https://as","token_endpoint":"https://as/token"}"#;
        let m: AuthServerMetadata = serde_json::from_str(json).unwrap();
        assert!(m.device_authorization_endpoint.is_none());
        assert!(m.registration_endpoint.is_none());
        assert!(m.scopes_supported.is_empty());
    }

    #[test]
    fn protected_resource_metadata_deserializes() {
        let json = r#"{"authorization_servers": ["https://as.example.com"]}"#;
        let m: ProtectedResourceMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(m.authorization_servers, vec!["https://as.example.com"]);
    }

    #[test]
    fn registration_request_is_public_device_client() {
        let req = ClientRegistrationRequest {
            client_name: "devboy-cli".into(),
            grant_types: vec![GRANT_DEVICE_CODE.into(), GRANT_REFRESH_TOKEN.into()],
            token_endpoint_auth_method: "none".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["token_endpoint_auth_method"], "none");
        assert_eq!(v["grant_types"][0], GRANT_DEVICE_CODE);
        assert_eq!(v["grant_types"][1], "refresh_token");
    }

    #[test]
    fn registration_response_deserializes() {
        let json = r#"{"client_id": "cli-xyz", "client_id_issued_at": 123, "client_secret": null}"#;
        let r: ClientRegistrationResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.client_id, "cli-xyz");
    }

    #[test]
    fn device_auth_response_deserializes_with_defaults() {
        let json = r#"{
            "device_code": "dc123",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://as/device",
            "verification_uri_complete": "https://as/device?user_code=WDJB-MJHT",
            "expires_in": 900,
            "interval": 5
        }"#;
        let d: DeviceAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(d.user_code, "WDJB-MJHT");
        assert_eq!(d.interval, 5);
        assert_eq!(
            d.verification_uri_complete.as_deref(),
            Some("https://as/device?user_code=WDJB-MJHT")
        );
    }

    #[test]
    fn device_auth_interval_defaults_to_5() {
        let json = r#"{"device_code":"d","user_code":"U","verification_uri":"https://as/d","expires_in":600}"#;
        let d: DeviceAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(d.interval, 5);
        assert!(d.verification_uri_complete.is_none());
    }

    #[test]
    fn token_response_deserializes() {
        let json = r#"{"access_token":"at","refresh_token":"rt","expires_in":7776000,"token_type":"Bearer"}"#;
        let t: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(t.access_token.expose_secret(), "at");
        assert_eq!(
            t.refresh_token.as_ref().map(|s| s.expose_secret()),
            Some("rt")
        );
        assert_eq!(t.expires_in, Some(7776000));
    }

    #[test]
    fn parse_oauth_error_extracts_code() {
        assert_eq!(
            parse_oauth_error(r#"{"error":"authorization_pending"}"#).as_deref(),
            Some("authorization_pending")
        );
        assert_eq!(
            parse_oauth_error(r#"{"error":"slow_down","error_description":"…"}"#).as_deref(),
            Some("slow_down")
        );
        assert!(parse_oauth_error("not json").is_none());
    }

    #[tokio::test]
    async fn refresh_returns_rotated_pair_on_success() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(POST).path("/token");
                then.status(200).header("content-type", "application/json").body(
                    r#"{"access_token":"new-at","refresh_token":"new-rt","expires_in":7776000,"token_type":"Bearer"}"#,
                );
            })
            .await;
        let http = reqwest::Client::new();
        let t = refresh(
            &http,
            &format!("{}/token", server.base_url()),
            "old-rt",
            "cli-x",
        )
        .await
        .unwrap();
        m.assert_async().await;
        assert_eq!(t.access_token.expose_secret(), "new-at");
        assert_eq!(
            t.refresh_token.as_ref().map(|s| s.expose_secret()),
            Some("new-rt")
        ); // rotated
    }

    #[tokio::test]
    async fn refresh_invalid_grant_surfaces_error() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/token");
                then.status(400)
                    .header("content-type", "application/json")
                    .body(r#"{"error":"invalid_grant"}"#);
            })
            .await;
        let http = reqwest::Client::new();
        let err = refresh(
            &http,
            &format!("{}/token", server.base_url()),
            "spent",
            "cli-x",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OAuthError::Oauth(c) if c == "invalid_grant"));
    }

    fn epoch() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn oauth_tokens_from_response_computes_expiry() {
        let resp = TokenResponse {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_in: Some(3600),
            token_type: Some("Bearer".into()),
        };
        let t = OAuthTokens::from_response(resp, epoch(), None).unwrap();
        assert_eq!(t.access_token.expose_secret(), "at");
        assert_eq!(t.refresh_token.expose_secret(), "rt");
        assert_eq!(t.expires_at, epoch() + Duration::seconds(3600));
    }

    #[test]
    fn from_response_clamps_out_of_range_expires_in() {
        // A hostile AS sends expires_in = i64::MAX (valid JSON). Previously
        // `now + Duration::seconds(i64::MAX)` panicked; now it clamps.
        let resp = TokenResponse {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_in: Some(i64::MAX),
            token_type: Some("Bearer".into()),
        };
        let t = OAuthTokens::from_response(resp, epoch(), None).expect("must not panic");
        // Clamped to <= 400 days, so the timestamp is well within range.
        assert!(t.expires_at <= epoch() + Duration::days(400));
        assert!(t.expires_at > epoch());
    }

    #[test]
    fn oauth_tokens_falls_back_to_prev_refresh() {
        let resp = TokenResponse {
            access_token: "at".into(),
            refresh_token: None,
            expires_in: Some(60),
            token_type: None,
        };
        let t = OAuthTokens::from_response(resp, epoch(), Some("old-rt")).unwrap();
        assert_eq!(t.refresh_token.expose_secret(), "old-rt");
    }

    #[test]
    fn oauth_tokens_no_refresh_anywhere_errors() {
        let resp = TokenResponse {
            access_token: "at".into(),
            refresh_token: None,
            expires_in: Some(60),
            token_type: None,
        };
        assert!(OAuthTokens::from_response(resp, epoch(), None).is_err());
    }

    #[test]
    fn oauth_tokens_near_expiry() {
        let t = OAuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at: epoch() + Duration::seconds(100),
        };
        // 50s before expiry, 60s skew → near expiry
        assert!(t.is_near_expiry(epoch() + Duration::seconds(50), Duration::seconds(60)));
        // 10s in, 60s skew → not near yet
        assert!(!t.is_near_expiry(epoch() + Duration::seconds(10), Duration::seconds(60)));
    }

    #[test]
    fn oauth_tokens_json_roundtrip() {
        let t = OAuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at: epoch(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: OAuthTokens = serde_json::from_str(&json).unwrap();
        assert_eq!(back.access_token.expose_secret(), "at");
        assert_eq!(back.expires_at, epoch());
    }
}
