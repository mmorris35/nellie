//! API key authentication middleware.
//!
//! Provides middleware for validating API key authentication on protected endpoints.
//! Supports both `Authorization: Bearer <key>` and `X-API-Key: <key>` headers.

use axum::http::HeaderMap;

/// API key authentication configuration.
#[derive(Debug, Clone)]
pub struct ApiKeyConfig {
    /// The expected API key. If None, authentication is disabled (dev mode).
    pub key: Option<String>,
    /// Whether to require authentication (if key is Some).
    pub required: bool,
}

impl ApiKeyConfig {
    /// Create a new API key config with a required key.
    #[must_use]
    pub const fn new(key: Option<String>) -> Self {
        let required = key.is_some();
        Self { key, required }
    }

    /// Check if authentication is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.key.is_some()
    }

    /// Validate an API key.
    #[must_use]
    pub fn validate(&self, provided_key: &str) -> bool {
        self.key
            .as_ref()
            .is_some_and(|expected| expected == provided_key)
    }
}

/// Extract API key from request headers.
///
/// Checks both `Authorization: Bearer <key>` and `X-API-Key: <key>` headers.
#[allow(dead_code)]
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // Check Authorization header (Bearer scheme)
    if let Some(auth_header) = headers.get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(key) = auth_str.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }

    // Check X-API-Key header
    if let Some(key_header) = headers.get("x-api-key") {
        if let Ok(key_str) = key_header.to_str() {
            return Some(key_str.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_key_config_with_key() {
        let config = ApiKeyConfig::new(Some("test-key".to_string()));
        assert!(config.is_enabled());
        assert!(config.validate("test-key"));
        assert!(!config.validate("wrong-key"));
    }

    #[test]
    fn test_api_key_config_without_key() {
        let config = ApiKeyConfig::new(None);
        assert!(!config.is_enabled());
        assert!(!config.validate("any-key"));
    }

    #[test]
    fn test_extract_api_key_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer my-secret-key".parse().unwrap());

        let key = extract_api_key(&headers);
        assert_eq!(key, Some("my-secret-key".to_string()));
    }

    #[test]
    fn test_extract_api_key_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "my-secret-key".parse().unwrap());

        let key = extract_api_key(&headers);
        assert_eq!(key, Some("my-secret-key".to_string()));
    }

    #[test]
    fn test_extract_api_key_bearer_preferred() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer bearer-key".parse().unwrap());
        headers.insert("x-api-key", "header-key".parse().unwrap());

        let key = extract_api_key(&headers);
        assert_eq!(key, Some("bearer-key".to_string()));
    }

    #[test]
    fn test_extract_api_key_not_found() {
        let headers = HeaderMap::new();
        let key = extract_api_key(&headers);
        assert_eq!(key, None);
    }

    #[test]
    fn test_extract_api_key_malformed_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());

        let key = extract_api_key(&headers);
        assert_eq!(key, None);
    }

    #[test]
    fn test_extract_api_key_bearer_no_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "my-key".parse().unwrap());

        let key = extract_api_key(&headers);
        assert_eq!(key, None);
    }
}
