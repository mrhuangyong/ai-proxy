//! Normalize ChatGPT wham/usage rate-limit payloads into a stable snapshot.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use serde::Serialize;
use serde_json::Value;

use super::{
    default_user_agent, store, HEADER_CHATGPT_ACCOUNT_ID, HEADER_ORIGINATOR, ORIGINATOR, USAGE_URL,
};
use crate::error::ProxyError;
use crate::http::SHARED_HTTP_CLIENT;

/// How long a fetched snapshot is served from the process cache.
const CACHE_TTL: Duration = Duration::from_secs(30);
/// Per-request timeout for the upstream usage call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Max upstream body characters echoed back in an error message.
const ERROR_BODY_MAX_CHARS: usize = 200;

static USAGE_CACHE: Lazy<Mutex<HashMap<String, (Instant, CodexUsageSnapshot)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// One rate-limit window after clamping and labeling.
#[derive(Debug, Clone, Serialize)]
pub struct CodexUsageWindow {
    pub label: String,
    pub limit_window_seconds: i64,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub reset_at: Option<String>,
    pub reset_after_seconds: Option<i64>,
}

/// Normalized Codex subscription quota snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct CodexUsageSnapshot {
    pub plan_type: Option<String>,
    pub allowed: bool,
    pub limit_reached: bool,
    pub windows: Vec<CodexUsageWindow>,
    pub fetched_at: String,
}

/// Map a wham/usage JSON payload into [`CodexUsageSnapshot`].
///
/// `primary_window` then `secondary_window` are kept when they are objects
/// with a finite numeric `used_percent` and an integer `limit_window_seconds`.
/// Null windows, null percents, and windows without an integer duration are dropped.
/// Percents are clamped to `0.0..=100.0` without rounding.
/// `remaining_percent` is `100.0 - used_percent`.
/// Duration and reset fields accept JSON integers only; float seconds are ignored.
pub fn normalize_wham_usage(raw: &Value, fetched_at: DateTime<Utc>) -> CodexUsageSnapshot {
    let plan_type = raw
        .get("plan_type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let rate_limit = raw.get("rate_limit").unwrap_or(&Value::Null);
    let allowed = rate_limit
        .get("allowed")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit_reached = rate_limit
        .get("limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let windows = ["primary_window", "secondary_window"]
        .into_iter()
        .filter_map(|key| normalize_window(rate_limit.get(key)))
        .collect();

    CodexUsageSnapshot {
        plan_type,
        allowed,
        limit_reached,
        windows,
        fetched_at: fetched_at.to_rfc3339(),
    }
}

/// Effective usage endpoint. `AI_PROXY_CODEX_USAGE_URL` overrides the default
/// (used by tests to point at a mock server).
pub fn usage_endpoint() -> String {
    std::env::var("AI_PROXY_CODEX_USAGE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| USAGE_URL.to_string())
}

fn cache_get_at(provider_id: &str, now: Instant) -> Option<CodexUsageSnapshot> {
    let cache = USAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let (stored_at, snap) = cache.get(provider_id)?;
    if now.saturating_duration_since(*stored_at) < CACHE_TTL {
        Some(snap.clone())
    } else {
        None
    }
}

fn cache_put_at(provider_id: &str, snap: CodexUsageSnapshot, now: Instant) {
    let mut cache = USAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(provider_id.to_string(), (now, snap));
}

/// Drop the cached snapshot for a provider (call after the bound account
/// changes: re-login, import, delete) so the next read refetches.
pub fn invalidate_cache(provider_id: &str) {
    let mut cache = USAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.remove(provider_id);
}

fn truncate_body(body: &str) -> String {
    let mut out: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
    if body.chars().count() > ERROR_BODY_MAX_CHARS {
        out.push('…');
    }
    out
}

/// Fetch the Codex subscription quota for a provider.
///
/// Results are cached per provider for 30 seconds. `force` skips the cache
/// read but still refreshes the cached entry.
pub async fn fetch_usage(
    provider_id: &str,
    force: bool,
) -> Result<CodexUsageSnapshot, ProxyError> {
    if !force {
        if let Some(hit) = cache_get_at(provider_id, Instant::now()) {
            return Ok(hit);
        }
    }

    let (token, account_id) = store::get_valid_access_token(provider_id).await?;
    let mut req = SHARED_HTTP_CLIENT
        .get(usage_endpoint())
        .timeout(REQUEST_TIMEOUT)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/json")
        .header(HEADER_ORIGINATOR, ORIGINATOR)
        .header("User-Agent", default_user_agent());
    if let Some(aid) = account_id.as_deref().filter(|s| !s.is_empty()) {
        req = req.header(HEADER_CHATGPT_ACCOUNT_ID, aid);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| ProxyError::Network(format!("拉取 Codex 额度请求失败: {}", e)))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| ProxyError::Network(format!("读取 Codex 额度响应失败: {}", e)))?;
    if !status.is_success() {
        return Err(ProxyError::Provider(format!(
            "拉取 Codex 额度失败 HTTP {}: {}",
            status.as_u16(),
            truncate_body(&body)
        )));
    }

    let raw: Value = serde_json::from_str(&body)
        .map_err(|e| ProxyError::Config(format!("Codex 额度响应解析失败: {}", e)))?;
    let snap = normalize_wham_usage(&raw, Utc::now());
    cache_put_at(provider_id, snap.clone(), Instant::now());
    Ok(snap)
}

/// Human label for a rate-limit window length.
pub fn window_label(limit_window_seconds: i64) -> String {
    match limit_window_seconds {
        18_000 => "5小时".to_string(),
        604_800 => "7天".to_string(),
        secs if secs > 0 && secs % 3600 == 0 => format!("{}小时", secs / 3600),
        secs => format!("{secs}秒窗口"),
    }
}

fn normalize_window(value: Option<&Value>) -> Option<CodexUsageWindow> {
    let obj = value?.as_object()?;
    let used = obj.get("used_percent")?;
    if used.is_null() {
        return None;
    }
    let used_percent = clamp_percent(used.as_f64()?)?;
    let limit_window_seconds = obj.get("limit_window_seconds").and_then(json_i64)?;
    let reset_after_seconds = obj.get("reset_after_seconds").and_then(json_i64);
    let reset_at = obj.get("reset_at").and_then(|v| {
        let secs = json_i64(v)?;
        DateTime::<Utc>::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
    });

    Some(CodexUsageWindow {
        label: window_label(limit_window_seconds),
        limit_window_seconds,
        used_percent,
        remaining_percent: 100.0 - used_percent,
        reset_at,
        reset_after_seconds,
    })
}

/// Clamp a finite percent into `0.0..=100.0` without rounding.
fn clamp_percent(n: f64) -> Option<f64> {
    if !n.is_finite() {
        return None;
    }
    Some(n.clamp(0.0, 100.0))
}

/// JSON integers only. Float seconds are rejected so they are not rounded.
fn json_i64(v: &Value) -> Option<i64> {
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    v.as_u64().and_then(|u| i64::try_from(u).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_snapshot(plan: &str) -> CodexUsageSnapshot {
        CodexUsageSnapshot {
            plan_type: Some(plan.to_string()),
            allowed: true,
            limit_reached: false,
            windows: vec![],
            fetched_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn cache_hits_within_ttl_and_expires_after() {
        let id = "test-cache-ttl-provider";
        let t0 = Instant::now();
        assert!(cache_get_at(id, t0).is_none());
        cache_put_at(id, sample_snapshot("plus"), t0);
        let hit = cache_get_at(id, t0 + Duration::from_secs(29)).expect("hit within ttl");
        assert_eq!(hit.plan_type.as_deref(), Some("plus"));
        assert!(cache_get_at(id, t0 + Duration::from_secs(30)).is_none());
        // overwrite (force-refresh path) replaces the entry
        cache_put_at(id, sample_snapshot("pro"), t0 + Duration::from_secs(40));
        let hit = cache_get_at(id, t0 + Duration::from_secs(41)).expect("refreshed");
        assert_eq!(hit.plan_type.as_deref(), Some("pro"));
    }

    #[test]
    fn invalidate_cache_removes_only_that_provider() {
        let t0 = Instant::now();
        cache_put_at("test-inval-a", sample_snapshot("plus"), t0);
        cache_put_at("test-inval-b", sample_snapshot("pro"), t0);
        invalidate_cache("test-inval-a");
        assert!(cache_get_at("test-inval-a", t0).is_none());
        assert!(cache_get_at("test-inval-b", t0).is_some());
        invalidate_cache("test-inval-missing");
    }

    #[test]
    fn cache_is_per_provider() {
        let t0 = Instant::now();
        cache_put_at("test-cache-a", sample_snapshot("plus"), t0);
        assert!(cache_get_at("test-cache-b", t0).is_none());
    }

    #[test]
    fn truncate_body_limits_length() {
        let long = "x".repeat(500);
        let out = truncate_body(&long);
        assert_eq!(out.chars().count(), ERROR_BODY_MAX_CHARS + 1);
        assert_eq!(truncate_body("short"), "short");
    }

    #[test]
    fn labels_known_windows() {
        assert_eq!(window_label(18000), "5小时");
        assert_eq!(window_label(604800), "7天");
        assert_eq!(window_label(3600), "1小时");
        assert_eq!(window_label(90), "90秒窗口");
    }

    #[test]
    fn normalize_skips_null_secondary_and_clamps() {
        let raw = json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 150,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 100,
                    "reset_at": 1730000000
                },
                "secondary_window": null
            }
        });
        let snap = normalize_wham_usage(&raw, chrono::Utc::now());
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].used_percent, 100.0);
        assert_eq!(snap.windows[0].remaining_percent, 0.0);
        assert_eq!(snap.windows[0].label, "5小时");
        assert!(snap.windows[0].reset_at.as_ref().unwrap().contains("T"));
    }

    #[test]
    fn normalize_skips_null_used_percent() {
        let raw = json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": null,
                    "limit_window_seconds": 18000
                },
                "secondary_window": {
                    "used_percent": 10,
                    "limit_window_seconds": 604800,
                    "reset_at": 1730000000
                }
            }
        });
        let snap = normalize_wham_usage(&raw, chrono::Utc::now());
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "7天");
        assert_eq!(snap.windows[0].used_percent, 10.0);
        assert_eq!(snap.windows[0].remaining_percent, 90.0);
    }

    #[test]
    fn normalize_preserves_fractional_percent() {
        let raw = json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 10.6,
                    "limit_window_seconds": 18000
                }
            }
        });
        let snap = normalize_wham_usage(&raw, chrono::Utc::now());
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].used_percent, 10.6);
        assert_eq!(snap.windows[0].remaining_percent, 89.4);
    }

    #[test]
    fn normalize_clamps_negative_percent_to_zero() {
        let raw = json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": -1.7,
                    "limit_window_seconds": 3600
                }
            }
        });
        let snap = normalize_wham_usage(&raw, chrono::Utc::now());
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].used_percent, 0.0);
        assert_eq!(snap.windows[0].remaining_percent, 100.0);
    }

    #[test]
    fn normalize_skips_missing_or_float_window_seconds() {
        let raw = json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 10.0
                },
                "secondary_window": {
                    "used_percent": 4.0,
                    "limit_window_seconds": 89.6,
                    "reset_after_seconds": 12,
                    "reset_at": 1730000000
                }
            }
        });
        let snap = normalize_wham_usage(&raw, chrono::Utc::now());
        assert!(snap.windows.is_empty());
    }

    #[test]
    fn normalize_ignores_float_reset_fields() {
        let raw = json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 20.0,
                    "limit_window_seconds": 3600,
                    "reset_after_seconds": 89.6,
                    "reset_at": 1730000000.6
                }
            }
        });
        let snap = normalize_wham_usage(&raw, chrono::Utc::now());
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "1小时");
        assert_eq!(snap.windows[0].limit_window_seconds, 3600);
        assert_eq!(snap.windows[0].reset_after_seconds, None);
        assert_eq!(snap.windows[0].reset_at, None);
    }
}
