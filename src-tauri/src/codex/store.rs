//! Encrypted storage for Codex OAuth accounts (one account per provider) and
//! the request-time access-token resolver with single-flight refresh.

use once_cell::sync::Lazy;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::ProxyError;
use crate::key::store::{decrypt_api_key, encrypt_api_key};

use super::jwt;
use super::oauth::{self, RefreshError};
use super::{REFRESH_MARGIN_SECS, PROVIDER_KIND_CODEX};

const NONCE_SIZE: usize = 12;

/// One provider's Codex account row (tokens still encrypted).
#[derive(Debug, Clone)]
pub struct CodexAccountRow {
    pub id: String,
    pub provider_id: String,
    pub chatgpt_account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub encrypted_access_token: Vec<u8>,
    pub access_nonce: Vec<u8>,
    pub encrypted_refresh_token: Vec<u8>,
    pub refresh_nonce: Vec<u8>,
    /// Present after migration 032; missing on legacy rows until next refresh/login.
    pub encrypted_id_token: Option<Vec<u8>>,
    pub id_nonce: Option<Vec<u8>>,
    /// RFC3339 unix timestamp of the access token's expiry (from id_token exp).
    pub expires_at: Option<i64>,
    pub last_refresh_at: Option<String>,
    pub status: String,
}

/// Plaintext token set for writing an official `~/.codex/auth.json`.
#[derive(Debug, Clone)]
pub struct OfficialAuthTokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: Option<String>,
    pub last_refresh_at: Option<String>,
}

/// Frontend-safe projection (never carries tokens).
#[derive(Debug, Clone, Serialize)]
pub struct CodexAccountSummary {
    pub id: String,
    pub provider_id: String,
    pub chatgpt_account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    /// RFC3339 display string; null when unknown.
    pub expires_at: Option<String>,
    pub last_refresh_at: Option<String>,
    /// `active` | `expired` | `missing` (no account bound yet).
    pub status: String,
}

fn row_to_account(row: sqlx::sqlite::SqliteRow) -> CodexAccountRow {
    let expires_at: Option<String> = row.get("expires_at");
    CodexAccountRow {
        id: row.get("id"),
        provider_id: row.get("provider_id"),
        chatgpt_account_id: row.get("chatgpt_account_id"),
        email: row.get("email"),
        plan_type: row.get("plan_type"),
        encrypted_access_token: row.get("encrypted_access_token"),
        access_nonce: row.get("access_nonce"),
        encrypted_refresh_token: row.get("encrypted_refresh_token"),
        refresh_nonce: row.get("refresh_nonce"),
        encrypted_id_token: row.get("encrypted_id_token"),
        id_nonce: row.get("id_nonce"),
        expires_at: expires_at.and_then(|s| s.parse::<i64>().ok()),
        last_refresh_at: row.get("last_refresh_at"),
        status: row.get("status"),
    }
}

const SELECT_COLUMNS: &str = "id, provider_id, chatgpt_account_id, email, plan_type, \
     encrypted_access_token, access_nonce, encrypted_refresh_token, refresh_nonce, \
     encrypted_id_token, id_nonce, \
     expires_at, last_refresh_at, status";

pub async fn get_by_provider(provider_id: &str) -> Result<Option<CodexAccountRow>, ProxyError> {
    let pool = crate::db::get_pool().await;
    let query = format!(
        "SELECT {} FROM codex_accounts WHERE provider_id = ?",
        SELECT_COLUMNS
    );
    sqlx::query(&query)
        .bind(provider_id)
        .fetch_optional(pool)
        .await
        .map(|opt| opt.map(row_to_account))
        .map_err(|e| ProxyError::Database(e))
}

pub async fn get_by_id(id: &str) -> Result<Option<CodexAccountRow>, ProxyError> {
    let pool = crate::db::get_pool().await;
    let query = format!("SELECT {} FROM codex_accounts WHERE id = ?", SELECT_COLUMNS);
    sqlx::query(&query)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map(|opt| opt.map(row_to_account))
        .map_err(|e| ProxyError::Database(e))
}

pub async fn delete_by_id(id: &str) -> Result<(), ProxyError> {
    // Resolve the owning provider first so its usage cache can be dropped.
    let provider_id = get_by_id(id).await?.map(|row| row.provider_id);
    let pool = crate::db::get_pool().await;
    sqlx::query("DELETE FROM codex_accounts WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| ProxyError::Database(e))?;
    if let Some(pid) = provider_id {
        super::usage::invalidate_cache(&pid);
    }
    Ok(())
}

pub async fn delete_by_provider(provider_id: &str) -> Result<(), ProxyError> {
    let pool = crate::db::get_pool().await;
    sqlx::query("DELETE FROM codex_accounts WHERE provider_id = ?")
        .bind(provider_id)
        .execute(pool)
        .await
        .map_err(|e| ProxyError::Database(e))?;
    super::usage::invalidate_cache(provider_id);
    Ok(())
}

/// Insert or replace the account bound to `provider_id` with a fresh token
/// set. Tokens are encrypted with the same AES-256-GCM store as API keys.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_tokens(
    provider_id: &str,
    tokens: &oauth::TokenSet,
    chatgpt_account_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
) -> Result<CodexAccountSummary, ProxyError> {
    let claims = jwt::parse_id_token(&tokens.id_token).ok();
    let account_id = chatgpt_account_id
        .map(str::to_string)
        .or_else(|| claims.as_ref().and_then(|c| c.chatgpt_account_id.clone()));
    let email_owned = email
        .map(str::to_string)
        .or_else(|| claims.as_ref().and_then(|c| c.email.clone()));
    let plan = plan_type
        .map(str::to_string)
        .or_else(|| claims.as_ref().and_then(|c| c.plan_type.clone()));
    let expires_at = claims.as_ref().and_then(|c| c.exp);

    let (enc_access, access_nonce) = encrypt_api_key(&tokens.access_token)
        .map_err(|e| ProxyError::KeyManagement(format!("access token 加密失败: {}", e)))?;
    let (enc_refresh, refresh_nonce) = encrypt_api_key(&tokens.refresh_token)
        .map_err(|e| ProxyError::KeyManagement(format!("refresh token 加密失败: {}", e)))?;
    let (enc_id, id_nonce) = encrypt_api_key(&tokens.id_token)
        .map_err(|e| ProxyError::KeyManagement(format!("id token 加密失败: {}", e)))?;
    let now_rfc3339 = chrono::Utc::now().to_rfc3339();

    let pool = crate::db::get_pool().await;
    sqlx::query(
        "INSERT INTO codex_accounts \
         (id, provider_id, chatgpt_account_id, email, plan_type, \
          encrypted_access_token, access_nonce, encrypted_refresh_token, refresh_nonce, \
          encrypted_id_token, id_nonce, \
          expires_at, last_refresh_at, status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?, ?) \
         ON CONFLICT(provider_id) DO UPDATE SET \
          chatgpt_account_id = excluded.chatgpt_account_id, \
          email = excluded.email, \
          plan_type = excluded.plan_type, \
          encrypted_access_token = excluded.encrypted_access_token, \
          access_nonce = excluded.access_nonce, \
          encrypted_refresh_token = excluded.encrypted_refresh_token, \
          refresh_nonce = excluded.refresh_nonce, \
          encrypted_id_token = excluded.encrypted_id_token, \
          id_nonce = excluded.id_nonce, \
          expires_at = excluded.expires_at, \
          last_refresh_at = excluded.last_refresh_at, \
          status = 'active', \
          updated_at = excluded.updated_at",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(provider_id)
    .bind(account_id.as_deref())
    .bind(email_owned.as_deref())
    .bind(plan.as_deref())
    .bind(&enc_access)
    .bind(&access_nonce.as_slice())
    .bind(&enc_refresh)
    .bind(&refresh_nonce.as_slice())
    .bind(&enc_id)
    .bind(&id_nonce.as_slice())
    .bind(expires_at.map(|v| v.to_string()))
    .bind(&now_rfc3339)
    .bind(&now_rfc3339)
    .bind(&now_rfc3339)
    .execute(pool)
    .await
    .map_err(|e| ProxyError::Database(e))?;
    super::usage::invalidate_cache(provider_id);

    summary_for_provider(provider_id)
        .await?
        .ok_or_else(|| ProxyError::Provider("codex 账号写入后读取失败".into()))
}

async fn update_tokens(
    account: &CodexAccountRow,
    tokens: &oauth::TokenSet,
) -> Result<(), ProxyError> {
    let claims = jwt::parse_id_token(&tokens.id_token).ok();
    let expires_at = claims.as_ref().and_then(|c| c.exp);
    let (enc_access, access_nonce) = encrypt_api_key(&tokens.access_token)
        .map_err(|e| ProxyError::KeyManagement(format!("access token 加密失败: {}", e)))?;
    let (enc_refresh, refresh_nonce) = encrypt_api_key(&tokens.refresh_token)
        .map_err(|e| ProxyError::KeyManagement(format!("refresh token 加密失败: {}", e)))?;
    let (enc_id, id_nonce) = encrypt_api_key(&tokens.id_token)
        .map_err(|e| ProxyError::KeyManagement(format!("id token 加密失败: {}", e)))?;

    let pool = crate::db::get_pool().await;
    sqlx::query(
        "UPDATE codex_accounts SET \
          chatgpt_account_id = COALESCE(?, chatgpt_account_id), \
          email = COALESCE(?, email), \
          plan_type = COALESCE(?, plan_type), \
          encrypted_access_token = ?, access_nonce = ?, \
          encrypted_refresh_token = ?, refresh_nonce = ?, \
          encrypted_id_token = ?, id_nonce = ?, \
          expires_at = ?, last_refresh_at = ?, status = 'active', updated_at = ? \
         WHERE id = ?",
    )
    .bind(claims.as_ref().and_then(|c| c.chatgpt_account_id.as_deref()))
    .bind(claims.as_ref().and_then(|c| c.email.as_deref()))
    .bind(claims.as_ref().and_then(|c| c.plan_type.as_deref()))
    .bind(&enc_access)
    .bind(&access_nonce.as_slice())
    .bind(&enc_refresh)
    .bind(&refresh_nonce.as_slice())
    .bind(&enc_id)
    .bind(&id_nonce.as_slice())
    .bind(expires_at.map(|v| v.to_string()))
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&account.id)
    .execute(pool)
    .await
    .map_err(|e| ProxyError::Database(e))?;
    Ok(())
}

pub async fn set_status(account_id: &str, status: &str) -> Result<(), ProxyError> {
    let pool = crate::db::get_pool().await;
    sqlx::query("UPDATE codex_accounts SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(account_id)
        .execute(pool)
        .await
        .map_err(|e| ProxyError::Database(e))?;
    Ok(())
}

/// Frontend-safe summary; `status = 'missing'` when no account is bound.
pub async fn summary_for_provider(
    provider_id: &str,
) -> Result<Option<CodexAccountSummary>, ProxyError> {
    let Some(row) = get_by_provider(provider_id).await? else {
        return Ok(Some(CodexAccountSummary {
            id: String::new(),
            provider_id: provider_id.to_string(),
            chatgpt_account_id: None,
            email: None,
            plan_type: None,
            expires_at: None,
            last_refresh_at: None,
            status: "missing".into(),
        }));
    };
    Ok(Some(summary_of(&row)))
}

fn summary_of(row: &CodexAccountRow) -> CodexAccountSummary {
    CodexAccountSummary {
        id: row.id.clone(),
        provider_id: row.provider_id.clone(),
        chatgpt_account_id: row.chatgpt_account_id.clone(),
        email: row.email.clone(),
        plan_type: row.plan_type.clone(),
        expires_at: row
            .expires_at
            .map(|exp| chrono::DateTime::from_timestamp(exp, 0).map(|dt| dt.to_rfc3339()))
            .flatten(),
        last_refresh_at: row.last_refresh_at.clone(),
        status: row.status.clone(),
    }
}

pub async fn list_summaries() -> Result<Vec<CodexAccountSummary>, ProxyError> {
    let pool = crate::db::get_pool().await;
    let query = format!(
        "SELECT {} FROM codex_accounts ORDER BY created_at ASC",
        SELECT_COLUMNS
    );
    let rows = sqlx::query(&query)
        .fetch_all(pool)
        .await
        .map_err(|e| ProxyError::Database(e))?;
    Ok(rows.into_iter().map(|r| summary_of(&row_to_account(r))).collect())
}

/// Per-provider refresh locks: concurrent requests hitting an expiring token
/// trigger exactly one network refresh.
static REFRESH_LOCKS: Lazy<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

async fn refresh_lock_for(provider_id: &str) -> Arc<Mutex<()>> {
    let mut map = REFRESH_LOCKS.lock().await;
    map.entry(provider_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn decrypt_blob(encrypted: &[u8], nonce: &[u8], what: &str) -> Result<String, ProxyError> {
    if nonce.len() != NONCE_SIZE {
        return Err(ProxyError::KeyManagement(format!(
            "codex {} nonce 长度无效",
            what
        )));
    }
    let mut nonce_arr = [0u8; NONCE_SIZE];
    nonce_arr.copy_from_slice(nonce);
    decrypt_api_key(encrypted, &nonce_arr)
}

/// Resolve a valid bearer token (+ account id) for a Codex provider,
/// refreshing proactively when the stored token is within the refresh margin
/// of expiry. Permanently-failed refreshes flip the account to `expired`.
pub async fn get_valid_access_token(
    provider_id: &str,
) -> Result<(String, Option<String>), ProxyError> {
    let row = get_by_provider(provider_id)
        .await?
        .ok_or_else(|| {
            ProxyError::Provider("Codex 渠道尚未登录，请先在供应商管理中完成登录".into())
        })?;

    let access = decrypt_blob(&row.encrypted_access_token, &row.access_nonce, "access token")?;
    let now = chrono::Utc::now().timestamp();
    let fresh = row.status == "active"
        && row
            .expires_at
            .map(|exp| now < exp - REFRESH_MARGIN_SECS)
            .unwrap_or(true);
    if fresh {
        return Ok((access, row.chatgpt_account_id.clone()));
    }

    let lock = refresh_lock_for(provider_id).await;
    let _guard = lock.lock().await;

    // Double-check: another request may have refreshed while we waited.
    let row = get_by_provider(provider_id).await?.ok_or_else(|| {
        ProxyError::Provider("Codex 渠道尚未登录，请先在供应商管理中完成登录".into())
    })?;
    let now = chrono::Utc::now().timestamp();
    let fresh = row.status == "active"
        && row
            .expires_at
            .map(|exp| now < exp - REFRESH_MARGIN_SECS)
            .unwrap_or(true);
    if fresh {
        let access =
            decrypt_blob(&row.encrypted_access_token, &row.access_nonce, "access token")?;
        return Ok((access, row.chatgpt_account_id.clone()));
    }

    let refresh = decrypt_blob(
        &row.encrypted_refresh_token,
        &row.refresh_nonce,
        "refresh token",
    )?;
    match oauth::refresh_tokens(&refresh).await {
        Ok(tokens) => {
            tracing::info!("[codex] provider {} access token refreshed", provider_id);
            update_tokens(&row, &tokens).await?;
            let claims = jwt::parse_id_token(&tokens.id_token).ok();
            let account_id = claims
                .as_ref()
                .and_then(|c| c.chatgpt_account_id.clone())
                .or(row.chatgpt_account_id.clone());
            Ok((tokens.access_token, account_id))
        }
        Err(RefreshError::Permanent(reason)) => {
            tracing::warn!(
                "[codex] provider {} refresh permanently failed: {}",
                provider_id,
                reason
            );
            let _ = set_status(&row.id, "expired").await;
            Err(ProxyError::Provider(format!(
                "Codex 登录已失效（{}），请重新登录",
                reason
            )))
        }
        Err(RefreshError::Transient(reason)) => {
            // Keep the previous token usable when the failure is transient and
            // the token itself has not technically expired yet.
            let still_valid = row
                .expires_at
                .map(|exp| chrono::Utc::now().timestamp() < exp)
                .unwrap_or(false);
            if still_valid {
                tracing::warn!(
                    "[codex] provider {} refresh failed transiently ({}), using current token",
                    provider_id,
                    reason
                );
                return Ok((access, row.chatgpt_account_id.clone()));
            }
            Err(ProxyError::Network(format!("Codex 令牌刷新失败: {}", reason)))
        }
    }
}

fn row_has_id_token(row: &CodexAccountRow) -> bool {
    matches!(
        (&row.encrypted_id_token, &row.id_nonce),
        (Some(enc), Some(nonce)) if !enc.is_empty() && nonce.len() == NONCE_SIZE
    )
}

/// Decrypt stored tokens into the shape needed for an official `auth.json`.
/// Legacy rows without `id_token` trigger a refresh first; permanent refresh
/// failure means the user must re-login in 供应商管理.
pub async fn tokens_for_auth_json(provider_id: &str) -> Result<OfficialAuthTokens, ProxyError> {
    let row = get_by_provider(provider_id).await?.ok_or_else(|| {
        ProxyError::Provider("该渠道未绑定 Codex 账号，请先在供应商管理中登录".into())
    })?;
    if row.status != "active" {
        return Err(ProxyError::Provider(
            "Codex 登录已失效，请先在供应商管理中重新登录".into(),
        ));
    }

    if !row_has_id_token(&row) {
        tracing::info!(
            "[codex] provider {} missing id_token; refreshing before auth.json export",
            provider_id
        );
        force_refresh(provider_id).await?;
    }

    let row = get_by_provider(provider_id).await?.ok_or_else(|| {
        ProxyError::Provider("该渠道未绑定 Codex 账号，请先在供应商管理中登录".into())
    })?;
    if !row_has_id_token(&row) {
        return Err(ProxyError::Provider(
            "无法获取 id_token（请在供应商管理中重新登录 Codex 官方账号后再注入）".into(),
        ));
    }

    let id_token = decrypt_blob(
        row.encrypted_id_token.as_ref().unwrap(),
        row.id_nonce.as_ref().unwrap(),
        "id token",
    )?;
    let access_token =
        decrypt_blob(&row.encrypted_access_token, &row.access_nonce, "access token")?;
    let refresh_token = decrypt_blob(
        &row.encrypted_refresh_token,
        &row.refresh_nonce,
        "refresh token",
    )?;

    Ok(OfficialAuthTokens {
        id_token,
        access_token,
        refresh_token,
        account_id: row.chatgpt_account_id,
        last_refresh_at: row.last_refresh_at,
    })
}

/// Force a refresh right now (manual "刷新令牌" button). Returns the summary.
pub async fn force_refresh(provider_id: &str) -> Result<CodexAccountSummary, ProxyError> {
    let row = get_by_provider(provider_id)
        .await?
        .ok_or_else(|| ProxyError::Provider("该渠道未绑定 Codex 账号".into()))?;
    let refresh = decrypt_blob(
        &row.encrypted_refresh_token,
        &row.refresh_nonce,
        "refresh token",
    )?;
    match oauth::refresh_tokens(&refresh).await {
        Ok(tokens) => {
            update_tokens(&row, &tokens).await?;
            summary_for_provider(provider_id)
                .await?
                .ok_or_else(|| ProxyError::Provider("刷新后读取账号失败".into()))
        }
        Err(RefreshError::Permanent(reason)) => {
            let _ = set_status(&row.id, "expired").await;
            Err(ProxyError::Provider(format!(
                "刷新失败，登录已失效（{}），请重新登录",
                reason
            )))
        }
        Err(RefreshError::Transient(reason)) => Err(ProxyError::Network(format!(
            "刷新失败（临时错误）: {}",
            reason
        ))),
    }
}

/// Whether this provider row is a Codex OAuth channel.
pub fn is_codex_provider(kind: &str) -> bool {
    kind == PROVIDER_KIND_CODEX
}
