-- Codex official (ChatGPT OAuth) channels: one Codex account per provider.
-- `providers.provider_kind` distinguishes standard API-key providers from
-- Codex OAuth providers, whose bearer credential is a refreshed OAuth access
-- token (stored encrypted here) instead of a static `api_keys` row.
ALTER TABLE providers ADD COLUMN provider_kind TEXT NOT NULL DEFAULT 'standard';

CREATE TABLE IF NOT EXISTS codex_accounts (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL UNIQUE REFERENCES providers(id) ON DELETE CASCADE,
    chatgpt_account_id TEXT,
    email TEXT,
    plan_type TEXT,
    encrypted_access_token BLOB NOT NULL,
    access_nonce BLOB NOT NULL,
    encrypted_refresh_token BLOB NOT NULL,
    refresh_nonce BLOB NOT NULL,
    expires_at TEXT,
    last_refresh_at TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT
);
