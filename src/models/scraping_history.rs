use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Status of a scraping job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScrapingStatus {
    Running,
    Success,
    AlreadyVoted,
    Failed,
}

impl Default for ScrapingStatus {
    fn default() -> Self {
        Self::Running
    }
}

/// Represents a record in the `scraping_history` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ScrapingRecord {
    pub id: i64,
    pub phone_account_id: i64,
    pub scraping_started_at: DateTime<Utc>,
    pub scraping_completed_at: Option<DateTime<Utc>>,
    pub scraping_status: ScrapingStatus,
    pub records_scraped: i64,
    pub error_message: Option<String>,
    pub response_status: Option<i64>,
    pub response_body: Option<String>,
    pub public_vote_count_before: Option<i64>,
    pub public_vote_count_after: Option<i64>,
}

/// Information returned to the frontend indicating eligibility and cooldown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapeCooldownInfo {
    pub phone_account_id: i64,
    pub is_eligible: bool,
    pub last_scraping_at: Option<DateTime<Utc>>,
    pub next_eligible_at: Option<DateTime<Utc>>,
    pub remaining_seconds: i64,
}
