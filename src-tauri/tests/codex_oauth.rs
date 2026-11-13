//! Integration tests for the Codex official (ChatGPT OAuth) channel.
//!
//! Pins the outbound contract end to end: the refreshed OAuth bearer token is
//! used instead of a static API key, the ChatGPT account headers
//! (`chatgpt-account-id`, `OpenAI-Beta`, `originator`) are injected, Responses
//! SSE is relayed byte-for-byte through the passthrough fast-path, and an
//! expiring token triggers a refresh-token grant (JSON, per the official
//! client) whose new credentials are persisted.
//!
//! All scenarios run inside ONE #[tokio::test]: the app uses a process-global
//! DB pool (OnceLock), so a single test binary may only initialize it once.
//! The OAuth token endpoint is redirected to a mock IdP via
//! `AI_PROXY_CODEX_TOKEN_URL` (mirrors the official override env var).

use ai_proxy_lib::codex::oauth;
use ai_proxy_lib::key::store::{decrypt_api_key, encrypt_api_key};
use ai_proxy_lib::server::router::create_router;
use base64::Engine;
use sqlx::SqlitePool;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal Responses SSE fixture relayed through the codex channel.
const RESPONSES_SSE: &str = concat!(
    "event: response.created\n",
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
    "event: response.output_text.delta\n",
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\",\"sequence_number\":1}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
);

fn make_id_token(exp_offset_secs: i64) -> String {
    let encode =
        |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let payload = serde_json::json!({
        "iss": "https://auth.openai.com",
        "exp": chrono::Utc::now().timestamp() + exp_offset_secs,
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acc-test-123",
            "chatgpt_plan_type": "plus"
        }
    });
    format!(
        "{}.{}.{}",
        encode(br#"{"alg":"RS256"}"#),
        encode(serde_json::to_string(&payload).unwrap().as_bytes()),
        "fake-signature"
    )
}

async fn seed_codex_provider(
    pool: &SqlitePool,
    upstream_uri: &str,
    pid: &str,
    model_name: &str,
) {
    sqlx::query(&format!(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled, provider_kind) \
         VALUES ('{pid}','codex-{pid}','{uri}/codex','responses','/responses','',1,'codex_oauth')",
        pid = pid,
        uri = upstream_uri,
    ))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "INSERT INTO provider_protocols (id, provider_id, format, base_url, endpoint_path, is_primary) \
         VALUES ('pp-{pid}','{pid}','responses',NULL,'/responses',1)",
        pid = pid,
    ))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) \
         VALUES ('m-{pid}','{pid}','{model}',NULL,1,272000)",
        pid = pid,
        model = model_name,
    ))
    .execute(pool)
    .await
    .unwrap();
}

/// Insert a codex_accounts row for `pid` with the given tokens/expiry.
async fn seed_codex_account(
    pool: &SqlitePool,
    pid: &str,
    access_token: &str,
    refresh_token: &str,
    expires_in_secs: i64,
) {
    let (enc_access, access_nonce) = encrypt_api_key(access_token).unwrap();
    let (enc_refresh, refresh_nonce) = encrypt_api_key(refresh_token).unwrap();
    let expires_at = chrono::Utc::now().timestamp() + expires_in_secs;
    sqlx::query(
        "INSERT INTO codex_accounts \
         (id, provider_id, chatgpt_account_id, email, plan_type, \
          encrypted_access_token, access_nonce, encrypted_refresh_token, refresh_nonce, \
          expires_at, last_refresh_at, status) \
         VALUES (?1, ?2, 'acc-test-123', 'user@example.com', 'plus', ?3, ?4, ?5, ?6, ?7, NULL, 'active')",
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

async fn mount_responses_sse(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(RESPONSES_SSE.as_bytes().to_vec(), "text/event-stream"),
        )
        .mount(server)
        .await;
}

async fn post_responses(
    addr: &std::net::SocketAddr,
    model: &str,
) -> (reqwest::StatusCode, String) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/responses", addr))
        .header("authorization", "Bearer any")
        .json(&serde_json::json!({
            "model": model,
            "stream": true,
            "input": "hi",
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.text().await.unwrap())
}

#[tokio::test]
async fn codex_channel_end_to_end() {
    // Point OAuth grants at a mock IdP (set before any request can refresh).
    let idp = MockServer::start().await;
    std::env::set_var("AI_PROXY_CODEX_TOKEN_URL", format!("{}/oauth/token", idp.uri()));

    // Global DB init (runs migrations incl. 031) — once per process.
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

    // ── Scenario 1: fresh token → bearer + account headers + SSE relay ────
    let server1 = MockServer::start().await;
    mount_responses_sse(&server1).await;
    seed_codex_provider(&pool, &server1.uri(), "codex1", "gpt-5.1-codex").await;
    seed_codex_account(&pool, "codex1", "fresh-access-token", "fresh-refresh-token", 3600).await;

    let (status, body) = post_responses(&addr, "gpt-5.1-codex").await;
    assert_eq!(status, 200, "codex passthrough request should succeed");
    assert_eq!(body, RESPONSES_SSE, "SSE must be relayed byte-for-byte");

    let received = server1.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let headers = &received[0].headers;
    assert_eq!(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some("Bearer fresh-access-token"),
        "the OAuth access token must be the bearer credential"
    );
    assert_eq!(
        headers.get("chatgpt-account-id").and_then(|v| v.to_str().ok()),
        Some("acc-test-123"),
        "chatgpt-account-id header required"
    );
    assert_eq!(
        headers.get("openai-beta").and_then(|v| v.to_str().ok()),
        Some("responses=experimental"),
        "OpenAI-Beta header required"
    );
    assert_eq!(
        headers.get("originator").and_then(|v| v.to_str().ok()),
        Some("codex_cli_rs"),
        "originator header required"
    );
    let upstream_body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(upstream_body["model"], "gpt-5.1-codex");

    // ── Scenario 2: expired token → proactive JSON refresh → new bearer ───
    let new_id_token = make_id_token(7200);
    let new_exp = {
        let payload_b64 = new_id_token.split('.').nth(1).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload_b64)
                .unwrap(),
        )
        .unwrap();
        payload["exp"].as_i64().unwrap()
    };
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_json(serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
            "refresh_token": "stale-refresh-token"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": new_id_token,
            "access_token": "refreshed-access-token",
            "refresh_token": "rotated-refresh-token"
        })))
        .expect(1)
        .mount(&idp)
        .await;

    let server2 = MockServer::start().await;
    mount_responses_sse(&server2).await;
    seed_codex_provider(&pool, &server2.uri(), "codex2", "gpt-5-codex").await;
    seed_codex_account(&pool, "codex2", "stale-access-token", "stale-refresh-token", -60).await;

    let (status, body) = post_responses(&addr, "gpt-5-codex").await;
    assert_eq!(status, 200);
    assert_eq!(body, RESPONSES_SSE);

    let received = server2.received_requests().await.unwrap();
    assert_eq!(
        received[0]
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some("Bearer refreshed-access-token"),
        "the refreshed token must be used for the upstream call"
    );

    // The refresh grant hit the mock IdP (expect(1) above enforces the count).
    // Persisted credentials must be rotated in the DB.
    let row: (Vec<u8>, Vec<u8>, String) = sqlx::query_as(
        "SELECT encrypted_access_token, access_nonce, expires_at FROM codex_accounts WHERE provider_id = 'codex2'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&row.1);
    let stored_access = decrypt_api_key(&row.0, &nonce).unwrap();
    assert_eq!(stored_access, "refreshed-access-token");
    assert_eq!(row.2, new_exp.to_string());

    // ── Scenario 3: permanent refresh failure → account marked expired ────
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_json(serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
            "refresh_token": "dead-refresh-token"
        })))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"error": "invalid_grant"})),
        )
        .mount(&idp)
        .await;

    let server3 = MockServer::start().await;
    mount_responses_sse(&server3).await;
    seed_codex_provider(&pool, &server3.uri(), "codex3", "gpt-5").await;
    seed_codex_account(&pool, "codex3", "stale-access-token", "dead-refresh-token", -60).await;

    let (status, _) = post_responses(&addr, "gpt-5").await;
    assert_eq!(status, 502, "permanent refresh failure must surface an error");

    let status: String = sqlx::query_scalar("SELECT status FROM codex_accounts WHERE provider_id = 'codex3'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "expired", "account must be marked expired");

    // ── Scenario 4: codex provider with no account → clear error ──────────
    let server4 = MockServer::start().await;
    mount_responses_sse(&server4).await;
    seed_codex_provider(&pool, &server4.uri(), "codex4", "gpt-5.1").await;

    let (status, body) = post_responses(&addr, "gpt-5.1").await;
    assert_eq!(status, 502);
    assert!(
        body.contains("尚未登录") || body.contains("登录"),
        "error should tell the user to log in first: {}",
        body
    );

    // ── Scenario 5: refresh error classification (permanent vs transient) ─
    // Distinct refresh tokens route to per-outcome mocks on the same IdP.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_json(serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
            "refresh_token": "classify-permanent"
        })))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"error": "invalid_grant"})),
        )
        .mount(&idp)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_json(serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
            "refresh_token": "classify-transient"
        })))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&idp)
        .await;

    match oauth::refresh_tokens("classify-permanent").await {
        Err(oauth::RefreshError::Permanent(msg)) => {
            assert!(msg.contains("invalid_grant"), "got: {}", msg);
        }
        other => panic!("expected Permanent, got {:?}", other.map(|_| ())),
    }
    match oauth::refresh_tokens("classify-transient").await {
        Err(oauth::RefreshError::Transient(_)) => {}
        other => panic!("expected Transient, got {:?}", other.map(|_| ())),
    }
}
