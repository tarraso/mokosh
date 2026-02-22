//! Authentication system for Mokosh
//!
//! Provides the `AuthProvider` trait for implementing custom authentication
//! mechanisms (passcode, Steam, Google OAuth, etc.) and a `MockAuthProvider`
//! for testing.

use crate::error::Result;
use async_trait::async_trait;

/// Result of an authentication attempt
#[derive(Debug, Clone, PartialEq)]
pub enum AuthResult {
    /// Authentication successful with session ID
    Success { session_id: String },

    /// Authentication failed with error message
    Failure { error_message: String },
}

/// Authentication provider trait
///
/// Implement this trait to provide custom authentication mechanisms.
/// The provider is called by the server when handling AUTH_REQUEST messages.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Authenticate a client with the given method and credentials
    ///
    /// # Arguments
    /// * `method` - Authentication method name (e.g., "passcode", "steam", "google")
    /// * `credentials` - Raw credential bytes (format depends on method)
    ///
    /// # Returns
    /// `AuthResult::Success` with session ID if authentication succeeds,
    /// `AuthResult::Failure` with error message otherwise.
    async fn authenticate(&self, method: &str, credentials: &[u8]) -> Result<AuthResult>;

    /// Returns list of supported authentication methods
    ///
    /// Used to populate the `available_auth_methods` field in HELLO_OK.
    fn supported_methods(&self) -> Vec<String>;
}

/// Mock authentication provider for testing
///
/// Accepts any credentials for the "mock" method and rejects everything else.
/// Generates a simple session ID based on the credentials length.
#[derive(Debug, Clone, Default)]
pub struct MockAuthProvider;

#[async_trait]
impl AuthProvider for MockAuthProvider {
    async fn authenticate(&self, method: &str, credentials: &[u8]) -> Result<AuthResult> {
        if method == "mock" {
            // Accept any credentials for "mock" method
            let session_id = format!("mock-session-{}", credentials.len());
            Ok(AuthResult::Success { session_id })
        } else {
            Ok(AuthResult::Failure {
                error_message: format!("Unsupported authentication method: {}", method),
            })
        }
    }

    fn supported_methods(&self) -> Vec<String> {
        vec!["mock".to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider_success() {
        let provider = MockAuthProvider;

        let result = provider.authenticate("mock", b"test-credentials").await.unwrap();

        match result {
            AuthResult::Success { session_id } => {
                assert_eq!(session_id, "mock-session-16");
            }
            AuthResult::Failure { .. } => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_mock_provider_unsupported_method() {
        let provider = MockAuthProvider;

        let result = provider.authenticate("passcode", b"secret123").await.unwrap();

        match result {
            AuthResult::Success { .. } => panic!("Expected failure"),
            AuthResult::Failure { error_message } => {
                assert_eq!(error_message, "Unsupported authentication method: passcode");
            }
        }
    }

    #[tokio::test]
    async fn test_mock_provider_supported_methods() {
        let provider = MockAuthProvider;

        let methods = provider.supported_methods();

        assert_eq!(methods, vec!["mock".to_string()]);
    }
}
