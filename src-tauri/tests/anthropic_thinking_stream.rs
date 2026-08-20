//! Regression test: reasoning-first streaming to an Anthropic client.
//!
//! Bug: when proxying a Completions upstream that emits `reasoning_content`
//! before any text (DeepSeek R1-style), the proxy's Anthropic stream path
//! opened a thinking `content_block_start` BEFORE ever emitting
//! `message_start` — the Anthropic SDK state machine rejects that with
//! "Unexpected event order, got content_block_start before message_start".
//!
//! The stream-start gate must treat `delta_thinking` as real content.

use ai_proxy_lib::key::store::encrypt_api_key;
use ai_proxy_lib::server::router::create_router;
use sqlx::SqlitePool;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Initialize the global DB pool (runs migrations) and seed one provider
/// (Completions format) + model + encrypted API key pointing at `upstream`.
async fn setup_global(upstream_uri: &str) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let path = db_path.to_str().unwrap();
    ai_proxy_lib::db::init::init_db(path).await.unwrap();
    let pool = SqlitePool::connect(&format!("sqlite:{}", path))
        .await
        .unwrap();

    sqlx::query(&format!(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent, enabled) VALUES ('p1','upstream','{}','completions','/v1/chat/completions','',1)",
        upstream_uri
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (id, provider_id, model_name, target_model, enabled, context_window) VALUES ('m1','p1','deepseek-r1','deepseek-r1',1,128000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (encrypted, nonce) = encrypt_api_key("sk-test-key").unwrap();
    sqlx::query("INSERT INTO api_keys (id, provider_id, label, encrypted_key, nonce, is_active) VALUES ('k1','p1','test',?1,?2,1)")
        .bind(&encrypted)
        .bind(nonce.to_vec())
        .execute(&pool)
        .await
        .unwrap();
}

/// Collect the ordered list of SSE `event:` names from a raw response body.
fn sse_event_names(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|l| l.strip_prefix("event: ").map(|e| e.trim().to_string()))
        .collect()
}

#[tokio::test]
async fn anthropic_stream_with_leading_reasoning_emits_message_start_first() {
    // Upstream emits reasoning_content BEFORE any text content.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me think\"}}]}\n\n\
              data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
              data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
              data: [DONE]\n\n"
                    .to_vec(),
                "text/event-stream",
            ),
        )
        .mount(&server)
        .await;

    setup_global(&server.uri()).await;

    // Serve the proxy router on an ephemeral port.
    let router = create_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/messages", addr))
        .header("x-api-key", "any")
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "deepseek-r1",
            "max_tokens": 1024,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |v| v.starts_with("text/event-stream")));

    let body = resp.text().await.unwrap();

    // THE regression assertion: the very first SSE event must be message_start.
    let events = sse_event_names(&body);
    assert!(!events.is_empty(), "no SSE events received, body: {body}");
    assert_eq!(
        events.first().unwrap(),
        "message_start",
        "first SSE event must be message_start, got order: {:?}\nbody: {}",
        events,
        body
    );

    // Full expected lifecycle: thinking block, then text block, then close
    // blocks (thinking first, then text), then message_delta + message_stop.
    assert_eq!(
        events,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ],
        "unexpected event sequence, body: {}",
        body
    );

    // Sanity: block types come through in the right order.
    assert!(body.contains("\"type\":\"thinking\""));
    assert!(body.contains("\"thinking\":\"Let me think\""));
    assert!(body.contains("\"text\":\"Hello\""));
    assert!(body.contains("\"stop_reason\":\"end_turn\""));
}
