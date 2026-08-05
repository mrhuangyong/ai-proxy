//! Non-streaming (plain JSON) upstream response handling.
//!
//! These tests guard against a regression where `run_upstream_session` parsed
//! non-SSE bodies through the SSE buffering loop, which could never detect a
//! completion signal and retried until exhaustion. Non-streaming responses
//! must now be read in full on success and returned as `CompletedBuffer`.

use ai_proxy_lib::converter::ir::ClientFormat;
use ai_proxy_lib::server::retry_invisible::RetryMode;
use ai_proxy_lib::server::retry_session::{run_upstream_session, RetryConfig, SessionOutcome};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg() -> RetryConfig {
    RetryConfig {
        max_attempts: 3,
        backoff_base_ms: 1,
        total_timeout: Duration::from_secs(30),
        mode: RetryMode::PreFirstToken,
        buffer_limit_bytes: 1024 * 1024,
    }
}

/// A non-streaming upstream that returns a plain JSON completion body must
/// succeed on the first attempt (retry_count == 0) with the full body intact.
#[tokio::test]
async fn non_stream_plain_json_returns_completed_buffer() {
    let server = MockServer::start().await;
    // A typical OpenAI-style chat completion JSON (NOT SSE).
    let body = r#"{"id":"chatcmpl-1","object":"chat.completion","model":"gpt-x","choices":[{"index":0,"message":{"role":"assistant","content":"Hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_string())
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", server.uri());
    let factory = move |_decrypted_key: &str| {
        let c = client.clone();
        let u = url.clone();
        async move {
            c.post(&u)
                .body("{}")
                .header("content-type", "application/json")
        }
    };

    let outcome = run_upstream_session(
        factory,
        vec!["k".into()],
        cfg(),
        ClientFormat::Completions,
        false,
    )
    .await;

    match outcome {
        SessionOutcome::CompletedBuffer {
            status,
            bytes,
            retry_count,
        } => {
            assert_eq!(status, 200);
            assert_eq!(
                retry_count, 0,
                "must not retry a successful non-stream body"
            );
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains("Hello"));
            // Sanity: body must NOT be the SSE-style completion marker path,
            // and must be valid JSON.
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["choices"][0]["message"]["content"], "Hello");
        }
        other => panic!("expected CompletedBuffer, got {:?}", other),
    }
}

/// Connection-level retry still applies to non-streaming requests: a 503
/// followed by a 200 JSON body must ultimately succeed with retry_count == 1.
#[tokio::test]
async fn non_stream_retries_5xx_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let body = r#"{"id":"chatcmpl-1","object":"chat.completion","model":"gpt-x","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_string())
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/chat/completions", server.uri());
    let factory = move |_decrypted_key: &str| {
        let c = client.clone();
        let u = url.clone();
        async move {
            c.post(&u)
                .body("{}")
                .header("content-type", "application/json")
        }
    };

    let outcome = run_upstream_session(
        factory,
        vec!["k".into()],
        cfg(),
        ClientFormat::Completions,
        false,
    )
    .await;

    match outcome {
        SessionOutcome::CompletedBuffer {
            retry_count, bytes, ..
        } => {
            assert_eq!(retry_count, 1, "should have retried once after 503");
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains("ok"));
        }
        other => panic!("expected CompletedBuffer, got {:?}", other),
    }
}

/// Non-streaming responses carry the same JSON body for Anthropic format
/// (message with content blocks). Guards that the short-circuit doesn't
/// accidentally require an SSE-shaped body.
#[tokio::test]
async fn non_stream_anthropic_json_returns_completed_buffer() {
    let server = MockServer::start().await;
    let body = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Hi there"}],"model":"claude-x","stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body.to_string())
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", server.uri());
    let factory = move |_decrypted_key: &str| {
        let c = client.clone();
        let u = url.clone();
        async move {
            c.post(&u)
                .body("{}")
                .header("content-type", "application/json")
        }
    };

    let outcome = run_upstream_session(
        factory,
        vec!["k".into()],
        cfg(),
        ClientFormat::Anthropic,
        false,
    )
    .await;

    match outcome {
        SessionOutcome::CompletedBuffer {
            status,
            bytes,
            retry_count,
        } => {
            assert_eq!(status, 200);
            assert_eq!(retry_count, 0);
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains("Hi there"));
        }
        other => panic!("expected CompletedBuffer, got {:?}", other),
    }
}

/// Regression: the proxy must send exactly ONE `Content-Type` header upstream.
/// `reqwest::RequestBuilder::json()` already sets it; a redundant
/// `.header("Content-Type", "application/json")` APPENDS a second value, which
/// strict upstreams (e.g. opencode-go) reject with 415 "Unsupported
/// content-type: application/json, application/json". This mirrors the exact
/// builder chain used in `handlers.rs` / `api.rs`.
#[tokio::test]
async fn upstream_request_has_single_content_type() {
    let body = serde_json::json!({
        "model": "deepseek-v4-flash",
        "temperature": 0.8,
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}],
    });
    let client = reqwest::Client::new();
    let req = client
        .post("https://example.com/v1/chat/completions")
        .json(&body)
        .build()
        .unwrap();

    let cts: Vec<_> = req
        .headers()
        .get_all("content-type")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert_eq!(
        cts,
        vec!["application/json"],
        "must send a single Content-Type header, got {cts:?}"
    );
}
