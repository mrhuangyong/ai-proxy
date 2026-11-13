//! Minimal id_token (JWT) claims reader for Codex OAuth logins.
//!
//! Only the payload claims the channel needs are extracted: the ChatGPT
//! account id, email, plan type and expiry. No signature verification — the
//! token comes straight from the HTTPS token endpoint over TLS.

use base64::Engine;
use serde_json::Value;

use crate::error::ProxyError;

/// Claims extracted from a Codex `id_token`.
#[derive(Debug, Clone, Default)]
pub struct IdTokenClaims {
    pub chatgpt_account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    /// `exp` claim (unix seconds) — drives proactive token refresh.
    pub exp: Option<i64>,
}

/// The auth claims live under this namespaced key in official id_tokens; some
/// payloads also carry the values at the top level, so both are checked.
const AUTH_CLAIM_KEY: &str = "https://api.openai.com/auth";

/// Decode the payload section of a JWT and pull the Codex-relevant claims.
pub fn parse_id_token(id_token: &str) -> Result<IdTokenClaims, ProxyError> {
    let payload_b64 = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| ProxyError::Config("id_token 格式无效：缺少 payload 段".into()))?;

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| ProxyError::Config(format!("id_token payload 解码失败: {}", e)))?;
    let json: Value = serde_json::from_slice(&payload)
        .map_err(|e| ProxyError::Config(format!("id_token payload 解析失败: {}", e)))?;

    // Values may sit at the top level or inside the namespaced auth claim.
    let auth = json.get(AUTH_CLAIM_KEY).cloned().unwrap_or(Value::Null);
    let str_field = |key: &str| -> Option<String> {
        json.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| auth.get(key).and_then(Value::as_str).map(str::to_string))
    };

    Ok(IdTokenClaims {
        chatgpt_account_id: str_field("chatgpt_account_id"),
        email: str_field("email"),
        plan_type: str_field("chatgpt_plan_type"),
        exp: json.get("exp").and_then(Value::as_i64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn make_id_token(payload: &Value) -> String {
        let encode = |bytes: &[u8]| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        };
        format!(
            "{}.{}.{}",
            encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            encode(serde_json::to_string(payload).unwrap().as_bytes()),
            "signature"
        )
    }

    #[test]
    fn parses_namespaced_auth_claim() {
        let token = make_id_token(&serde_json::json!({
            "iss": "https://auth.openai.com",
            "exp": 1_800_000_000i64,
            "email": "user@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc-123",
                "chatgpt_plan_type": "plus"
            }
        }));
        let claims = parse_id_token(&token).unwrap();
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acc-123"));
        assert_eq!(claims.plan_type.as_deref(), Some("plus"));
        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
        assert_eq!(claims.exp, Some(1_800_000_000));
    }

    #[test]
    fn parses_top_level_claims() {
        let token = make_id_token(&serde_json::json!({
            "exp": 1_700_000_000i64,
            "chatgpt_account_id": "acc-top",
            "chatgpt_plan_type": "pro"
        }));
        let claims = parse_id_token(&token).unwrap();
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acc-top"));
        assert_eq!(claims.plan_type.as_deref(), Some("pro"));
        assert_eq!(claims.email, None);
    }

    #[test]
    fn rejects_malformed_token() {
        assert!(parse_id_token("not-a-jwt").is_err());
    }
}
