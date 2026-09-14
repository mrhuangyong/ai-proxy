//! Public model discovery endpoints advertise only the qualified
//! `provider_name/model_name` form. Bare model names must no longer appear in
//! `/v1/models` (OpenAI `data[]` + codex `models[]`) nor in
//! `/v1beta/models` (Gemini `models[]`).
//!
//! NOTE: the handlers route through the process-global pool (OnceLock), so the
//! whole binary must share a single initialized database. All scenarios
//! therefore live inside one test.

use ai_proxy_lib::server::handlers::{handle_gemini_list_models, handle_list_models};
use axum::body::to_bytes;
use serde_json::Value;
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

    // Provider "wwai" shares the bare name with opencode: the qualified form is
    // what disambiguates them in the listing.
    sqlx::query(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('p2','wwai','https://api.wwai.cn','completions','/v1/chat/completions','',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m2','p2','deepseek-v4-flash','flash-wwai',1,128000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // A model whose NAME contains a slash: still one entry, advertised as
    // provider/qwen/qwen3.6-27b.
    sqlx::query(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('p3','lmstudio','http://localhost:1234','completions','/v1/chat/completions','',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m3','p3','qwen/qwen3.6-27b','qwen3.6-27b',1,272000)",
    )
    .execute(&pool)
    .await
    .unwrap();
}

async fn json_body(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn model_listings_only_advertise_qualified_ids() {
    setup_global().await;

    // /v1/models: OpenAI data[] carries exactly one qualified entry per route.
    let body = json_body(handle_list_models().await).await;
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 3, "one entry per route, no bare duplicates");
    let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"opencode/deepseek-v4-flash"));
    assert!(ids.contains(&"wwai/deepseek-v4-flash"));
    assert!(ids.contains(&"lmstudio/qwen/qwen3.6-27b"));
    for id in &ids {
        assert_ne!(*id, "deepseek-v4-flash", "bare name must not be listed");
    }

    // /v1/models: codex models[] uses the qualified form everywhere.
    let codex = body["models"].as_array().unwrap();
    assert_eq!(codex.len(), 3);
    for m in codex {
        let slug = m["slug"].as_str().unwrap();
        let display_name = m["display_name"].as_str().unwrap();
        let name = m["name"].as_str().unwrap();
        assert_eq!(slug, name);
        assert_eq!(display_name, name);
        assert!(
            slug != "deepseek-v4-flash" && slug != "qwen/qwen3.6-27b",
            "bare model names must not leak into slug/display_name: {}",
            slug
        );
    }
    let slugs: Vec<&str> = codex.iter().map(|m| m["slug"].as_str().unwrap()).collect();
    assert!(slugs.contains(&"opencode/deepseek-v4-flash"));
    assert!(slugs.contains(&"wwai/deepseek-v4-flash"));
    assert!(slugs.contains(&"lmstudio/qwen/qwen3.6-27b"));

    // /v1beta/models: Gemini names are models/<provider>/<model> only.
    let gemini_body = json_body(handle_gemini_list_models().await).await;
    let gemini = gemini_body["models"].as_array().unwrap();
    assert_eq!(gemini.len(), 3, "one entry per route, no bare duplicates");
    let names: Vec<&str> = gemini.iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"models/opencode/deepseek-v4-flash"));
    assert!(names.contains(&"models/wwai/deepseek-v4-flash"));
    assert!(names.contains(&"models/lmstudio/qwen/qwen3.6-27b"));
    for name in &names {
        assert_ne!(
            *name, "models/deepseek-v4-flash",
            "bare name must not be listed"
        );
    }
}
