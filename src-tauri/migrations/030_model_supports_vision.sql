-- Migration 030: per-model vision (image input) capability flag.
--
-- Drives Codex `input_modalities` in GET /v1/models and ~/.codex/models.json,
-- and strips IrContentPart::Image during failover sanitization when disabled.
-- Defaults to 1 (enabled) so existing models keep advertising image input.
ALTER TABLE provider_models ADD COLUMN supports_vision INTEGER NOT NULL DEFAULT 1;
