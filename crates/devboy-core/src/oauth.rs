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

use serde::Deserialize;
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
}
