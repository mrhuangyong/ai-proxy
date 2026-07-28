use super::pool::{get_pool, init_pool};
use tracing::info;

pub async fn init_db(db_path: &str) -> Result<(), sqlx::Error> {
    init_pool(db_path).await?;
    let pool = get_pool().await;

    let migration = include_str!("../../migrations/001_init.sql");
    sqlx::query(migration).execute(pool).await?;

    let migration2 = include_str!("../../migrations/002_proxy_auth_key.sql");
    sqlx::query(migration2).execute(pool).await?;

    // Migration 003: check if already applied by looking for 'format' column on providers
    let has_format: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('providers') WHERE name = 'format'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_format {
        let migration3 = include_str!("../../migrations/003_simplify_routing.sql");
        sqlx::query(migration3).execute(pool).await?;
        info!("Applied migration 003: simplify routing");
    }

    // Migration 004: drop auth_type/auth_header (determined by format now)
    let has_auth_type: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('providers') WHERE name = 'auth_type'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if has_auth_type {
        let migration4 = include_str!("../../migrations/004_drop_auth_columns.sql");
        sqlx::query(migration4).execute(pool).await?;
        info!("Applied migration 004: drop auth columns");
    }

    // Migration 005: add cached_tokens and ttft_ms columns
    let has_ttft: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('request_logs') WHERE name = 'ttft_ms'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_ttft {
        let migration5 = include_str!("../../migrations/005_add_cache_ttft.sql");
        sqlx::query(migration5).execute(pool).await?;
        info!("Applied migration 005: add cached_tokens and ttft_ms");
    }

    // Migration 006: app_configs table
    let has_app_configs: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='app_configs'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_app_configs {
        let migration6 = include_str!("../../migrations/006_app_configs.sql");
        sqlx::query(migration6).execute(pool).await?;
        info!("Applied migration 006: app_configs table");
    }

    // Migration 007: add work_dir and model_config columns
    let has_work_dir: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('app_configs') WHERE name = 'work_dir'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_work_dir {
        let migration7 = include_str!("../../migrations/007_app_configs_v2.sql");
        sqlx::query(migration7).execute(pool).await?;
        info!("Applied migration 007: add work_dir and model_config columns");
    }

    // Migration 008: add timeout settings
    let has_request_timeout: bool =
        sqlx::query_scalar("SELECT COUNT(*) > 0 FROM settings WHERE key = 'request_timeout'")
            .fetch_one(pool)
            .await
            .unwrap_or(false);

    if !has_request_timeout {
        let migration8 = include_str!("../../migrations/008_add_timeout_settings.sql");
        sqlx::query(migration8).execute(pool).await?;
        info!("Applied migration 008: add timeout settings");
    }

    // Migration 009: drop usage_stats table (statistics now derived from request_logs)
    let has_usage_stats: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='usage_stats'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if has_usage_stats {
        let migration9 = include_str!("../../migrations/009_drop_usage_stats.sql");
        sqlx::query(migration9).execute(pool).await?;
        info!("Applied migration 009: drop usage_stats table");
    }

    // Migration 010: add endpoint_path column to providers
    let has_endpoint_path: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('providers') WHERE name = 'endpoint_path'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_endpoint_path {
        let migration10 = include_str!("../../migrations/010_add_endpoint_path.sql");
        sqlx::query(migration10).execute(pool).await?;
        info!("Applied migration 010: add endpoint_path column");
    }

    // Migration 011: MCP Server management tables
    let has_mcp_servers: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='mcp_servers'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_mcp_servers {
        let migration11 = include_str!("../../migrations/011_create_mcp_tables.sql");
        sqlx::query(migration11).execute(pool).await?;
        info!("Applied migration 011: MCP Server management tables");
    }

    // Migration 012: Skill management tables
    let has_skill_sources: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='skill_sources'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_skill_sources {
        let migration12 = include_str!("../../migrations/012_create_skill_tables.sql");
        sqlx::query(migration12).execute(pool).await?;
        info!("Applied migration 012: Skill management tables");
    }

    // Migration 013: users table (server mode authentication)
    let has_users: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='users'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_users {
        let migration13 = include_str!("../../migrations/013_add_users_table.sql");
        sqlx::query(migration13).execute(pool).await?;
        info!("Applied migration 013: users table");
    }

    // Migration 014: add context_window column to provider_models
    let has_context_window: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('provider_models') WHERE name = 'context_window'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_context_window {
        let migration14 = include_str!("../../migrations/014_add_context_window.sql");
        sqlx::query(migration14).execute(pool).await?;
        info!("Applied migration 014: add context_window column");
    }

    // Migration 015: add is_broken_symlink column to skills
    let has_broken_symlink: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('skills') WHERE name = 'is_broken_symlink'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_broken_symlink {
        let migration15 = include_str!("../../migrations/015_add_broken_symlink.sql");
        sqlx::query(migration15).execute(pool).await?;
        info!("Applied migration 015: add is_broken_symlink column");
    }

    // Migration 016: add enabled column to providers
    let has_provider_enabled: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('providers') WHERE name = 'enabled'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_provider_enabled {
        let migration16 = include_str!("../../migrations/016_add_provider_enabled.sql");
        sqlx::query(migration16).execute(pool).await?;
        info!("Applied migration 016: add enabled column to providers");
    }

    // Migration 017: add extract_system_from_messages setting
    let has_extract_system: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM settings WHERE key = 'extract_system_from_messages'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_extract_system {
        let migration17 = include_str!("../../migrations/017_add_extract_system_from_messages.sql");
        sqlx::query(migration17).execute(pool).await?;
        info!("Applied migration 017: add extract_system_from_messages setting");
    }

    // Migration 018: add final_usage_json and upstream_usage_events_json columns to request_logs
    let has_final_usage: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('request_logs') WHERE name = 'final_usage_json'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_final_usage {
        let migration18 = include_str!("../../migrations/018_add_upstream_usage.sql");
        sqlx::query(migration18).execute(pool).await?;
        info!("Applied migration 018: add upstream usage columns to request_logs");
    }

    // Migration 019: add upstream retry tracking columns to request_logs
    let has_retry_count: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('request_logs') WHERE name = 'upstream_retry_count'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_retry_count {
        let migration19 = include_str!("../../migrations/019_add_retry_columns.sql");
        sqlx::query(migration19).execute(pool).await?;
        info!("Applied migration 019: upstream retry tracking columns");
    }

    // Migration 020: split request_logs.model into model + target_model
    let has_target_model: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('request_logs') WHERE name = 'target_model'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_target_model {
        let migration20 = include_str!("../../migrations/020_add_target_model.sql");
        // Strip comment lines first, then split on ';' and run each non-empty statement.
        let stripped: String = migration20
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for stmt in stripped.split(';') {
            let trimmed = stmt.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed).execute(pool).await?;
            }
        }
        info!("Applied migration 020: split model into model + target_model");
    }

    // Migration 021: record downstream (client) User-Agent on request_logs
    let has_client_user_agent: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('request_logs') WHERE name = 'client_user_agent'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_client_user_agent {
        let migration21 = include_str!("../../migrations/021_add_client_user_agent.sql");
        sqlx::query(migration21).execute(pool).await?;
        info!("Applied migration 021: add client_user_agent to request_logs");
    }

    // Migration 022: global custom upstream User-Agent
    let has_upstream_user_agent: bool =
        sqlx::query_scalar("SELECT COUNT(*) > 0 FROM settings WHERE key = 'upstream_user_agent'")
            .fetch_one(pool)
            .await
            .unwrap_or(false);

    if !has_upstream_user_agent {
        let migration22 = include_str!("../../migrations/022_add_upstream_user_agent.sql");
        sqlx::query(migration22).execute(pool).await?;
        info!("Applied migration 022: add upstream_user_agent setting");
    }

    // Migration 023: per-provider upstream User-Agent override
    let has_provider_upstream_user_agent: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('providers') WHERE name = 'upstream_user_agent'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_provider_upstream_user_agent {
        let migration23 = include_str!("../../migrations/023_add_provider_upstream_user_agent.sql");
        sqlx::query(migration23).execute(pool).await?;
        info!("Applied migration 023: add upstream_user_agent to providers");
    }

    // Migration 024: virtual models (failover routing layer)
    let has_virtual_models: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='virtual_models'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_virtual_models {
        let migration24 = include_str!("../../migrations/024_virtual_models.sql");
        sqlx::query(migration24).execute(pool).await?;
        info!("Applied migration 024: virtual models");
    }

    // Migration 025: backup & sync settings
    let has_backup_sync_settings: bool =
        sqlx::query_scalar("SELECT COUNT(*) > 0 FROM settings WHERE key = 'sync_webdav_url'")
            .fetch_one(pool)
            .await
            .unwrap_or(false);

    if !has_backup_sync_settings {
        let migration25 = include_str!("../../migrations/025_backup_sync_settings.sql");
        sqlx::query(migration25).execute(pool).await?;
        info!("Applied migration 025: backup & sync settings");
    }

    // Migration 026: per-model capability flags for failover parameter sanitization.
    // All flags default to permissive (1) so existing models behave unchanged.
    let has_model_caps: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('provider_models') WHERE name = 'supports_thinking'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_model_caps {
        let migration26 = include_str!("../../migrations/026_model_capabilities.sql");
        // The migration contains multiple ALTER TABLE statements; SQLite needs
        // them executed one at a time.
        let stripped: String = migration26
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for stmt in stripped.split(';') {
            let trimmed = stmt.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed).execute(pool).await?;
            }
        }
        info!("Applied migration 026: model capabilities");
    }

    info!("Database schema initialized");
    Ok(())
}
