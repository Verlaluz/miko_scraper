# Handoff Context

## Project

- Rust/Tokio Telegram bot located at `/home/eyobs/ELDCP/miko_scraper`.
- Docker Compose service/container: `miko_scraper`.
- Do not expose or log authentication tokens, session payloads, bot tokens, or other secrets.

## Current Behavior & Cooldown Logic

- The old 12-hour global interval and separate Rule A/Rule B timers have been replaced with a unified daily schedule.
- State and timers are tracked **strictly per-account** in SQLite.
- `scraping_history` persists `next_eligible_at` and `retry_after` per record.
- **Shared Daily Run**: Runs starting at `02:00 UTC` (two hours after the source website resets at `00:00 UTC`).
- **Batched Claims (100 accounts / 5 min)**:
  - Accounts are processed in batches of 100 with a 5-minute gap between batches.
  - Concurrency within each batch is bounded to 5 simultaneous requests (preventing database pool exhaustion and 429 rate limits).
- **1-Hour Error Retry**:
  - If any account fails during voting, its `retry_after` is set to 1 hour in the future.
  - The scheduler continues running batches until all ready accounts are claimed.
  - If pending retries exist, the scheduler waits for their 1-hour window to mature, claims them, and only sleeps until tomorrow's 02:00 UTC once all eligible accounts are claimed.
- **Single Timer Rule on Success**: After a confirmed claim or already-voted outcome, `next_eligible_at` is set to tomorrow's `02:00 UTC`.
- All date/time calculations and storage use UTC (`DateTime<Utc>`).
- Admin commands `/rep`, `/pay`, `/check`, `/admin`, `/claim`, and `/help` are live in Telegram (strictly restricted to administrators via `is_admin`).
- Non-admin `/start` only exposes public worker commands (`/register`, `/worker`, `/cancel`).
- Registration confirmation message is streamlined (removed 24/7/12-hour automated phrasing; removed chat probe notification).

## Validation Commands

```bash
docker compose build miko_scraper
docker compose up -d miko_scraper
docker compose ps
docker compose logs --tail=50 miko_scraper
```
