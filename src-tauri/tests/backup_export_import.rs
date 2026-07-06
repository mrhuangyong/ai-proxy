use ai_proxy_lib::backup::export::export_bundle_with_passphrase;
use ai_proxy_lib::backup::import::import_bundle;
use sqlx::{Row, SqlitePool};

/// Run the full migration chain (001..025) so the schema matches production.
/// This faithfully mirrors `db/init.rs` against an isolated in-memory pool,
/// including its **existence guards**.
///
/// Two important deviations from a naive "apply every file unconditionally":
/// 1. Migration 004 (`DROP COLUMN auth_type/auth_header`) is a **no-op on fresh
///    installs** — the current `001_init.sql` never creates those columns, so an
///    unconditional `DROP COLUMN` errors with "no such column". Production
///    `init.rs` guards it behind `has_auth_type`; we do the same.
/// 2. Migration 020 contains multiple statements (ALTER + UPDATE). sqlx::query
///    only executes the *first* statement of a multi-statement string, so it
///    must be split on `;` and applied individually (exactly as init.rs does).
///
/// NOTE: the lib crate name is `ai_proxy_lib` (per `[lib] name` in Cargo.toml),
/// NOT `ai_proxy` as the original brief assumed.
async fn apply_all_migrations(pool: &SqlitePool) {
    // 001 + 002: base schema + proxy_auth_key seed. Idempotent.
    for sql in [
        include_str!("../migrations/001_init.sql"),
        include_str!("../migrations/002_proxy_auth_key.sql"),
    ] {
        sqlx::query(sql).execute(pool).await.unwrap();
    }

    // 003: add providers.format (guarded — ADD COLUMN is not re-runnable).
    let has_format: bool = pragma_has_column(pool, "providers", "format").await;
    if !has_format {
        sqlx::query(include_str!("../migrations/003_simplify_routing.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 004: drop auth_type/auth_header — NO-OP on fresh installs (columns never
    // created by the current 001). Guard exactly like init.rs.
    let has_auth_type: bool = pragma_has_column(pool, "providers", "auth_type").await;
    if has_auth_type {
        sqlx::query(include_str!("../migrations/004_drop_auth_columns.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 005: cached_tokens / ttft_ms on request_logs (guarded).
    if !pragma_has_column(pool, "request_logs", "ttft_ms").await {
        sqlx::query(include_str!("../migrations/005_add_cache_ttft.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 006: app_configs table (guarded by table existence).
    if !has_table(pool, "app_configs").await {
        sqlx::query(include_str!("../migrations/006_app_configs.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 007: app_configs work_dir / model_config (guarded).
    if !pragma_has_column(pool, "app_configs", "work_dir").await {
        sqlx::query(include_str!("../migrations/007_app_configs_v2.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 008: timeout settings (guarded by setting existence).
    if !has_setting(pool, "request_timeout").await {
        sqlx::query(include_str!("../migrations/008_add_timeout_settings.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 009: drop usage_stats + add index (guarded by table existence).
    if has_table(pool, "usage_stats").await {
        sqlx::query(include_str!("../migrations/009_drop_usage_stats.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 010: providers.endpoint_path (guarded).
    if !pragma_has_column(pool, "providers", "endpoint_path").await {
        sqlx::query(include_str!("../migrations/010_add_endpoint_path.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 011: MCP tables (guarded).
    if !has_table(pool, "mcp_servers").await {
        sqlx::query(include_str!("../migrations/011_create_mcp_tables.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 012: skill tables (guarded).
    if !has_table(pool, "skill_sources").await {
        sqlx::query(include_str!("../migrations/012_create_skill_tables.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 013: users table (guarded).
    if !has_table(pool, "users").await {
        sqlx::query(include_str!("../migrations/013_add_users_table.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 014: provider_models.context_window (guarded).
    if !pragma_has_column(pool, "provider_models", "context_window").await {
        sqlx::query(include_str!("../migrations/014_add_context_window.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 015: skills.is_broken_symlink (guarded).
    if !pragma_has_column(pool, "skills", "is_broken_symlink").await {
        sqlx::query(include_str!("../migrations/015_add_broken_symlink.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 016: providers.enabled (guarded).
    if !pragma_has_column(pool, "providers", "enabled").await {
        sqlx::query(include_str!("../migrations/016_add_provider_enabled.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 017: extract_system_from_messages setting (guarded).
    if !has_setting(pool, "extract_system_from_messages").await {
        sqlx::query(include_str!(
            "../migrations/017_add_extract_system_from_messages.sql"
        ))
        .execute(pool)
        .await
        .unwrap();
    }

    // 018: request_logs usage JSON columns (guarded).
    if !pragma_has_column(pool, "request_logs", "final_usage_json").await {
        sqlx::query(include_str!("../migrations/018_add_upstream_usage.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 019: request_logs retry columns (guarded).
    if !pragma_has_column(pool, "request_logs", "upstream_retry_count").await {
        sqlx::query(include_str!("../migrations/019_add_retry_columns.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 020: request_logs.target_model — multi-statement, split on ';'.
    if !pragma_has_column(pool, "request_logs", "target_model").await {
        let m20 = include_str!("../migrations/020_add_target_model.sql");
        let stripped: String = m20
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for stmt in stripped.split(';') {
            let trimmed = stmt.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed).execute(pool).await.unwrap();
            }
        }
    }

    // 021: request_logs.client_user_agent (guarded).
    if !pragma_has_column(pool, "request_logs", "client_user_agent").await {
        sqlx::query(include_str!("../migrations/021_add_client_user_agent.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 022: upstream_user_agent setting (guarded).
    if !has_setting(pool, "upstream_user_agent").await {
        sqlx::query(include_str!(
            "../migrations/022_add_upstream_user_agent.sql"
        ))
        .execute(pool)
        .await
        .unwrap();
    }

    // 023: providers.upstream_user_agent (guarded).
    if !pragma_has_column(pool, "providers", "upstream_user_agent").await {
        sqlx::query(include_str!(
            "../migrations/023_add_provider_upstream_user_agent.sql"
        ))
        .execute(pool)
        .await
        .unwrap();
    }

    // 024: virtual_models tables (guarded).
    if !has_table(pool, "virtual_models").await {
        sqlx::query(include_str!("../migrations/024_virtual_models.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    // 025: backup & sync settings (guarded).
    if !has_setting(pool, "sync_webdav_url").await {
        sqlx::query(include_str!("../migrations/025_backup_sync_settings.sql"))
            .execute(pool)
            .await
            .unwrap();
    }
}

/// `SELECT COUNT(*) > 0 FROM pragma_table_info('tbl') WHERE name = 'col'`.
async fn pragma_has_column(pool: &SqlitePool, table: &str, col: &str) -> bool {
    let sql = format!(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('{}') WHERE name = '{}'",
        table, col
    );
    let row = sqlx::query(&sql).fetch_one(pool).await.unwrap();
    let v: i64 = row.get(0);
    v != 0
}

/// `SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?`.
async fn has_table(pool: &SqlitePool, table: &str) -> bool {
    let row = sqlx::query("SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?")
        .bind(table)
        .fetch_one(pool)
        .await
        .unwrap();
    let v: i64 = row.get(0);
    v != 0
}

/// `SELECT COUNT(*) > 0 FROM settings WHERE key = ?`.
async fn has_setting(pool: &SqlitePool, key: &str) -> bool {
    let row = sqlx::query("SELECT COUNT(*) > 0 FROM settings WHERE key = ?")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap();
    let v: i64 = row.get(0);
    v != 0
}

async fn setup_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    apply_all_migrations(&pool).await;
    // Seed a provider + model + api_key + sensitive setting.
    let (enc, nonce) = ai_proxy_lib::key::store::encrypt_api_key("sk-test").unwrap();
    sqlx::query(
        // `format` has a CHECK constraint: must be one of
        // completions/responses/anthropic/gemini (the brief's 'openai' is invalid).
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('p1','Test','https://x','responses','/v1','',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m1','p1','gpt-4','gpt-4',1,128000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO api_keys (id, provider_id, label, encrypted_key, nonce) VALUES ('k1','p1','main',?1,?2)")
        .bind(&enc[..])
        .bind(&nonce[..])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE settings SET value = 'secret-token' WHERE key = 'proxy_auth_key'")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

#[tokio::test]
async fn test_export_produces_valid_bundle() {
    let pool = setup_pool().await;
    let bytes = export_bundle_with_passphrase(&pool, "mypass")
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["kdf"]["algorithm"], "pbkdf2-sha256");
    assert_eq!(v["data"]["providers"][0]["name"], "Test");
    // api_keys should have re-encrypted encrypted_key (base64 string, non-empty).
    assert!(v["data"]["api_keys"][0]["encrypted_key"].is_string());
    assert_ne!(v["data"]["api_keys"][0]["encrypted_key"], "");
}

#[tokio::test]
async fn test_export_then_import_roundtrip() {
    let pool = setup_pool().await;
    let bytes = export_bundle_with_passphrase(&pool, "pw123").await.unwrap();
    // Wipe tables
    sqlx::query("DELETE FROM providers")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM api_keys")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM settings")
        .execute(&pool)
        .await
        .unwrap();
    // Import
    import_bundle(&pool, &bytes, Some("pw123")).await.unwrap();
    // Verify provider restored
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM providers")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    // Verify api_key decrypts correctly via the key store
    let row: (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT encrypted_key, nonce FROM api_keys WHERE id='k1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let nonce_arr: [u8; 12] = row.1.as_slice().try_into().unwrap();
    let plain = ai_proxy_lib::key::store::decrypt_api_key(&row.0, &nonce_arr).unwrap();
    assert_eq!(plain, "sk-test");
}

#[tokio::test]
async fn test_import_wrong_passphrase_does_not_mutate() {
    let pool = setup_pool().await;
    let bytes = export_bundle_with_passphrase(&pool, "right").await.unwrap();
    let result = import_bundle(&pool, &bytes, Some("WRONG")).await;
    assert!(result.is_err());
    // DB must be untouched: provider still present
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM providers")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}
