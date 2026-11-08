use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::collections::HashMap;

/// Lifecycle status of an authentication session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionStatus {
    Active,
    Expired,
    Revoked,
}

impl Default for SessionStatus {
    fn default() -> Self {
        Self::Active
    }
}

/// Represents the encrypted row stored in the `authentication_sessions` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AuthenticationSession {
    pub id: i64,
    pub phone_account_id: i64,
    pub encrypted_session_data: Vec<u8>,
    pub nonce: Vec<u8>,
    pub session_status: SessionStatus,
    pub authenticated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_validated_at: DateTime<Utc>,
}

/// The sensitive session payload that is encrypted at rest using AES-256-GCM.
/// Never stored as plaintext in the database or written to application logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPayload {
    /// Target authentication token (e.g. JWT, Bearer token, or API key)
    pub token: Option<String>,
    /// Stored session cookies key-value pairs
    pub cookies: HashMap<String, String>,
    /// Any custom headers or metadata required by the target site
    pub custom_headers: HashMap<String, String>,
}
