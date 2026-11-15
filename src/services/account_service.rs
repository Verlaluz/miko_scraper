use crate::auth::otp::OtpProvider;
use crate::auth::session::pack_and_encrypt_session;
use crate::errors::AppError;
use crate::models::{AuthenticationSession, PhoneAccount, RegistrationStatus, SessionPayload};
use chrono::{DateTime, Utc};
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
    pub is_blocked: bool,
}

#[derive(Debug, Clone)]
pub struct WorkerPaymentStats {
    pub telegram_id: i64,
    pub display_name: String,
    pub account_count: i64,
    pub cooldown_count: i64,
    pub paid_cooldown: i64,
    pub remaining_cooldown: i64,
    pub is_blocked: bool,
}

#[derive(Debug, Clone)]
pub struct PendingOtpAccount {
    pub phone_number: String,
    pub worker_name: Option<String>,
    pub worker_telegram_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct WorkerCheckAccount {
    pub id: i64,
    pub phone_number: String,
    pub worker_name: String,
    pub worker_telegram_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct WorkerApplication {
    pub telegram_id: i64,
    pub display_name: String,
    pub registered_at: String,
    pub approval_status: String,
    pub is_blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyResult {
    Submitted,
    AlreadyActive(String),
    AlreadyPending,
}

#[derive(Debug, Clone)]
pub struct WorkerClaimStat {
    pub display_name: String,
    pub telegram_id: Option<i64>,
    pub claimed_accounts: i64,
}

#[derive(Debug, Clone)]
pub struct FailedAccountDetail {
    pub account_id: i64,
    pub phone_number: String,
    pub label: Option<String>,
    pub error_message: String,
    pub response_status: Option<i64>,
    pub retry_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct DailyClaimSummary {
    pub total_verified: i64,
    pub claimed_count: i64,
    pub already_voted_count: i64,
    pub failed_scrapes_count: i64,
    pub total_error_count: i64,
    pub not_claimed_count: i64,
    pub pending_otp_count: i64,
    pub failed_accounts: Vec<FailedAccountDetail>,
}

#[derive(Debug, Clone)]
pub struct ActivityReport20m {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub new_accounts_count: i64,
    pub claimed_accounts_count: i64,
    pub worker_claim_stats: Vec<WorkerClaimStat>,
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

    /// Step 1 of registration: creates one account row and dispatches its OTP.
    /// Existing phone numbers cannot be re-registered if they have an active session.
    /// New accounts are identified by phone_accounts.created_at; updating an existing account preserves created_at.
    pub async fn initiate_registration(&self, raw_phone: &str) -> Result<String, AppError> {
        let phone = self.normalize_phone_number(raw_phone)?;
        let now = Utc::now();

        let existing = sqlx::query_as::<_, PhoneAccount>(
            "SELECT id, phone_number, label, registration_status, created_at, updated_at FROM phone_accounts WHERE phone_number = ?"
        )
        .bind(&phone)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(acc) = existing {
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

            // Otherwise, update updated_at and allow re-verification while preserving original created_at
            sqlx::query("UPDATE phone_accounts SET registration_status = 'PENDING_OTP', updated_at = ? WHERE id = ?")
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
        sqlx::query("UPDATE phone_accounts SET added_by_telegram_id = ? WHERE phone_number = ? AND added_by_telegram_id IS NULL")
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
        Ok(sqlx::query_as::<_, (i64, String, i64, i64, i64)>(
            "SELECT w.telegram_id,
                    w.display_name,
                    COUNT(DISTINCT a.id) AS account_count,
                    SUM(CASE
                        WHEN a.registration_status = 'VERIFIED'
                         AND latest.completed_at IS NOT NULL
                         AND datetime(latest.completed_at, '+12 hours') > datetime('now')
                        THEN 1 ELSE 0
                    END) AS cooldown_count,
                    COALESCE(w.is_blocked, 0) AS is_blocked
             FROM telegram_workers w
             LEFT JOIN phone_accounts a ON a.added_by_telegram_id = w.telegram_id
             LEFT JOIN (
                SELECT phone_account_id, MAX(scraping_completed_at) AS completed_at
                FROM scraping_history
                WHERE scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                GROUP BY phone_account_id
             ) latest ON latest.phone_account_id = a.id
             GROUP BY w.telegram_id, w.display_name
             ORDER BY account_count DESC, w.display_name ASC",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(telegram_id, display_name, account_count, cooldown_count, is_blocked)| {
            WorkerStatistics {
                telegram_id,
                display_name,
                account_count,
                cooldown_count,
                is_blocked: is_blocked != 0,
            }
        })
        .collect())
    }

    /// Returns accounts first registered by each worker during the last 12 hours.
    pub async fn payment_statistics(&self) -> Result<Vec<WorkerStatistics>, AppError> {
        Ok(sqlx::query_as::<_, (i64, String, i64, i64, i64)>(
            "SELECT w.telegram_id,
                    w.display_name,
                    COUNT(DISTINCT a.id) AS account_count,
                    SUM(CASE
                        WHEN a.registration_status = 'VERIFIED'
                         AND latest.completed_at IS NOT NULL
                         AND datetime(latest.completed_at, '+12 hours') > datetime('now')
                        THEN 1 ELSE 0
                    END) AS cooldown_count,
                    COALESCE(w.is_blocked, 0) AS is_blocked
             FROM telegram_workers w
             INNER JOIN phone_accounts a
                 ON a.added_by_telegram_id = w.telegram_id
                AND datetime(a.created_at) >= datetime('now', '-12 hours')
             LEFT JOIN (
                SELECT phone_account_id, MAX(scraping_completed_at) AS completed_at
                FROM scraping_history
                WHERE scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                GROUP BY phone_account_id
             ) latest ON latest.phone_account_id = a.id
             GROUP BY w.telegram_id, w.display_name
             ORDER BY account_count DESC, w.display_name ASC",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(telegram_id, display_name, account_count, cooldown_count, is_blocked)| {
            WorkerStatistics {
                telegram_id,
                display_name,
                account_count,
                cooldown_count,
                is_blocked: is_blocked != 0,
            }
        })
        .collect())
    }

    /// Fetches live payment statistics for all workers, including cooldown count, paid count, and remaining unpaid.
    pub async fn worker_payment_statistics(&self) -> Result<Vec<WorkerPaymentStats>, AppError> {
        Ok(sqlx::query_as::<_, (i64, String, i64, i64, i64, i64)>(
            "SELECT w.telegram_id,
                    w.display_name,
                    COUNT(DISTINCT a.id) AS account_count,
                    SUM(CASE
                        WHEN a.registration_status = 'VERIFIED'
                         AND latest.completed_at IS NOT NULL
                         AND datetime(latest.completed_at, '+24 hours') > datetime('now')
                        THEN 1 ELSE 0
                    END) AS cooldown_count,
                    COALESCE(w.paid_cooldown, 0) AS paid_cooldown,
                    COALESCE(w.is_blocked, 0) AS is_blocked
             FROM telegram_workers w
             LEFT JOIN phone_accounts a ON a.added_by_telegram_id = w.telegram_id
             LEFT JOIN (
                SELECT phone_account_id, MAX(scraping_completed_at) AS completed_at
                FROM scraping_history
                WHERE scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                GROUP BY phone_account_id
             ) latest ON latest.phone_account_id = a.id
             GROUP BY w.telegram_id, w.display_name
             ORDER BY account_count DESC, w.display_name ASC",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(telegram_id, display_name, account_count, cooldown_count, paid_cooldown, is_blocked)| {
            let remaining_cooldown = (cooldown_count - paid_cooldown).max(0);
            WorkerPaymentStats {
                telegram_id,
                display_name,
                account_count,
                cooldown_count,
                paid_cooldown,
                remaining_cooldown,
                is_blocked: is_blocked != 0,
            }
        })
        .collect())
    }

    /// Checks whether a worker is blocked or unapproved by their Telegram ID.
    pub async fn is_worker_blocked(&self, telegram_id: i64) -> Result<bool, AppError> {
        let row = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT COALESCE(is_blocked, 0), approval_status FROM telegram_workers WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row
            .map(|(b, status)| b != 0 || status.as_deref() == Some("REJECTED"))
            .unwrap_or(false))
    }

    /// Fetches worker approval and blocked details.
    pub async fn get_worker_status_details(
        &self,
        telegram_id: i64,
    ) -> Result<Option<(String, String, bool)>, AppError> {
        let row = sqlx::query_as::<_, (String, Option<String>, i64)>(
            "SELECT display_name, approval_status, COALESCE(is_blocked, 0) FROM telegram_workers WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(name, status, b)| {
            (name, status.unwrap_or_else(|| "APPROVED".to_string()), b != 0)
        }))
    }

    /// Applies as a new worker.
    pub async fn apply_as_worker(
        &self,
        telegram_id: i64,
        display_name: &str,
    ) -> Result<ApplyResult, AppError> {
        let existing = sqlx::query_as::<_, (String, Option<String>, i64)>(
            "SELECT display_name, approval_status, COALESCE(is_blocked, 0)
             FROM telegram_workers WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((current_name, status, is_blocked)) = existing {
            let status = status.unwrap_or_else(|| "APPROVED".to_string());
            if status == "APPROVED" && is_blocked == 0 {
                return Ok(ApplyResult::AlreadyActive(current_name));
            } else if status == "PENDING" {
                sqlx::query("UPDATE telegram_workers SET display_name = ? WHERE telegram_id = ?")
                    .bind(display_name)
                    .bind(telegram_id)
                    .execute(&self.pool)
                    .await?;
                return Ok(ApplyResult::AlreadyPending);
            } else {
                sqlx::query(
                    "UPDATE telegram_workers 
                     SET display_name = ?, approval_status = 'PENDING', is_blocked = 1, registered_at = ? 
                     WHERE telegram_id = ?"
                )
                .bind(display_name)
                .bind(Utc::now().to_rfc3339())
                .bind(telegram_id)
                .execute(&self.pool)
                .await?;
                return Ok(ApplyResult::Submitted);
            }
        }

        sqlx::query(
            "INSERT INTO telegram_workers (telegram_id, display_name, registered_at, paid_cooldown, is_blocked, approval_status)
             VALUES (?, ?, ?, 0, 1, 'PENDING')"
        )
        .bind(telegram_id)
        .bind(display_name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(ApplyResult::Submitted)
    }

    /// Lists workers whose application is PENDING.
    pub async fn list_pending_worker_applications(&self) -> Result<Vec<WorkerApplication>, AppError> {
        let rows = sqlx::query_as::<_, (i64, String, String, Option<String>, i64)>(
            "SELECT telegram_id, display_name, registered_at, approval_status, COALESCE(is_blocked, 0)
             FROM telegram_workers
             WHERE approval_status = 'PENDING'
             ORDER BY registered_at DESC"
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(telegram_id, display_name, registered_at, approval_status, is_blocked)| {
                WorkerApplication {
                    telegram_id,
                    display_name,
                    registered_at,
                    approval_status: approval_status.unwrap_or_else(|| "PENDING".to_string()),
                    is_blocked: is_blocked != 0,
                }
            })
            .collect())
    }

    /// Approves a pending application, activating the worker.
    pub async fn approve_worker_application(&self, telegram_id: i64) -> Result<Option<String>, AppError> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT display_name FROM telegram_workers WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        let (name,) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        sqlx::query(
            "UPDATE telegram_workers SET approval_status = 'APPROVED', is_blocked = 0 WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .execute(&self.pool)
        .await?;

        Ok(Some(name))
    }

    /// Rejects a pending application, marking as REJECTED and blocked.
    pub async fn reject_worker_application(&self, telegram_id: i64) -> Result<Option<String>, AppError> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT display_name FROM telegram_workers WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        let (name,) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        sqlx::query(
            "UPDATE telegram_workers SET approval_status = 'REJECTED', is_blocked = 1 WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .execute(&self.pool)
        .await?;

        Ok(Some(name))
    }

    /// Toggles the blocked status of a worker, returning their name and new blocked state.
    pub async fn toggle_worker_blocked_status(
        &self,
        telegram_id: i64,
    ) -> Result<Option<(String, bool)>, AppError> {
        let row = sqlx::query_as::<_, (String, i64)>(
            "SELECT display_name, COALESCE(is_blocked, 0) FROM telegram_workers WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        let (display_name, current_blocked) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        let new_blocked = if current_blocked != 0 { 0 } else { 1 };
        sqlx::query("UPDATE telegram_workers SET is_blocked = ? WHERE telegram_id = ?")
            .bind(new_blocked)
            .bind(telegram_id)
            .execute(&self.pool)
            .await?;

        Ok(Some((display_name, new_blocked != 0)))
    }

    /// Sets the blocked status explicitly.
    pub async fn set_worker_blocked_status(
        &self,
        telegram_id: i64,
        blocked: bool,
    ) -> Result<Option<String>, AppError> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT display_name FROM telegram_workers WHERE telegram_id = ?"
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        let (display_name,) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        sqlx::query("UPDATE telegram_workers SET is_blocked = ? WHERE telegram_id = ?")
            .bind(if blocked { 1 } else { 0 })
            .bind(telegram_id)
            .execute(&self.pool)
            .await?;

        Ok(Some(display_name))
    }

    /// Fetches payment statistics for a specific worker by Telegram ID.
    pub async fn get_worker_payment_stats_by_id(
        &self,
        telegram_id: i64,
    ) -> Result<Option<WorkerPaymentStats>, AppError> {
        let workers = self.worker_payment_statistics().await?;
        Ok(workers.into_iter().find(|w| w.telegram_id == telegram_id))
    }

    /// Fetches a worker by their 1-based position in the `/cash` list.
    pub async fn get_worker_by_list_number(
        &self,
        list_number: usize,
    ) -> Result<Option<(WorkerPaymentStats, usize, usize)>, AppError> {
        let workers = self.worker_payment_statistics().await?;
        let total = workers.len();
        if list_number >= 1 && list_number <= total {
            let worker = workers[list_number - 1].clone();
            Ok(Some((worker, list_number, total)))
        } else {
            Ok(None)
        }
    }

    /// Records a payment in cooldown units for a worker.
    pub async fn record_worker_payment(
        &self,
        worker_telegram_id: i64,
        amount: i64,
    ) -> Result<WorkerPaymentStats, AppError> {
        if amount <= 0 {
            return Err(AppError::Payment(
                "Payment amount must be greater than 0.".to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;

        // 1. Fetch current worker details
        let worker_row = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT display_name, paid_cooldown, COALESCE(is_blocked, 0) FROM telegram_workers WHERE telegram_id = ?",
        )
        .bind(worker_telegram_id)
        .fetch_optional(&mut *tx)
        .await?;

        let (display_name, current_paid, is_blocked) = match worker_row {
            Some(row) => row,
            None => {
                return Err(AppError::Payment(format!(
                    "Worker with ID {} not found.",
                    worker_telegram_id
                )))
            }
        };

        // 2. Fetch live cooldown count
        let (cooldown_count,): (Option<i64>,) = sqlx::query_as(
            "SELECT SUM(CASE
                 WHEN a.registration_status = 'VERIFIED'
                  AND latest.completed_at IS NOT NULL
                  AND datetime(latest.completed_at, '+24 hours') > datetime('now')
                 THEN 1 ELSE 0
             END)
             FROM phone_accounts a
             LEFT JOIN (
                SELECT phone_account_id, MAX(scraping_completed_at) AS completed_at
                FROM scraping_history
                WHERE scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                GROUP BY phone_account_id
             ) latest ON latest.phone_account_id = a.id
             WHERE a.added_by_telegram_id = ?",
        )
        .bind(worker_telegram_id)
        .fetch_one(&mut *tx)
        .await?;

        let cooldown = cooldown_count.unwrap_or(0);
        let remaining = (cooldown - current_paid).max(0);
        if amount > remaining {
            return Err(AppError::Payment(format!(
                "Cannot pay {} cooldowns to {}. Remaining unpaid is only {}.",
                amount, display_name, remaining
            )));
        }

        let new_paid = current_paid + amount;
        let now_str = Utc::now().to_rfc3339();

        // 3. Update paid_cooldown
        sqlx::query("UPDATE telegram_workers SET paid_cooldown = ? WHERE telegram_id = ?")
            .bind(new_paid)
            .bind(worker_telegram_id)
            .execute(&mut *tx)
            .await?;

        // 4. Insert into payment log
        sqlx::query(
            "INSERT INTO worker_payments (worker_telegram_id, amount_paid, paid_at, is_reversed) VALUES (?, ?, ?, 0)",
        )
        .bind(worker_telegram_id)
        .bind(amount)
        .bind(&now_str)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let (account_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(DISTINCT id) FROM phone_accounts WHERE added_by_telegram_id = ?",
        )
        .bind(worker_telegram_id)
        .fetch_one(&self.pool)
        .await?;

        Ok(WorkerPaymentStats {
            telegram_id: worker_telegram_id,
            display_name,
            account_count,
            cooldown_count: cooldown,
            paid_cooldown: new_paid,
            remaining_cooldown: (cooldown - new_paid).max(0),
            is_blocked: is_blocked != 0,
        })
    }

    /// Reverses the most recent non-reversed payment for a worker.
    pub async fn undo_worker_last_payment(
        &self,
        worker_telegram_id: i64,
    ) -> Result<Option<(WorkerPaymentStats, i64)>, AppError> {
        let mut tx = self.pool.begin().await?;

        // Find last non-reversed payment
        let last_payment = sqlx::query_as::<_, (i64, i64)>(
            "SELECT id, amount_paid FROM worker_payments 
             WHERE worker_telegram_id = ? AND is_reversed = 0 
             ORDER BY id DESC LIMIT 1",
        )
        .bind(worker_telegram_id)
        .fetch_optional(&mut *tx)
        .await?;

        let (payment_id, amount_to_undo) = match last_payment {
            Some(p) => p,
            None => return Ok(None),
        };

        let now_str = Utc::now().to_rfc3339();

        // Mark payment as reversed
        sqlx::query("UPDATE worker_payments SET is_reversed = 1, reversed_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(payment_id)
            .execute(&mut *tx)
            .await?;

        // Deduct paid_cooldown
        sqlx::query("UPDATE telegram_workers SET paid_cooldown = MAX(0, paid_cooldown - ?) WHERE telegram_id = ?")
            .bind(amount_to_undo)
            .bind(worker_telegram_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        let updated_stats = self.get_worker_payment_stats_by_id(worker_telegram_id).await?;
        if let Some(stats) = updated_stats {
            Ok(Some((stats, amount_to_undo)))
        } else {
            Ok(None)
        }
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

    pub async fn worker_check_accounts(&self) -> Result<(Vec<WorkerCheckAccount>, Vec<WorkerCheckAccount>), AppError> {
        let rows = sqlx::query_as::<_, (i64, String, String, Option<i64>, String, String, i64)>(
            "SELECT a.id, a.phone_number, COALESCE(w.display_name, 'Unassigned worker'),
                    w.telegram_id,
                    a.created_at, a.updated_at,
                    CASE WHEN date(a.created_at) = date('now', 'utc') THEN 1 ELSE 0 END AS is_new
             FROM phone_accounts a
             LEFT JOIN telegram_workers w ON w.telegram_id = a.added_by_telegram_id
             WHERE a.added_by_telegram_id IS NOT NULL
               AND (date(a.created_at) = date('now', 'utc')
                    OR date(a.updated_at) = date('now', 'utc'))
             ORDER BY a.updated_at DESC, a.id DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut new_accounts = Vec::new();
        let mut updated_accounts = Vec::new();
        for (id, phone_number, worker_name, worker_telegram_id, created_at, updated_at, is_new) in rows {
            let account = WorkerCheckAccount {
                id,
                phone_number,
                worker_name,
                worker_telegram_id,
                created_at,
                updated_at,
            };
            if is_new == 1 {
                new_accounts.push(account);
            } else {
                updated_accounts.push(account);
            }
        }
        Ok((new_accounts, updated_accounts))
    }

    /// Generates a summary of newly registered accounts and status-201 claims in the last 20 minutes.
    pub async fn activity_report_20m(&self) -> Result<ActivityReport20m, AppError> {
        let end_time = Utc::now();
        let start_time = end_time - chrono::Duration::minutes(20);
        let start_str = start_time.to_rfc3339();
        let end_str = end_time.to_rfc3339();

        let (new_accounts_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM phone_accounts
             WHERE datetime(created_at) >= datetime(?) AND datetime(created_at) <= datetime(?)"
        )
        .bind(&start_str)
        .bind(&end_str)
        .fetch_one(&self.pool)
        .await?;

        let (claimed_accounts_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(DISTINCT phone_account_id) FROM scraping_history
             WHERE response_status = 201
               AND datetime(COALESCE(scraping_completed_at, scraping_started_at)) >= datetime(?)
               AND datetime(COALESCE(scraping_completed_at, scraping_started_at)) <= datetime(?)"
        )
        .bind(&start_str)
        .bind(&end_str)
        .fetch_one(&self.pool)
        .await?;

        let worker_rows = sqlx::query_as::<_, (String, Option<i64>, i64)>(
            "SELECT COALESCE(w.display_name, 'Direct / Admin') AS name,
                    w.telegram_id,
                    COUNT(DISTINCT a.id) AS claimed_accounts
             FROM scraping_history h
             INNER JOIN phone_accounts a ON a.id = h.phone_account_id
             LEFT JOIN telegram_workers w ON w.telegram_id = a.added_by_telegram_id
             WHERE h.response_status = 201
               AND datetime(COALESCE(h.scraping_completed_at, h.scraping_started_at)) >= datetime(?)
               AND datetime(COALESCE(h.scraping_completed_at, h.scraping_started_at)) <= datetime(?)
             GROUP BY w.telegram_id, w.display_name
             ORDER BY claimed_accounts DESC, name ASC"
        )
        .bind(&start_str)
        .bind(&end_str)
        .fetch_all(&self.pool)
        .await?;

        let worker_claim_stats = worker_rows
            .into_iter()
            .map(|(display_name, telegram_id, claimed_accounts)| WorkerClaimStat {
                display_name,
                telegram_id,
                claimed_accounts,
            })
            .collect();

        Ok(ActivityReport20m {
            start_time,
            end_time,
            new_accounts_count,
            claimed_accounts_count,
            worker_claim_stats,
        })
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

    /// Fetches verified accounts that are ready to claim in the current daily cycle.
    /// Excludes accounts that have already succeeded or been marked already voted today,
    /// and accounts currently in a 1-hour error backoff window.
    pub async fn get_ready_to_claim_accounts(&self, limit: i64) -> Result<Vec<PhoneAccount>, AppError> {
        let now = Utc::now();
        let midnight = crate::scheduler::rate_limit::last_midnight_utc(now);
        let midnight_str = midnight.to_rfc3339();
        let now_str = now.to_rfc3339();

        let accounts = sqlx::query_as::<_, PhoneAccount>(
            "SELECT p.id, p.phone_number, p.label, p.registration_status, p.created_at, p.updated_at
             FROM phone_accounts p
             WHERE p.registration_status = 'VERIFIED'
               AND NOT EXISTS (
                   SELECT 1 FROM scraping_history h
                   WHERE h.phone_account_id = p.id
                     AND h.scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                     AND datetime(COALESCE(h.scraping_completed_at, h.scraping_started_at)) >= datetime(?)
               )
               AND NOT EXISTS (
                   SELECT 1 FROM scraping_history h
                   WHERE h.phone_account_id = p.id
                     AND h.scraping_status = 'FAILED'
                     AND h.retry_after IS NOT NULL
                     AND datetime(h.retry_after) > datetime(?)
               )
             ORDER BY p.id DESC
             LIMIT ?"
        )
        .bind(&midnight_str)
        .bind(&now_str)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(accounts)
    }

    /// Finds the earliest pending retry timestamp for accounts currently in a 1-hour backoff.
    pub async fn get_earliest_pending_retry(&self) -> Result<Option<DateTime<Utc>>, AppError> {
        let now = Utc::now();
        let midnight = crate::scheduler::rate_limit::last_midnight_utc(now);
        let midnight_str = midnight.to_rfc3339();
        let now_str = now.to_rfc3339();

        let row: Option<(String,)> = sqlx::query_as(
            "SELECT h.retry_after
             FROM scraping_history h
             INNER JOIN phone_accounts p ON p.id = h.phone_account_id
             WHERE p.registration_status = 'VERIFIED'
               AND h.scraping_status = 'FAILED'
               AND h.retry_after IS NOT NULL
               AND datetime(h.retry_after) > datetime(?)
               AND datetime(COALESCE(h.scraping_completed_at, h.scraping_started_at)) >= datetime(?)
               AND NOT EXISTS (
                   SELECT 1 FROM scraping_history h2
                   WHERE h2.phone_account_id = p.id
                     AND h2.scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                     AND datetime(COALESCE(h2.scraping_completed_at, h2.scraping_started_at)) >= datetime(?)
               )
             ORDER BY datetime(h.retry_after) ASC
             LIMIT 1"
        )
        .bind(&now_str)
        .bind(&midnight_str)
        .bind(&midnight_str)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((ts,)) = row {
            if let Ok(dt) = DateTime::parse_from_rfc3339(&ts) {
                return Ok(Some(dt.with_timezone(&Utc)));
            }
        }
        Ok(None)
    }

    /// Computes the daily claim summary for verified accounts since today's 00:00 UTC.
    pub async fn get_daily_claim_summary(&self) -> Result<DailyClaimSummary, AppError> {
        let now = Utc::now();
        let midnight = crate::scheduler::rate_limit::last_midnight_utc(now);
        let midnight_str = midnight.to_rfc3339();

        let (total_verified,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM phone_accounts WHERE registration_status = 'VERIFIED'"
        )
        .fetch_one(&self.pool)
        .await?;

        let (pending_otp_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM phone_accounts WHERE registration_status != 'VERIFIED'"
        )
        .fetch_one(&self.pool)
        .await?;

        let (fresh_claimed_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(DISTINCT phone_account_id) FROM scraping_history
             WHERE response_status = 201
               AND datetime(COALESCE(scraping_completed_at, scraping_started_at)) >= datetime(?)"
        )
        .bind(&midnight_str)
        .fetch_one(&self.pool)
        .await?;

        let (already_voted_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(DISTINCT phone_account_id) FROM scraping_history
             WHERE (scraping_status = 'ALREADY_VOTED' OR (response_status = 409 AND scraping_status != 'FAILED'))
               AND datetime(COALESCE(scraping_completed_at, scraping_started_at)) >= datetime(?)"
        )
        .bind(&midnight_str)
        .fetch_one(&self.pool)
        .await?;

        let _total_claimed_count = fresh_claimed_count + already_voted_count;

        let failed_rows = sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, Option<i64>, Option<String>)>(
            "SELECT h.phone_account_id, p.phone_number, p.label, h.error_message, h.response_status, h.retry_after
             FROM scraping_history h
             JOIN phone_accounts p ON p.id = h.phone_account_id
             WHERE h.scraping_status = 'FAILED'
               AND datetime(COALESCE(h.scraping_completed_at, h.scraping_started_at)) >= datetime(?)
               AND NOT EXISTS (
                   SELECT 1 FROM scraping_history h2
                   WHERE h2.phone_account_id = h.phone_account_id
                     AND h2.scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                     AND datetime(COALESCE(h2.scraping_completed_at, h2.scraping_started_at)) >= datetime(?)
               )
             GROUP BY h.phone_account_id
             ORDER BY h.id DESC"
        )
        .bind(&midnight_str)
        .bind(&midnight_str)
        .fetch_all(&self.pool)
        .await?;

        let failed_accounts: Vec<FailedAccountDetail> = failed_rows
            .into_iter()
            .map(|(account_id, phone_number, label, error_msg, response_status, retry_str)| {
                let retry_after = retry_str
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|d| d.with_timezone(&Utc));
                FailedAccountDetail {
                    account_id,
                    phone_number,
                    label,
                    error_message: error_msg.unwrap_or_else(|| "Unknown error".to_string()),
                    response_status,
                    retry_after,
                }
            })
            .collect();

        let claimed_count = fresh_claimed_count;
        let failed_scrapes_count = failed_accounts.len() as i64;
        let total_error_count = already_voted_count + failed_scrapes_count;
        let not_claimed_count = (total_verified - claimed_count - total_error_count).max(0);

        Ok(DailyClaimSummary {
            total_verified,
            claimed_count,
            already_voted_count,
            failed_scrapes_count,
            total_error_count,
            not_claimed_count,
            pending_otp_count,
            failed_accounts,
        })
    }

    /// Fetches up to `limit` unclaimed accounts (ready to claim or in retry) for preview.
    pub async fn get_unclaimed_accounts(&self, limit: i64) -> Result<Vec<PhoneAccount>, AppError> {
        let now = Utc::now();
        let midnight = crate::scheduler::rate_limit::last_midnight_utc(now);
        let midnight_str = midnight.to_rfc3339();

        let accounts = sqlx::query_as::<_, PhoneAccount>(
            "SELECT p.id, p.phone_number, p.label, p.registration_status, p.created_at, p.updated_at
             FROM phone_accounts p
             WHERE p.registration_status = 'VERIFIED'
               AND NOT EXISTS (
                   SELECT 1 FROM scraping_history h
                   WHERE h.phone_account_id = p.id
                     AND h.scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
                     AND datetime(COALESCE(h.scraping_completed_at, h.scraping_started_at)) >= datetime(?)
               )
             ORDER BY p.id DESC
             LIMIT ?"
        )
        .bind(&midnight_str)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(accounts)
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
