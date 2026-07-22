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
pub fn parse_www_authenticate(value: &str) -> Option<String> {
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

/// Discover authorization-server metadata for an upstream, starting from its
/// `WWW-Authenticate` challenge (RFC 9728 → RFC 8414).
pub async fn discover(
    http: &reqwest::Client,
    www_authenticate: &str,
) -> Result<AuthServerMetadata, OAuthError> {
    let resource_meta_url =
        parse_www_authenticate(www_authenticate).ok_or(OAuthError::NoResourceMetadata)?;
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

/// Fetch RFC 8414 authorization-server metadata from
/// `<issuer>/.well-known/oauth-authorization-server`.
pub async fn fetch_as_metadata(
    http: &reqwest::Client,
    issuer: &str,
) -> Result<AuthServerMetadata, OAuthError> {
    let url = format!(
        "{}/.well-known/oauth-authorization-server",
        issuer.trim_end_matches('/')
    );
    http.get(&url)
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .error_for_status()
        .map_err(|e| OAuthError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| OAuthError::Malformed(e.to_string()))
}

/// Device + refresh grant type identifiers this client registers for.
pub const GRANT_DEVICE_CODE: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub const GRANT_REFRESH_TOKEN: &str = "refresh_token";

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
        grant_types: vec![GRANT_DEVICE_CODE.to_string(), GRANT_REFRESH_TOKEN.to_string()],
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
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
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
        let tokens: TokenResponse =
            serde_json::from_str(&body).map_err(|e| OAuthError::Malformed(e.to_string()))?;
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
        return serde_json::from_str(&body).map_err(|e| OAuthError::Malformed(e.to_string()));
    }
    match parse_oauth_error(&body) {
        Some(code) => Err(OAuthError::Oauth(code)),
        None => Err(OAuthError::Oauth(format!("HTTP {status}: {body}"))),
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
        assert_eq!(t.access_token, "at");
        assert_eq!(t.refresh_token.as_deref(), Some("rt"));
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
        let t = refresh(&http, &format!("{}/token", server.base_url()), "old-rt", "cli-x")
            .await
            .unwrap();
        m.assert_async().await;
        assert_eq!(t.access_token, "new-at");
        assert_eq!(t.refresh_token.as_deref(), Some("new-rt")); // rotated
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
        let err = refresh(&http, &format!("{}/token", server.base_url()), "spent", "cli-x")
            .await
            .unwrap_err();
        assert!(matches!(err, OAuthError::Oauth(c) if c == "invalid_grant"));
    }
}
