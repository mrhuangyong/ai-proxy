-- Migration 020: split request_logs.model into model (request model) + target_model (resolved upstream model)
-- Previously the `model` column stored either "foo" or "foo -> bar" depending on whether an interceptor
-- rewrote the model name. Splitting into two columns makes per-model statistics accurate.
ALTER TABLE request_logs ADD COLUMN target_model TEXT NOT NULL DEFAULT '';

-- Backfill: for legacy rows the old `model` value already holds the (possibly mapped) display string.
-- Copy it verbatim into target_model so no data is lost.
UPDATE request_logs SET target_model = model;
