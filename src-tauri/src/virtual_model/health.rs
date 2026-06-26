//! Background health checker for `virtual_model_mappings` rows that have been
//! marked `available=0`. Periodically sends a tiny probe request (1 token) to
//! each unavailable mapping; on success the mapping is restored (`available=1`,
//! counters reset).

use std::time::Duration;

use tracing::{info, warn};

use crate::db::get_pool;
use crate::key::rotation::{KeyRotation, RotationStrategy};
use crate::key::store::decrypt_api_key;
use crate::virtual_model::manager::VirtualRouter;

/// Spawn the periodic health checker task. Should be called once after the DB
/// pool and proxy are ready.
pub fn spawn_health_checker() {
    tokio::spawn(async move {
        // Stagger startup so we don't race with proxy initialization.
        tokio::time::sleep(Duration::from_secs(30)).await;
        info!("[failover-health] background health checker started");
        loop {
            let interval = load_interval().await;
            tokio::time::sleep(Duration::from_secs(interval.max(15))).await;

            let targets = VirtualRouter::list_unavailable_for_probe().await;
            if targets.is_empty() {
                continue;
            }
            let max_tokens = load_probe_max_tokens().await;

            for (mapping_id, model_name, provider_id) in targets {
                if let Err(e) = probe(&provider_id, &model_name, max_tokens).await {
                    warn!(
                        "[failover-health] probe failed for mapping {} ({} via {}): {}",
                        mapping_id, model_name, provider_id, e
                    );
                    continue;
                }
                info!(
                    "[failover-health] mapping {} ({}) for {} recovered",
                    mapping_id, model_name, provider_id
                );
                VirtualRouter::mark_available(&mapping_id).await;
            }
        }
    });
}

async fn load_interval() -> u64 {
    let pool = get_pool().await;
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'virtual_model_health_interval_secs'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.parse().ok())
    .unwrap_or(60)
}

async fn load_probe_max_tokens() -> u32 {
    let pool = get_pool().await;
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'virtual_model_probe_max_tokens'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.parse().ok())
    .unwrap_or(1)
}

/// Send a 1-token probe to a (provider_id, model_name) pair. Uses the same
/// key path as the live proxy so the probe exercises real auth + endpoint.
/// Returns Ok(()) if the upstream returned any HTTP response with status
/// <= 500; returns Err on transport failure or 5xx status.
async fn probe(
    provider_id: &str,
    model_name: &str,
    max_tokens: u32,
) -> Result<(), String> {
    use serde_json::json;

    let selected_key = KeyRotation::get_next_key(provider_id, &RotationStrategy::LeastUsed)
        .await
        .map_err(|e| format!("key rotation: {}", e))?;

    let mut nonce = [0u8; 12];
    if selected_key.nonce.len() == 12 {
        nonce.copy_from_slice(&selected_key.nonce);
    } else {
        return Err("invalid nonce length".into());
    }
    let api_key = decrypt_api_key(&selected_key.encrypted_key, &nonce)
        .map_err(|e| format!("decrypt: {}", e))?;

    // Resolve base url & format for this provider.
    let pool = get_pool().await;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT base_url, format FROM providers WHERE id = ?")
            .bind(provider_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("db: {}", e))?;
    let (base_url, format) = row.ok_or_else(|| "provider not found".to_string())?;

    let body = json!({
        "model": model_name,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": max_tokens,
        "stream": false,
    });

    let client = crate::http::SHARED_HTTP_CLIENT.clone();
    let url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        probe_path(&format)
    );
    let mut req = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(30));
    req = match format.as_str() {
        "anthropic" => req
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01"),
        _ => req.bearer_auth(&api_key),
    };

    let resp = req.send().await.map_err(|e| format!("send: {}", e))?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 429 || status.is_client_error() {
        // Any 4xx (except ones we'd treat as fatal) still means "endpoint
        // reachable" — e.g. 400/401 means the provider is up; treat as healthy.
        Ok(())
    } else {
        Err(format!("upstream returned status {}", status))
    }
}

fn probe_path(format: &str) -> &'static str {
    match format {
        "completions" => "v1/chat/completions",
        "responses" => "v1/responses",
        "anthropic" => "v1/messages",
        "gemini" => "v1beta/models/probe:generateContent",
        _ => "v1/chat/completions",
    }
}