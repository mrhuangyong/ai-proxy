-- Track upstream retry behavior per request.
-- upstream_retry_count: number of retries before this request's final outcome (0 = succeeded on first try)
-- upstream_last_error: short snippet of the last failure reason, NULL if no retry happened
ALTER TABLE request_logs ADD COLUMN upstream_retry_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_logs ADD COLUMN upstream_last_error TEXT;
