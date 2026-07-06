use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::error::{SyncError, SyncResult};
use crate::key::store::{encrypt_api_key, KeyStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub enabled: bool,
    pub webdav_url: String,
    pub webdav_username: String,
    /// Plaintext only in memory; never serialized to API responses directly
    /// (handler strips it). Stored master-key-encrypted in DB.
    #[serde(skip_serializing)]
    pub webdav_password: String,
    pub webdav_path: String,
    pub auto_enabled: bool,
    pub auto_interval_minutes: u32,
    pub sync_on_change: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webdav_url: String::new(),
            webdav_username: String::new(),
            webdav_password: String::new(),
            webdav_path: "ai-proxy-backups/".into(),
            auto_enabled: false,
            auto_interval_minutes: 60,
            sync_on_change: false,
        }
    }
}

pub async fn load_config(pool: &SqlitePool) -> SyncResult<SyncConfig> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings WHERE key LIKE 'sync_%'")
            .fetch_all(pool)
            .await?;
    let map: std::collections::HashMap<String, String> = rows.into_iter().collect();
    let get = |k: &str| map.get(k).cloned().unwrap_or_default();
    let pwd = decrypt_password(&get("sync_webdav_password")).unwrap_or_default();
    Ok(SyncConfig {
        enabled: get("sync_enabled") == "true",
        webdav_url: get("sync_webdav_url"),
        webdav_username: get("sync_webdav_username"),
        webdav_password: pwd,
        webdav_path: {
            let p = get("sync_webdav_path");
            if p.is_empty() {
                "ai-proxy-backups/".into()
            } else {
                p
            }
        },
        auto_enabled: get("sync_auto_enabled") == "true",
        auto_interval_minutes: get("sync_auto_interval_minutes").parse().unwrap_or(60),
        sync_on_change: get("sync_on_change") == "true",
    })
}

pub async fn save_config(pool: &SqlitePool, cfg: &SyncConfig) -> SyncResult<()> {
    let mut updates: Vec<(&str, String)> = vec![
        (
            "sync_enabled",
            if cfg.enabled {
                "true".into()
            } else {
                "false".into()
            },
        ),
        ("sync_webdav_url", cfg.webdav_url.clone()),
        ("sync_webdav_username", cfg.webdav_username.clone()),
        ("sync_webdav_path", cfg.webdav_path.clone()),
        (
            "sync_auto_enabled",
            if cfg.auto_enabled {
                "true".into()
            } else {
                "false".into()
            },
        ),
        (
            "sync_auto_interval_minutes",
            cfg.auto_interval_minutes.to_string(),
        ),
        (
            "sync_on_change",
            if cfg.sync_on_change {
                "true".into()
            } else {
                "false".into()
            },
        ),
    ];
    // Only update password if a new one is provided (non-empty).
    if !cfg.webdav_password.is_empty() {
        let (ct, nonce) =
            encrypt_api_key(&cfg.webdav_password).map_err(|e| SyncError::Other(e.to_string()))?;
        let stored = format!("{}:{}", B64.encode(nonce), B64.encode(ct));
        updates.push(("sync_webdav_password", stored));
    }
    for (k, v) in updates {
        sqlx::query("UPDATE settings SET value = ?, updated_at = datetime('now') WHERE key = ?")
            .bind(v)
            .bind(k)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn update_sync_status(pool: &SqlitePool, status: &str, error: &str) -> SyncResult<()> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE settings SET value = ?, updated_at = datetime('now') WHERE key = 'sync_last_upload_at'")
        .bind(&now).execute(pool).await?;
    sqlx::query("UPDATE settings SET value = ?, updated_at = datetime('now') WHERE key = 'sync_last_upload_status'")
        .bind(status).execute(pool).await?;
    sqlx::query(
        "UPDATE settings SET value = ?, updated_at = datetime('now') WHERE key = 'sync_last_error'",
    )
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

fn decrypt_password(stored: &str) -> SyncResult<String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    let parts: Vec<&str> = stored.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(SyncError::Other("sync_webdav_password 格式无效".into()));
    }
    let nonce = B64
        .decode(parts[0])
        .map_err(|_| SyncError::Other("b64".into()))?;
    let ct = B64
        .decode(parts[1])
        .map_err(|_| SyncError::Other("b64".into()))?;
    let nonce_arr: [u8; 12] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| SyncError::Other("nonce len".into()))?;
    let plain =
        KeyStore::new(&KeyStore::derive_key().map_err(|e| SyncError::Other(e.to_string()))?)
            .decrypt(&ct, &nonce_arr)
            .map_err(|e| SyncError::Other(e.to_string()))?;
    String::from_utf8(plain).map_err(|_| SyncError::Other("utf8".into()))
}
