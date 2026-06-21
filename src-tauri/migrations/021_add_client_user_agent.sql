-- Migration 021: record downstream (client) User-Agent for each request log
ALTER TABLE request_logs ADD COLUMN client_user_agent TEXT;
