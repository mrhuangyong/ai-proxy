//! Codex OAuth protocol: PKCE generation, authorize URL, code exchange and
//! refresh-token grant. Mirrors `openai/codex` `codex-rs/login`.
//!
//! Wire details that must not drift:
//! - code exchange is **form-encoded**; refresh is **JSON** (45s timeout).
//! - redirect_uri must match the loopback callback server exactly.

use base64::Engine;
use ring::digest::{digest, SHA256};
use serde::Deserialize;
use std::time::Duration;

use crate::error::ProxyError;
use crate::http::SHARED_HTTP_CLIENT;

use super::{CLIENT_ID, ISSUER};

/// PKCE proof pair (verifier never leaves this process except in the token
/// POST; only the S256 challenge goes into the authorize URL).
#[derive(Debug, Clone)]
pub struct PkceCodes {
    pub code_verifier: String,
    pub code_challenge: String,
}

/// Tokens returned by the OAuth endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenSet {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
}

/// Refresh failures that mean "re-login required" vs "try again later".
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("{0}")]
    Permanent(String),
    #[error("{0}")]
    Transient(String),
}

fn uuid_fallback_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    for chunk in bytes.chunks_mut(16) {
        let u = *uuid::Uuid::new_v4().as_bytes();
        chunk.copy_from_slice(&u[..chunk.len()]);
    }
    bytes
}

pub fn generate_pkce() -> PkceCodes {
    let rng = ring::rand::SystemRandom::new();
    // SystemRandom failure is effectively impossible; fall back to uuid-based
    // entropy rather than panicking inside a request handler.
    // `generate` needs an explicit `[u8; N]` — it does not infer from a slice sink.
    let bytes: [u8; 64] = match ring::rand::generate(&rng) {
        Ok(r) => r.expose(),
        Err(_) => uuid_fallback_bytes(),
    };

    // Verifier: URL-safe base64 without padding (86 chars, within 43..128).
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge_digest = digest(&SHA256, code_verifier.as_bytes());
    let code_challenge =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(challenge_digest.as_ref());
    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

pub fn generate_state() -> String {
    let rng = ring::rand::SystemRandom::new();
    let bytes: [u8; 32] = match ring::rand::generate(&rng) {
        Ok(r) => r.expose(),
        Err(_) => uuid_fallback_bytes(),
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Effective OAuth token endpoint. `AI_PROXY_CODEX_TOKEN_URL` overrides it —
/// mirrors the official client's `CODEX_REFRESH_TOKEN_URL_OVERRIDE` and is
/// what the integration tests use to point grants at a mock IdP.
pub fn token_endpoint() -> String {
    std::env::var("AI_PROXY_CODEX_TOKEN_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{}/oauth/token", ISSUER))
}

/// Minimal percent-encoding for query values (all our values are ASCII-safe
/// apart from spaces in the scope string, but encode defensively).
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Build the authorize URL exactly as the official client does, including the
/// extra parameters Codex backends expect (`originator` etc.).
pub fn build_authorize_url(pkce: &PkceCodes, state: &str, redirect_uri: &str) -> String {
    let mut url = format!(
        "{}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&scope={}",
        ISSUER,
        encode_query_value(CLIENT_ID),
        encode_query_value(redirect_uri),
        encode_query_value(&pkce.code_challenge),
        encode_query_value(state),
        encode_query_value(super::AUTHORIZE_SCOPE),
    );
    url.push_str("&id_token_add_organizations=true");
    url.push_str("&codex_cli_simplified_flow=true");
    url.push_str(&format!(
        "&originator={}",
        encode_query_value(super::ORIGINATOR)
    ));
    url
}

#[derive(Deserialize)]
struct TokenEndpointError {
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn describe_rejection(status: u16, body: &str) -> String {
    match serde_json::from_str::<TokenEndpointError>(body) {
        Ok(parsed) => {
            let code = parsed.error.unwrap_or_else(|| "unknown_error".into());
            match parsed.error_description {
                Some(desc) if !desc.trim().is_empty() => format!("{}: {}", code, desc),
                _ => code,
            }
        }
        Err(_) => format!("HTTP {} {}", status, &body.chars().take(200).collect::<String>()),
    }
}

/// Exchange an authorization code for the initial token set (form-encoded).
pub async fn exchange_code(
    code: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
) -> Result<TokenSet, ProxyError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", pkce.code_verifier.as_str()),
    ];
    let response = SHARED_HTTP_CLIENT
        .post(token_endpoint())
        .header("Content-Type", "application/x-www-form-urlencoded")
        // Form string (not `.form()`) so ordering matches the official client.
        .body(form_urlencoded(&form))
        .send()
        .await
        .map_err(|e| ProxyError::Network(format!("OAuth 授权码交换请求失败: {}", e)))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| ProxyError::Network(format!("读取授权码交换响应失败: {}", e)))?;
    if status != 200 {
        return Err(ProxyError::Provider(format!(
            "OAuth 授权码交换失败: {}",
            describe_rejection(status, &body)
        )));
    }
    serde_json::from_str(&body)
        .map_err(|e| ProxyError::Config(format!("OAuth 授权码交换响应解析失败: {}", e)))
}

/// Refresh an access token via the refresh-token grant (JSON-encoded, 45s
/// timeout — matching the official client's refresh transport settings).
pub async fn refresh_tokens(refresh_token: &str) -> Result<TokenSet, RefreshError> {
    let payload = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": CLIENT_ID,
        "refresh_token": refresh_token,
    });
    let response = SHARED_HTTP_CLIENT
        .post(token_endpoint())
        .timeout(Duration::from_secs(45))
        .json(&payload)
        .send()
        .await
        .map_err(|e| RefreshError::Transient(format!("刷新请求网络失败: {}", e)))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| RefreshError::Transient(format!("读取刷新响应失败: {}", e)))?;

    if status == 200 {
        return serde_json::from_str(&body)
            .map_err(|e| RefreshError::Transient(format!("刷新响应解析失败: {}", e)));
    }
    // 400 invalid_grant / 401 → the refresh token is dead: re-login required.
    // Anything else (5xx, network flakes) is worth retrying later.
    let detail = describe_rejection(status, &body);
    if status == 400 || status == 401 {
        Err(RefreshError::Permanent(detail))
    } else {
        Err(RefreshError::Transient(detail))
    }
}

/// application/x-www-form-urlencoded serializer (no `url` crate dependency).
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                encode_query_value(k),
                encode_query_value(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn pkce_verifier_matches_challenge() {
        let pkce = generate_pkce();
        assert!(
            (43..=128).contains(&pkce.code_verifier.len()),
            "verifier length {} out of RFC 7636 range",
            pkce.code_verifier.len()
        );
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(digest(&SHA256, pkce.code_verifier.as_bytes()).as_ref());
        assert_eq!(pkce.code_challenge, expected);
    }

    #[test]
    fn pkce_pairs_are_unique() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.code_verifier, b.code_verifier);
    }

    #[test]
    fn state_is_url_safe_and_unique() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn authorize_url_carries_all_parameters() {
        let pkce = PkceCodes {
            code_verifier: "verifier-value".into(),
            code_challenge: "challenge-value".into(),
        };
        let url = build_authorize_url(&pkce, "state-1", "http://127.0.0.1:1455/auth/callback");
        assert!(url.starts_with(&format!("{}/oauth/authorize?", ISSUER)));
        for needle in [
            "response_type=code",
            &format!("client_id={}", CLIENT_ID),
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback",
            "code_challenge=challenge-value",
            "code_challenge_method=S256",
            "state=state-1",
            "scope=openid%20profile%20email%20offline_access",
            "id_token_add_organizations=true",
            "codex_cli_simplified_flow=true",
            &format!("originator={}", super::super::ORIGINATOR),
        ] {
            assert!(url.contains(needle), "authorize URL missing {needle}: {url}");
        }
    }

    #[test]
    fn form_encoding_is_ordered_and_escaped() {
        let body = form_urlencoded(&[("grant_type", "authorization_code"), ("code", "a b&c")]);
        assert_eq!(body, "grant_type=authorization_code&code=a%20b%26c");
    }
}
