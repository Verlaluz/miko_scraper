-- Add is_blocked status to telegram_workers table
ALTER TABLE telegram_workers ADD COLUMN is_blocked INTEGER NOT NULL DEFAULT 0;
