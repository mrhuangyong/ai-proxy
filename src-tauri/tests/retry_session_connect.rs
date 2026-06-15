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
