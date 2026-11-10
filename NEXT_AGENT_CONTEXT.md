# Handoff Context

## Project

- Rust/Tokio Telegram bot located at `/home/eyobs/ELDCP/miko_scraper`.
- Docker Compose service/container: `miko_scraper`.
- Do not expose or log authentication tokens, session payloads, bot tokens, or other secrets.

## Current Behavior & Cooldown Logic

- The old 12-hour global interval and separate Rule A/Rule B timers have been replaced with a unified daily schedule.
- State and timers are tracked **strictly per-account** in SQLite.
- `scraping_history` persists `next_eligible_at` per record.
- **Shared Daily Run**: Runs once per day at `02:00 UTC` (two hours after the source website resets at `00:00 UTC`).
- **Single Timer Rule**: After any confirmed outcome (successful claim or already voted since last 00:00 UTC), `next_eligible_at` is set to the next `02:00 UTC`.
- **Initial Pass & Catch-Up**: On startup, any account whose `next_eligible_at` is in the past is processed immediately.
- **Inter-record Stagger & Retries**: 2–5 second random delay between records, with up to 3 retries (10s, 20s, 40s) on failure.
- All date/time calculations and storage use UTC (`DateTime<Utc>`).
- Admin commands `/rep`, `/pay`, `/check`, `/admin`, and `/claim` are live in Telegram.

## Validation Commands

```bash
docker compose build miko_scraper
docker compose up -d miko_scraper
docker compose ps
docker compose logs --tail=50 miko_scraper
```
