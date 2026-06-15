//! Upstream-interruption invisible retry helpers.
//!
//! Pure functions for error classification, first-token detection, and
//! backoff computation. The orchestration loop lives in `retry_session`.

use reqwest::StatusCode;

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
        assert!(should_retry(None, Some(ErrKind::Network), BufferState::PreFirstToken));
        assert!(should_retry(None, Some(ErrKind::Network), BufferState::FullBuffer));
        // Already transparent: can't retry
        assert!(!should_retry(None, Some(ErrKind::Network), BufferState::Transparent));
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
        assert!(should_retry(None, Some(ErrKind::StreamInterrupted), BufferState::PreFirstToken));
        // FullBuffer: still in buffer, retry
        assert!(should_retry(None, Some(ErrKind::StreamInterrupted), BufferState::FullBuffer));
        // Transparent: already emitted bytes, can't retry
        assert!(!should_retry(None, Some(ErrKind::StreamInterrupted), BufferState::Transparent));
    }
}
