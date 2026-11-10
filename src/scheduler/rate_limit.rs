use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use crate::models::ScrapeCooldownInfo;

/// The time-of-day at which the target website resets its daily quota (UTC).
/// Confirmed empirically: all accounts become eligible immediately after 00:00 UTC.
pub const DAILY_RESET_HOUR: u32 = 0;
pub const DAILY_RESET_MINUTE: u32 = 0;

/// Fixed UTC time-of-day at which the daily run executes for all accounts.
/// Set two hours after the website's 00:00 UTC reset to ensure the quota is
/// fully refreshed before the bot acts.
pub const DAILY_RUN_HOUR: u32 = 2;
pub const DAILY_RUN_MINUTE: u32 = 0;

/// Minimum and maximum inter-record delay (seconds) during the daily run.
/// A random value in [MIN, MAX] is chosen between consecutive account actions
/// to avoid bursting all requests at once.
pub const INTER_RECORD_DELAY_MIN_SECS: u64 = 2;
pub const INTER_RECORD_DELAY_MAX_SECS: u64 = 5;

/// Returns the next 02:00 UTC boundary strictly after `reference`.
/// If `reference` is exactly at 02:00 UTC, returns the *following* day's 02:00 UTC.
pub fn next_daily_run_utc(reference: DateTime<Utc>) -> DateTime<Utc> {
    let today_run = Utc
        .with_ymd_and_hms(
            reference.year(),
            reference.month(),
            reference.day(),
            DAILY_RUN_HOUR,
            DAILY_RUN_MINUTE,
            0,
        )
        .single()
        .expect("valid date");

    if reference < today_run {
        today_run
    } else {
        today_run + Duration::days(1)
    }
}

/// Returns the last 00:00 UTC that occurred at or before `now`.
pub fn last_midnight_utc(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()
        .expect("valid date")
}

/// Computes per-account eligibility based on UTC day boundaries and the fixed
/// 02:00 UTC daily run time.
///
/// # Rules
/// - **Never acted on** → eligible immediately.
/// - **Last completed before today's 00:00 UTC** → eligible (quota has reset).
/// - **Last completed at or after today's 00:00 UTC** (already acted today):
///   - `is_eligible = false`
///   - `next_eligible_at = next 02:00 UTC`
///
/// The stored `next_eligible_at` set by the scraping service (always the next
/// 02:00 UTC) takes precedence over this fallback when it is present.
pub fn calculate_cooldown(
    phone_account_id: i64,
    last_successful_scrape: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> ScrapeCooldownInfo {
    let last_midnight = last_midnight_utc(now);

    match last_successful_scrape {
        // Never acted on — always eligible.
        None => ScrapeCooldownInfo {
            phone_account_id,
            is_eligible: true,
            last_scraping_at: None,
            next_eligible_at: None,
            remaining_seconds: 0,
        },

        Some(last_time) => {
            // Eligible if the last successful action was BEFORE the current
            // 00:00 UTC boundary (the website has already reset the quota for today).
            if last_time < last_midnight {
                ScrapeCooldownInfo {
                    phone_account_id,
                    is_eligible: true,
                    last_scraping_at: Some(last_time),
                    next_eligible_at: Some(last_midnight), // already passed — just informational
                    remaining_seconds: 0,
                }
            } else {
                // Already acted since last 00:00 UTC reset. Next run is at 02:00 UTC tomorrow.
                let next = next_daily_run_utc(now);
                let remaining = (next - now).num_seconds().max(0);
                ScrapeCooldownInfo {
                    phone_account_id,
                    is_eligible: false,
                    last_scraping_at: Some(last_time),
                    next_eligible_at: Some(next),
                    remaining_seconds: remaining,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).single().unwrap()
    }

    #[test]
    fn never_acted_is_eligible() {
        let now = utc(2026, 11, 10, 14, 0, 0);
        let info = calculate_cooldown(1, None, now);
        assert!(info.is_eligible);
        assert_eq!(info.remaining_seconds, 0);
    }

    #[test]
    fn acted_yesterday_is_eligible_today() {
        // Voted at 23:50 UTC yesterday → before today's midnight → eligible.
        let now = utc(2026, 11, 10, 14, 0, 0);
        let last = utc(2026, 11, 9, 23, 50, 0);
        let info = calculate_cooldown(1, Some(last), now);
        assert!(info.is_eligible);
        assert_eq!(info.remaining_seconds, 0);
    }

    #[test]
    fn acted_this_morning_is_not_eligible() {
        // Voted at 00:30 UTC today → after today's midnight → not eligible.
        // Next run is at 02:00 UTC tomorrow.
        let now = utc(2026, 11, 10, 14, 0, 0);
        let last = utc(2026, 11, 10, 0, 30, 0);
        let info = calculate_cooldown(1, Some(last), now);
        assert!(!info.is_eligible);
        // next eligible = tomorrow 02:00 UTC = 2026-11-11T02:00:00Z
        let expected_next = utc(2026, 11, 11, 2, 0, 0);
        assert_eq!(info.next_eligible_at, Some(expected_next));
        // remaining ≈ (11-11 02:00 - 11-10 14:00) = 12 hours = 43200 s
        assert!(info.remaining_seconds > 43190 && info.remaining_seconds <= 43200);
    }

    #[test]
    fn acted_exactly_at_midnight_is_not_eligible() {
        // Edge: voted exactly at today's 00:00 UTC → still counts as today.
        let now = utc(2026, 11, 10, 6, 0, 0);
        let last = utc(2026, 11, 10, 0, 0, 0);
        let info = calculate_cooldown(1, Some(last), now);
        assert!(!info.is_eligible);
    }

    #[test]
    fn next_daily_run_utc_advances_correctly() {
        // Before 02:00 → today's 02:00
        let t = utc(2026, 11, 10, 1, 30, 0);
        let next = next_daily_run_utc(t);
        assert_eq!(next, utc(2026, 11, 10, 2, 0, 0));
    }

    #[test]
    fn next_daily_run_utc_after_run_time_advances_to_tomorrow() {
        // After 02:00 today → tomorrow's 02:00
        let t = utc(2026, 11, 10, 14, 30, 0);
        let next = next_daily_run_utc(t);
        assert_eq!(next, utc(2026, 11, 11, 2, 0, 0));
    }

    #[test]
    fn next_daily_run_utc_at_exact_run_time_advances_to_following_day() {
        let t = utc(2026, 11, 10, 2, 0, 0);
        let next = next_daily_run_utc(t);
        assert_eq!(next, utc(2026, 11, 11, 2, 0, 0));
    }

    #[test]
    fn last_midnight_utc_returns_today_midnight() {
        let t = utc(2026, 11, 10, 15, 45, 30);
        let last = last_midnight_utc(t);
        assert_eq!(last, utc(2026, 11, 10, 0, 0, 0));
    }
}
