-- Migration 022: global custom upstream User-Agent (empty = passthrough client UA)
INSERT OR IGNORE INTO settings (key, value) VALUES ('upstream_user_agent', '');
