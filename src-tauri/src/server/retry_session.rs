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
use crate::server::retry_invisible::{
    classify_body_error, compute_backoff_ms, is_first_business_chunk, should_retry, BufferState,
    ErrKind, RetryMode,
};

/// Check whether the buffered SSE bytes contain a proper stream termination signal
/// (e.g. `[DONE]` for OpenAI/Responses, `message_stop` for Anthropic, or Gemini finish).
fn is_stream_complete(format: &ClientFormat, buffered: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buffered);
    // [DONE] is a universal SSE termination marker used across all formats
    if text.contains("[DONE]") {
        return true;
    }
    match format {
        ClientFormat::Completions | ClientFormat::Responses => false, // [DONE] already checked
        ClientFormat::Anthropic => text.contains("\"message_stop\""),
        ClientFormat::Gemini => text.contains("\"finishReason\"") || text.contains("finishReason"),
    }
}

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
            Self::CompletedBuffer {
                status,
                bytes,
                retry_count,
            } => f
                .debug_struct("CompletedBuffer")
                .field("status", status)
                .field("bytes_len", &bytes.len())
                .field("retry_count", retry_count)
                .finish(),
            Self::StartedStreaming {
                status,
                buffered_bytes,
                remaining: _,
                retry_count,
            } => f
                .debug_struct("StartedStreaming")
                .field("status", status)
                .field("buffered_bytes_len", &buffered_bytes.len())
                .field("remaining", &"<BoxStream>")
                .field("retry_count", retry_count)
                .finish(),
            Self::Exhausted {
                last_status,
                last_error,
                retry_count,
                partial_buffer,
            } => f
                .debug_struct("Exhausted")
                .field("last_status", last_status)
                .field("last_error", last_error)
                .field("retry_count", retry_count)
                .field(
                    "partial_buffer_len",
                    &partial_buffer.as_ref().map(|b| b.len()),
                )
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
/// `client_format`: client format, used for first-business-chunk detection.
/// `is_stream`: whether the upstream response is an SSE stream. When false,
/// the response body is read in full on success and returned as
/// `CompletedBuffer` without the SSE buffering loop — mid-body invisible
/// retry is impossible for non-streaming responses.
pub async fn run_upstream_session<F, Fut>(
    mut req_factory: F,
    decrypted_keys: Vec<String>,
    config: RetryConfig,
    client_format: ClientFormat,
    is_stream: bool,
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
                last_error: format!("total timeout {}s exceeded", config.total_timeout.as_secs()),
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

        // 200 OK — enter buffer phase.
        //
        // Non-streaming: upstream returns a plain JSON body (not SSE). The
        // SSE buffering loop below cannot detect completion for such a body
        // (is_first_business_chunk relies on `data:` prefixes), so we read
        // the full body here on success and return it as CompletedBuffer.
        // Read failures are treated as connection-level errors and retried.
        if !is_stream {
            let bytes_result = tokio::time::timeout(
                config.total_timeout.max(Duration::from_secs(1)),
                resp.bytes(),
            )
            .await;
            match bytes_result {
                Ok(Ok(b)) => {
                    // Some providers (e.g. iflytek) return HTTP 200 with a retryable
                    // business error embedded in the JSON body. Detect and retry
                    // before handing the body to the downstream client.
                    if let Some(err) = classify_body_error(&b, &client_format) {
                        last_status = Some(status);
                        last_error = format!("business error in 200 body: {}", err.message);
                        tracing::warn!(
                            "[retry] non-stream 200 body error, retrying: {}",
                            err.message
                        );
                        let wait = compute_backoff_ms(attempt, config.backoff_base_ms, None);
                        sleep(Duration::from_millis(wait)).await;
                        attempt += 1;
                        continue;
                    }
                    return SessionOutcome::CompletedBuffer {
                        status,
                        bytes: b.to_vec(),
                        retry_count: attempt,
                    };
                }
                Ok(Err(e)) => {
                    last_status = Some(status);
                    last_error = format!("non-stream body read: {}", e);
                    let state = current_buffer_state(config.mode, false);
                    if !should_retry(None, Some(ErrKind::Network), state) {
                        return SessionOutcome::Exhausted {
                            last_status,
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
                Err(_) => {
                    last_status = Some(status);
                    last_error = "non-stream body read timeout".into();
                    let state = current_buffer_state(config.mode, false);
                    if !should_retry(None, Some(ErrKind::Network), state) {
                        return SessionOutcome::Exhausted {
                            last_status,
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
            }
        }

        // Streaming path.
        // Read SSE chunks until either:
        //   (a) we see a first-business-chunk -> transition to StartedStreaming
        //   (b) full_buffer mode and stream ends -> CompletedBuffer
        //   (c) stream errors / stalls mid-buffer -> retry
        //   (d) buffer hits size cap -> Exhausted with partial
        let mut buffered: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut buffered_lines: Vec<String> = Vec::new(); // for is_first_business_chunk inspection
        let mut line_buf: Vec<u8> = Vec::new();
        let mut hit_first_business = false;

        loop {
            let chunk_opt = tokio::time::timeout(Duration::from_secs(600), stream.next()).await;
            let chunk = match chunk_opt {
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(_e))) => {
                    // mid-stream error — retry only if still in buffer state
                    last_status = Some(status);
                    last_error = "upstream stream error".into();
                    let wait = compute_backoff_ms(attempt, config.backoff_base_ms, None);
                    sleep(Duration::from_millis(wait)).await;
                    attempt += 1;
                    break; // outer loop continues
                }
                Ok(None) => {
                    // stream ended
                    if config.mode == RetryMode::FullBuffer {
                        if is_stream_complete(&client_format, &buffered) {
                            return SessionOutcome::CompletedBuffer {
                                status,
                                bytes: std::mem::take(&mut buffered),
                                retry_count: attempt,
                            };
                        }
                        // full_buffer: stream ended without completion signal — retry
                        last_status = Some(status);
                        last_error = "full_buffer stream ended without completion signal".into();
                        let wait = compute_backoff_ms(attempt, config.backoff_base_ms, None);
                        sleep(Duration::from_millis(wait)).await;
                        attempt += 1;
                        break;
                    }
                    // pre_first_token: did we ever see a business chunk?
                    if hit_first_business {
                        // impossible: would have transitioned already
                        return SessionOutcome::CompletedBuffer {
                            status,
                            bytes: std::mem::take(&mut buffered),
                            retry_count: attempt,
                        };
                    }
                    // stream ended without any business chunk — suspicious interruption
                    last_status = Some(status);
                    last_error = "stream ended before any business chunk".into();
                    let wait = compute_backoff_ms(attempt, config.backoff_base_ms, None);
                    sleep(Duration::from_millis(wait)).await;
                    attempt += 1;
                    break;
                }
                Err(_) => {
                    // stall timeout — same handling as stream error
                    last_status = Some(status);
                    last_error = "upstream stall".into();
                    let wait = compute_backoff_ms(attempt, config.backoff_base_ms, None);
                    sleep(Duration::from_millis(wait)).await;
                    attempt += 1;
                    break;
                }
            };

            // append to buffer
            if buffered.len() + chunk.len() > config.buffer_limit_bytes {
                // buffer cap exceeded
                return SessionOutcome::Exhausted {
                    last_status: Some(status),
                    last_error: format!("buffer cap {} bytes exceeded", config.buffer_limit_bytes),
                    retry_count: attempt,
                    partial_buffer: Some(buffered),
                };
            }
            buffered.extend_from_slice(&chunk);

            // Before treating any chunk as "first business", check whether the
            // upstream embedded a retryable business error in the SSE buffer
            // (providers like iflytek return HTTP 200 + an error event). If so,
            // retry while we are still in the invisible buffer state.
            if !hit_first_business {
                if let Some(err) = classify_body_error(&buffered, &client_format) {
                    last_status = Some(status);
                    last_error = format!("business error in stream: {}", err.message);
                    tracing::warn!(
                        "[retry] stream business error in buffer, retrying: {}",
                        err.message
                    );
                    let wait = compute_backoff_ms(attempt, config.backoff_base_ms, None);
                    sleep(Duration::from_millis(wait)).await;
                    attempt += 1;
                    break; // outer loop continues with next attempt
                }
            }

            // scan chunk for newlines, build complete lines, test for first business
            line_buf.extend_from_slice(&chunk);
            while let Some(nl) = line_buf.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = line_buf[..nl].to_vec();
                line_buf = line_buf[nl + 1..].to_vec();
                let line = String::from_utf8_lossy(&line_bytes).into_owned();
                // accumulate multi-line SSE event for inspection
                if !line.is_empty() {
                    buffered_lines.push(line.clone());
                } else {
                    // empty line == event boundary. Inspect accumulated event.
                    if !buffered_lines.is_empty() {
                        let event_text = buffered_lines.join("\n");
                        if is_first_business_chunk(&client_format, &event_text) {
                            hit_first_business = true;
                            if config.mode == RetryMode::PreFirstToken {
                                // transition: yield remaining stream
                                return SessionOutcome::StartedStreaming {
                                    status,
                                    buffered_bytes: std::mem::take(&mut buffered),
                                    remaining: stream.boxed(),
                                    retry_count: attempt,
                                };
                            }
                        }
                        buffered_lines.clear();
                    }
                }
            }
        }
        // outer continue — next attempt
    }
}
