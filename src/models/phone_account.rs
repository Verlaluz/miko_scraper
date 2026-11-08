use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// State machine for a phone account's registration lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RegistrationStatus {
    PendingOtp,
    Verified,
    Revoked,
}

impl Default for RegistrationStatus {
    fn default() -> Self {
        Self::PendingOtp
    }
}

/// Represents a row in the `phone_accounts` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PhoneAccount {
    pub id: i64,
    pub phone_number: String,
    pub label: Option<String>,
    pub registration_status: RegistrationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
