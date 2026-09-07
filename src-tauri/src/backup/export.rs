use base64::{engine::general_purpose::STANDARD as B64, Engine};
use sqlx::{Column, Row, SqlitePool, ValueRef};

use super::bundle::{BackupBundle, BackupData, KdfParams};
use super::crypto::{derive_key, generate_salt, passphrase_encrypt};
use super::error::{BackupError, BackupResult};
use super::sensitive::{is_sensitive_setting_key, mcp_json_has_secret};
use crate::key::store::{decrypt_api_key, encrypt_api_key, KeyStore};

const BUNDLE_VERSION: u32 = 1;
const PBKDF2_ITERATIONS: u32 = 200_000;

/// Export using the passphrase persisted in `settings.backup_passphrase`.
pub async fn export_bundle(pool: &SqlitePool) -> BackupResult<Vec<u8>> {
    let passphrase = read_stored_passphrase(pool).await?;
    export_bundle_with_passphrase(pool, &passphrase).await
}

/// Export using an explicit passphrase (also used by tests).
pub async fn export_bundle_with_passphrase(
    pool: &SqlitePool,
    passphrase: &str,
) -> BackupResult<Vec<u8>> {
    let salt = generate_salt();
    let key = derive_key(passphrase, &salt);

    let mut data = BackupData::default();
    let empty: &[&str] = &[];
    data.providers = read_table(pool, "providers", empty).await?;
    data.provider_protocols = read_table(pool, "provider_protocols", empty).await?;
    data.provider_models = read_table(pool, "provider_models", empty).await?;
    data.api_keys = read_table(pool, "api_keys", API_KEY_BLOB_COLS).await?;
    data.interceptor_rules = read_table(pool, "interceptor_rules", empty).await?;
    data.virtual_models = read_table(pool, "virtual_models", empty).await?;
    data.virtual_model_mappings = read_table(pool, "virtual_model_mappings", empty).await?;
    data.mcp_servers = read_table(pool, "mcp_servers", empty).await?;
    data.mcp_app_bindings = read_table(pool, "mcp_app_bindings", empty).await?;
    data.app_configs = read_table(pool, "app_configs", empty).await?;
    data.users = read_table(pool, "users", empty).await?;
    data.settings = read_table(pool, "settings", empty).await?;

    // Filter out machine-bound secret rows — these are encrypted with the source
    // machine's master key and cannot be decrypted on a different machine.
    // The target machine must set its own passphrase / WebDAV password.
    data.settings.retain(|row| {
        let k = row.get("key").and_then(|v| v.as_str()).unwrap_or("");
        !matches!(k, "backup_passphrase" | "sync_webdav_password")
    });

    // Re-encrypt sensitive fields with the passphrase key.
    encrypt_api_keys(&mut data.api_keys, &key)?;
    encrypt_sensitive_settings(&mut data.settings, &key)?;
    encrypt_mcp_secrets(&mut data.mcp_servers, &key)?;

    let bundle = BackupBundle {
        version: BUNDLE_VERSION,
        created_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        kdf: KdfParams {
            algorithm: "pbkdf2-sha256".into(),
            iterations: PBKDF2_ITERATIONS,
            salt: B64.encode(salt),
            key_len: 32,
        },
        encrypted_fields: vec![
            "api_keys.encrypted_key".into(),
            "settings.proxy_auth_key".into(),
            "mcp_servers.env".into(),
            "mcp_servers.headers".into(),
        ],
        data,
    };

    Ok(serde_json::to_vec_pretty(&bundle)?)
}

/// Read all rows of a table as Vec<serde_json::Value>. `blob_cols` lists the
/// column names that are BLOB type (decoded to base64); all other columns are
/// decoded by attempting TEXT → INTEGER → REAL → BLOB in turn. This explicit
/// approach avoids fragile reflection on sqlx internals and makes round-tripping
/// deterministic across SQLite's dynamic typing.
async fn read_table(
    pool: &SqlitePool,
    table: &str,
    blob_cols: &[&str],
) -> BackupResult<Vec<serde_json::Value>> {
    // table name is a hardcoded constant from the call site, not user input.
    let sql = format!("SELECT * FROM {}", table);
    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row_to_json(row, blob_cols));
    }
    Ok(out)
}

/// Convert a row to a JSON object. `blob_cols` columns are always decoded as
/// BLOB → base64 string. All other columns are decoded by trying types in turn:
/// TEXT (Option<String>) first, then INTEGER (Option<i64>), then REAL
/// (Option<f64>), then BLOB (Option<Vec<u8>> → base64). SQLite is dynamically
/// typed, so a single declared-type strategy fails for INTEGER columns such as
/// `api_keys.is_active`; the fallback chain handles every storage class.
fn row_to_json(row: &sqlx::sqlite::SqliteRow, blob_cols: &[&str]) -> serde_json::Value {
    use serde_json::json;
    // Column::name and Row::try_get are both brought into scope by the module
    // imports above (`use sqlx::{Column, Row}`).
    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name();
        if blob_cols.contains(&name) {
            let bytes: Vec<u8> = row.get(name);
            map.insert(name.into(), serde_json::Value::String(B64.encode(&bytes)));
            continue;
        }
        // Preserve NULL as JSON `null`. The old code tried Option<String> first
        // and flattened NULL to "" via unwrap_or_default(), which made import
        // bind an empty string into INTEGER columns (TEXT ''), breaking sqlx
        // decode of Option<i64>. Emitting null lets import bind a real NULL.
        if row.try_get_raw(name).map(|r| r.is_null()).unwrap_or(false) {
            map.insert(name.into(), serde_json::Value::Null);
            continue;
        }
        // Non-NULL values: try TEXT, then INTEGER, then REAL, then BLOB.
        if let Ok(val) = row.try_get::<String, _>(name) {
            map.insert(name.into(), serde_json::Value::String(val));
        } else if let Ok(val) = row.try_get::<i64, _>(name) {
            map.insert(name.into(), json!(val));
        } else if let Ok(val) = row.try_get::<f64, _>(name) {
            map.insert(name.into(), json!(val));
        } else if let Ok(val) = row.try_get::<Vec<u8>, _>(name) {
            map.insert(name.into(), serde_json::Value::String(B64.encode(&val)));
        } else {
            // Unreachable for non-NULL values; keep a safe fallback.
            map.insert(name.into(), serde_json::Value::Null);
        }
    }
    serde_json::Value::Object(map)
}

/// Schema knowledge: which columns are BLOB per table (used for correct
/// base64 round-tripping). Only api_keys has BLOB columns.
const API_KEY_BLOB_COLS: &[&str] = &["encrypted_key", "nonce"];

fn encrypt_api_keys(rows: &mut [serde_json::Value], key: &[u8; 32]) -> BackupResult<()> {
    for row in rows.iter_mut() {
        let obj = row.as_object_mut().ok_or(BackupError::InvalidFormat)?;
        // DB columns: encrypted_key (BLOB -> base64 string), nonce (BLOB -> base64)
        let enc_b64 = obj
            .get("encrypted_key")
            .and_then(|v| v.as_str())
            .ok_or(BackupError::InvalidFormat)?;
        let nonce_b64 = obj
            .get("nonce")
            .and_then(|v| v.as_str())
            .ok_or(BackupError::InvalidFormat)?;

        let enc_blob = B64
            .decode(enc_b64)
            .map_err(|_| BackupError::InvalidFormat)?;
        let nonce_blob = B64
            .decode(nonce_b64)
            .map_err(|_| BackupError::InvalidFormat)?;
        let nonce_arr: [u8; 12] = nonce_blob
            .as_slice()
            .try_into()
            .map_err(|_| BackupError::InvalidFormat)?;

        // Decrypt with machine master key → plaintext key
        let plaintext = decrypt_api_key(&enc_blob, &nonce_arr)
            .map_err(|e| BackupError::Other(e.to_string()))?;
        // Re-encrypt with passphrase key
        let (new_ct, new_nonce) = passphrase_encrypt(plaintext.as_bytes(), key)?;

        obj.insert("encrypted_key".into(), B64.encode(new_ct).into());
        obj.insert("nonce".into(), B64.encode(new_nonce).into());
    }
    Ok(())
}

fn encrypt_sensitive_settings(rows: &mut [serde_json::Value], key: &[u8; 32]) -> BackupResult<()> {
    for row in rows.iter_mut() {
        let obj = row.as_object_mut().ok_or(BackupError::InvalidFormat)?;
        let k = obj
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if is_sensitive_setting_key(&k) {
            if let Some(val) = obj.get("value").and_then(|v| v.as_str()).map(String::from) {
                if !val.is_empty() {
                    let (ct, nonce) = passphrase_encrypt(val.as_bytes(), key)?;
                    obj.insert(
                        "value".into(),
                        format!("enc:{}:{}", B64.encode(nonce), B64.encode(ct)).into(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn encrypt_mcp_secrets(rows: &mut [serde_json::Value], key: &[u8; 32]) -> BackupResult<()> {
    for row in rows.iter_mut() {
        let obj = row.as_object_mut().ok_or(BackupError::InvalidFormat)?;
        for col in &["env", "headers"] {
            if let Some(val) = obj.get(*col).and_then(|v| v.as_str()).map(String::from) {
                if !val.is_empty() && mcp_json_has_secret(&val) {
                    let (ct, nonce) = passphrase_encrypt(val.as_bytes(), key)?;
                    obj.insert(
                        (*col).into(),
                        format!("enc:{}:{}", B64.encode(nonce), B64.encode(ct)).into(),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Read the persisted, master-key-encrypted passphrase from settings and
/// decrypt it with the machine master key. Stored format is "nonce_b64:ct_b64".
pub(crate) async fn read_stored_passphrase(pool: &SqlitePool) -> BackupResult<String> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'backup_passphrase'")
            .fetch_optional(pool)
            .await?;
    let stored = row.map(|(v,)| v).unwrap_or_default();
    if stored.is_empty() {
        return Err(BackupError::PassphraseNotSet);
    }
    // stored is "nonce_b64:ct_b64" (persisted via key::store::encrypt_api_key).
    let parts: Vec<&str> = stored.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(BackupError::Other("backup_passphrase 格式无效".into()));
    }
    let nonce = B64
        .decode(parts[0])
        .map_err(|_| BackupError::InvalidFormat)?;
    let ct = B64
        .decode(parts[1])
        .map_err(|_| BackupError::InvalidFormat)?;
    let nonce_arr: [u8; 12] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| BackupError::InvalidFormat)?;
    let master = KeyStore::derive_key().map_err(|e| BackupError::Other(e.to_string()))?;
    let store = KeyStore::new(&master);
    let plain_bytes = store
        .decrypt(&ct, &nonce_arr)
        .map_err(|e| BackupError::Other(e.to_string()))?;
    String::from_utf8(plain_bytes).map_err(|_| BackupError::InvalidFormat)
}

/// Persist a passphrase, master-key-encrypted, as "nonce_b64:ct_b64".
///
/// Uses an UPSERT: the `backup_passphrase` settings row can be absent (e.g.
/// after a restore, which strips machine-bound secrets, or on installs where
/// migration 025 was skipped because `sync_webdav_url` already existed). A plain
/// UPDATE would silently affect 0 rows and the status would stay "未设置".
pub async fn store_passphrase(pool: &SqlitePool, passphrase: &str) -> BackupResult<()> {
    let (ct, nonce) = encrypt_api_key(passphrase).map_err(|e| BackupError::Other(e.to_string()))?;
    let stored = format!("{}:{}", B64.encode(nonce), B64.encode(ct));
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) \
         VALUES ('backup_passphrase', ?, datetime('now')) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
    )
    .bind(&stored)
    .execute(pool)
    .await?;
    Ok(())
}
