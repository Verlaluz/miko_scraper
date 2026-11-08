-- Migration: Add label to phone_accounts table
ALTER TABLE phone_accounts ADD COLUMN label TEXT DEFAULT NULL;
