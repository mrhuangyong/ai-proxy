//! Model routing: the `provider_name/model_name` qualified form.
//!
//! A downstream request may target a specific provider with
//! `provider_name/model_name` (e.g. `opencode/deepseek-v4-flash`). Bare model
//! names (including ones containing `/`, like `qwen/qwen3.6-27b`) must keep
//! resolving exactly as before.
//!
//! NOTE: `ProviderManager` routes through a process-global pool (OnceLock), so
//! the whole binary must share a single initialized database. All scenarios
//! therefore live inside one test.

use ai_proxy_lib::provider::manager::ProviderManager;
use sqlx::SqlitePool;

/// Initialize the global pool (runs all migrations), then seed providers/models
/// directly via a second connection to the same file.
async fn setup_global() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let path = db_path.to_str().unwrap();
    ai_proxy_lib::db::init::init_db(path).await.unwrap();
    let pool = SqlitePool::connect(&format!("sqlite:{}", path))
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('p1','opencode','https://api.opencode.dev','completions','/v1/chat/completions','',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m1','p1','deepseek-v4-flash','deepseek-v4-flash',1,272000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m2','p1','deepseek-v4-pro','deepseek-v4-pro',1,272000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Provider "wwai" with a model sharing a bare name with opencode's.
    sqlx::query(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('p2','wwai','https://api.wwai.cn','completions','/v1/chat/completions','',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m3','p2','deepseek-v4-flash','flash-wwai',1,128000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Provider "lmstudio" with a model whose NAME contains a slash.
    sqlx::query(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('p3','lmstudio','http://localhost:1234','completions','/v1/chat/completions','',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m4','p3','qwen/qwen3.6-27b','qwen3.6-27b',1,272000)",
    )
    .execute(&pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn qualified_and_bare_model_routing() {
    setup_global().await;

    // Bare name: opencode and wwai both have deepseek-v4-flash; picks one.
    let route = ProviderManager::find_for_model("deepseek-v4-flash")
        .await
        .unwrap();
    assert!(["opencode", "wwai"].contains(&route.provider_name.as_str()));

    // Qualified name pins the provider.
    let route = ProviderManager::find_for_model("opencode/deepseek-v4-flash")
        .await
        .unwrap();
    assert_eq!(route.provider_name, "opencode");
    assert_eq!(route.target_model, "deepseek-v4-flash");

    // wwai maps the same bare model to a different target_model.
    let route = ProviderManager::find_for_model("wwai/deepseek-v4-flash")
        .await
        .unwrap();
    assert_eq!(route.provider_name, "wwai");
    assert_eq!(route.target_model, "flash-wwai");

    // Provider name is matched case-insensitively.
    let route = ProviderManager::find_for_model("OpenCode/deepseek-v4-pro")
        .await
        .unwrap();
    assert_eq!(route.provider_name, "opencode");

    // Literal slash model still resolves as a bare name (exact match wins).
    let route = ProviderManager::find_for_model("qwen/qwen3.6-27b")
        .await
        .unwrap();
    assert_eq!(route.provider_name, "lmstudio");
    assert_eq!(route.target_model, "qwen3.6-27b");

    // Qualified name whose model itself contains a slash.
    let route = ProviderManager::find_for_model("lmstudio/qwen/qwen3.6-27b")
        .await
        .unwrap();
    assert_eq!(route.provider_name, "lmstudio");
    assert_eq!(route.target_model, "qwen3.6-27b");

    // Empty-string target_model / endpoint_path (restored DBs may carry ''
    // instead of NULL) must fall back to sane defaults instead of producing
    // empty target or a "/" URL.
    let pool = ai_proxy_lib::db::get_pool().await;
    sqlx::query("UPDATE provider_models SET target_model = '' WHERE id = 'm4'")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE providers SET endpoint_path = '' WHERE id = 'p3'")
        .execute(pool)
        .await
        .unwrap();
    let route = ProviderManager::find_for_model("lmstudio/qwen/qwen3.6-27b")
        .await
        .unwrap();
    assert_eq!(
        route.target_model, "qwen/qwen3.6-27b",
        "empty target_model must fall back to model_name"
    );
    assert_eq!(
        route.endpoint_path, "/v1/chat/completions",
        "empty endpoint_path must fall back to default path"
    );

    // Unknown provider / model errors.
    assert!(ProviderManager::find_for_model("nope/does-not-exist")
        .await
        .is_err());
    assert!(ProviderManager::find_for_model("opencode/does-not-exist")
        .await
        .is_err());
    assert!(ProviderManager::find_for_model("does-not-exist")
        .await
        .is_err());

    // Disabled provider is skipped by qualified name too.
    let pool = ai_proxy_lib::db::get_pool().await;
    sqlx::query("UPDATE providers SET enabled = 0 WHERE id = 'p1'")
        .execute(pool)
        .await
        .unwrap();
    assert!(
        ProviderManager::find_for_model("opencode/deepseek-v4-flash")
            .await
            .is_err()
    );
}
