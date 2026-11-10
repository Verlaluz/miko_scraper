-- Adds per-account next-eligible timestamp and the rule that produced it.
-- Rule A: bot claimed successfully now → 24 hours after confirmed success.
-- Rule B: already claimed since last 00:00 UTC → next 00:00 UTC.
ALTER TABLE scraping_history ADD COLUMN next_eligible_at TEXT;
ALTER TABLE scraping_history ADD COLUMN cooldown_rule TEXT;
