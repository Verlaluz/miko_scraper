CREATE TABLE IF NOT EXISTS telegram_workers (
    telegram_id INTEGER PRIMARY KEY,
    display_name TEXT NOT NULL,
    registered_at TEXT NOT NULL
);

ALTER TABLE phone_accounts ADD COLUMN added_by_telegram_id INTEGER;

CREATE INDEX IF NOT EXISTS idx_phone_accounts_added_by_telegram_id
ON phone_accounts(added_by_telegram_id);
