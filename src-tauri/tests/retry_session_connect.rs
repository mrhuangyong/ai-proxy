use ai_proxy_lib::converter::ir::ClientFormat;
use ai_proxy_lib::server::retry_invisible::RetryMode;
use ai_proxy_lib::server::retry_session::{run_upstream_session, RetryConfig};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg() -> RetryConfig {
    RetryConfig {
        max_attempts: 3,
        backoff_base_ms: 1, // tests run fast
        total_timeout: Duration::from_secs(30),
        mode: RetryMode::PreFirstToken,
        buffer_limit_bytes: 1024 * 1024,
    }
}

#[tokio::test]
async fn connect_503_then_200_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream busy"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", server.uri());
    let factory = move |_decrypted_key: &str| {
        let c = client.clone();
        let u = url.clone();
        async move { c.post(&u).body("{}").header("content-type", "application/json") }
    };

    let outcome = run_upstream_session(
        factory,
        vec!["test-key".to_string()],
        cfg(),
        ClientFormat::Anthropic,
    )
    .await;

    match outcome {
        ai_proxy_lib::server::retry_session::SessionOutcome::StartedStreaming {
            retry_count, ..
        } => {
            assert_eq!(retry_count, 2);
        }
        other => panic!("expected StartedStreaming, got {:?}", other),
    }
}

#[tokio::test]
async fn pre_first_token_interruption_retries_invisibly() {
    let server = wiremock::MockServer::start().await;

    // Attempt 1: stream opens, emits only meta events, then abruptly closes
    // WITHOUT a content_block_delta. pre_first_token mode should retry.
    wiremock::Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
                "text/event-stream",
            ),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Attempt 2: full normal stream
    wiremock::Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", server.uri());
    let factory = move |_decrypted_key: &str| {
        let c = client.clone();
        let u = url.clone();
        async move { c.post(&u).body("{}").header("content-type", "application/json") }
    };

    let outcome = run_upstream_session(
        factory,
        vec!["k".into()],
        RetryConfig {
            max_attempts: 3,
            backoff_base_ms: 1,
            total_timeout: Duration::from_secs(30),
            mode: RetryMode::PreFirstToken,
            buffer_limit_bytes: 1024 * 1024,
        },
        ai_proxy_lib::converter::ir::ClientFormat::Anthropic,
    )
    .await;

    match outcome {
        ai_proxy_lib::server::retry_session::SessionOutcome::StartedStreaming {
            retry_count, buffered_bytes, ..
        } => {
            assert_eq!(retry_count, 1, "should have retried once");
            // buffered_bytes should include message_start from attempt 2
            assert!(buffered_bytes.windows(13).any(|w| w == b"message_start"));
        }
        other => panic!("expected StartedStreaming, got {:?}", other),
    }
}

#[tokio::test]
async fn full_buffer_collects_complete_stream() {
    let server = wiremock::MockServer::start().await;
    let body = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\ndata: [DONE]\n\n";
    wiremock::Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_vec(), "text/event-stream"))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", server.uri());
    let factory = move |_decrypted_key: &str| {
        let c = client.clone();
        let u = url.clone();
        async move { c.post(&u).body("{}").header("content-type", "application/json") }
    };

    let outcome = run_upstream_session(
        factory,
        vec!["k".into()],
        RetryConfig {
            max_attempts: 3,
            backoff_base_ms: 1,
            total_timeout: Duration::from_secs(30),
            mode: RetryMode::FullBuffer,
            buffer_limit_bytes: 1024 * 1024,
        },
        ai_proxy_lib::converter::ir::ClientFormat::Anthropic,
    )
    .await;

    match outcome {
        ai_proxy_lib::server::retry_session::SessionOutcome::CompletedBuffer { bytes, retry_count, .. } => {
            assert_eq!(retry_count, 0);
            assert!(bytes.windows(6).any(|w| w == b"[DONE]"));
        }
        other => panic!("expected CompletedBuffer, got {:?}", other),
    }
}

#[tokio::test]
async fn full_buffer_retries_on_midstream_interrupt() {
    let server = wiremock::MockServer::start().await;

    // Attempt 1: starts streaming, emits a business chunk, then abruptly ends.
    wiremock::Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
            "text/event-stream",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Attempt 2: complete stream
    wiremock::Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", server.uri());
    let factory = move |_decrypted_key: &str| {
        let c = client.clone();
        let u = url.clone();
        async move { c.post(&u).body("{}").header("content-type", "application/json") }
    };

    let outcome = run_upstream_session(
        factory,
        vec!["k".into()],
        RetryConfig {
            max_attempts: 3,
            backoff_base_ms: 1,
            total_timeout: Duration::from_secs(30),
            mode: RetryMode::FullBuffer,
            buffer_limit_bytes: 1024 * 1024,
        },
        ai_proxy_lib::converter::ir::ClientFormat::Anthropic,
    )
    .await;

    match outcome {
        ai_proxy_lib::server::retry_session::SessionOutcome::CompletedBuffer {
            bytes, retry_count, ..
        } => {
            assert_eq!(retry_count, 1);
            assert!(bytes.windows(6).any(|w| w == b"[DONE]"));
        }
        other => panic!("expected CompletedBuffer, got {:?}", other),
    }
}
