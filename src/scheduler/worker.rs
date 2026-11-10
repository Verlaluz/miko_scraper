use crate::models::{RegistrationStatus, ScrapingStatus};
use crate::scheduler::rate_limit::{
    last_midnight_utc, next_daily_run_utc, DAILY_RUN_HOUR, DAILY_RUN_MINUTE,
    INTER_RECORD_DELAY_MAX_SECS, INTER_RECORD_DELAY_MIN_SECS,
};
use crate::services::{AccountService, ScrapingService};
use chrono::Utc;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Maximum number of per-account retry attempts on failure before giving up
/// for this run and moving on to the next account.
const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Base delay (seconds) for the first retry after a failure.
/// Each subsequent retry doubles this value (exponential back-off).
const RETRY_BASE_DELAY_SECS: u64 = 10;

/// Starts the daily autonomous background scheduler.
///
/// ## Timing
/// All accounts share a single fixed run time: 02:00 UTC every day, two hours
/// after the target website resets its daily quota at 00:00 UTC.
/// Constants [`DAILY_RUN_HOUR`] and [`DAILY_RUN_MINUTE`] configure this.
///
/// ## Startup catch-up
/// On the very first tick after launch the worker checks every verified account.
/// Any account whose `next_eligible_at` is in the past (i.e., the scheduled 02:00
/// UTC run was missed) is processed immediately, so that a restart after downtime
/// does not forfeit the day's actions.
///
/// ## Per-record stagger
/// A random delay in [`INTER_RECORD_DELAY_MIN_SECS`]..=[`INTER_RECORD_DELAY_MAX_SECS`]
/// seconds is inserted between consecutive account actions to avoid request bursts.
///
/// ## Failure handling
/// If an account fails, its `next_eligible_at` is not updated. The worker retries
/// up to [`MAX_RETRY_ATTEMPTS`] times with exponential back-off, logs the failure,
/// and continues with the remaining accounts.
pub fn start_autonomous_scheduler(
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    _check_interval: Duration, // kept for API compatibility; timing is now wall-clock based
) {
    tokio::spawn(async move {
        tracing::info!(
            "Daily scheduler started. Scheduled run time: {:02}:{:02} UTC every day.",
            DAILY_RUN_HOUR,
            DAILY_RUN_MINUTE
        );

        // On startup, immediately run a catch-up pass so that any accounts whose
        // scheduled 02:00 UTC run was missed while the program was not running are
        // processed without waiting until the next scheduled time.
        tracing::info!("Daily scheduler: running startup catch-up pass.");
        run_daily_pass(&account_service, &scraping_service, true).await;

        loop {
            // Sleep until the next 02:00 UTC.
            let now = Utc::now();
            let next_run = next_daily_run_utc(now);
            let wait_duration = (next_run - now)
                .to_std()
                .unwrap_or(Duration::from_secs(0));

            tracing::info!(
                "Daily scheduler: next run scheduled at {} UTC (in {:.1} hours).",
                next_run.format("%Y-%m-%dT%H:%M:%SZ"),
                wait_duration.as_secs_f64() / 3600.0
            );

            sleep(wait_duration).await;

            tracing::info!(
                "Daily scheduler: 02:00 UTC reached — starting daily run."
            );
            run_daily_pass(&account_service, &scraping_service, false).await;
        }
    });
}

/// Executes one full daily pass over all verified accounts.
///
/// When `catch_up` is true, only accounts whose `next_eligible_at` is already
/// in the past (missed runs) are processed; accounts that are on-schedule are
/// skipped so that we do not double-fire if startup happens before 02:00 UTC.
///
/// When `catch_up` is false (the normal 02:00 UTC trigger) every eligible
/// account is processed regardless of whether the timer is past or not, because
/// the website's quota has just reset.
async fn run_daily_pass(
    account_service: &Arc<AccountService>,
    scraping_service: &Arc<ScrapingService>,
    catch_up: bool,
) {
    let accounts = match account_service.list_accounts().await {
        Ok(accs) => accs,
        Err(e) => {
            tracing::error!("Daily scheduler: failed to list accounts: {}", e);
            return;
        }
    };

    let now = Utc::now();
    let last_midnight = last_midnight_utc(now);

    // Verify website reset assumption: log a warning if the daily reset time
    // appears to differ from 00:00 UTC (not enforced here — just reported).
    // (Actual detection would require a live website probe; this comment
    // documents where such a check would be inserted.)

    let mut total_verified = 0u32;
    let mut total_eligible = 0u32;
    let mut total_skipped = 0u32;
    let mut first_account = true;

    for account in accounts {
        if account.registration_status != RegistrationStatus::Verified {
            continue;
        }
        total_verified += 1;

        // Determine eligibility.
        let cooldown = match scraping_service.get_cooldown_status(account.id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    "Daily scheduler: failed to get cooldown for account #{}: {}",
                    account.id,
                    e
                );
                continue;
            }
        };

        // During a normal 02:00 UTC run, an account is eligible if it has not
        // yet acted since the last 00:00 UTC reset (i.e., last success is before
        // today's midnight, or it has never acted).
        //
        // During a catch-up pass, additionally require that next_eligible_at is
        // already in the past to avoid acting too early.
        let should_act = if catch_up {
            // Catch-up: only act if the scheduled time for this account has passed.
            cooldown.is_eligible
                && cooldown
                    .next_eligible_at
                    .map(|t| t <= now)
                    .unwrap_or(true) // no stored timer → never acted → act now
        } else {
            // Normal daily run: act if not completed since last midnight.
            cooldown.is_eligible
                || cooldown
                    .last_scraping_at
                    .map(|t| t < last_midnight)
                    .unwrap_or(true)
        };

        if !should_act {
            tracing::debug!(
                "Daily scheduler: account #{} skipped (already acted today or on-schedule).",
                account.id
            );
            total_skipped += 1;
            continue;
        }

        // Insert inter-record delay between accounts (not before the first one).
        if !first_account {
            let delay_secs = INTER_RECORD_DELAY_MIN_SECS
                + (pseudo_rand_u64() % (INTER_RECORD_DELAY_MAX_SECS - INTER_RECORD_DELAY_MIN_SECS + 1));
            tracing::debug!(
                "Daily scheduler: waiting {} s before next account.",
                delay_secs
            );
            sleep(Duration::from_secs(delay_secs)).await;
        }
        first_account = false;
        total_eligible += 1;

        // Attempt the action with retry on failure.
        process_account_with_retry(scraping_service, account.id).await;
    }

    tracing::info!(
        "Daily scheduler: pass complete — {} verified, {} acted on, {} skipped.",
        total_verified,
        total_eligible,
        total_skipped
    );
}

/// Attempts the daily action for one account, retrying up to [`MAX_RETRY_ATTEMPTS`]
/// times with exponential back-off on failure.  On success (regardless of whether
/// the website reported a new vote or already-voted), the scraping service sets
/// `next_eligible_at` to the next 02:00 UTC.  On persistent failure the timer is
/// not updated and the account will be retried in the next daily run.
async fn process_account_with_retry(scraping_service: &Arc<ScrapingService>, account_id: i64) {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match scraping_service.start_scraping(account_id).await {
            Ok(rec) => {
                match rec.scraping_status {
                    ScrapingStatus::AlreadyVoted => {
                        tracing::info!(
                            "Daily scheduler: account #{} — already voted today (confirmed by website); \
                             next_eligible_at set to next 02:00 UTC.",
                            account_id
                        );
                    }
                    ScrapingStatus::Success => {
                        tracing::info!(
                            "Daily scheduler: account #{} — voted successfully ({} record(s)); \
                             next_eligible_at set to next 02:00 UTC.",
                            account_id,
                            rec.records_scraped
                        );
                    }
                    other => {
                        tracing::warn!(
                            "Daily scheduler: account #{} — unexpected status {:?} on attempt {}.",
                            account_id,
                            other,
                            attempt
                        );
                    }
                }
                return; // success — stop retrying
            }
            Err(err) => {
                if attempt >= MAX_RETRY_ATTEMPTS {
                    tracing::error!(
                        "Daily scheduler: account #{} — all {} attempt(s) failed. \
                         Skipping until next daily run. Last error: {}",
                        account_id,
                        MAX_RETRY_ATTEMPTS,
                        err
                    );
                    return;
                }

                let backoff_secs = RETRY_BASE_DELAY_SECS * (1u64 << (attempt - 1));
                tracing::warn!(
                    "Daily scheduler: account #{} — attempt {}/{} failed: {}. \
                     Retrying in {} s.",
                    account_id,
                    attempt,
                    MAX_RETRY_ATTEMPTS,
                    err,
                    backoff_secs
                );
                sleep(Duration::from_secs(backoff_secs)).await;
            }
        }
    }
}

/// A simple deterministic pseudo-random u64 seeded from the current nanosecond
/// timestamp.  Used only for the inter-record delay jitter; not cryptographic.
fn pseudo_rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}

pub fn start_daily_database_backup(pool: SqlitePool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            match crate::db::backups::create_daily_backup(&pool).await {
                Ok(path) => tracing::info!("Daily SQLite backup created at {:?}", path),
                Err(error) => tracing::error!("Daily SQLite backup failed: {}", error),
            }
        }
    });
}
