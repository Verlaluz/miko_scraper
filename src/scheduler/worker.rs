use crate::models::RegistrationStatus;
use crate::services::{AccountService, ScrapingService};
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Starts an autonomous background worker loop that runs 24/7.
///
/// Every `check_interval`, it inspects all verified accounts in the database.
/// Whenever an account reaches its 12-hour cooldown expiration, the worker
/// automatically claims the +5 free votes without requiring any manual button click.
pub fn start_autonomous_scheduler(
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    check_interval: Duration,
) {
    tokio::spawn(async move {
        tracing::info!(
            "Autonomous 12-Hour Scraping Worker started. Checking every {} seconds...",
            check_interval.as_secs()
        );

        let mut interval = tokio::time::interval(check_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let concurrency_limit = Arc::new(Semaphore::new(8));

        loop {
            interval.tick().await;

            tracing::debug!("Autonomous worker: checking for accounts eligible to claim votes...");

            let accounts = match account_service.list_accounts().await {
                Ok(accs) => accs,
                Err(e) => {
                    tracing::error!("Autonomous worker failed to list accounts: {}", e);
                    continue;
                }
            };

            let mut eligible_accounts = Vec::new();
            for account in accounts {
                if account.registration_status != RegistrationStatus::Verified {
                    continue;
                }

                // Check 12-hour cooldown status
                match scraping_service.get_cooldown_status(account.id).await {
                    Ok(cooldown) => {
                        if cooldown.is_eligible {
                            tracing::info!(
                                "Autonomous worker: Account #{} ({}) is eligible! Claiming free votes automatically...",
                                account.id, account.phone_number
                            );
                            eligible_accounts.push(account);
                        } else {
                            tracing::debug!(
                                "Account #{} ({}) on cooldown. {}s remaining.",
                                account.id, account.phone_number, cooldown.remaining_seconds
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Autonomous worker: Failed checking cooldown for account #{}: {}",
                            account.id, e
                        );
                    }
                }
            }

            if eligible_accounts.is_empty() {
                continue;
            }

            tracing::info!(
                "Autonomous worker: {} eligible account(s) queued for concurrent claims.",
                eligible_accounts.len()
            );

            let mut tasks = JoinSet::new();
            for account in eligible_accounts {
                let scraping_service = Arc::clone(&scraping_service);
                let concurrency_limit = Arc::clone(&concurrency_limit);
                tasks.spawn(async move {
                    let permit = match concurrency_limit.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => {
                            tracing::error!(
                                "Autonomous worker: concurrency limiter closed before account #{} could run.",
                                account.id
                            );
                            return;
                        }
                    };

                    let result = scraping_service.start_scraping(account.id).await;
                    drop(permit);

                    match result {
                        Ok(rec) => {
                            tracing::info!(
                                "Autonomous worker: Successfully claimed votes for #{}! Records: {}, Status: {:?}",
                                account.id,
                                rec.records_scraped,
                                rec.scraping_status
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                "Autonomous worker: Failed or skipped claim for #{}: {}",
                                account.id,
                                err
                            );
                        }
                    }
                });
            }

            while let Some(result) = tasks.join_next().await {
                if let Err(err) = result {
                    tracing::error!("Autonomous worker claim task failed: {}", err);
                }
            }
        }
    });
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
