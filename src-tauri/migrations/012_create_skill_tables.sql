CREATE TABLE IF NOT EXISTS skill_sources (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    is_global INTEGER NOT NULL DEFAULT 0,
    is_default INTEGER NOT NULL DEFAULT 0,
    discovery_order INTEGER NOT NULL DEFAULT 99,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS skills (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    source_id TEXT NOT NULL REFERENCES skill_sources(id) ON DELETE CASCADE,
    skill_path TEXT NOT NULL,
    is_symlink INTEGER NOT NULL DEFAULT 0,
    link_target TEXT,
    has_skill_md INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_skills_source_id ON skills(source_id);
CREATE INDEX IF NOT EXISTS idx_skills_name ON skills(name);
