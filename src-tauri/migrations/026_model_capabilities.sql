-- Migration 026: per-model capability flags.
--
-- These flags drive parameter sanitization during failover rotation so that a
-- parameter valid for one upstream model is stripped/clamped before being sent
-- to a different model reached via virtual-model rotation.
--
-- All flags default to 1 (enabled / permissive) so that EXISTING models behave
-- exactly as before until the user explicitly opts a capability off. Only
-- `max_output_tokens` is NULL by default (NULL = do not clamp).
ALTER TABLE provider_models ADD COLUMN supports_thinking INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_tools INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_temperature INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_top_p INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_top_k INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_presence_penalty INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_frequency_penalty INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_seed INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_response_format INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_stream_options INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN supports_stop INTEGER NOT NULL DEFAULT 1;
ALTER TABLE provider_models ADD COLUMN max_output_tokens INTEGER;
ALTER TABLE provider_models ADD COLUMN extra_passthrough INTEGER NOT NULL DEFAULT 1;
