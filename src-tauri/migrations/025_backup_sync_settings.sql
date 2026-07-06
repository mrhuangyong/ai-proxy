-- Migration 025: backup & sync settings
INSERT OR IGNORE INTO settings (key, value, updated_at) VALUES
  ('backup_passphrase', '', datetime('now')),
  ('sync_enabled', 'false', datetime('now')),
  ('sync_backend', 'webdav', datetime('now')),
  ('sync_webdav_url', '', datetime('now')),
  ('sync_webdav_username', '', datetime('now')),
  ('sync_webdav_path', 'ai-proxy-backups/', datetime('now')),
  ('sync_webdav_password', '', datetime('now')),
  ('sync_auto_enabled', 'false', datetime('now')),
  ('sync_auto_interval_minutes', '60', datetime('now')),
  ('sync_on_change', 'false', datetime('now')),
  ('sync_dirty', 'false', datetime('now')),
  ('sync_last_upload_at', '', datetime('now')),
  ('sync_last_upload_status', '', datetime('now')),
  ('sync_last_error', '', datetime('now'));
