use crate::converter::ir::ClientFormat;
use crate::db::get_pool;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::endpoint::{ApiKeyInfo, Provider, ProviderModel};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedRoute {
    pub provider_id: String,
    pub provider_name: String,
    pub base_url: String,
    pub target_format: ClientFormat,
    pub target_model: String,
    pub endpoint_path: String,
    pub upstream_user_agent: String,
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
struct DbProviderModel {
    id: String,
    provider_id: String,
    model_name: String,
    target_model: Option<String>,
    context_window: i64,
    enabled: i64,
    created_at: String,
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
    pub async fn find_for_model_on_provider(
        model: &str,
        provider_id: Option<&str>,
    ) -> Result<ResolvedRoute, crate::error::ProxyError> {
        let pool = get_pool().await;
        info!("Looking up route for model: {}", model);

        let matched: DbProviderModel = match provider_id {
            Some(pid) => {
                sqlx::query_as(
                    "SELECT pm.id, pm.provider_id, pm.model_name, pm.target_model, pm.context_window, pm.enabled, pm.created_at
                     FROM provider_models pm
                     JOIN providers p ON p.id = pm.provider_id
                     WHERE pm.model_name = ? COLLATE NOCASE AND pm.enabled = 1 AND p.enabled = 1 AND pm.provider_id = ?
                     LIMIT 1",
                )
                .bind(model)
                .bind(pid)
                .fetch_one(pool)
                .await
            }
            None => {
                sqlx::query_as(
                    "SELECT pm.id, pm.provider_id, pm.model_name, pm.target_model, pm.context_window, pm.enabled, pm.created_at
                     FROM provider_models pm
                     JOIN providers p ON p.id = pm.provider_id
                     WHERE pm.model_name = ? COLLATE NOCASE AND pm.enabled = 1 AND p.enabled = 1
                     LIMIT 1",
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

        let target_model = matched
            .target_model
            .clone()
            .unwrap_or_else(|| matched.model_name.clone());
        let target_format = parse_client_format(&provider.format)?;
        let endpoint_path = provider
            .endpoint_path
            .map(|p| {
                if p.starts_with('/') {
                    p
                } else {
                    format!("/{}", p)
                }
            })
            .unwrap_or_else(|| default_path_for_format(&target_format, &target_model));

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
            "SELECT id, provider_id, model_name, target_model, context_window, enabled, created_at
             FROM provider_models WHERE provider_id = ? ORDER BY model_name",
        )
        .bind(provider_id)
        .fetch_all(pool)
        .await
        .map_err(|e| crate::error::ProxyError::Database(e))?;

        Ok(rows
            .into_iter()
            .map(|r| ProviderModel {
                id: r.id,
                provider_id: r.provider_id,
                model_name: r.model_name,
                target_model: r.target_model,
                context_window: r.context_window as u64,
                enabled: r.enabled != 0,
                created_at: r.created_at,
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
