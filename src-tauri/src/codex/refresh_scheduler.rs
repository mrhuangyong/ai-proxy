//! Background scheduler that proactively refreshes Codex OAuth access tokens
//! before they expire — so idle channels stay valid without waiting for traffic.

use std::time::Duration;

use tracing::{info, warn};

use super::store::{self, CodexAccountRow};
use super::REFRESH_MARGIN_SECS;

/// How often the scheduler wakes to scan accounts.
const TICK_SECS: u64 = 60;
/// Delay before the first tick so DB / proxy init can finish.
const STARTUP_DELAY_SECS: u64 = 15;

/// Spawn the periodic Codex token refresh task. Call once after the DB pool
/// is ready (same place as the failover health checker).
pub fn spawn_token_refresh_scheduler() {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(STARTUP_DELAY_SECS)).await;
        info!("[codex-refresh] background token refresh scheduler started");
        let mut ticker = tokio::time::interval(Duration::from_secs(TICK_SECS));
        loop {
            ticker.tick().await;
            if let Err(e) = refresh_due_accounts().await {
                warn!("[codex-refresh] tick failed: {}", e);
            }
        }
    });
}

fn account_needs_refresh(row: &CodexAccountRow) -> bool {
    if row.status != "active" {
        return false;
    }
    let Some(exp) = row.expires_at else {
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    now >= exp - REFRESH_MARGIN_SECS
}

/// Refresh every active Codex account whose access token is within the
/// proactive refresh window (or already expired).
pub async fn refresh_due_accounts() -> Result<(), String> {
    let rows = store::list_account_rows()
        .await
        .map_err(|e| e.to_string())?;
    for row in rows {
        if !account_needs_refresh(&row) {
            continue;
        }
        match store::force_refresh(&row.provider_id).await {
            Ok(_) => {
                info!(
                    "[codex-refresh] refreshed access token for provider {}",
                    row.provider_id
                );
            }
            Err(e) => {
                warn!(
                    "[codex-refresh] refresh failed for provider {}: {}",
                    row.provider_id, e
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, expires_in_secs: Option<i64>) -> CodexAccountRow {
        CodexAccountRow {
            id: "a1".into(),
            provider_id: "p1".into(),
            chatgpt_account_id: None,
            email: None,
            plan_type: None,
            encrypted_access_token: vec![],
            access_nonce: vec![],
            encrypted_refresh_token: vec![],
            refresh_nonce: vec![],
            encrypted_id_token: None,
            id_nonce: None,
            expires_at: expires_in_secs.map(|s| chrono::Utc::now().timestamp() + s),
            last_refresh_at: None,
            status: status.into(),
        }
    }

    #[test]
    fn needs_refresh_when_within_margin() {
        assert!(account_needs_refresh(&row("active", Some(60))));
        assert!(account_needs_refresh(&row("active", Some(-10))));
        assert!(!account_needs_refresh(&row("active", Some(REFRESH_MARGIN_SECS + 120))));
        assert!(!account_needs_refresh(&row("expired", Some(60))));
        assert!(!account_needs_refresh(&row("active", None)));
    }
}
