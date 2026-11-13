//! Codex OAuth login orchestration: PKCE session registry, one-shot loopback
//! callback server (port 1455, fallback 1457), system-browser handoff and
//! status polling for the frontend.
//!
//! Only one login session is active at a time (the callback port is shared).

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Html};
use axum::routing::get;
use axum::Router;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};

use crate::error::ProxyError;

use super::jwt;
use super::oauth::{self, PkceCodes};
use super::store::{self, CodexAccountSummary};
use super::{CALLBACK_PATH, DEFAULT_CALLBACK_PORT, FALLBACK_CALLBACK_PORT};

/// Total window for the user to complete the browser login.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long a terminal (completed/failed) session stays queryable after the
/// loopback server stops. The frontend polls every ~1.5s; without this grace
/// window it often misses `completed` because we shut down + remove the
/// session immediately after the callback.
const SESSION_STATUS_RETENTION: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginStatus {
    Pending,
    /// Nested `account` (not flattened): `CodexAccountSummary` also has a
    /// `status` field (`active`/`expired`), which would overwrite this tag
    /// if the summary were inlined — leaving the UI stuck on "pending".
    Completed { account: CodexAccountSummary },
    Failed { error: String },
}

pub struct LoginSession {
    pub login_id: String,
    pub provider_id: String,
    pub pkce: PkceCodes,
    pub state: String,
    pub redirect_uri: String,
    pub port: u16,
    pub status: Mutex<LoginStatus>,
}

static SESSIONS: Lazy<Mutex<HashMap<String, Arc<LoginSession>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// The single active login: its shutdown channel for cancel/timeout.
struct ActiveLogin {
    login_id: String,
    shutdown_tx: watch::Sender<bool>,
}

static ACTIVE: Lazy<Mutex<Option<ActiveLogin>>> = Lazy::new(|| Mutex::new(None));

async fn set_status(login_id: &str, status: LoginStatus) {
    let sessions = SESSIONS.lock().await;
    if let Some(session) = sessions.get(login_id) {
        *session.status.lock().await = status;
    }
}

async fn get_status(login_id: &str) -> Option<LoginStatus> {
    let sessions = SESSIONS.lock().await;
    let session = sessions.get(login_id)?;
    let status = session.status.lock().await.clone();
    Some(status)
}

/// Start a login: cancel any in-flight session, bind the loopback callback
/// server, build the authorize URL and open the system browser.
pub async fn start_login(provider_id: &str) -> Result<(String, String), ProxyError> {
    // One login at a time: shut down any previous session first.
    cancel_active("已发起新的登录").await;

    let pkce = oauth::generate_pkce();
    let state = oauth::generate_state();

    // Bind 127.0.0.1:1455, falling back to the official 1457 port.
    let (listener, port) = {
        let mut last_err = None;
        let mut bound = None;
        for port in [DEFAULT_CALLBACK_PORT, FALLBACK_CALLBACK_PORT] {
            match tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await {
                Ok(l) => {
                    bound = Some((l, port));
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        match bound {
            Some(pair) => pair,
            None => {
                return Err(ProxyError::Config(format!(
                    "无法绑定 OAuth 回调端口 {}/{}（可能被占用，请关闭占用进程后重试）: {}",
                    DEFAULT_CALLBACK_PORT,
                    FALLBACK_CALLBACK_PORT,
                    last_err.map(|e| e.to_string()).unwrap_or_default()
                )))
            }
        }
    };

    let redirect_uri = format!("http://127.0.0.1:{}{}", port, CALLBACK_PATH);
    let authorize_url = oauth::build_authorize_url(&pkce, &state, &redirect_uri);

    let login_id = uuid::Uuid::new_v4().to_string();
    let session = Arc::new(LoginSession {
        login_id: login_id.clone(),
        provider_id: provider_id.to_string(),
        pkce,
        state,
        redirect_uri: redirect_uri.clone(),
        port,
        status: Mutex::new(LoginStatus::Pending),
    });
    SESSIONS.lock().await.insert(login_id.clone(), session.clone());

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    {
        let mut active = ACTIVE.lock().await;
        *active = Some(ActiveLogin {
            login_id: login_id.clone(),
            shutdown_tx: shutdown_tx.clone(),
        });
    }

    let app = Router::new()
        .route(CALLBACK_PATH, get(handle_callback))
        .with_state(session);
    let login_id_for_server = login_id.clone();
    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
        });
        // Hard timeout: a login that never completes must not leave a server
        // bound forever.
        let _ = tokio::time::timeout(LOGIN_TIMEOUT, server).await;
        // If still pending after the window (or shutdown), fail it.
        let still_pending = {
            let sessions = SESSIONS.lock().await;
            sessions
                .get(&login_id_for_server)
                .map(|s| {
                    s.status
                        .try_lock()
                        .map(|g| matches!(&*g, LoginStatus::Pending))
                        .unwrap_or(true)
                })
                .unwrap_or(false)
        };
        if still_pending {
            set_status(
                &login_id_for_server,
                LoginStatus::Failed {
                    error: "登录超时（10 分钟未完成浏览器授权）".into(),
                },
            )
            .await;
        }
        // Only retire the active slot if a NEWER login hasn't replaced it.
        {
            let mut active = ACTIVE.lock().await;
            if active.as_ref().map(|a| a.login_id.as_str()) == Some(login_id_for_server.as_str()) {
                active.take();
            }
        }
        // Keep Completed/Failed visible to status polls; drop after grace.
        tokio::time::sleep(SESSION_STATUS_RETENTION).await;
        SESSIONS.lock().await.remove(&login_id_for_server);
    });

    // Best-effort browser open; the frontend also shows the URL as a manual
    // fallback (and for headless/server deployments).
    let opened = open_in_browser(&authorize_url);
    if !opened {
        tracing::info!("[codex] could not spawn a browser; user must open the authorize URL manually");
    }

    Ok((login_id, authorize_url))
}

/// Process `GET /auth/callback`: validate state, exchange the code, persist
/// the tokens and flip the session to completed/failed.
async fn handle_callback(
    State(session): State<Arc<LoginSession>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Some(error) = params.get("error").filter(|e| !e.is_empty()) {
        let description = params
            .get("error_description")
            .map(|d| format!(": {}", d))
            .unwrap_or_default();
        let message = format!("授权失败 {}{}", error, description);
        *session.status.lock().await = LoginStatus::Failed { error: message.clone() };
        cancel_active_session(&session.login_id).await;
        return Html(login_error_html(&message));
    }

    let Some(code) = params.get("code").filter(|c| !c.is_empty()) else {
        let message = String::from("回调缺少授权码，登录未完成");
        *session.status.lock().await = LoginStatus::Failed { error: message.clone() };
        cancel_active_session(&session.login_id).await;
        return Html(login_error_html(&message));
    };

    // CSRF protection: the state echoed by the IdP must match ours.
    let state_ok = params
        .get("state")
        .map(|s| constant_time_eq(s.as_bytes(), session.state.as_bytes()))
        .unwrap_or(false);
    if !state_ok {
        // State mismatch is not fatal for a *different* pending session's
        // straggler callback, but we do fail this login.
        let message = String::from("回调 state 校验失败，请重新发起登录");
        *session.status.lock().await = LoginStatus::Failed { error: message.clone() };
        cancel_active_session(&session.login_id).await;
        return Html(login_error_html(&message));
    }

    match oauth::exchange_code(code, &session.redirect_uri, &session.pkce).await {
        Ok(tokens) => {
            let claims = jwt::parse_id_token(&tokens.id_token).ok();
            match store::upsert_tokens(
                &session.provider_id,
                &tokens,
                None,
                None,
                None,
            )
            .await
            {
                Ok(summary) => {
                    let email = summary.email.clone().unwrap_or_else(|| "未知账号".into());
                    tracing::info!(
                        "[codex] provider {} login completed (account: {})",
                        session.provider_id,
                        email
                    );
                    *session.status.lock().await =
                        LoginStatus::Completed { account: summary };
                    cancel_active_session(&session.login_id).await;
                    Html(login_success_html(
                        email.as_str(),
                        claims.as_ref().and_then(|c| c.plan_type.as_deref()).unwrap_or(""),
                    ))
                }
                Err(e) => {
                    let message = format!("凭证保存失败: {}", e);
                    *session.status.lock().await = LoginStatus::Failed { error: message.clone() };
                    cancel_active_session(&session.login_id).await;
                    Html(login_error_html(&message))
                }
            }
        }
        Err(e) => {
            let message = format!("授权码交换失败: {}", e);
            *session.status.lock().await = LoginStatus::Failed { error: message.clone() };
            cancel_active_session(&session.login_id).await;
            Html(login_error_html(&message))
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn cancel_active_session(login_id: &str) {
    let mut active = ACTIVE.lock().await;
    if active.as_ref().map(|a| a.login_id.as_str()) == Some(login_id) {
        if let Some(a) = active.take() {
            let _ = a.shutdown_tx.send(true);
        }
    }
}

/// Cancel whatever login is in flight (used by the cancel endpoint and when a
/// new login starts).
pub async fn cancel_active(reason: &str) {
    let mut active = ACTIVE.lock().await;
    if let Some(a) = active.take() {
        let _ = a.shutdown_tx.send(true);
        set_status(&a.login_id, LoginStatus::Failed { error: reason.to_string() }).await;
    }
}

/// Poll a login session's status. `None` when the id is unknown.
pub async fn login_status(login_id: &str) -> Option<LoginStatus> {
    get_status(login_id).await
}

/// Import credentials from the local official Codex CLI login
/// (`~/.codex/auth.json`) into a provider's account row.
pub async fn import_from_local_auth(provider_id: &str) -> Result<CodexAccountSummary, ProxyError> {
    let home = dirs::home_dir()
        .ok_or_else(|| ProxyError::Config("无法定位用户主目录".into()))?;
    let path = home.join(".codex").join("auth.json");
    let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
        ProxyError::Config(format!(
            "读取本机 Codex 登录失败（{}）: {}。请确认本机 codex CLI 已用 ChatGPT 账号登录",
            path.display(),
            e
        ))
    })?;
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| ProxyError::Config(format!("auth.json 解析失败: {}", e)))?;

    let tokens_obj = parsed
        .get("tokens")
        .filter(|t| t.is_object())
        .ok_or_else(|| {
            ProxyError::Config(
                "auth.json 中没有 ChatGPT OAuth tokens（本机 codex 可能使用 API Key 模式登录）".into(),
            )
        })?;
    let token_set = oauth::TokenSet {
        id_token: tokens_obj
            .get("id_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProxyError::Config("auth.json tokens 缺少 id_token".into()))?
            .to_string(),
        access_token: tokens_obj
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProxyError::Config("auth.json tokens 缺少 access_token".into()))?
            .to_string(),
        refresh_token: tokens_obj
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProxyError::Config("auth.json tokens 缺少 refresh_token".into()))?
            .to_string(),
    };
    let account_id = tokens_obj
        .get("account_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    store::upsert_tokens(
        provider_id,
        &token_set,
        if account_id.is_empty() { None } else { Some(account_id) },
        None,
        None,
    )
    .await
}

/// Best-effort system-browser open (mirrors the apps launcher approach).
fn open_in_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .is_ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = url;
        false
    }
}

fn login_success_html(email: &str, plan: &str) -> String {
    let plan_chip = if plan.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="chip">{}</span>"#, html_escape(plan))
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>登录成功</title>
<style>
body {{ font-family: -apple-system, "Segoe UI", sans-serif; background:#0d1117; color:#e6edf3;
       display:flex; align-items:center; justify-content:center; height:100vh; margin:0; }}
.card {{ background:#161b22; border:1px solid #30363d; border-radius:12px; padding:40px 48px;
        text-align:center; box-shadow:0 8px 30px rgba(0,0,0,.4); }}
h1 {{ margin:0 0 8px; font-size:22px; }}
p {{ margin:6px 0; color:#8b949e; }}
.chip {{ display:inline-block; margin-top:10px; padding:2px 10px; border-radius:999px;
        background:#1f6feb33; color:#58a6ff; font-size:13px; }}
.ok {{ color:#3fb950; font-size:34px; }}
</style></head>
<body><div class="card">
<div class="ok">✓</div>
<h1>Codex 登录成功</h1>
<p>{email} 已绑定到本应用的 Codex 渠道，可以关闭此页面。</p>
{plan_chip}
</div></body></html>"#,
        email = html_escape(email),
        plan_chip = plan_chip
    )
}

fn login_error_html(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>登录失败</title>
<style>
body {{ font-family: -apple-system, "Segoe UI", sans-serif; background:#0d1117; color:#e6edf3;
       display:flex; align-items:center; justify-content:center; height:100vh; margin:0; }}
.card {{ background:#161b22; border:1px solid #30363d; border-radius:12px; padding:40px 48px;
        max-width:520px; text-align:center; }}
h1 {{ margin:0 0 12px; font-size:20px; color:#f85149; }}
p {{ margin:6px 0; color:#8b949e; word-break:break-all; }}
</style></head>
<body><div class="card">
<h1>Codex 登录失败</h1>
<p>{message}</p>
<p>请返回应用重新发起登录。</p>
</div></body></html>"#,
        message = html_escape(message)
    )
}

fn html_escape(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn completed_status_tag_is_not_overwritten_by_account_status() {
        let status = LoginStatus::Completed {
            account: CodexAccountSummary {
                id: "a".into(),
                provider_id: "p".into(),
                chatgpt_account_id: Some("c".into()),
                email: Some("u@example.com".into()),
                plan_type: Some("plus".into()),
                expires_at: None,
                last_refresh_at: None,
                status: "active".into(),
            },
        };
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["status"], "completed");
        assert_eq!(v["account"]["status"], "active");
        assert_eq!(v["account"]["email"], "u@example.com");
    }

    #[test]
    fn html_pages_escape_interpolation() {
        let html = login_success_html("<script>x</script>", "plus");
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
        let err = login_error_html("boom & <bad>");
        assert!(err.contains("boom &amp; &lt;bad&gt;"));
    }
}
