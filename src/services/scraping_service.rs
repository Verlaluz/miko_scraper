use crate::auth::session::{decrypt_and_unpack_session, validate_and_refresh_session};
use crate::errors::AppError;
use crate::models::{AuthenticationSession, PhoneAccount, ScrapeCooldownInfo, ScrapingRecord, ScrapingStatus};
use crate::scheduler::rate_limit::{calculate_cooldown, next_daily_run_utc};
use crate::scraper::engine::ScraperEngine;
use chrono::{DateTime, Duration, Utc};
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
        self.execute_scraping_flow(phone_account_id, false).await
    }

    /// Evaluates the per-account cooldown status using UTC day-boundary rules.
    /// An account is eligible when it has never acted, or when its last
    /// successful completion occurred before today's 00:00 UTC boundary.
    pub async fn get_cooldown_status(&self, phone_account_id: i64) -> Result<ScrapeCooldownInfo, AppError> {
        // Prefer the explicit per-account next_eligible_at that was stored after the
        // last completed job (set by Rule A or Rule B). Fall back to deriving it from
        // scraping_completed_at when the column is not yet populated (e.g., old rows).
        let last_record: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT scraping_completed_at, next_eligible_at
             FROM scraping_history
             WHERE phone_account_id = ? AND scraping_status IN ('SUCCESS', 'ALREADY_VOTED')
             ORDER BY scraping_completed_at DESC LIMIT 1"
        )
        .bind(phone_account_id)
        .fetch_optional(&self.pool)
        .await?;

        let now = Utc::now();

        // If a stored next_eligible_at exists, use it directly for eligibility.
        if let Some((_, Some(next_str))) = &last_record {
            if let Ok(next) = DateTime::parse_from_rfc3339(next_str).map(|d| d.with_timezone(&Utc)) {
                let last_time = last_record
                    .as_ref()
                    .and_then(|(ts, _)| ts.as_deref())
                    .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                    .map(|d| d.with_timezone(&Utc));

                let is_eligible = now >= next;
                let remaining = if is_eligible { 0 } else { (next - now).num_seconds().max(0) };
                let mut cooldown = ScrapeCooldownInfo {
                    phone_account_id,
                    is_eligible,
                    last_scraping_at: last_time,
                    next_eligible_at: Some(next),
                    remaining_seconds: remaining,
                };

                // Still honour transient retry_after overrides.
                self.apply_retry_after_override(&mut cooldown, now).await?;
                return Ok(cooldown);
            }
        }

        // Fallback: derive eligibility from scraping_completed_at via day-boundary rules.
        let last_time: Option<DateTime<Utc>> = last_record
            .and_then(|(ts, _)| ts)
            .and_then(|ts| DateTime::parse_from_rfc3339(&ts).ok())
            .map(|d| d.with_timezone(&Utc));

        let mut cooldown = calculate_cooldown(phone_account_id, last_time, now);
        self.apply_retry_after_override(&mut cooldown, now).await?;
        Ok(cooldown)
    }

    /// Applies a transient retry_after from a recent FAILED record if it overrides
    /// the standard eligibility window. This handles 429/5xx back-off for a specific account.
    async fn apply_retry_after_override(
        &self,
        cooldown: &mut ScrapeCooldownInfo,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let retry_after: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT retry_after FROM scraping_history
             WHERE phone_account_id = ? AND scraping_status = 'FAILED' AND retry_after IS NOT NULL
             ORDER BY scraping_completed_at DESC LIMIT 1",
        )
        .bind(cooldown.phone_account_id)
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
        Ok(())
    }

    /// Executes the complete scraping pipeline with strict concurrency guards,
    /// per-account UTC day-boundary eligibility, persistent session decryption, and audit logging.
    pub async fn start_scraping(&self, phone_account_id: i64) -> Result<ScrapingRecord, AppError> {
        self.start_scraping_internal(phone_account_id, false).await
    }

    /// Performs one live website probe after successful OTP re-authentication.
    /// This bypasses only the stale local cooldown for this explicit operation.
    pub async fn start_scraping_after_reauthentication(
        &self,
        phone_account_id: i64,
    ) -> Result<ScrapingRecord, AppError> {
        self.start_scraping_internal(phone_account_id, true).await
    }

    async fn start_scraping_internal(
        &self,
        phone_account_id: i64,
        bypass_cooldown: bool,
    ) -> Result<ScrapingRecord, AppError> {
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
        let result = self
            .execute_scraping_flow(phone_account_id, bypass_cooldown)
            .await;

        // Release the lock
        {
            let mut active = active_jobs_ref.write().await;
            active.remove(&phone_account_id);
        }

        result
    }

    async fn execute_scraping_flow(
        &self,
        phone_account_id: i64,
        bypass_cooldown: bool,
    ) -> Result<ScrapingRecord, AppError> {
        // 2. Atomic 12-Hour Cooldown Verification
        if !bypass_cooldown {
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
        } else {
            tracing::warn!(
                "Running one explicit post-reauthentication website probe for account #{}; local cooldown bypassed.",
                phone_account_id
            );
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
                let status = if data.already_voted {
                    "ALREADY_VOTED"
                } else {
                    "SUCCESS"
                };
                let records_scraped = if data.already_voted { 0 } else { data.records_count };

                // Single timer rule: after a confirmed outcome (success or already-voted),
                // set next_eligible_at to the next 02:00 UTC.  This is the shared daily
                // run time for all accounts, two hours after the website's 00:00 UTC reset.
                // The timer is started only now that the outcome is confirmed.
                let next_eligible_at = next_daily_run_utc(completed_at);

                tracing::info!(
                    "Account #{}: {} — next eligible at {} UTC.",
                    phone_account_id,
                    status,
                    next_eligible_at.format("%Y-%m-%dT%H:%M:%SZ")
                );

                sqlx::query(
                    "UPDATE scraping_history 
                     SET scraping_completed_at = ?, scraping_status = ?, records_scraped = ?, error_message = NULL
                         , response_status = ?, response_body = ?, public_vote_count_before = ?, public_vote_count_after = ?
                         , next_eligible_at = ?
                     WHERE id = ?"
                )
                .bind(completed_at.to_rfc3339())
                .bind(status)
                .bind(records_scraped)
                .bind(data.response_status)
                .bind(Self::truncate_response_body(&data.response_body))
                .bind(data.public_vote_count_before)
                .bind(data.public_vote_count_after)
                .bind(next_eligible_at.to_rfc3339())
                .bind(history_id)
                .execute(&self.pool)
                .await?;

                tracing::info!(
                    "Scraping job #{} completed with status {} and {} new records.",
                    history_id,
                    status,
                    records_scraped
                );

                Ok(ScrapingRecord {
                    id: history_id,
                    phone_account_id,
                    scraping_started_at: started_at,
                    scraping_completed_at: Some(completed_at),
                    scraping_status: if data.already_voted {
                        ScrapingStatus::AlreadyVoted
                    } else {
                        ScrapingStatus::Success
                    },
                    records_scraped,
                    error_message: None,
                    response_status: Some(i64::from(data.response_status)),
                    response_body: Some(data.response_body),
                    public_vote_count_before: data.public_vote_count_before,
                    public_vote_count_after: data.public_vote_count_after,
                })
            }
            Err(err) => {
                let err_msg = err.to_string();
                let response = match &err {
                    AppError::ScraperResponse { status, body } => {
                        Some((i64::from(*status), Self::truncate_response_body(body)))
                    }
                    _ => None,
                };
                sqlx::query(
                    "UPDATE scraping_history 
                     SET scraping_completed_at = ?, scraping_status = 'FAILED', error_message = ?,
                         retry_after = ?, response_status = ?, response_body = ?,
                         public_vote_count_before = ?, public_vote_count_after = ?
                     WHERE id = ?"
                )
                .bind(completed_at.to_rfc3339())
                .bind(&err_msg)
                .bind(if Self::is_transient_scrape_error(&err) {
                    Some((completed_at + Duration::hours(1)).to_rfc3339())
                } else {
                    None
                })
                .bind(response.as_ref().map(|(status, _)| *status))
                .bind(response.as_ref().map(|(_, body)| body))
                .bind(None::<i64>)
                .bind(None::<i64>)
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

    fn truncate_response_body(body: &str) -> String {
        const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024;
        if body.len() <= MAX_RESPONSE_BODY_BYTES {
            return body.to_string();
        }

        let mut end = MAX_RESPONSE_BODY_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...[truncated]", &body[..end])
    }

    /// Fetches the recent scraping history for a specific phone account.
    pub async fn get_history(&self, phone_account_id: i64, limit: i64) -> Result<Vec<ScrapingRecord>, AppError> {
        let records = sqlx::query_as::<_, ScrapingRecord>(
            "SELECT id, phone_account_id, scraping_started_at, scraping_completed_at, scraping_status, records_scraped, error_message, response_status, response_body, public_vote_count_before, public_vote_count_after
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
