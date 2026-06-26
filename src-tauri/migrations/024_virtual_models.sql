-- Virtual models: a logical name that maps to one or more real provider_models
CREATE TABLE IF NOT EXISTS virtual_models (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  description TEXT,
  current_mapping_id TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Mapping between a virtual model and a real provider_model.
-- Real side identified by "provider_name/model_name" stored in `label`.
CREATE TABLE IF NOT EXISTS virtual_model_mappings (
  id TEXT PRIMARY KEY,
  virtual_model_id TEXT NOT NULL REFERENCES virtual_models(id) ON DELETE CASCADE,
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  provider_model_id TEXT NOT NULL REFERENCES provider_models(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 100,
  enabled INTEGER NOT NULL DEFAULT 1,
  available INTEGER NOT NULL DEFAULT 1,
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  failover_count INTEGER NOT NULL DEFAULT 0,
  last_failure_at TEXT,
  last_checked_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(virtual_model_id, provider_model_id)
);

CREATE INDEX IF NOT EXISTS idx_vmm_virtual ON virtual_model_mappings(virtual_model_id);
CREATE INDEX IF NOT EXISTS idx_vmm_avail ON virtual_model_mappings(available, enabled);

-- Defaults for failover behaviour, stored in settings.
INSERT OR IGNORE INTO settings (key, value) VALUES
  ('virtual_model_failure_threshold', '3'),
  ('virtual_model_max_failover', '3'),
  ('virtual_model_health_interval_secs', '60'),
  ('virtual_model_probe_max_tokens', '1');