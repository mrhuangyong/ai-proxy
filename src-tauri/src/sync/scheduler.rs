use std::time::Duration;

use chrono::Utc;

use super::config::{load_config, update_sync_status};
use super::error::SyncError;
use super::webdav::WebDavClient;
use crate::backup::export::export_bundle;
use crate::db::get_pool;

/// Spawn the background auto-sync scheduler. Call once at startup.
pub fn start_scheduler() {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            if let Err(e) = tick_once().await {
                tracing::warn!("auto sync tick error: {}", e);
            }
        }
    });
}

async fn tick_once() -> Result<(), SyncError> {
    let pool = get_pool().await;
    let cfg = load_config(pool).await?;
    if !cfg.enabled || !cfg.auto_enabled {
        return Ok(());
    }
    if !should_sync_now(pool, cfg.auto_interval_minutes).await? {
        return Ok(());
    }
    run_upload(pool, &cfg).await
}

async fn should_sync_now(pool: &sqlx::SqlitePool, interval_min: u32) -> Result<bool, SyncError> {
    // sync_dirty forces an immediate sync regardless of interval.
    let dirty: (String,) =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'sync_dirty'")
            .fetch_optional(pool)
            .await?
            .unwrap_or(("false".into(),));
    if dirty.0 == "true" {
        sqlx::query("UPDATE settings SET value = 'false', updated_at = datetime('now') WHERE key = 'sync_dirty'")
            .execute(pool).await?;
        return Ok(true);
    }
    let last: (String,) =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'sync_last_upload_at'")
            .fetch_optional(pool)
            .await?
            .unwrap_or((String::new(),));
    if last.0.is_empty() {
        return Ok(true);
    }
    match chrono::DateTime::parse_from_rfc3339(&last.0) {
        Ok(t) => Ok(Utc::now().signed_duration_since(t.with_timezone(&Utc).fixed_offset()).num_minutes()
            >= interval_min as i64),
        Err(_) => Ok(true),
    }
}

pub async fn run_upload(pool: &sqlx::SqlitePool, cfg: &super::config::SyncConfig) -> Result<(), SyncError> {
    let client = WebDavClient::from_config(cfg)?;
    let bytes = export_bundle(pool).await?;
    let filename = format!(
        "ai-proxy-backup-{}.json",
        Utc::now().format("%Y-%m-%dT%H-%M-%SZ")
    );
    client.upload(&filename, &bytes).await?;
    update_sync_status(pool, "success", "").await?;
    Ok(())
}
