//! Upstream-interruption invisible retry helpers.
//!
//! Pure functions for error classification, first-token detection, and
//! backoff computation. The orchestration loop lives in `retry_session`.

use reqwest::StatusCode;
use serde_json;

use crate::converter::ir::ClientFormat;

/// Which buffer state an upstream session is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// pre_first_token mode: no business chunk emitted yet.
    PreFirstToken,
    /// full_buffer mode: accumulating until upstream stream ends.
    FullBuffer,
    /// Already emitted at least one business byte to downstream —
    /// interruption now is visible and cannot be retried.
    Transparent,
}

/// Categorized upstream error for retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrKind {
    /// reqwest::Error (connect / TLS / timeout / body read)
    Network,
    /// Stream returned Err mid-flight, or upstream stall timeout
    StreamInterrupted,
}

/// Decide whether to retry given the current state.
///
/// `status`: HTTP status if we got headers; `None` for network-level failures
/// `err_kind`: classified error kind if no HTTP status; `None` if we have status
/// `state`: current buffer state — Transparent never retries
pub fn should_retry(
    status: Option<StatusCode>,
    err_kind: Option<ErrKind>,
    state: BufferState,
) -> bool {
    if state == BufferState::Transparent {
        return false;
    }
    if let Some(s) = status {
        if s == StatusCode::TOO_MANY_REQUESTS || s.is_server_error() {
            return true;
        }
        return false;
    }
    match err_kind {
        Some(ErrKind::Network) | Some(ErrKind::StreamInterrupted) => true,
        None => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryMode {
    PreFirstToken,
    FullBuffer,
}

impl RetryMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "full_buffer" => RetryMode::FullBuffer,
            _ => RetryMode::PreFirstToken,
        }
    }
}

/// Maximum backoff multiplier cap (<< 6 = 64x).
const BACKOFF_SHIFT_CAP: u32 = 6;

/// Compute exponential backoff in milliseconds, honoring Retry-After when present.
///
/// - `attempt` is 0-indexed.
/// - `base_ms` is the unit; effective wait is `base_ms * 2^min(attempt, 6)`.
/// - If `retry_after_secs` is `Some(s)` and s > 0, returns `s * 1000` instead.
pub fn compute_backoff_ms(attempt: u32, base_ms: u64, retry_after_secs: Option<u64>) -> u64 {
    if let Some(secs) = retry_after_secs {
        if secs > 0 {
            return secs.saturating_mul(1000);
        }
    }
    let shift = attempt.min(BACKOFF_SHIFT_CAP);
    base_ms.saturating_mul(1u64 << shift)
}

/// Returns true if the SSE line is the first "business" chunk — i.e., the
/// first delta carrying actual content (text / thinking / tool call).
///
/// Lines that are SSE comments, empty, [DONE], or pure meta/handshake events
/// return false. Used to decide when pre_first_token mode transitions to
/// Transparent.
pub fn is_first_business_chunk(format: &ClientFormat, line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') {
        return false;
    }
    // Each SSE event may contain multiple lines (event:/data:). Find data lines.
    let data_payloads: Vec<&str> = trimmed
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:"))
        })
        .collect();
    if data_payloads.is_empty() {
        return false;
    }
    for raw in data_payloads {
        let raw = raw.trim();
        if raw == "[DONE]" {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if matches_first_for_format(format, &v) {
            return true;
        }
    }
    false
}

fn matches_first_for_format(format: &ClientFormat, v: &serde_json::Value) -> bool {
    match format {
        ClientFormat::Completions => {
            if let Some(delta) = v["choices"].get(0).and_then(|c| c.get("delta")) {
                // Non-empty text content counts as first business byte.
                if delta
                    .get("content")
                    .and_then(|c| c.as_str())
                    .map_or(false, |s| !s.is_empty())
                {
                    return true;
                }
                // Tool-call deltas count too: a tool_call request is real output,
                // and treating it as "not first" would buffer it forever in
                // PreFirstToken mode, hanging tool-calling streaming requests.
                if delta.get("tool_calls").is_some() {
                    return true;
                }
                // Non-empty reasoning_content (DeepSeek-style thinking) counts.
                if delta
                    .get("reasoning_content")
                    .and_then(|c| c.as_str())
                    .map_or(false, |s| !s.is_empty())
                {
                    return true;
                }
            }
            false
        }
        ClientFormat::Anthropic => {
            // content_block_delta with text_delta / input_json_delta / thinking_delta
            let t = v["type"].as_str().unwrap_or("");
            if t == "content_block_delta" {
                let dt = v["delta"]["type"].as_str().unwrap_or("");
                return matches!(dt, "text_delta" | "input_json_delta" | "thinking_delta");
            }
            // content_block_start with non-empty content also counts
            if t == "content_block_start" {
                // tool_use start counts as first business byte
                if v["content_block"]["type"].as_str() == Some("tool_use") {
                    return true;
                }
            }
            false
        }
        ClientFormat::Gemini => v["candidates"]
            .get(0)
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.get(0))
            .map_or(false, |part| {
                part.get("text").is_some() || part.get("functionCall").is_some()
            }),
        ClientFormat::Responses => {
            let t = v["type"].as_str().unwrap_or("");
            matches!(
                t,
                "response.output_text.delta" | "response.function_call_arguments.delta"
            ) && (v.get("delta").is_some() || v.get("arguments").is_some())
        }
    }
}

/// Generate a downstream-format SSE error trailer. Used when buffer cap is hit
/// mid-stream and we must close the downstream connection with an explicit error.
pub fn error_trailer_event(format: ClientFormat, message: &str) -> String {
    let safe = message.replace('"', "\\\"").replace('\n', " ");
    match format {
        ClientFormat::Anthropic => {
            format!(
                "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"overloaded_error\",\"message\":\"{}\"}}}}\n\n",
                safe
            )
        }
        ClientFormat::Completions | ClientFormat::Responses => {
            format!(
                "data: {{\"error\":{{\"message\":\"{}\",\"type\":\"server_error\"}}}}\n\n",
                safe
            )
        }
        ClientFormat::Gemini => {
            format!(": error: {}\n\n", safe)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_exponential_growth() {
        // base=500ms, no retry-after
        assert_eq!(compute_backoff_ms(0, 500, None), 500);
        assert_eq!(compute_backoff_ms(1, 500, None), 1000);
        assert_eq!(compute_backoff_ms(2, 500, None), 2000);
        assert_eq!(compute_backoff_ms(3, 500, None), 4000);
    }

    #[test]
    fn backoff_caps_at_shift_6() {
        // attempt=6 and attempt=10 should both be 500 * 64 = 32000
        assert_eq!(compute_backoff_ms(6, 500, None), 32000);
        assert_eq!(compute_backoff_ms(10, 500, None), 32000);
        assert_eq!(compute_backoff_ms(20, 500, None), 32000);
    }

    #[test]
    fn backoff_retry_after_overrides() {
        // Retry-After: 5 -> 5000ms regardless of attempt
        assert_eq!(compute_backoff_ms(0, 500, Some(5)), 5000);
        assert_eq!(compute_backoff_ms(10, 500, Some(5)), 5000);
    }

    #[test]
    fn backoff_retry_after_zero_falls_back() {
        // Retry-After: 0 should not override
        assert_eq!(compute_backoff_ms(2, 500, Some(0)), 2000);
    }

    #[test]
    fn backoff_saturates_on_overflow() {
        // Huge base shouldn't panic
        assert_eq!(compute_backoff_ms(50, u64::MAX, None), u64::MAX);
    }

    #[test]
    fn should_retry_network_error_in_buffer_state() {
        assert!(should_retry(
            None,
            Some(ErrKind::Network),
            BufferState::PreFirstToken
        ));
        assert!(should_retry(
            None,
            Some(ErrKind::Network),
            BufferState::FullBuffer
        ));
        // Already transparent: can't retry
        assert!(!should_retry(
            None,
            Some(ErrKind::Network),
            BufferState::Transparent
        ));
    }

    #[test]
    fn should_retry_429_and_5xx() {
        for code in [429, 500, 502, 503, 504] {
            assert!(
                should_retry(
                    Some(StatusCode::from_u16(code).unwrap()),
                    None,
                    BufferState::PreFirstToken
                ),
                "code {} should retry",
                code
            );
        }
    }

    #[test]
    fn should_not_retry_4xx_other_than_429() {
        for code in [400, 401, 403, 404, 422] {
            assert!(
                !should_retry(
                    Some(StatusCode::from_u16(code).unwrap()),
                    None,
                    BufferState::PreFirstToken
                ),
                "code {} should NOT retry",
                code
            );
        }
    }

    #[test]
    fn should_retry_stream_midway_error_only_in_buffer_state() {
        // PreFirstToken: still in buffer, retry
        assert!(should_retry(
            None,
            Some(ErrKind::StreamInterrupted),
            BufferState::PreFirstToken
        ));
        // FullBuffer: still in buffer, retry
        assert!(should_retry(
            None,
            Some(ErrKind::StreamInterrupted),
            BufferState::FullBuffer
        ));
        // Transparent: already emitted bytes, can't retry
        assert!(!should_retry(
            None,
            Some(ErrKind::StreamInterrupted),
            BufferState::Transparent
        ));
    }

    #[test]
    fn first_chunk_openai_completions() {
        // delta with content -> first
        let line = r#"data: {"choices":[{"delta":{"content":"Hi"}}]}"#;
        assert!(is_first_business_chunk(&ClientFormat::Completions, line));

        // role-only delta -> NOT first
        let line = r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert!(!is_first_business_chunk(&ClientFormat::Completions, line));

        // [DONE] -> not first
        assert!(!is_first_business_chunk(
            &ClientFormat::Completions,
            "data: [DONE]"
        ));
    }

    #[test]
    fn first_chunk_openai_completions_tool_calls() {
        // A streaming tool_call delta (no content) MUST count as first business,
        // otherwise PreFirstToken mode buffers it forever and hangs the request.
        let line = r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#;
        assert!(
            is_first_business_chunk(&ClientFormat::Completions, line),
            "tool_call delta must be treated as first business chunk"
        );

        // Incremental arguments fragment also counts.
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#;
        assert!(is_first_business_chunk(&ClientFormat::Completions, line));

        // Non-empty reasoning_content counts (DeepSeek-style thinking).
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#;
        assert!(is_first_business_chunk(&ClientFormat::Completions, line));
    }

    #[test]
    fn first_chunk_anthropic() {
        // message_start is meta -> not first
        let line = r#"event: message_start
data: {"type":"message_start"}"#;
        assert!(!is_first_business_chunk(&ClientFormat::Anthropic, line));

        // content_block_delta with text -> first
        let line = r#"event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#;
        assert!(is_first_business_chunk(&ClientFormat::Anthropic, line));
    }

    #[test]
    fn first_chunk_gemini() {
        let line = r#"data: {"candidates":[{"content":{"parts":[{"text":"Hi"}]}}]}"#;
        assert!(is_first_business_chunk(&ClientFormat::Gemini, line));

        // empty candidates -> not first
        let line = r#"data: {"candidates":[]}"#;
        assert!(!is_first_business_chunk(&ClientFormat::Gemini, line));
    }

    #[test]
    fn first_chunk_responses() {
        let line = r#"data: {"type":"response.output_text.delta","delta":"Hi"}"#;
        assert!(is_first_business_chunk(&ClientFormat::Responses, line));

        let line = r#"data: {"type":"response.created"}"#;
        assert!(!is_first_business_chunk(&ClientFormat::Responses, line));
    }

    #[test]
    fn first_chunk_ignores_non_data_lines() {
        assert!(!is_first_business_chunk(
            &ClientFormat::Completions,
            ": ping"
        ));
        assert!(!is_first_business_chunk(&ClientFormat::Completions, ""));
    }

    #[test]
    fn error_trailer_anthropic_format() {
        let s = error_trailer_event(ClientFormat::Anthropic, "retry exhausted");
        assert!(s.contains("event: error"));
        assert!(s.contains("\"type\":\"overloaded_error\""));
        assert!(s.contains("retry exhausted"));
    }

    #[test]
    fn error_trailer_openai_format() {
        let s = error_trailer_event(ClientFormat::Completions, "retry exhausted");
        assert!(s.starts_with("data: "));
        assert!(s.contains("\"type\":\"server_error\""));
        assert!(s.contains("retry exhausted"));
    }

    #[test]
    fn error_trailer_gemini_uses_comment() {
        let s = error_trailer_event(ClientFormat::Gemini, "retry exhausted");
        assert!(s.starts_with(": error"));
    }
}
