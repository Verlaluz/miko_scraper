use crate::auth::session::{decrypt_and_unpack_session, validate_and_refresh_session};
use crate::errors::AppError;
use crate::models::{AuthenticationSession, PhoneAccount, ScrapeCooldownInfo, ScrapingRecord, ScrapingStatus};
use crate::scheduler::rate_limit::calculate_cooldown;
use crate::scraper::engine::ScraperEngine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountVoteStatus {
    pub account_id: i64,
    pub phone_number: String,
    pub label: Option<String>,
    pub is_eligible: bool,
    pub remaining_seconds: i64,
    pub next_eligible_at: Option<DateTime<Utc>>,
    pub last_vote_at: Option<DateTime<Utc>>,
    pub has_voted_recently: bool,
}

pub struct ScrapingService {
    pool: SqlitePool,
    secret_key: [u8; 32],
    scraper_engine: Arc<dyn ScraperEngine>,
    active_jobs: Arc<RwLock<HashSet<i64>>>,
}

impl ScrapingService {
    pub fn new(
        pool: SqlitePool,
        secret_key: [u8; 32],
        scraper_engine: Arc<dyn ScraperEngine>,
    ) -> Self {
        Self {
            pool,
            secret_key,
            scraper_engine,
            active_jobs: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Fetches the vote eligibility and last voting status for a list of accounts.
    pub async fn get_all_accounts_vote_status(&self, accounts: &[PhoneAccount]) -> Result<Vec<AccountVoteStatus>, AppError> {
        let mut results = Vec::new();

        for acc in accounts {
            let cooldown = self.get_cooldown_status(acc.id).await?;
            let has_voted = !cooldown.is_eligible && cooldown.last_scraping_at.is_some();

            results.push(AccountVoteStatus {
                account_id: acc.id,
                phone_number: acc.phone_number.clone(),
                label: acc.label.clone(),
                is_eligible: cooldown.is_eligible,
                remaining_seconds: cooldown.remaining_seconds,
                next_eligible_at: cooldown.next_eligible_at,
                last_vote_at: cooldown.last_scraping_at,
                has_voted_recently: has_voted,
            });
        }

        Ok(results)
    }

    /// Synchronizes live voting status for an account from the target website.
    pub async fn sync_account_status(&self, phone_account_id: i64) -> Result<ScrapingRecord, AppError> {
        self.execute_scraping_flow(phone_account_id).await
    }

    /// Evaluates the 12-hour cooldown status for a specific phone account.
    pub async fn get_cooldown_status(&self, phone_account_id: i64) -> Result<ScrapeCooldownInfo, AppError> {
        let last_record: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT scraping_completed_at FROM scraping_history 
             WHERE phone_account_id = ? AND scraping_status = 'SUCCESS' 
             ORDER BY scraping_completed_at DESC LIMIT 1"
        )
        .bind(phone_account_id)
        .fetch_optional(&self.pool)
        .await?;

        let last_time: Option<DateTime<Utc>> = last_record
            .and_then(|r| r.0)
            .and_then(|ts| DateTime::parse_from_rfc3339(&ts).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let now = Utc::now();
        let mut cooldown = calculate_cooldown(phone_account_id, last_time, now);
        let retry_after: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT retry_after FROM scraping_history
             WHERE phone_account_id = ? AND scraping_status = 'FAILED' AND retry_after IS NOT NULL
             ORDER BY scraping_completed_at DESC LIMIT 1",
        )
        .bind(phone_account_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(retry_at) = retry_after
            .and_then(|row| row.0)
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc))
        {
            if retry_at > now && (cooldown.is_eligible || Some(retry_at) < cooldown.next_eligible_at) {
                cooldown.is_eligible = false;
                cooldown.next_eligible_at = Some(retry_at);
                cooldown.remaining_seconds = (retry_at - now).num_seconds().max(0);
            }
        }

        Ok(cooldown)
    }

    /// Executes the complete scraping pipeline with strict concurrency guards,
    /// 12-hour cooldown enforcement, persistent session decryption, and audit logging.
    pub async fn start_scraping(&self, phone_account_id: i64) -> Result<ScrapingRecord, AppError> {
        // 1. Acquire in-memory lock to prevent concurrent scrapes for the same phone account
        {
            let mut active = self.active_jobs.write().await;
            if active.contains(&phone_account_id) {
                return Err(AppError::ScrapeAlreadyInProgress(phone_account_id));
            }
            active.insert(phone_account_id);
        }

        // Use a deferred cleanup pattern to ensure the lock is always removed even on error
        let active_jobs_ref = Arc::clone(&self.active_jobs);
        let result = self.execute_scraping_flow(phone_account_id).await;

        // Release the lock
        {
            let mut active = active_jobs_ref.write().await;
            active.remove(&phone_account_id);
        }

        result
    }

    async fn execute_scraping_flow(&self, phone_account_id: i64) -> Result<ScrapingRecord, AppError> {
        // 2. Atomic 12-Hour Cooldown Verification
        let cooldown = self.get_cooldown_status(phone_account_id).await?;
        if !cooldown.is_eligible {
            return Err(AppError::CooldownActive {
                remaining_seconds: cooldown.remaining_seconds,
                next_eligible_at_utc: cooldown
                    .next_eligible_at
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default(),
            });
        }

        // 3. Retrieve and Validate Persistent Authentication Session
        let mut session = sqlx::query_as::<_, AuthenticationSession>(
            "SELECT id, phone_account_id, encrypted_session_data, nonce, session_status, authenticated_at, expires_at, last_validated_at 
             FROM authentication_sessions 
             WHERE phone_account_id = ? AND session_status = 'ACTIVE' 
             ORDER BY id DESC LIMIT 1"
        )
        .bind(phone_account_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AppError::SessionInvalid(phone_account_id))?;

        let is_valid = validate_and_refresh_session(&self.pool, &mut session).await?;
        if !is_valid {
            return Err(AppError::SessionInvalid(phone_account_id));
        }

        // 4. Decrypt the session data in-memory (AES-256-GCM)
        let session_payload = decrypt_and_unpack_session(&self.secret_key, &session)?;

        // 5. Insert initial audit row in scraping_history with RUNNING status
        let started_at = Utc::now();
        let history_id = sqlx::query(
            "INSERT INTO scraping_history (phone_account_id, scraping_started_at, scraping_status, records_scraped) 
             VALUES (?, ?, 'RUNNING', 0)"
        )
        .bind(phone_account_id)
        .bind(started_at.to_rfc3339())
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        tracing::info!(
            "Started scraping job #{} for phone account #{}",
            history_id,
            phone_account_id
        );

        // 6. Execute the scraper engine
        let scrape_res = self.scraper_engine.scrape(&session_payload).await;
        let completed_at = Utc::now();

        match scrape_res {
            Ok(data) => {
                // Record success
                sqlx::query(
                    "UPDATE scraping_history 
                     SET scraping_completed_at = ?, scraping_status = 'SUCCESS', records_scraped = ?, error_message = NULL 
                     WHERE id = ?"
                )
                .bind(completed_at.to_rfc3339())
                .bind(data.records_count)
                .bind(history_id)
                .execute(&self.pool)
                .await?;

                tracing::info!(
                    "Scraping job #{} succeeded with {} records scraped.",
                    history_id,
                    data.records_count
                );

                Ok(ScrapingRecord {
                    id: history_id,
                    phone_account_id,
                    scraping_started_at: started_at,
                    scraping_completed_at: Some(completed_at),
                    scraping_status: ScrapingStatus::Success,
                    records_scraped: data.records_count,
                    error_message: None,
                })
            }
            Err(err) => {
                let err_msg = err.to_string();
                sqlx::query(
                    "UPDATE scraping_history 
                     SET scraping_completed_at = ?, scraping_status = 'FAILED', error_message = ?,
                         retry_after = ?
                     WHERE id = ?"
                )
                .bind(completed_at.to_rfc3339())
                .bind(&err_msg)
                .bind(if Self::is_transient_scrape_error(&err) {
                    Some((completed_at + chrono::Duration::hours(1)).to_rfc3339())
                } else {
                    None
                })
                .bind(history_id)
                .execute(&self.pool)
                .await?;

                tracing::error!("Scraping job #{} failed: {}", history_id, err_msg);

                Err(err)
            }
        }
    }

    fn is_transient_scrape_error(error: &AppError) -> bool {
        let message = error.to_string().to_lowercase();
        message.contains("network error")
            || message.contains("status 429")
            || message.contains("status 500")
            || message.contains("status 501")
            || message.contains("status 502")
            || message.contains("status 503")
            || message.contains("status 504")
    }

    /// Fetches the recent scraping history for a specific phone account.
    pub async fn get_history(&self, phone_account_id: i64, limit: i64) -> Result<Vec<ScrapingRecord>, AppError> {
        let records = sqlx::query_as::<_, ScrapingRecord>(
            "SELECT id, phone_account_id, scraping_started_at, scraping_completed_at, scraping_status, records_scraped, error_message 
             FROM scraping_history 
             WHERE phone_account_id = ? 
             ORDER BY id DESC LIMIT ?"
        )
        .bind(phone_account_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(records)
    }

    /// Resets cooldown in SQLite for rapid development testing.
    pub async fn reset_cooldown_for_test(&self, phone_account_id: i64) -> Result<(), AppError> {
        sqlx::query("DELETE FROM scraping_history WHERE phone_account_id = ?")
            .bind(phone_account_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
