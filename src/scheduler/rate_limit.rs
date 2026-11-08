use chrono::{DateTime, Duration, Utc};
use crate::models::ScrapeCooldownInfo;

pub const COOLDOWN_HOURS: i64 = 12;

/// Calculates whether a phone account is eligible to scrape,
/// the next eligible UTC timestamp, and remaining seconds.
pub fn calculate_cooldown(
    phone_account_id: i64,
    last_successful_scrape: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> ScrapeCooldownInfo {
    match last_successful_scrape {
        None => ScrapeCooldownInfo {
            phone_account_id,
            is_eligible: true,
            last_scraping_at: None,
            next_eligible_at: None,
            remaining_seconds: 0,
        },
        Some(last_time) => {
            let next_eligible = last_time + Duration::hours(COOLDOWN_HOURS);
            if now >= next_eligible {
                ScrapeCooldownInfo {
                    phone_account_id,
                    is_eligible: true,
                    last_scraping_at: Some(last_time),
                    next_eligible_at: Some(next_eligible),
                    remaining_seconds: 0,
                }
            } else {
                let remaining = (next_eligible - now).num_seconds().max(0);
                ScrapeCooldownInfo {
                    phone_account_id,
                    is_eligible: false,
                    last_scraping_at: Some(last_time),
                    next_eligible_at: Some(next_eligible),
                    remaining_seconds: remaining,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_never_scraped_is_eligible() {
        let now = Utc::now();
        let info = calculate_cooldown(1, None, now);
        assert!(info.is_eligible);
        assert_eq!(info.remaining_seconds, 0);
    }

    #[test]
    fn test_scraped_13_hours_ago_is_eligible() {
        let now = Utc::now();
        let last = now - Duration::hours(13);
        let info = calculate_cooldown(1, Some(last), now);
        assert!(info.is_eligible);
        assert_eq!(info.remaining_seconds, 0);
    }

    #[test]
    fn test_scraped_2_hours_ago_is_not_eligible() {
        let now = Utc::now();
        let last = now - Duration::hours(2);
        let info = calculate_cooldown(1, Some(last), now);
        assert!(!info.is_eligible);
        // Expecting approximately 10 hours = 36000 seconds
        assert!(info.remaining_seconds > 35900 && info.remaining_seconds <= 36000);
    }
}
