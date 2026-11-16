use crate::models::{PhoneAccount, ScrapingStatus};
use crate::scheduler::rate_limit::{
    next_daily_run_utc, today_run_utc, DAILY_RUN_HOUR, DAILY_RUN_MINUTE,
};
use crate::services::{AccountService, ScrapingService};
use chrono::Utc;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::sleep;

/// Number of accounts to vote in each batch window.
const BATCH_SIZE: usize = 100;

/// Gap between consecutive batches (5 minutes).
const BATCH_INTERVAL_SECS: u64 = 5 * 60;

/// Maximum concurrent voting tasks within a single batch of 100.
/// 5 concurrent tasks ensure steady throughput (~20-30s for 100 accounts)
/// without starving the 10-connection SQLite pool or tripping target server rate limits.
const MAX_CONCURRENT_TASKS: usize = 5;

/// Starts the daily autonomous background scheduler.
///
/// ## Timing & Behavior
/// - Daily runs start at 05:00 UTC (08:00 AM Ethiopian Time EAT).
/// - If started after 05:00 UTC (or on restart), it begins processing immediately.
/// - Accounts are claimed in batches of 100 with a 5-minute gap between batches.
/// - Within each batch, up to 5 accounts vote concurrently.
/// - If any account encounters an error, its next attempt is scheduled 1 hour later (`retry_after`).
/// - The batching continues until all ready accounts are claimed.
/// - If all ready accounts are claimed but some accounts are awaiting their 1-hour retry,
///   the scheduler waits until that retry window opens, claims them, and only sleeps until
///   tomorrow's 05:00 UTC once all eligible accounts are fully finished.
pub fn start_autonomous_scheduler(
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    _check_interval: Duration, // kept for API compatibility; timing is wall-clock based
) {
    tokio::spawn(async move {
        tracing::info!(
            "Autonomous daily scheduler started. Scheduled run time: {:02}:{:02} UTC every day. \
             Batch size: {}, batch interval: {}s, max concurrency: {}.",
            DAILY_RUN_HOUR,
            DAILY_RUN_MINUTE,
            BATCH_SIZE,
            BATCH_INTERVAL_SECS,
            MAX_CONCURRENT_TASKS,
        );

        loop {
            let now = Utc::now();
            let today_run = today_run_utc(now);

            // If it's before today's scheduled run time, wait until it starts.
            if now < today_run {
                let wait_duration = (today_run - now)
                    .to_std()
                    .unwrap_or(Duration::from_secs(0));

                tracing::info!(
                    "Daily scheduler: waiting until today's {:02}:{:02} UTC (in {:.1} hours).",
                    DAILY_RUN_HOUR,
                    DAILY_RUN_MINUTE,
                    wait_duration.as_secs_f64() / 3600.0
                );

                sleep(wait_duration).await;
            }

            tracing::info!(
                "Daily scheduler: {:02}:{:02} UTC cycle active — beginning batched claiming pass.",
                DAILY_RUN_HOUR,
                DAILY_RUN_MINUTE
            );
            run_batched_daily_pass(&account_service, &scraping_service).await;

            // Pass complete (all accounts claimed and all 1-hour retries completed for today).
            // Sleep until tomorrow's scheduled run time.
            let now = Utc::now();
            let next_run = next_daily_run_utc(now);
            let wait_duration = (next_run - now)
                .to_std()
                .unwrap_or(Duration::from_secs(0));

            tracing::info!(
                "Daily scheduler: finished today's run. Next daily run scheduled at {} UTC (in {:.1} hours).",
                next_run.format("%Y-%m-%dT%H:%M:%SZ"),
                wait_duration.as_secs_f64() / 3600.0
            );

            sleep(wait_duration).await;
        }
    });
}

/// Runs batched claiming passes of 100 accounts every 5 minutes until all ready
/// accounts and pending 1-hour retries are exhausted.
async fn run_batched_daily_pass(
    account_service: &Arc<AccountService>,
    scraping_service: &Arc<ScrapingService>,
) {
    let mut batch_number = 0;

    loop {
        // 1. Fetch next batch of up to 100 accounts ready to claim right now
        let ready_accounts = match account_service.get_ready_to_claim_accounts(BATCH_SIZE as i64).await {
            Ok(accs) => accs,
            Err(e) => {
                tracing::error!("Daily scheduler: failed to query ready accounts: {}", e);
                sleep(Duration::from_secs(30)).await;
                continue;
            }
        };

        if !ready_accounts.is_empty() {
            batch_number += 1;
            let batch_start = tokio::time::Instant::now();
            let count = ready_accounts.len();
            tracing::info!(
                "Daily scheduler: starting batch #{} ({} accounts)...",
                batch_number,
                count
            );

            // Execute the batch with bounded concurrency (up to 5 parallel tasks)
            process_batch_concurrently(scraping_service, ready_accounts).await;

            tracing::info!(
                "Daily scheduler: batch #{} completed. Waiting 5 minutes before next batch...",
                batch_number
            );

            // Enforce the 5-minute gap between batches
            let elapsed = batch_start.elapsed();
            let batch_interval = Duration::from_secs(BATCH_INTERVAL_SECS);
            if elapsed < batch_interval {
                sleep(batch_interval - elapsed).await;
            }
            continue;
        }

        // 2. No accounts ready right now. Check if any accounts have a pending 1-hour retry today.
        match account_service.get_earliest_pending_retry().await {
            Ok(Some(retry_at)) => {
                let now = Utc::now();
                if retry_at > now {
                    let wait_secs = (retry_at - now).num_seconds().max(1) as u64;
                    tracing::info!(
                        "Daily scheduler: all current ready accounts processed. Account retry due at {} UTC (in {} s). Waiting...",
                        retry_at.format("%Y-%m-%dT%H:%M:%SZ"),
                        wait_secs
                    );
                    // Sleep until retry is due (capped at 5 minutes to stay responsive to newly added accounts)
                    sleep(Duration::from_secs(wait_secs.min(BATCH_INTERVAL_SECS))).await;
                    continue;
                } else {
                    // Retry is already due, loop immediately to fetch and process it
                    continue;
                }
            }
            Ok(None) => {
                // No ready accounts and no pending retries for today. Pass complete!
                tracing::info!(
                    "Daily scheduler: all verified accounts claimed and no retries pending for today. Daily pass complete."
                );
                break;
            }
            Err(e) => {
                tracing::error!("Daily scheduler: failed to check pending retries: {}", e);
                sleep(Duration::from_secs(30)).await;
                break;
            }
        }
    }
}

/// Executes up to [`BATCH_SIZE`] accounts concurrently with bounded concurrency
/// via a Tokio [`Semaphore`].
async fn process_batch_concurrently(
    scraping_service: &Arc<ScrapingService>,
    accounts: Vec<PhoneAccount>,
) {
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));
    let mut handles = Vec::with_capacity(accounts.len());

    for account in accounts {
        let svc = Arc::clone(scraping_service);
        let sem = Arc::clone(&semaphore);
        handles.push(tokio::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            process_account_once(&svc, account.id).await;
        }));
    }

    for handle in handles {
        if let Err(e) = handle.await {
            tracing::error!("Daily scheduler: batch task panicked: {:?}", e);
        }
    }
}

/// Attempts the vote action for one account once.
///
/// - On success (`SUCCESS` or `ALREADY_VOTED`), sets `next_eligible_at` to tomorrow 05:00 UTC.
/// - On any error, `scraping_service` writes `retry_after = completed_at + 1 hour` to the DB row.
///   The account will be retried in the next batch after that 1-hour window elapses.
async fn process_account_once(scraping_service: &Arc<ScrapingService>, account_id: i64) {
    match scraping_service.start_scraping(account_id).await {
        Ok(rec) => match rec.scraping_status {
            ScrapingStatus::AlreadyVoted => {
                tracing::info!(
                    "Daily scheduler: account #{} — already voted today; next eligible at {:02}:{:02} UTC tomorrow.",
                    account_id, DAILY_RUN_HOUR, DAILY_RUN_MINUTE
                );
            }
            ScrapingStatus::Success => {
                tracing::info!(
                    "Daily scheduler: account #{} — voted successfully ({} free votes claimed).",
                    account_id, rec.records_scraped
                );
            }
            other => {
                tracing::warn!(
                    "Daily scheduler: account #{} — unexpected status {:?}.",
                    account_id, other
                );
            }
        },
        Err(err) => {
            tracing::error!(
                "Daily scheduler: account #{} — error during vote: {}. Scheduled retry in 1 hour.",
                account_id, err
            );
        }
    }
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
