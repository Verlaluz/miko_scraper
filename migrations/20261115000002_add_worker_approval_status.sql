-- Add approval_status to telegram_workers table (APPROVED, PENDING, REJECTED)
ALTER TABLE telegram_workers ADD COLUMN approval_status TEXT NOT NULL DEFAULT 'APPROVED';
