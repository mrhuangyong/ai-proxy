use crate::converter::ir::ClientFormat;
use crate::converter::sanitize::ModelCapabilities;
use crate::db::get_pool;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::endpoint::{ApiKeyInfo, Provider, ProviderModel, ProviderProtocol};

/// One upstream protocol a provider speaks, with the effective base URL and
/// final endpoint path already resolved (override or per-format default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedProtocol {
    pub format: ClientFormat,
    /// Effective base URL: the protocol's override when set, else the
    /// provider-level default.
    pub base_url: String,
    pub endpoint_path: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRoute {
    pub provider_id: String,
    pub provider_name: String,
    pub base_url: String,
    pub target_format: ClientFormat,
    pub target_model: String,
    pub endpoint_path: String,
    pub upstream_user_agent: String,
    pub capabilities: ModelCapabilities,
    /// All configured upstream protocols (primary first). Used to decide
    /// whether a request can be forwarded as-is (passthrough) when the
    /// downstream client protocol matches one of them.
    #[serde(default)]
    pub protocols: Vec<ResolvedProtocol>,
}

impl ResolvedRoute {
    /// The protocol config for `format`, if this provider speaks it.
    pub fn protocol_for(&self, format: &ClientFormat) -> Option<&ResolvedProtocol> {
        self.protocols.iter().find(|p| &p.format == format)
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct DbProvider {
    id: String,
    name: String,
    base_url: String,
    format: String,
    endpoint_path: Option<String>,
    enabled: i64,
    upstream_user_agent: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct DbProviderProtocol {
    format: String,
    base_url: Option<String>,
    endpoint_path: Option<String>,
    is_primary: i64,
}

/// Normalize a stored endpoint path: empty → None (use the per-format
/// default), otherwise ensure it starts with '/'.
fn normalize_endpoint_path(p: Option<String>) -> Option<String> {
    p.filter(|s| !s.is_empty()).map(|s| {
        if s.starts_with('/') {
            s
        } else {
            format!("/{}", s)
        }
    })
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct DbProviderModel {
    id: String,
    provider_id: String,
    model_name: String,
    target_model: Option<String>,
    context_window: i64,
    enabled: i64,
    created_at: String,
    // Capability columns (migration 026). Option<i64> so older rows / partial
    // reads still deserialize; None is treated as the permissive default.
    supports_thinking: Option<i64>,
    supports_tools: Option<i64>,
    supports_temperature: Option<i64>,
    supports_top_p: Option<i64>,
    supports_top_k: Option<i64>,
    supports_presence_penalty: Option<i64>,
    supports_frequency_penalty: Option<i64>,
    supports_seed: Option<i64>,
    supports_response_format: Option<i64>,
    supports_stream_options: Option<i64>,
    supports_stop: Option<i64>,
    max_output_tokens: Option<i64>,
    extra_passthrough: Option<i64>,
}

impl DbProviderModel {
    /// Constant SELECT clause for the capability columns, used by every
    /// query that reads provider_models so they all stay in sync.
    const CAP_COLS: &'static str = "pm.supports_thinking, pm.supports_tools, pm.supports_temperature, pm.supports_top_p, pm.supports_top_k, pm.supports_presence_penalty, pm.supports_frequency_penalty, pm.supports_seed, pm.supports_response_format, pm.supports_stream_options, pm.supports_stop, pm.max_output_tokens, pm.extra_passthrough";

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_thinking: self.supports_thinking.unwrap_or(1) != 0,
            supports_tools: self.supports_tools.unwrap_or(1) != 0,
            supports_temperature: self.supports_temperature.unwrap_or(1) != 0,
            supports_top_p: self.supports_top_p.unwrap_or(1) != 0,
            supports_top_k: self.supports_top_k.unwrap_or(1) != 0,
            supports_presence_penalty: self.supports_presence_penalty.unwrap_or(1) != 0,
            supports_frequency_penalty: self.supports_frequency_penalty.unwrap_or(1) != 0,
            supports_seed: self.supports_seed.unwrap_or(1) != 0,
            supports_response_format: self.supports_response_format.unwrap_or(1) != 0,
            supports_stream_options: self.supports_stream_options.unwrap_or(1) != 0,
            supports_stop: self.supports_stop.unwrap_or(1) != 0,
            max_output_tokens: self.max_output_tokens.map(|v| v as u32),
            extra_passthrough: self.extra_passthrough.unwrap_or(1) != 0,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct DbApiKeyInfo {
    id: String,
    label: String,
    is_active: i64,
    usage_count: i64,
    last_used_at: Option<String>,
    created_at: String,
}

pub struct ProviderManager;

impl ProviderManager {
    pub async fn list() -> Result<Vec<Provider>, crate::error::ProxyError> {
        let pool = get_pool().await;
        let db_providers: Vec<DbProvider> =
            sqlx::query_as("SELECT id, name, base_url, format, endpoint_path, enabled, upstream_user_agent FROM providers ORDER BY name")
                .fetch_all(pool)
                .await
                .map_err(|e| crate::error::ProxyError::Database(e))?;

        let mut providers = Vec::new();
        for p in db_providers {
            let models = Self::fetch_models(&p.id).await?;
            let api_keys = Self::fetch_api_keys_info(&p.id).await?;
            let protocols = Self::fetch_provider_protocols(&p).await?;
            providers.push(Provider {
                id: p.id,
                name: p.name,
                base_url: p.base_url,
                format: p.format,
                endpoint_path: p.endpoint_path,
                enabled: p.enabled != 0,
                upstream_user_agent: p.upstream_user_agent,
                models,
                api_keys,
                protocols,
            });
        }
        Ok(providers)
    }

    #[allow(dead_code)]
    pub async fn get_by_id(provider_id: &str) -> Result<Provider, crate::error::ProxyError> {
        let pool = get_pool().await;
        let p: DbProvider = sqlx::query_as(
            "SELECT id, name, base_url, format, endpoint_path, enabled, upstream_user_agent FROM providers WHERE id = ?",
        )
        .bind(provider_id)
        .fetch_one(pool)
        .await
        .map_err(|e| crate::error::ProxyError::Database(e))?;

        let models = Self::fetch_models(&p.id).await?;
        let api_keys = Self::fetch_api_keys_info(&p.id).await?;
        let protocols = Self::fetch_provider_protocols(&p).await?;

        Ok(Provider {
            id: p.id,
            name: p.name,
            base_url: p.base_url,
            format: p.format,
            endpoint_path: p.endpoint_path,
            enabled: p.enabled != 0,
            upstream_user_agent: p.upstream_user_agent,
            models,
            api_keys,
            protocols,
        })
    }

    pub async fn find_for_model(model: &str) -> Result<ResolvedRoute, crate::error::ProxyError> {
        Self::find_for_model_on_provider(model, None).await
    }

    /// Resolve a model name into a concrete upstream route, optionally
    /// constrained to a specific `provider_id`. When `provider_id` is `None`
    /// the behaviour matches `find_for_model` (picks any enabled provider
    /// that has the model). When provided, the search is restricted to that
    /// provider's `provider_models` rows — this is essential for the test-model
    /// flow where the user explicitly picked a provider to test against.
    ///
    /// When `provider_id` is `None`, the `provider_name/model_name` form is also
    /// supported: if the exact model name is not found and the string contains
    /// `/`, it is split at the FIRST slash into `(provider_name, model_name)`
    /// and resolved against that provider only. Exact model-name matching takes
    /// priority so literal model names that themselves contain `/` (e.g.
    /// `qwen/qwen3.6-27b`) keep working unchanged.
    pub async fn find_for_model_on_provider(
        model: &str,
        provider_id: Option<&str>,
    ) -> Result<ResolvedRoute, crate::error::ProxyError> {
        if let Some(pid) = provider_id {
            return Self::resolve_in_provider(model, Some(pid)).await;
        }

        // provider_id is None: try the whole string as a model name first.
        match Self::resolve_in_provider(model, None).await {
            Ok(route) => return Ok(route),
            Err(_) => {}
        }

        // Fall back to "provider_name/model_name" (split at the FIRST slash).
        if let Some((provider_name, model_name)) = model.split_once('/') {
            let pool = get_pool().await;
            let provider_id: Option<String> = sqlx::query_scalar(
                "SELECT id FROM providers WHERE name = ? COLLATE NOCASE AND enabled = 1 LIMIT 1",
            )
            .bind(provider_name)
            .fetch_optional(pool)
            .await
            .map_err(|e| crate::error::ProxyError::Database(e))?;
            if let Some(pid) = provider_id {
                if let Ok(route) = Self::resolve_in_provider(model_name, Some(&pid)).await {
                    return Ok(route);
                }
            }
        }

        Err(crate::error::ProxyError::Routing(format!(
            "no provider found for model '{}'",
            model
        )))
    }

    /// The core query: resolve `model` within `provider_id` (None = any enabled
    /// provider). Returns the concrete route if exactly one row matches.
    async fn resolve_in_provider(
        model: &str,
        provider_id: Option<&str>,
    ) -> Result<ResolvedRoute, crate::error::ProxyError> {
        let pool = get_pool().await;
        info!("Looking up route for model: {}", model);

        let matched: DbProviderModel = match provider_id {
            Some(pid) => {
                sqlx::query_as(
                    &format!(
                        "SELECT pm.id, pm.provider_id, pm.model_name, pm.target_model, pm.context_window, pm.enabled, pm.created_at, {}
                         FROM provider_models pm
                         JOIN providers p ON p.id = pm.provider_id
                         WHERE pm.model_name = ? COLLATE NOCASE AND pm.enabled = 1 AND p.enabled = 1 AND pm.provider_id = ?
                         LIMIT 1",
                        DbProviderModel::CAP_COLS
                    ),
                )
                .bind(model)
                .bind(pid)
                .fetch_one(pool)
                .await
            }
            None => {
                sqlx::query_as(
                    &format!(
                        "SELECT pm.id, pm.provider_id, pm.model_name, pm.target_model, pm.context_window, pm.enabled, pm.created_at, {}
                         FROM provider_models pm
                         JOIN providers p ON p.id = pm.provider_id
                         WHERE pm.model_name = ? COLLATE NOCASE AND pm.enabled = 1 AND p.enabled = 1
                         LIMIT 1",
                        DbProviderModel::CAP_COLS
                    ),
                )
                .bind(model)
                .fetch_one(pool)
                .await
            }
        }
        .map_err(|_| crate::error::ProxyError::Routing(
            format!("no provider found for model '{}'", model)
        ))?;

        let pool = get_pool().await;
        let provider: DbProvider = sqlx::query_as(
            "SELECT id, name, base_url, format, endpoint_path, enabled, upstream_user_agent FROM providers WHERE id = ?",
        )
        .bind(&matched.provider_id)
        .fetch_one(pool)
        .await
        .map_err(|e| crate::error::ProxyError::Database(e))?;

        // Empty string is treated the same as NULL: fall back to the literal
        // model name. (Dev DBs can carry '' instead of NULL after a restore;
        // sending an empty `model` upstream breaks providers like lmstudio.)
        let target_model = match matched.target_model.as_deref() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => matched.model_name.clone(),
        };
        let protocols = Self::resolve_protocols_for(
            &provider.id,
            &provider.base_url,
            &provider.format,
            provider.endpoint_path.clone(),
            &target_model,
        )
        .await?;
        // The primary protocol row mirrors providers.format/endpoint_path;
        // it is the conversion target when the client protocol has no match.
        let primary = protocols
            .iter()
            .find(|p| p.is_primary)
            .expect("resolve_protocols_for guarantees a primary entry");
        let target_format = primary.format.clone();
        let endpoint_path = primary.endpoint_path.clone();

        info!(
            "Route resolved: {} -> {} ({}) via {}",
            model, target_model, provider.format, provider.name
        );

        Ok(ResolvedRoute {
            provider_id: provider.id,
            provider_name: provider.name,
            base_url: provider.base_url,
            target_format,
            target_model,
            endpoint_path,
            upstream_user_agent: provider.upstream_user_agent,
            capabilities: matched.capabilities(),
            protocols,
        })
    }

    /// Toggle the enabled state of a provider. Returns the new state.
    pub async fn toggle_enabled(provider_id: &str) -> Result<bool, crate::error::ProxyError> {
        let pool = get_pool().await;
        let current: (i64,) = sqlx::query_as("SELECT enabled FROM providers WHERE id = ?")
            .bind(provider_id)
            .fetch_one(pool)
            .await
            .map_err(|e| crate::error::ProxyError::Database(e))?;

        let new_enabled: i64 = if current.0 != 0 { 0 } else { 1 };
        let pool = get_pool().await;
        sqlx::query("UPDATE providers SET enabled = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(new_enabled)
            .bind(provider_id)
            .execute(pool)
            .await
            .map_err(|e| crate::error::ProxyError::Database(e))?;

        info!(
            "Provider {} enabled toggled to {}",
            provider_id,
            new_enabled != 0
        );
        Ok(new_enabled != 0)
    }

    async fn fetch_models(
        provider_id: &str,
    ) -> Result<Vec<ProviderModel>, crate::error::ProxyError> {
        let pool = get_pool().await;
        let rows: Vec<DbProviderModel> = sqlx::query_as(
            &format!(
                "SELECT pm.id, pm.provider_id, pm.model_name, pm.target_model, pm.context_window, pm.enabled, pm.created_at, {}
                 FROM provider_models pm WHERE pm.provider_id = ? ORDER BY pm.model_name",
                DbProviderModel::CAP_COLS
            ),
        )
        .bind(provider_id)
        .fetch_all(pool)
        .await
        .map_err(|e| crate::error::ProxyError::Database(e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let caps = r.capabilities();
                ProviderModel {
                    id: r.id,
                    provider_id: r.provider_id,
                    model_name: r.model_name,
                    target_model: r.target_model,
                    context_window: r.context_window as u64,
                    enabled: r.enabled != 0,
                    created_at: r.created_at,
                    capabilities: caps,
                }
            })
            .collect())
    }

    async fn fetch_api_keys_info(
        provider_id: &str,
    ) -> Result<Vec<ApiKeyInfo>, crate::error::ProxyError> {
        let pool = get_pool().await;
        let rows: Vec<DbApiKeyInfo> = sqlx::query_as(
            "SELECT id, label, is_active, usage_count, last_used_at, created_at
             FROM api_keys WHERE provider_id = ? ORDER BY created_at",
        )
        .bind(provider_id)
        .fetch_all(pool)
        .await
        .map_err(|e| crate::error::ProxyError::Database(e))?;

        Ok(rows
            .into_iter()
            .map(|r| ApiKeyInfo {
                id: r.id,
                label: r.label,
                is_active: r.is_active != 0,
                usage_count: r.usage_count as u32,
                last_used_at: r.last_used_at,
                created_at: r.created_at,
            })
            .collect())
    }

    /// Raw protocol rows for the CRUD API. Guaranteed non-empty: synthesized
    /// from the provider-level columns when the table has none (pre-migration
    /// / legacy data).
    async fn fetch_provider_protocols(
        p: &DbProvider,
    ) -> Result<Vec<ProviderProtocol>, crate::error::ProxyError> {
        let rows = Self::fetch_protocol_rows(&p.id).await?;
        if rows.is_empty() {
            return Ok(vec![ProviderProtocol {
                format: p.format.clone(),
                base_url: None,
                endpoint_path: p.endpoint_path.clone(),
                is_primary: true,
            }]);
        }
        Ok(rows
            .into_iter()
            .map(|r| ProviderProtocol {
                format: r.format,
                base_url: r.base_url.filter(|s| !s.is_empty()),
                endpoint_path: normalize_endpoint_path(r.endpoint_path),
                is_primary: r.is_primary != 0,
            })
            .collect())
    }

    async fn fetch_protocol_rows(
        provider_id: &str,
    ) -> Result<Vec<DbProviderProtocol>, crate::error::ProxyError> {
        let pool = get_pool().await;
        sqlx::query_as(
            "SELECT format, base_url, endpoint_path, is_primary
             FROM provider_protocols WHERE provider_id = ?
             ORDER BY is_primary DESC, created_at ASC",
        )
        .bind(provider_id)
        .fetch_all(pool)
        .await
        .map_err(crate::error::ProxyError::Database)
    }

    /// Resolve the provider's protocol rows into effective base URLs and
    /// final endpoint paths (override or per-format default, with the target
    /// model already substituted for Gemini paths). Guaranteed to contain
    /// exactly one primary entry; synthesized from the provider-level columns
    /// when the table has no rows, preserving the legacy single-protocol
    /// behaviour.
    pub async fn resolve_protocols_for(
        provider_id: &str,
        provider_base_url: &str,
        provider_format: &str,
        provider_endpoint_path: Option<String>,
        target_model: &str,
    ) -> Result<Vec<ResolvedProtocol>, crate::error::ProxyError> {
        let rows = Self::fetch_protocol_rows(provider_id).await?;

        let mut protocols: Vec<ResolvedProtocol> = Vec::new();
        for r in rows {
            let format = match parse_client_format(&r.format) {
                Ok(f) => f,
                Err(_) => {
                    warn!(
                        "Skipping provider protocol with invalid format '{}'",
                        r.format
                    );
                    continue;
                }
            };
            let base_url = r
                .base_url
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| provider_base_url.to_string());
            let endpoint_path = normalize_endpoint_path(r.endpoint_path)
                .unwrap_or_else(|| default_path_for_format(&format, target_model));
            protocols.push(ResolvedProtocol {
                format,
                base_url,
                endpoint_path,
                is_primary: r.is_primary != 0,
            });
        }

        if protocols.is_empty() {
            let format = parse_client_format(provider_format)?;
            protocols.push(ResolvedProtocol {
                format: format.clone(),
                base_url: provider_base_url.to_string(),
                endpoint_path: normalize_endpoint_path(provider_endpoint_path)
                    .unwrap_or_else(|| default_path_for_format(&format, target_model)),
                is_primary: true,
            });
        }

        if !protocols.iter().any(|p| p.is_primary) {
            protocols[0].is_primary = true;
        }

        Ok(protocols)
    }
}

pub fn parse_client_format(format: &str) -> Result<ClientFormat, crate::error::ProxyError> {
    match format {
        "completions" => Ok(ClientFormat::Completions),
        "responses" => Ok(ClientFormat::Responses),
        "anthropic" => Ok(ClientFormat::Anthropic),
        "gemini" => Ok(ClientFormat::Gemini),
        other => Err(crate::error::ProxyError::Config(format!(
            "unknown target format: {}",
            other
        ))),
    }
}

fn default_path_for_format(format: &ClientFormat, target_model: &str) -> String {
    match format {
        ClientFormat::Completions => "/v1/chat/completions".to_string(),
        ClientFormat::Responses => "/v1/responses".to_string(),
        ClientFormat::Anthropic => "/v1/messages".to_string(),
        ClientFormat::Gemini => format!("/v1beta/models/{}:generateContent", target_model),
    }
}

/// Join a provider base URL with an endpoint path, collapsing a duplicated
/// trailing segment. Many OpenAI-compatible providers hand out base URLs that
/// already end in `/v1` (or `/v1beta`, ...), while the default endpoint paths
/// also start with that segment — a plain concat would produce
/// `https://h/v1/v1/responses`. When the base's last path segment repeats at
/// the start of the path, it is emitted only once.
pub fn join_base_url_and_path(base_url: &str, endpoint_path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let prefixed;
    let path = if endpoint_path.starts_with('/') {
        endpoint_path
    } else {
        prefixed = format!("/{}", endpoint_path);
        &prefixed
    };
    if let Some((_, last_seg)) = base.rsplit_once('/') {
        let seg_with_slash = format!("/{}", last_seg);
        if !last_seg.is_empty() && path.starts_with(&seg_with_slash) {
            return format!("{}{}", base, &path[seg_with_slash.len()..]);
        }
    }
    format!("{}{}", base, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_collapses_duplicated_version_segment() {
        // Base already carries /v1 → default paths must not duplicate it.
        assert_eq!(
            join_base_url_and_path("https://h/v1", "/v1/responses"),
            "https://h/v1/responses"
        );
        assert_eq!(
            join_base_url_and_path("https://h/v1/", "/v1/chat/completions"),
            "https://h/v1/chat/completions"
        );
        assert_eq!(
            join_base_url_and_path("https://h/v1beta", "/v1beta/models/gemini:generateContent"),
            "https://h/v1beta/models/gemini:generateContent"
        );
        // Nested prefixes only dedupe the LAST segment.
        assert_eq!(
            join_base_url_and_path("https://h/api/v1", "/v1/messages"),
            "https://h/api/v1/messages"
        );
    }

    #[test]
    fn join_plain_concat_when_no_overlap() {
        assert_eq!(
            join_base_url_and_path("https://h", "/v1/responses"),
            "https://h/v1/responses"
        );
        assert_eq!(
            join_base_url_and_path("https://h/", "/v1/messages"),
            "https://h/v1/messages"
        );
        // Custom endpoint that intentionally does NOT repeat the base's last
        // segment keeps a plain concat.
        assert_eq!(
            join_base_url_and_path("https://h/v1", "/chat/completions"),
            "https://h/v1/chat/completions"
        );
        // Path without a leading '/' is normalized.
        assert_eq!(
            join_base_url_and_path("https://h/v1", "v1/models"),
            "https://h/v1/models"
        );
        // A bare host never dedupes.
        assert_eq!(
            join_base_url_and_path("https://h", "/v1/v1/responses"),
            "https://h/v1/v1/responses"
        );
    }
    use crate::converter::sanitize::ModelCapabilities;

    fn route_with_protocols(protocols: Vec<ResolvedProtocol>) -> ResolvedRoute {
        ResolvedRoute {
            provider_id: "p1".into(),
            provider_name: "test".into(),
            base_url: "https://default".into(),
            target_format: ClientFormat::Completions,
            target_model: "m".into(),
            endpoint_path: "/v1/chat/completions".into(),
            upstream_user_agent: String::new(),
            capabilities: ModelCapabilities::permissive(),
            protocols,
        }
    }

    #[test]
    fn protocol_for_matches_by_format() {
        let route = route_with_protocols(vec![
            ResolvedProtocol {
                format: ClientFormat::Anthropic,
                base_url: "https://anthropic.internal".into(),
                endpoint_path: "/v1/messages".into(),
                is_primary: true,
            },
            ResolvedProtocol {
                format: ClientFormat::Completions,
                base_url: "https://default".into(),
                endpoint_path: "/v1/chat/completions".into(),
                is_primary: false,
            },
        ]);

        let hit = route
            .protocol_for(&ClientFormat::Anthropic)
            .expect("anthropic configured");
        assert_eq!(hit.base_url, "https://anthropic.internal");
        assert!(
            route
                .protocol_for(&ClientFormat::Anthropic)
                .unwrap()
                .is_primary
        );
        assert!(route.protocol_for(&ClientFormat::Completions).is_some());
        assert!(route.protocol_for(&ClientFormat::Gemini).is_none());
        assert!(route.protocol_for(&ClientFormat::Responses).is_none());
    }

    #[test]
    fn protocol_for_empty_list_matches_nothing() {
        let route = route_with_protocols(vec![]);
        assert!(route.protocol_for(&ClientFormat::Anthropic).is_none());
    }
}
