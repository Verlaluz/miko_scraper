use crate::auth::crypto::{decrypt_session, encrypt_session};
use crate::errors::AppError;
use crate::models::{AuthenticationSession, SessionPayload, SessionStatus};
use chrono::Utc;
use sqlx::SqlitePool;

/// Serializes and encrypts a `SessionPayload` using AES-256-GCM, returning the encrypted bytes and nonce.
pub fn pack_and_encrypt_session(
    key: &[u8; 32],
    payload: &SessionPayload,
) -> Result<(Vec<u8>, Vec<u8>), AppError> {
    let json_bytes = serde_json::to_vec(payload)
        .map_err(|e| AppError::Internal(format!("Failed to serialize session payload: {}", e)))?;
    encrypt_session(key, &json_bytes)
}

/// Decrypts and deserializes a stored `AuthenticationSession` into its typed `SessionPayload`.
pub fn decrypt_and_unpack_session(
    key: &[u8; 32],
    session: &AuthenticationSession,
) -> Result<SessionPayload, AppError> {
    let decrypted_bytes = decrypt_session(
        key,
        &session.encrypted_session_data,
        &session.nonce,
    )?;
    serde_json::from_slice(&decrypted_bytes)
        .map_err(|e| AppError::Internal(format!("Failed to deserialize decrypted session: {}", e)))
}

/// Validates whether a stored session is still valid.
///
/// 1. Checks timestamp expiry if `expires_at` is set.
/// 2. If valid, updates `last_validated_at` in SQLite.
pub async fn validate_and_refresh_session(
    pool: &SqlitePool,
    session: &mut AuthenticationSession,
) -> Result<bool, AppError> {
    let now = Utc::now();

    // Check expiration timestamp if present
    if let Some(exp) = session.expires_at {
        if now >= exp {
            session.session_status = SessionStatus::Expired;
            sqlx::query(
                "UPDATE authentication_sessions SET session_status = 'EXPIRED' WHERE id = ?"
            )
            .bind(session.id)
            .execute(pool)
            .await?;
            return Ok(false);
        }
    }

    if session.session_status != SessionStatus::Active {
        return Ok(false);
    }

    // Update last_validated_at
    sqlx::query(
        "UPDATE authentication_sessions SET last_validated_at = ? WHERE id = ?"
    )
    .bind(now.to_rfc3339())
    .bind(session.id)
    .execute(pool)
    .await?;

    session.last_validated_at = now;
    Ok(true)
}
