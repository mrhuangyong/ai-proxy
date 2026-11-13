//! Integration test for `GET /api/codex/accounts/:provider_id/usage`.
//!
//! The ChatGPT wham/usage URL is redirected to a wiremock server via
//! `AI_PROXY_CODEX_USAGE_URL`. All scenarios run inside ONE #[tokio::test]:
//! the app uses a process-global DB pool (OnceLock), so a single test binary
//! may only initialize it once.

use ai_proxy_lib::key::store::encrypt_api_key;
use ai_proxy_lib::server::router::create_router;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn seed_codex_provider(pool: &SqlitePool, pid: &str) {
    sqlx::query(&format!(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled, provider_kind) \
         VALUES ('{pid}','codex-{pid}','http://127.0.0.1:1/codex','responses','/responses','',1,'codex_oauth')",
        pid = pid,
    ))
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_standard_provider(pool: &SqlitePool, pid: &str) {
    sqlx::query(&format!(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled, provider_kind) \
         VALUES ('{pid}','std-{pid}','http://127.0.0.1:1','completions','/v1/chat/completions','',1,'standard')",
        pid = pid,
    ))
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_codex_account(pool: &SqlitePool, pid: &str, access_token: &str) {
    let (enc_access, access_nonce) = encrypt_api_key(access_token).unwrap();
    let (enc_refresh, refresh_nonce) = encrypt_api_key("unused-refresh-token").unwrap();
    let expires_at = chrono::Utc::now().timestamp() + 3600;
    sqlx::query(
        "INSERT INTO codex_accounts \
         (id, provider_id, chatgpt_account_id, email, plan_type, \
          encrypted_access_token, access_nonce, encrypted_refresh_token, refresh_nonce, \
          expires_at, last_refresh_at, status) \
         VALUES (?1, ?2, 'acc-usage-123', 'user@example.com', 'plus', ?3, ?4, ?5, ?6, ?7, NULL, 'active')",
    )
    .bind(format!("acc-{}", pid))
    .bind(pid)
    .bind(&enc_access)
    .bind(access_nonce.to_vec())
    .bind(&enc_refresh)
    .bind(refresh_nonce.to_vec())
    .bind(expires_at.to_string())
    .execute(pool)
    .await
    .unwrap();
}

/// Server builds wrap `/api` in JWT auth; mint a token with the default secret.
#[cfg(feature = "server")]
fn auth_header() -> Option<String> {
    let secret = std::env::var("AI_PROXY_JWT_SECRET")
        .unwrap_or_else(|_| "ai-proxy-default-jwt-secret-change-in-production".to_string());
    let claims = serde_json::json!({
        "sub": "test",
        "username": "test",
        "role": "admin",
        "exp": chrono::Utc::now().timestamp() + 3600,
    });
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap();
    Some(format!("Bearer {}", token))
}

#[cfg(not(feature = "server"))]
fn auth_header() -> Option<String> {
    None
}

async fn get_usage(
    addr: &std::net::SocketAddr,
    provider_id: &str,
    query: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut req = reqwest::Client::new().get(format!(
        "http://{}/api/codex/accounts/{}/usage{}",
        addr, provider_id, query
    ));
    if let Some(h) = auth_header() {
        req = req.header("authorization", h);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    (status, body)
}

#[tokio::test]
async fn codex_usage_endpoint_end_to_end() {
    let upstream = MockServer::start().await;
    std::env::set_var(
        "AI_PROXY_CODEX_USAGE_URL",
        format!("{}/backend-api/wham/usage", upstream.uri()),
    );

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    ai_proxy_lib::db::init::init_db(db_path.to_str().unwrap())
        .await
        .unwrap();
    let pool = SqlitePool::connect(&format!("sqlite:{}", db_path.to_str().unwrap()))
        .await
        .unwrap();

    let router = create_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 42.5,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 600,
                    "reset_at": 1730000000
                },
                "secondary_window": {
                    "used_percent": 10,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 3600,
                    "reset_at": 1730500000
                }
            }
        })))
        .mount(&upstream)
        .await;

    seed_codex_provider(&pool, "usage1").await;
    seed_codex_account(&pool, "usage1", "usage-access-token").await;

    // ── 1) first call hits upstream and normalizes windows ────────────────
    let (status, body) = get_usage(&addr, "usage1", "").await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true, "body: {}", body);
    let data = &body["data"];
    assert_eq!(data["plan_type"], "plus");
    assert_eq!(data["allowed"], true);
    assert_eq!(data["limit_reached"], false);
    let windows = data["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0]["label"], "5小时");
    assert_eq!(windows[0]["used_percent"], 42.5);
    assert_eq!(windows[0]["remaining_percent"], 57.5);
    assert_eq!(windows[0]["limit_window_seconds"], 18000);
    assert_eq!(windows[0]["reset_after_seconds"], 600);
    assert!(windows[0]["reset_at"].as_str().unwrap().contains('T'));
    assert_eq!(windows[1]["label"], "7天");
    assert_eq!(windows[1]["used_percent"], 10.0);
    assert_eq!(windows[1]["remaining_percent"], 90.0);

    let received = upstream.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let headers = &received[0].headers;
    assert_eq!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer usage-access-token")
    );
    assert_eq!(
        headers
            .get("chatgpt-account-id")
            .and_then(|v| v.to_str().ok()),
        Some("acc-usage-123")
    );

    // ── 2) second call without force is served from cache ─────────────────
    let (status, body) = get_usage(&addr, "usage1", "").await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["windows"][0]["used_percent"], 42.5);
    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        1,
        "cached call must not hit upstream"
    );

    // ── 3) ?force=1 bypasses the cache ────────────────────────────────────
    let (status, body) = get_usage(&addr, "usage1", "?force=1").await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        2,
        "force=1 must hit upstream again"
    );

    // ── 4) standard provider is rejected without touching upstream ────────
    seed_standard_provider(&pool, "std1").await;
    let (_, body) = get_usage(&addr, "std1", "").await;
    assert_eq!(body["success"], false, "body: {}", body);
    assert!(body["error"].as_str().unwrap().contains("Codex"));

    // ── 5) unknown provider is rejected ───────────────────────────────────
    let (_, body) = get_usage(&addr, "does-not-exist", "").await;
    assert_eq!(body["success"], false);
    assert!(body["error"].as_str().unwrap().contains("不存在"));

    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        2,
        "rejected providers must not hit upstream"
    );
}
