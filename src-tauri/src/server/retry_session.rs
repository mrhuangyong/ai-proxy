//! Upstream session loop: connect → buffer → decide → retry or yield.
//!
//! The session is responsible for deciding WHEN to retry mid-stream while
//! remaining invisible to the downstream client. Handlers consume the
//! `SessionOutcome` and decide how to translate it to the wire format.

use std::time::Duration;

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::time::sleep;

use crate::converter::ir::ClientFormat;
use crate::server::retry_invisible::{compute_backoff_ms, should_retry, BufferState, ErrKind, RetryMode};

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub backoff_base_ms: u64,
    pub total_timeout: Duration,
    pub mode: RetryMode,
    pub buffer_limit_bytes: usize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            backoff_base_ms: 500,
            total_timeout: Duration::from_secs(600),
            mode: RetryMode::PreFirstToken,
            buffer_limit_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Outcome of an upstream session.
pub enum SessionOutcome {
    /// full_buffer mode completed: we have the full SSE byte stream in memory.
    CompletedBuffer {
        status: reqwest::StatusCode,
        bytes: Vec<u8>,
        retry_count: u32,
    },
    /// pre_first_token mode: first business chunk arrived. Caller must forward
    /// `buffered_bytes` first, then continue reading `remaining`.
    StartedStreaming {
        status: reqwest::StatusCode,
        buffered_bytes: Vec<u8>,
        remaining: BoxStream<'static, Result<Bytes, reqwest::Error>>,
        retry_count: u32,
    },
    /// All retries exhausted (or total timeout hit).
    Exhausted {
        last_status: Option<reqwest::StatusCode>,
        last_error: String,
        retry_count: u32,
        /// partial buffer if we accumulated bytes in full_buffer mode but gave up
        partial_buffer: Option<Vec<u8>>,
    },
    /// Non-retryable HTTP error (4xx except 429)
    Fatal {
        status: reqwest::StatusCode,
        body: String,
    },
}

impl std::fmt::Debug for SessionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompletedBuffer { status, bytes, retry_count } => f
                .debug_struct("CompletedBuffer")
                .field("status", status)
                .field("bytes_len", &bytes.len())
                .field("retry_count", retry_count)
                .finish(),
            Self::StartedStreaming { status, buffered_bytes, remaining: _, retry_count } => f
                .debug_struct("StartedStreaming")
                .field("status", status)
                .field("buffered_bytes_len", &buffered_bytes.len())
                .field("remaining", &"<BoxStream>")
                .field("retry_count", retry_count)
                .finish(),
            Self::Exhausted { last_status, last_error, retry_count, partial_buffer } => f
                .debug_struct("Exhausted")
                .field("last_status", last_status)
                .field("last_error", last_error)
                .field("retry_count", retry_count)
                .field("partial_buffer_len", &partial_buffer.as_ref().map(|b| b.len()))
                .finish(),
            Self::Fatal { status, body } => f
                .debug_struct("Fatal")
                .field("status", status)
                .field("body", body)
                .finish(),
        }
    }
}

/// Currently unused; reserved for future cancellation-token based abort.
#[allow(dead_code)]
pub fn current_buffer_state(mode: RetryMode, transparent: bool) -> BufferState {
    if transparent {
        return BufferState::Transparent;
    }
    match mode {
        RetryMode::PreFirstToken => BufferState::PreFirstToken,
        RetryMode::FullBuffer => BufferState::FullBuffer,
    }
}

/// Load RetryConfig from the settings table. Falls back to defaults.
pub async fn load_config_from_db(format: ClientFormat) -> RetryConfig {
    let pool_ref = crate::db::pool::get_pool().await;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM settings WHERE key IN ('upstream_max_retries', 'upstream_retry_backoff_base_ms', 'upstream_invisible_retry_mode', 'upstream_invisible_retry_total_timeout_secs', 'upstream_invisible_retry_buffer_limit_mb')",
    )
    .fetch_all(pool_ref)
    .await
    .unwrap_or_default();
    let map: std::collections::HashMap<String, String> = rows.into_iter().collect();

    let _ = format; // currently unused, reserved
    RetryConfig {
        max_attempts: map
            .get("upstream_max_retries")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(10)
            .max(1),
        backoff_base_ms: map
            .get("upstream_retry_backoff_base_ms")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(500),
        total_timeout: Duration::from_secs(
            map.get("upstream_invisible_retry_total_timeout_secs")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(600),
        ),
        mode: RetryMode::from_str(
            map.get("upstream_invisible_retry_mode")
                .map(String::as_str)
                .unwrap_or("pre_first_token"),
        ),
        buffer_limit_bytes: (map
            .get("upstream_invisible_retry_buffer_limit_mb")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(32))
            .saturating_mul(1024 * 1024),
    }
}

/// Run an upstream session with retry.
///
/// `req_factory`: closure that takes a decrypted API key and returns a fresh
/// `RequestBuilder`. Called once per attempt — never reuse across attempts.
/// `decrypted_keys`: pool of API keys to rotate through. Each retry advances
/// to the next key (wraps around if fewer keys than attempts).
/// `format`: client format, used for first-business-chunk detection.
pub async fn run_upstream_session<F, Fut>(
    mut req_factory: F,
    decrypted_keys: Vec<String>,
    config: RetryConfig,
    _format: ClientFormat,
) -> SessionOutcome
where
    F: FnMut(&str) -> Fut,
    Fut: std::future::Future<Output = reqwest::RequestBuilder>,
{
    let start = std::time::Instant::now();
    let n_keys = decrypted_keys.len().max(1);
    let mut last_status: Option<reqwest::StatusCode> = None;
    let mut last_error = String::new();
    let mut attempt: u32 = 0;

    loop {
        if attempt >= config.max_attempts {
            return SessionOutcome::Exhausted {
                last_status,
                last_error,
                retry_count: attempt,
                partial_buffer: None,
            };
        }
        if start.elapsed() >= config.total_timeout {
            return SessionOutcome::Exhausted {
                last_status,
                last_error: format!(
                    "total timeout {}s exceeded",
                    config.total_timeout.as_secs()
                ),
                retry_count: attempt,
                partial_buffer: None,
            };
        }

        let key = &decrypted_keys[(attempt as usize) % n_keys];
        let builder = req_factory(key).await;
        let send_result = builder.send().await;

        let resp = match send_result {
            Ok(r) => r,
            Err(e) => {
                last_status = None;
                last_error = format!("network: {}", e);
                let state = current_buffer_state(config.mode, false);
                if !should_retry(None, Some(ErrKind::Network), state) {
                    return SessionOutcome::Exhausted {
                        last_status: None,
                        last_error,
                        retry_count: attempt,
                        partial_buffer: None,
                    };
                }
                let wait = compute_backoff_ms(attempt, config.backoff_base_ms, None);
                sleep(Duration::from_millis(wait)).await;
                attempt += 1;
                continue;
            }
        };

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            last_status = Some(status);
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let body = resp.bytes().await.unwrap_or_default();
            last_error = format!(
                "HTTP {}: {}",
                status.as_u16(),
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
            let wait = compute_backoff_ms(attempt, config.backoff_base_ms, retry_after_secs);
            sleep(Duration::from_millis(wait)).await;
            attempt += 1;
            continue;
        }

        if !status.is_success() {
            // 4xx (except 429): fatal
            let body = resp.bytes().await.unwrap_or_default();
            return SessionOutcome::Fatal {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            };
        }

        // 200 OK — stream phase. Task 12 will add buffer/retry logic.
        // For now, immediately return StartedStreaming with empty buffer.
        let remaining = resp.bytes_stream().boxed();
        return SessionOutcome::StartedStreaming {
            status,
            buffered_bytes: Vec::new(),
            remaining,
            retry_count: attempt,
        };
    }
}
