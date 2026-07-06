//! Regression tests for HTTP 200 + retryable business error in body.
//!
//! Some upstream providers (e.g. iflytek/Spark) return HTTP 200 with a JSON
//! error body instead of a proper 429/5xx status. The session must detect
//! such bodies (via `classify_body_error`) and retry — masking the error from
//! the downstream client until retries are exhausted.

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

/// Non-streaming: iflytek-style 200 + {"error":{"code":10310,...}} must be
/// retried on every attempt and ultimately surface as Exhausted (not handed
/// to the downstream client as a "successful" body).
#[tokio::test]
async fn non_stream_200_business_error_is_retried_until_exhausted() {
    let server = MockServer::start().await;
    let body = r#"{"error":{"code":10310,"message":"The system is busy, please try again later.","type":"api_error"},"id":"cht000d30ab","type":"error"}"#;
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
    let factory = move |_key: &str| {
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
        false, // non-stream
    )
    .await;

    match outcome {
        SessionOutcome::Exhausted {
            retry_count,
            last_error,
            ..
        } => {
            assert!(retry_count >= 1, "must retry at least once, got {retry_count}");
            assert!(
                last_error.contains("10310") || last_error.contains("business error"),
                "last_error should mention the business error, got: {last_error}"
            );
        }
        other => panic!("expected Exhausted, got {:?}", other),
    }
}

/// Non-streaming: a 200 body error followed by a real success must succeed
/// on retry — proving the retry is actually re-issuing the request.
#[tokio::test]
async fn non_stream_200_business_error_then_succeeds() {
    let server = MockServer::start().await;
    let err_body = r#"{"error":{"code":10310,"message":"system is busy, please try again later"}}"#;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(err_body.to_string())
                .insert_header("content-type", "application/json"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let ok_body = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Hi"}],"model":"claude-x","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(ok_body.to_string())
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", server.uri());
    let factory = move |_key: &str| {
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
            retry_count, bytes, ..
        } => {
            assert_eq!(retry_count, 1, "should retry exactly once then succeed");
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains("Hi"), "final body should be the success response");
        }
        other => panic!("expected CompletedBuffer, got {:?}", other),
    }
}

/// Streaming: a 200 SSE stream whose only event is an error must be retried
/// (detected while still in the invisible buffer state, before any business
/// chunk is forwarded downstream).
#[tokio::test]
async fn stream_200_sse_error_event_is_retried() {
    let server = MockServer::start().await;
    // Anthropic-style error SSE event, then stream ends.
    let sse = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse.to_string())
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", server.uri());
    let factory = move |_key: &str| {
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
        true, // stream
    )
    .await;

    match outcome {
        SessionOutcome::Exhausted { retry_count, .. } => {
            assert!(retry_count >= 1, "stream business error must be retried");
        }
        other => panic!("expected Exhausted, got {:?}", other),
    }
}
