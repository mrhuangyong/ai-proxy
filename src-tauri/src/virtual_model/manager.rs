//! Virtual model routing: resolve a virtual model name into a real provider_models
//! route with sticky-failover semantics.
//!
//! Each virtual model has many `virtual_model_mappings` rows pointing at real
//! `provider_models`. The router picks one (sticky by `virtual_models.current_mapping_id`),
//! and on consecutive failures marks the mapping `available=0`, then re-resolves
//! to the next viable mapping.

use sqlx::FromRow;
use tracing::{info, warn};

use crate::converter::ir::ClientFormat;
use crate::db::get_pool;
use crate::error::ProxyError;
use crate::provider::manager::{parse_client_format, ResolvedRoute};

/// A resolved route plus the mapping id used to track success/failure.
#[derive(Debug, Clone)]
pub struct ResolvedFailover {
    pub route: ResolvedRoute,
    pub mapping_id: String,
    /// The original virtual model name (echoed back to the client in `body.model`)
    pub virtual_name: String,
}

#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct DbMapping {
    pub id: String,
    pub virtual_model_id: String,
    pub provider_id: String,
    pub provider_model_id: String,
    pub label: String,
    pub priority: i64,
    pub enabled: i64,
    pub available: i64,
    pub consecutive_failures: i64,
    pub failover_count: i64,
    pub last_failure_at: Option<String>,
    pub last_checked_at: Option<String>,
    pub created_at: String,
}

/// A real model candidate returned by `resolve`, ready for the proxy handler.
#[derive(Debug, Clone, FromRow)]
struct DbResolvableCandidate {
    mapping_id: String,
    provider_id: String,
    model_name: String,
    target_model: Option<String>,
    base_url: String,
    format: String,
    endpoint_path: Option<String>,
    upstream_user_agent: String,
    /// Aliased in SQL `ORDER BY` for sticky-first sorting; not read from the row.
    #[allow(dead_code)]
    sticky: i64,
}

pub struct VirtualRouter;

impl VirtualRouter {
    /// Resolve a virtual model name into a concrete upstream route.
    /// Honours the sticky `current_mapping_id` if it is still enabled & available.
    pub async fn resolve(virtual_name: &str) -> Result<ResolvedFailover, ProxyError> {
        let pool = get_pool().await;

        // Candidate rows; sticky one sorts first via `sticky` DESC, then by
        // priority ASC, failover_count ASC, created_at ASC.
        let candidates: Vec<DbResolvableCandidate> = sqlx::query_as(
            "SELECT
                m.id AS mapping_id,
                m.provider_id,
                pm.model_name,
                pm.target_model,
                p.base_url,
                p.format,
                p.endpoint_path,
                p.upstream_user_agent,
                CASE WHEN v.current_mapping_id = m.id THEN 1 ELSE 0 END AS sticky
             FROM virtual_model_mappings m
             JOIN virtual_models v ON v.id = m.virtual_model_id
             JOIN provider_models pm ON pm.id = m.provider_model_id
             JOIN providers p ON p.id = m.provider_id
             WHERE v.name = ? COLLATE NOCASE
               AND v.enabled = 1
               AND m.enabled = 1
               AND m.available = 1
               AND p.enabled = 1
               AND pm.enabled = 1
             ORDER BY sticky DESC, m.priority ASC, m.failover_count ASC, m.created_at ASC
             LIMIT 1",
        )
        .bind(virtual_name)
        .fetch_all(pool)
        .await
        .map_err(|e| ProxyError::Database(e))?;

        let c = candidates
            .into_iter()
            .next()
            .ok_or_else(|| {
                ProxyError::Routing(format!(
                    "no available mapping for virtual model '{}'",
                    virtual_name
                ))
            })?;

        // Persist the sticky anchor so subsequent requests stay on the same mapping.
        sqlx::query(
            "UPDATE virtual_models SET current_mapping_id = ?, updated_at = datetime('now')
             WHERE name = ? COLLATE NOCASE AND COALESCE(current_mapping_id, '') != ?",
        )
        .bind(&c.mapping_id)
        .bind(virtual_name)
        .bind(&c.mapping_id)
        .execute(pool)
        .await?;

        let target_model = c
            .target_model
            .clone()
            .unwrap_or_else(|| c.model_name.clone());
        let target_format = parse_client_format(&c.format)?;
        let endpoint_path = c
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
            "[failover] {} -> {} ({}) [mapping={}]",
            virtual_name, target_model, c.format, c.mapping_id
        );

        Ok(ResolvedFailover {
            route: ResolvedRoute {
                provider_id: c.provider_id,
                provider_name: String::new(), // not needed for forwarding
                base_url: c.base_url,
                target_format,
                target_model,
                endpoint_path,
                upstream_user_agent: c.upstream_user_agent,
            },
            mapping_id: c.mapping_id,
            virtual_name: virtual_name.to_string(),
        })
    }

    /// Record a successful upstream call: clear consecutive failures, set
    /// `available=1` if it had been marked down, update last_checked_at.
    pub async fn record_success(mapping_id: &str) {
        let pool = get_pool().await;
        let _ = sqlx::query(
            "UPDATE virtual_model_mappings
             SET consecutive_failures = 0,
                 available = 1,
                 last_checked_at = datetime('now')
             WHERE id = ?",
        )
        .bind(mapping_id)
        .execute(pool)
        .await;
    }

    /// Record a failed upstream call. When `consecutive_failures` reaches the
    /// threshold, the mapping is marked `available=0`, its `failover_count`
    /// incremented, and the virtual model's sticky anchor is cleared so the
    /// next request picks the next mapping.
    ///
    /// Returns `true` if this failure caused the mapping to become unavailable.
    pub async fn record_failure(mapping_id: &str, threshold: u32) -> bool {
        let pool = get_pool().await;

        // Snapshot current state so we can decide the new values for the UPDATE.
        let cur: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT available, consecutive_failures, failover_count
             FROM virtual_model_mappings WHERE id = ?",
        )
        .bind(mapping_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        let (cur_avail, cur_failures, cur_failover) = match cur {
            Some(v) => v,
            None => return false,
        };
        let new_failures = cur_failures + 1;
        let crosses = new_failures >= threshold as i64;
        // Mapping becomes unavailable the moment the threshold is crossed.
        let was_available = cur_avail == 1;
        let new_avail: i64 = if crosses { 0 } else { cur_avail };
        let new_failover: i64 = if crosses && was_available { cur_failover + 1 } else { cur_failover };

        let pool = get_pool().await;
        let _ = sqlx::query(
            "UPDATE virtual_model_mappings
             SET consecutive_failures = ?,
                 last_failure_at = datetime('now'),
                 available = ?,
                 failover_count = ?
             WHERE id = ?",
        )
        .bind(new_failures)
        .bind(new_avail)
        .bind(new_failover)
        .bind(mapping_id)
        .execute(pool)
        .await;

        let became_unavailable = crosses && was_available;

        // Drop the sticky anchor so subsequent resolve() picks a fresh mapping.
        if became_unavailable {
            let pool = get_pool().await;
            let _ = sqlx::query(
                "UPDATE virtual_models
                 SET current_mapping_id = NULL, updated_at = datetime('now')
                 WHERE current_mapping_id = ?",
            )
            .bind(mapping_id)
            .execute(pool)
            .await;
            warn!(
                "[failover] mapping {} marked unavailable after consecutive failures",
                mapping_id
            );
        }
        became_unavailable
    }

    /// Mark a mapping as available and reset its failure counters.
    /// Used by the health checker (or manual recovery).
    pub async fn mark_available(mapping_id: &str) {
        let pool = get_pool().await;
        let _ = sqlx::query(
            "UPDATE virtual_model_mappings
             SET available = 1, consecutive_failures = 0, last_checked_at = datetime('now')
             WHERE id = ?",
        )
        .bind(mapping_id)
        .execute(pool)
        .await;
    }

    /// Manually set the `available` state of a mapping.
    pub async fn set_available(mapping_id: &str, available: bool) -> Result<(), ProxyError> {
        let pool = get_pool().await;
        sqlx::query(
            "UPDATE virtual_model_mappings
             SET available = ?, consecutive_failures = 0, last_checked_at = datetime('now')
             WHERE id = ?",
        )
        .bind(if available { 1 } else { 0 })
        .bind(mapping_id)
        .execute(pool)
        .await
        .map_err(|e| ProxyError::Database(e))?;
        if !available {
            // Drop sticky anchor if pointing to a now-disabled mapping.
            let pool = get_pool().await;
            let _ = sqlx::query(
                "UPDATE virtual_models
                 SET current_mapping_id = NULL, updated_at = datetime('now')
                 WHERE current_mapping_id = ?",
            )
            .bind(mapping_id)
            .execute(pool)
            .await;
        }
        Ok(())
    }

    /// List unavailable mappings that are still enabled (candidates for health probing).
    pub async fn list_unavailable_for_probe() -> Vec<(String, String, String)> {
        let pool = get_pool().await;
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT m.id, pm.model_name, m.provider_id
             FROM virtual_model_mappings m
             JOIN provider_models pm ON pm.id = m.provider_model_id
             WHERE m.available = 0 AND m.enabled = 1",
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default()
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