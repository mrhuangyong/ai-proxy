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
    let has_request_timeout: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM settings WHERE key = 'request_timeout'",
    )
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

    info!("Database schema initialized");
    Ok(())
}
