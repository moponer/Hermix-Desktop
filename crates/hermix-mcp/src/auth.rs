//! MCP Authentication
//!
//! Provides authentication for remote MCP servers:
//! - **McpAuthProvider**: Trait for authentication providers
//! - **BearerTokenAuth**: Simple bearer token authentication

use serde::{Deserialize, Serialize};

use crate::McpError;

// ---------------------------------------------------------------------------
// McpAuthProvider trait
// ---------------------------------------------------------------------------

/// Trait for providing authentication tokens for MCP server connections.
/// Implementations can provide different authentication mechanisms:
/// - Bearer token (simple static token)
/// - Custom (any mechanism that produces a token string)
#[async_trait::async_trait]
pub trait McpAuthProvider: Send + Sync {
    /// Get an authentication token.
    /// Returns a token string suitable for use in an Authorization header.
    /// For OAuth, this will automatically refresh expired tokens.
    async fn get_token(&self) -> Result<String, McpError>;

    /// Get the type name of this auth provider (for logging/debugging).
    fn provider_type(&self) -> &str;
}

// ---------------------------------------------------------------------------
// BearerTokenAuth
// ---------------------------------------------------------------------------

/// Simple bearer token authentication provider.
/// Use this when you have a static API key or token that doesn't expire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BearerTokenAuth {
    /// The bearer token to use for authentication.
    pub token: String,
}

impl BearerTokenAuth {
    /// Create a new bearer token auth provider.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

#[async_trait::async_trait]
impl McpAuthProvider for BearerTokenAuth {
    async fn get_token(&self) -> Result<String, McpError> {
        if self.token.is_empty() {
            return Err(McpError::Auth("Bearer token is empty".to_string()));
        }
        Ok(self.token.clone())
    }

    fn provider_type(&self) -> &str {
        "bearer"
    }
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bearer_token_auth() {
        let auth = BearerTokenAuth::new("test-token-123");
        assert_eq!(auth.token, "test-token-123");
        assert_eq!(auth.provider_type(), "bearer");
    }

    #[tokio::test]
    async fn test_bearer_token_get_token() {
        let auth = BearerTokenAuth::new("test-token-123");
        let token = auth.get_token().await.unwrap();
        assert_eq!(token, "test-token-123");
    }

    #[tokio::test]
    async fn test_bearer_token_empty() {
        let auth = BearerTokenAuth::new("");
        let result = auth.get_token().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_bearer_token_serialization() {
        let auth = BearerTokenAuth::new("token123");
        let json = serde_json::to_string(&auth).unwrap();
        let deserialized: BearerTokenAuth = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.token, "token123");
    }
}
