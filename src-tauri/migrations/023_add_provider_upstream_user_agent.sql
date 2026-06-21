-- Migration 023: per-provider upstream User-Agent override (empty = inherit global)
ALTER TABLE providers ADD COLUMN upstream_user_agent TEXT NOT NULL DEFAULT '';
