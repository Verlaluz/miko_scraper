-- Migrations: Initial Schema for Miko Scraper
-- Enforce Foreign Keys in SQLite
PRAGMA foreign_keys = ON;

-- 1. Phone Accounts Table
CREATE TABLE IF NOT EXISTS phone_accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    phone_number TEXT NOT NULL UNIQUE,
    registration_status TEXT NOT NULL DEFAULT 'PENDING_OTP',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_phone_accounts_phone_number ON phone_accounts(phone_number);

-- 2. Authentication Sessions Table
-- Stores AES-256-GCM encrypted session blobs and unique nonces
CREATE TABLE IF NOT EXISTS authentication_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    phone_account_id INTEGER NOT NULL,
    encrypted_session_data BLOB NOT NULL,
    nonce BLOB NOT NULL,
    session_status TEXT NOT NULL DEFAULT 'ACTIVE',
    authenticated_at TEXT NOT NULL,
    expires_at TEXT,
    last_validated_at TEXT NOT NULL,
    FOREIGN KEY(phone_account_id) REFERENCES phone_accounts(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_auth_sessions_account_id ON authentication_sessions(phone_account_id);

-- 3. Scraping History Table
CREATE TABLE IF NOT EXISTS scraping_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    phone_account_id INTEGER NOT NULL,
    scraping_started_at TEXT NOT NULL,
    scraping_completed_at TEXT,
    scraping_status TEXT NOT NULL DEFAULT 'RUNNING',
    records_scraped INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    FOREIGN KEY(phone_account_id) REFERENCES phone_accounts(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_scraping_history_account_completed 
ON scraping_history(phone_account_id, scraping_status, scraping_completed_at DESC);
