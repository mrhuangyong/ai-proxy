//! Integration tests for the protocol passthrough fast-path (migration 028).
//!
//! When a provider speaks the client's protocol natively (provider_protocols
//! row), the proxy must forward the request body as-is and relay the response
//! byte-for-byte — no IR round-trip. These tests pin that behaviour end to
//! end: upstream-observed request body, downstream-observed SSE bytes, the
//! model-alias rewrite, the global kill-switch, and the is_passthrough log.
//!
//! All scenarios run inside ONE #[tokio::test]: the app uses a process-global
//! DB pool (OnceLock), so a single test binary may only initialize it once.

use ai_proxy_lib::key::store::encrypt_api_key;
use ai_proxy_lib::server::router::create_router;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Anthropic SSE fixture including a NON-STANDARD custom event — the passthrough
/// relay must keep it byte-for-byte, while a regenerated stream would drop it.
const ANTHROPIC_SSE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-x\",\"content\":[],\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
    "event: custom_vendor_event\n",
    "data: {\"vendor_extension\":true}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

async fn mount_anthropic_sse(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(ANTHROPIC_SSE.as_bytes().to_vec(), "text/event-stream"),
        )
        .mount(server)
        .await;
}

async fn seed_anthropic_provider(
    pool: &SqlitePool,
    upstream_uri: &str,
    pid: &str,
    model_name: &str,
    target_model: &str,
) {
    sqlx::query(&format!(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('{pid}','upstream-{pid}','{uri}','anthropic',NULL,'',1)",
        pid = pid,
        uri = upstream_uri,
    ))
    .execute(pool)
    .await
    .unwrap();
    // The provider speaks Anthropic natively → client /v1/messages requests
    // are eligible for passthrough.
    sqlx::query(&format!(
        "INSERT INTO provider_protocols (id, provider_id, format, base_url, endpoint_path, is_primary) VALUES ('pp-{pid}','{pid}','anthropic',NULL,NULL,1)",
        pid = pid,
    ))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m-{pid}','{pid}','{model}','{target}',1,128000)",
        pid = pid,
        model = model_name,
        target = target_model,
    ))
    .execute(pool)
    .await
    .unwrap();

    let (encrypted, nonce) = encrypt_api_key("sk-test-key").unwrap();
    sqlx::query(&format!(
        "INSERT INTO api_keys (id, provider_id, label, encrypted_key, nonce, is_active) VALUES ('k-{pid}','{pid}','test',?1,?2,1)",
        pid = pid,
    ))
    .bind(&encrypted)
    .bind(nonce.to_vec())
    .execute(pool)
    .await
    .unwrap();
}

async fn post_anthropic_stream(
    addr: &std::net::SocketAddr,
    model: &str,
) -> (reqwest::StatusCode, String) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/messages", addr))
        .header("x-api-key", "any")
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": model,
            "max_tokens": 128,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.text().await.unwrap())
}

async fn last_log_row(pool: &SqlitePool) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT is_passthrough, prompt_tokens, completion_tokens, status_code FROM request_logs ORDER BY rowid DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn passthrough_end_to_end() {
    // Global DB init (runs migrations incl. 028) — once per process.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    ai_proxy_lib::db::init::init_db(db_path.to_str().unwrap())
        .await
        .unwrap();
    let pool = SqlitePool::connect(&format!("sqlite:{}", db_path.to_str().unwrap()))
        .await
        .unwrap();

    // Serve the proxy router on an ephemeral port.
    let router = create_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // ── Scenario 1: body + SSE relayed byte-for-byte ──────────────────────
    let server1 = MockServer::start().await;
    mount_anthropic_sse(&server1).await;
    seed_anthropic_provider(&pool, &server1.uri(), "passth1", "claude-x", "claude-x").await;

    // Non-standard top-level field: must survive the passthrough untouched
    // (the IR round-trip would demote it into an `extra` bucket).
    let request_body = serde_json::json!({
        "model": "claude-x",
        "max_tokens": 128,
        "stream": true,
        "metadata": { "user_id": "u-123" },
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/messages", addr))
        .header("x-api-key", "any")
        .header("anthropic-version", "2023-06-01")
        .json(&request_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // Response relayed byte-for-byte, custom vendor event included.
    assert_eq!(body, ANTHROPIC_SSE, "passthrough must relay SSE untouched");

    // Request reached the upstream on the anthropic path, as-is.
    let received = server1.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    assert!(received[0].url.path().ends_with("/v1/messages"));
    let upstream_body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(
        upstream_body, request_body,
        "request body must be forwarded as-is"
    );

    // ── Scenario 2: model alias rewritten at the JSON level ───────────────
    let server2 = MockServer::start().await;
    mount_anthropic_sse(&server2).await;
    seed_anthropic_provider(
        &pool,
        &server2.uri(),
        "passth2",
        "my-alias",
        "real-upstream-model",
    )
    .await;

    let (status, _) = post_anthropic_stream(&addr, "my-alias").await;
    assert_eq!(status, 200);
    let received = server2.received_requests().await.unwrap();
    let upstream_body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(upstream_body["model"], "real-upstream-model");

    // ── Scenario 3: usage extracted + is_passthrough logged ───────────────
    let server3 = MockServer::start().await;
    mount_anthropic_sse(&server3).await;
    seed_anthropic_provider(&pool, &server3.uri(), "passth3", "claude-x3", "claude-x3").await;

    let (status, _) = post_anthropic_stream(&addr, "claude-x3").await;
    assert_eq!(status, 200);
    // The stream-guard logs asynchronously; give it a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (is_pt, prompt, completion, status_code) = last_log_row(&pool).await;
    assert_eq!(is_pt, 1, "log must be marked as passthrough");
    assert_eq!(prompt, 10, "prompt tokens from message_start");
    assert_eq!(completion, 5, "completion tokens from message_delta");
    assert_eq!(status_code, 200);

    // ── Scenario 4: global kill-switch falls back to conversion ──────────
    sqlx::query("INSERT INTO settings (key, value) VALUES ('passthrough_enabled', 'false')")
        .execute(&pool)
        .await
        .unwrap();
    let server4 = MockServer::start().await;
    mount_anthropic_sse(&server4).await;
    seed_anthropic_provider(&pool, &server4.uri(), "passth4", "claude-x4", "claude-x4").await;

    let (status, body) = post_anthropic_stream(&addr, "claude-x4").await;
    assert_eq!(status, 200);
    // The regenerated stream is a proxy reconstruction: it cannot contain the
    // upstream's custom vendor event (which the raw relay preserves).
    assert!(
        !body.contains("vendor_extension"),
        "kill switch off must go through the conversion path"
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (is_pt, _, _, _) = last_log_row(&pool).await;
    assert_eq!(
        is_pt, 0,
        "conversion-path request must not be marked passthrough"
    );
}
