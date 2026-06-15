//! Upstream-interruption invisible retry helpers.
//!
//! Pure functions for error classification, first-token detection, and
//! backoff computation. The orchestration loop lives in `retry_session`.

use crate::converter::ir::ClientFormat;
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
