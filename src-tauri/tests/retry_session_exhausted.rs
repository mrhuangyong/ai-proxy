use ai_proxy_lib::converter::ir::ClientFormat;
use ai_proxy_lib::server::retry_invisible::RetryMode;
use ai_proxy_lib::server::retry_session::{run_upstream_session, RetryConfig, SessionOutcome};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn exhausted_returns_exhausted_with_last_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_string("backend on fire"))
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
            max_attempts: 2,
            backoff_base_ms: 1,
            total_timeout: Duration::from_secs(30),
            mode: RetryMode::PreFirstToken,
            buffer_limit_bytes: 1024,
        },
        ClientFormat::Anthropic,
    )
    .await;

    match outcome {
        SessionOutcome::Exhausted { retry_count, last_status, last_error, partial_buffer } => {
            assert_eq!(retry_count, 2);
            assert_eq!(last_status, Some(reqwest::StatusCode::SERVICE_UNAVAILABLE));
            assert!(last_error.contains("503"));
            assert!(last_error.contains("backend on fire"));
            assert!(partial_buffer.is_none());
        }
        other => panic!("expected Exhausted, got {:?}", other),
    }
}

#[tokio::test]
async fn total_timeout_short_circuits() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(wiremock::ResponseTemplate::new(503))
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
            max_attempts: 100,
            backoff_base_ms: 500,
            total_timeout: Duration::from_millis(100),
            mode: RetryMode::PreFirstToken,
            buffer_limit_bytes: 1024,
        },
        ClientFormat::Anthropic,
    )
    .await;

    match outcome {
        SessionOutcome::Exhausted { last_error, .. } => {
            assert!(last_error.contains("total timeout"), "got: {}", last_error);
        }
        other => panic!("expected Exhausted, got {:?}", other),
    }
}
