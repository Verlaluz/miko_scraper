use crate::models::RegistrationStatus;
use crate::services::{AccountService, ScrapingService};
use std::sync::Arc;
use std::time::Duration;

/// Starts an autonomous background worker loop that runs 24/7.
///
/// Every 60 seconds, it inspects all verified accounts in the database.
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
        // First tick completes immediately
        interval.tick().await;

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

                            match scraping_service.start_scraping(account.id).await {
                                Ok(rec) => {
                                    tracing::info!(
                                        "Autonomous worker: Successfully claimed votes for #{}! Records: {}, Status: {:?}",
                                        account.id, rec.records_scraped, rec.scraping_status
                                    );
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        "Autonomous worker: Failed or skipped claim for #{}: {}",
                                        account.id, err
                                    );
                                }
                            }
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
        }
    });
}
