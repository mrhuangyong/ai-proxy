//! Codex official (ChatGPT OAuth) channel support.
//!
//! The app acts as a Codex client: it performs the official PKCE OAuth login
//! against `auth.openai.com`, stores the resulting tokens encrypted, and
//! forwards downstream traffic to `https://chatgpt.com/backend-api/codex`
//! with the refreshed bearer token plus the ChatGPT account headers.
//!
//! Constants mirror the upstream `openai/codex` client implementation.

pub mod jwt;
pub mod login;
pub mod oauth;
pub mod refresh_scheduler;
pub mod store;
pub mod usage;

/// OAuth client id of the official Codex CLI (openai/codex `codex-rs/login`).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// OAuth issuer (authorize + token endpoints hang off this base).
pub const ISSUER: &str = "https://auth.openai.com";
pub const AUTHORIZE_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
/// Local loopback port the official CLI uses for the login callback; 1457 is
/// its registered fallback when 1455 is taken.
pub const DEFAULT_CALLBACK_PORT: u16 = 1455;
pub const FALLBACK_CALLBACK_PORT: u16 = 1457;
pub const CALLBACK_PATH: &str = "/auth/callback";

/// `originator` identifying the official Rust CLI.
pub const ORIGINATOR: &str = "codex_cli_rs";
pub const HEADER_ORIGINATOR: &str = "originator";
pub const HEADER_OPENAI_BETA: &str = "OpenAI-Beta";
pub const OPENAI_BETA_VALUE: &str = "responses=experimental";
pub const HEADER_CHATGPT_ACCOUNT_ID: &str = "chatgpt-account-id";

/// ChatGPT backend for Codex requests. The Responses endpoint is
/// `{DEFAULT_BASE_URL}/responses`; image generation/edits hang off the same
/// base (`{DEFAULT_BASE_URL}/images/...`).
pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const DEFAULT_ENDPOINT_PATH: &str = "/responses";

/// ChatGPT subscription rate-limit usage endpoint (`AI_PROXY_CODEX_USAGE_URL`
/// overrides it for tests).
pub const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// `providers.provider_kind` values (migration 031).
pub const PROVIDER_KIND_STANDARD: &str = "standard";
pub const PROVIDER_KIND_CODEX: &str = "codex_oauth";

/// Default upstream User-Agent for Codex channels (approximates the official
/// CLI format; the exact string is not validated server-side, the originator
/// header carries the client identity).
pub fn default_user_agent() -> String {
    format!(
        "{}/0.55.0 ({} {}) Terminal",
        ORIGINATOR,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Seed model slugs for a newly created Codex channel (user-editable).
/// Kept in sync with the ChatGPT-auth picker from a current Codex
/// `~/.codex/models_cache.json` (visibility=list only; hide internals).
pub const DEFAULT_MODELS: &[&str] = &[
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6.1-terra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
];

/// Refresh this many seconds before the stored expiry (official clients
/// proactively refresh 5 minutes ahead).
pub const REFRESH_MARGIN_SECS: i64 = 5 * 60;

use crate::error::ProxyError;
use crate::provider::manager::ResolvedRoute;
use std::collections::HashMap;

/// Resolve the outbound bearer material for a provider.
///
/// For Codex OAuth channels this returns `Some(access_token)` after proactive
/// refresh and injects the required ChatGPT headers (`chatgpt-account-id`,
/// `OpenAI-Beta`, `originator`, default UA when unset) into `extra_headers`.
/// Standard providers return `None` and are untouched — their static API-key
/// path keeps working exactly as before.
///
/// Call this once per request, after interceptor/UA header assembly and before
/// the passthrough/conversion branch: both share `extra_headers`, and the
/// returned token replaces the (empty) static key everywhere a bearer is set.
pub async fn resolve_outbound_auth(
    route: &ResolvedRoute,
    extra_headers: &mut HashMap<String, String>,
) -> Result<Option<String>, ProxyError> {
    if route.provider_kind != PROVIDER_KIND_CODEX {
        return Ok(None);
    }
    let (token, account_id) = store::get_valid_access_token(&route.provider_id).await?;
    if let Some(account) = account_id.filter(|a| !a.is_empty()) {
        extra_headers.insert(HEADER_CHATGPT_ACCOUNT_ID.to_string(), account);
    }
    extra_headers.insert(HEADER_OPENAI_BETA.to_string(), OPENAI_BETA_VALUE.to_string());
    extra_headers
        .entry(HEADER_ORIGINATOR.to_string())
        .or_insert_with(|| ORIGINATOR.to_string());
    if !extra_headers.contains_key("user-agent") && route.upstream_user_agent.is_empty() {
        extra_headers.insert("user-agent".to_string(), default_user_agent());
    }
    Ok(Some(token))
}
