-- Persist ChatGPT id_token so we can write a full official auth.json
-- (tokens.id_token / access_token / refresh_token / account_id) when
-- launching Codex with "inject official credentials".
ALTER TABLE codex_accounts ADD COLUMN encrypted_id_token BLOB;
ALTER TABLE codex_accounts ADD COLUMN id_nonce BLOB;
