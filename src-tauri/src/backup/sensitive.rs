/// Fixed allowlist of sensitive field paths (not heuristic scanning).
///
/// - `api_keys` rows: columns `encrypted_key` + `nonce` hold master-key
///   ciphertext; on export we decrypt→re-encrypt with the passphrase key.
/// - `settings` rows: when `key` == `proxy_auth_key`, the `value` is plaintext
///   and must be passphrase-encrypted.
/// - `mcp_servers` rows: `env` and `headers` columns are JSON strings that may
///   contain token-like fields; we encrypt the whole JSON blob when present.
pub const SENSITIVE_SETTING_KEYS: &[&str] = &["proxy_auth_key"];

pub fn is_sensitive_setting_key(key: &str) -> bool {
    SENSITIVE_SETTING_KEYS.contains(&key)
}

/// MCP token-like field names recognized inside `env`/`headers` JSON.
/// When any of these keys exist in the JSON object, the whole blob is
/// treated as sensitive (encrypted wholesale, not field-by-field).
pub const MCP_SENSITIVE_FIELDS: &[&str] = &["token", "secret", "apiKey", "api_key"];

/// Returns true if the JSON string contains any sensitive MCP field name as a key.
pub fn mcp_json_has_secret(json_str: &str) -> bool {
    for f in MCP_SENSITIVE_FIELDS {
        // Match JSON object key:  "token"  /  "apiKey" etc.
        let needle = format!("\"{}\"", f);
        if json_str.contains(&needle) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sensitive_setting_keys() {
        assert!(is_sensitive_setting_key("proxy_auth_key"));
        assert!(!is_sensitive_setting_key("http_port"));
    }

    #[test]
    fn test_mcp_secret_detection() {
        assert!(mcp_json_has_secret(r#"{"token":"abc"}"#));
        assert!(mcp_json_has_secret(r#"{"apiKey":"x"}"#));
        assert!(!mcp_json_has_secret(r#"{"command":"node"}"#));
        assert!(!mcp_json_has_secret(""));
    }
}
