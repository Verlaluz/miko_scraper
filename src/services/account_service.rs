use crate::auth::otp::OtpProvider;
use crate::auth::session::pack_and_encrypt_session;
use crate::errors::AppError;
use crate::models::{AuthenticationSession, PhoneAccount, RegistrationStatus, SessionPayload, SessionStatus};
use chrono::Utc;
use sqlx::SqlitePool;
use std::sync::Arc;

pub struct AccountService {
    pool: SqlitePool,
    secret_key: [u8; 32],
    otp_provider: Arc<dyn OtpProvider>,
}

#[derive(Debug, Clone)]
pub struct WorkerStatistics {
    pub telegram_id: i64,
    pub display_name: String,
    pub account_count: i64,
    pub cooldown_count: i64,
}

#[derive(Debug, Clone)]
pub struct PendingOtpAccount {
    pub phone_number: String,
    pub worker_name: Option<String>,
    pub worker_telegram_id: Option<i64>,
}

impl AccountService {
    pub fn new(
        pool: SqlitePool,
        secret_key: [u8; 32],
        otp_provider: Arc<dyn OtpProvider>,
    ) -> Self {
        Self {
            pool,
            secret_key,
            otp_provider,
        }
    }

    /// Normalizes and validates an Ethiopian phone number without needing an instance.
    pub fn normalize_phone(input: &str) -> Result<String, AppError> {
        let cleaned: String = input
            .trim()
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-' && *c != '(' && *c != ')')
            .collect();

        let digits: String = if cleaned.starts_with('+') {
            cleaned[1..].to_string()
        } else {
            cleaned
        };

        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(AppError::InvalidPhoneNumber(
                "Phone number must contain only numbers".to_string(),
            ));
        }

        let canonical = if digits.starts_with("09") && digits.len() == 10 {
            format!("+251{}", &digits[1..])
        } else if digits.starts_with("07") && digits.len() == 10 {
            format!("+251{}", &digits[1..])
        } else if (digits.starts_with('9') || digits.starts_with('7')) && digits.len() == 9 {
            format!("+251{}", digits)
        } else if digits.starts_with("251") && digits.len() == 12 {
            format!("+{}", digits)
        } else if digits.len() >= 9 && digits.len() <= 15 {
            format!("+{}", digits)
        } else {
            return Err(AppError::InvalidPhoneNumber(
                "Invalid Ethiopian phone number. Please enter in format 0912345678 or +251912345678".to_string(),
            ));
        };

        Ok(canonical)
    }

    /// Normalizes and validates an Ethiopian phone number.
    /// Accepts:
    /// - Local format: "0912345678", "0712345678"
    /// - 9-digit format: "912345678", "712345678"
    /// - International format: "251912345678", "+251912345678"
    /// Automatically standardizes all formats to canonical E.164: "+251..."
    pub fn normalize_phone_number(&self, input: &str) -> Result<String, AppError> {
        Self::normalize_phone(input)
    }

    /// Step 1 of registration: Normalizes phone, checks if already verified,
    /// dispatches OTP via target provider, and ensures an account row exists in `PENDING_OTP` state.
    pub async fn initiate_registration(&self, raw_phone: &str) -> Result<String, AppError> {
        let phone = self.normalize_phone_number(raw_phone)?;
        let now = Utc::now();

        // Check if an existing account exists
        let existing = sqlx::query_as::<_, PhoneAccount>(
            "SELECT id, phone_number, label, registration_status, created_at, updated_at FROM phone_accounts WHERE phone_number = ?"
        )
        .bind(&phone)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(acc) = existing {
            // If already verified, check if it has an active session
            if acc.registration_status == RegistrationStatus::Verified {
                let has_session: Option<(i64,)> = sqlx::query_as(
                    "SELECT id FROM authentication_sessions WHERE phone_account_id = ? AND session_status = 'ACTIVE' LIMIT 1"
                )
                .bind(acc.id)
                .fetch_optional(&self.pool)
                .await?;

                if has_session.is_some() {
                    return Err(AppError::AccountAlreadyExists(format!(
                        "Phone number {} is already registered and active (Account #{})",
                        phone, acc.id
                    )));
                }
            }

            // Otherwise, update updated_at and allow re-verification
            sqlx::query("UPDATE phone_accounts SET updated_at = ? WHERE id = ?")
                .bind(now.to_rfc3339())
                .bind(acc.id)
                .execute(&self.pool)
                .await?;
        } else {
            // Insert new pending account
            sqlx::query(
                "INSERT INTO phone_accounts (phone_number, registration_status, created_at, updated_at) VALUES (?, 'PENDING_OTP', ?, ?)"
            )
            .bind(&phone)
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await?;
        }

        // Request OTP from target provider
        let challenge_id = self.otp_provider.request_otp(&phone).await?;
        tracing::info!("Dispatched OTP challenge for phone: {}", phone);

        Ok(challenge_id)
    }

    pub async fn attribute_account(
        &self,
        raw_phone: &str,
        telegram_id: i64,
    ) -> Result<(), AppError> {
        let phone = self.normalize_phone_number(raw_phone)?;
        sqlx::query("UPDATE phone_accounts SET added_by_telegram_id = ? WHERE phone_number = ?")
            .bind(telegram_id)
            .bind(phone)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn register_worker(&self, telegram_id: i64, display_name: &str) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO telegram_workers (telegram_id, display_name, registered_at)
             VALUES (?, ?, ?)
             ON CONFLICT(telegram_id) DO UPDATE SET display_name = excluded.display_name",
        )
        .bind(telegram_id)
        .bind(display_name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn worker_statistics(&self) -> Result<Vec<WorkerStatistics>, AppError> {
        Ok(sqlx::query_as::<_, (i64, String, i64, i64)>(
            "SELECT w.telegram_id,
                    w.display_name,
                    COUNT(DISTINCT a.id) AS account_count,
                    SUM(CASE
                        WHEN a.registration_status = 'VERIFIED'
                         AND latest.completed_at IS NOT NULL
                         AND datetime(latest.completed_at, '+12 hours') > datetime('now')
                        THEN 1 ELSE 0
                    END) AS cooldown_count
             FROM telegram_workers w
             LEFT JOIN phone_accounts a ON a.added_by_telegram_id = w.telegram_id
             LEFT JOIN (
                SELECT phone_account_id, MAX(scraping_completed_at) AS completed_at
                FROM scraping_history
                WHERE scraping_status = 'SUCCESS'
                GROUP BY phone_account_id
             ) latest ON latest.phone_account_id = a.id
             GROUP BY w.telegram_id, w.display_name
             ORDER BY account_count DESC, w.display_name ASC",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(telegram_id, display_name, account_count, cooldown_count)| {
            WorkerStatistics {
                telegram_id,
                display_name,
                account_count,
                cooldown_count,
            }
        })
        .collect())
    }

    pub async fn pending_otp_accounts(&self) -> Result<Vec<PendingOtpAccount>, AppError> {
        Ok(sqlx::query_as::<_, (String, Option<String>, Option<i64>)>(
            "SELECT a.phone_number, w.display_name, w.telegram_id
             FROM phone_accounts a
             LEFT JOIN telegram_workers w ON w.telegram_id = a.added_by_telegram_id
             WHERE a.registration_status = 'PENDING_OTP'
             ORDER BY a.created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(phone_number, worker_name, worker_telegram_id)| PendingOtpAccount {
            phone_number,
            worker_name,
            worker_telegram_id,
        })
        .collect())
    }

    /// Step 2 of registration: Submits OTP to target site for verification.
    /// If valid, encrypts session at rest and updates account status to `VERIFIED`.
    pub async fn verify_and_save_session(
        &self,
        raw_phone: &str,
        otp_code: &str,
        challenge_id: &str,
    ) -> Result<PhoneAccount, AppError> {
        let phone = self.normalize_phone_number(raw_phone)?;

        let account = sqlx::query_as::<_, PhoneAccount>(
            "SELECT id, phone_number, label, registration_status, created_at, updated_at FROM phone_accounts WHERE phone_number = ?"
        )
        .bind(&phone)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::AccountNotFound(0))?;

        // Verify with target site
        let session_payload: SessionPayload = self
            .otp_provider
            .verify_otp(&phone, otp_code, challenge_id)
            .await?;

        // Encrypt the session payload at rest using AES-256-GCM
        let (encrypted_data, nonce) = pack_and_encrypt_session(&self.secret_key, &session_payload)?;

        let now = Utc::now();

        // Run in transaction: update account status and store encrypted session
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "UPDATE phone_accounts SET registration_status = 'VERIFIED', updated_at = ? WHERE id = ?"
        )
        .bind(now.to_rfc3339())
        .bind(account.id)
        .execute(&mut *tx)
        .await?;

        // Mark any old active sessions for this account as REVOKED
        sqlx::query(
            "UPDATE authentication_sessions SET session_status = 'REVOKED' WHERE phone_account_id = ? AND session_status = 'ACTIVE'"
        )
        .bind(account.id)
        .execute(&mut *tx)
        .await?;

        // Insert new active encrypted session
        sqlx::query(
            "INSERT INTO authentication_sessions (phone_account_id, encrypted_session_data, nonce, session_status, authenticated_at, last_validated_at) VALUES (?, ?, ?, 'ACTIVE', ?, ?)"
        )
        .bind(account.id)
        .bind(encrypted_data)
        .bind(nonce)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let updated_account = PhoneAccount {
            id: account.id,
            phone_number: phone,
            label: account.label,
            registration_status: RegistrationStatus::Verified,
            created_at: account.created_at,
            updated_at: now,
        };

        tracing::info!(
            "Successfully verified and saved encrypted session for account id {}",
            account.id
        );

        Ok(updated_account)
    }

    /// Updates the custom label (e.g., Mom, Dad, Self) for an account.
    pub async fn update_account_label(&self, id: i64, label: &str) -> Result<PhoneAccount, AppError> {
        let now = Utc::now();
        sqlx::query("UPDATE phone_accounts SET label = ?, updated_at = ? WHERE id = ?")
            .bind(label)
            .bind(now.to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;

        self.get_account_by_id(id).await
    }

    /// Lists all registered phone accounts.
    pub async fn list_accounts(&self) -> Result<Vec<PhoneAccount>, AppError> {
        let accounts = sqlx::query_as::<_, PhoneAccount>(
            "SELECT id, phone_number, label, registration_status, created_at, updated_at FROM phone_accounts ORDER BY id DESC"
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(accounts)
    }

    /// Fetches an account by ID.
    pub async fn get_account_by_id(&self, id: i64) -> Result<PhoneAccount, AppError> {
        let account = sqlx::query_as::<_, PhoneAccount>(
            "SELECT id, phone_number, label, registration_status, created_at, updated_at FROM phone_accounts WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AppError::AccountNotFound(id))?;

        Ok(account)
    }

    /// Deletes a phone account along with its associated authentication sessions and scraping history.
    pub async fn delete_account(&self, id: i64) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM scraping_history WHERE phone_account_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        sqlx::query("DELETE FROM authentication_sessions WHERE phone_account_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        let res = sqlx::query("DELETE FROM phone_accounts WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        if res.rows_affected() == 0 {
            return Err(AppError::AccountNotFound(id));
        }

        tx.commit().await?;
        tracing::info!("Deleted phone account #{} and associated records", id);
        Ok(())
    }

    /// Fetches the active authentication session for an account.
    pub async fn get_active_session(&self, phone_account_id: i64) -> Result<AuthenticationSession, AppError> {
        let session = sqlx::query_as::<_, AuthenticationSession>(
            "SELECT id, phone_account_id, encrypted_session_data, nonce, session_status, authenticated_at, expires_at, last_validated_at FROM authentication_sessions WHERE phone_account_id = ? AND session_status = 'ACTIVE' ORDER BY id DESC LIMIT 1"
        )
        .bind(phone_account_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AppError::SessionInvalid(phone_account_id))?;

        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phone_normalization_formats() {
        assert_eq!(AccountService::normalize_phone("0946692896").unwrap(), "+251946692896");
        assert_eq!(AccountService::normalize_phone("0712345678").unwrap(), "+251712345678");
        assert_eq!(AccountService::normalize_phone("946692896").unwrap(), "+251946692896");
        assert_eq!(AccountService::normalize_phone("+251946692896").unwrap(), "+251946692896");
        assert_eq!(AccountService::normalize_phone("251946692896").unwrap(), "+251946692896");
        assert_eq!(AccountService::normalize_phone(" 0946-69-28-96 ").unwrap(), "+251946692896");
    }
}
