ALTER TABLE telegram_workers ADD COLUMN paid_cooldown INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS worker_payments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    worker_telegram_id INTEGER NOT NULL,
    amount_paid INTEGER NOT NULL,
    paid_at TEXT NOT NULL,
    is_reversed INTEGER NOT NULL DEFAULT 0,
    reversed_at TEXT,
    FOREIGN KEY(worker_telegram_id) REFERENCES telegram_workers(telegram_id)
);

CREATE INDEX IF NOT EXISTS idx_worker_payments_worker_id ON worker_payments(worker_telegram_id);
