-- Multi-protocol upstreams: one provider may expose several API protocols
-- (completions / responses / anthropic / gemini), each optionally overriding
-- the provider-level base_url and endpoint path. When the downstream client
-- protocol matches one of these rows the proxy forwards the request body
-- as-is (passthrough) instead of converting through the IR layer.
--
-- `providers.format` / `providers.endpoint_path` remain as the mirror of the
-- primary (is_primary = 1) protocol row for backward compatibility with
-- probe / health-check / legacy callers.
CREATE TABLE IF NOT EXISTS provider_protocols (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    format TEXT NOT NULL CHECK(format IN ('completions', 'responses', 'anthropic', 'gemini')),
    base_url TEXT,
    endpoint_path TEXT,
    is_primary INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(provider_id, format)
);

-- Seed one primary protocol row per existing provider so current behaviour is
-- unchanged after the migration.
INSERT OR IGNORE INTO provider_protocols (id, provider_id, format, base_url, endpoint_path, is_primary)
SELECT lower(hex(randomblob(16))), id, format, NULL, endpoint_path, 1 FROM providers;

-- Observability: mark passthrough (non-converted) requests in the log store.
ALTER TABLE request_logs ADD COLUMN is_passthrough INTEGER NOT NULL DEFAULT 0;
