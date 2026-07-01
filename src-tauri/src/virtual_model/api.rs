//! Management API types & handlers for virtual models and their mappings.
//! Wired into `server/api.rs::api_routes()`.

use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::get_pool;
use crate::error::ProxyError;
use crate::provider::manager::ProviderManager;
use crate::server::api::{err_json, ok, ApiError, ApiResponse};
use crate::virtual_model::manager::DbMapping;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct VirtualModelRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub current_mapping_id: Option<String>,
    pub enabled: i64, // 0/1 from DB; serialized as number (frontend coerces when needed)
}

#[derive(Debug, Clone, Serialize)]
pub struct MappingDto {
    pub id: String,
    pub virtual_model_id: String,
    pub provider_id: String,
    pub provider_model_id: String,
    pub label: String,
    pub priority: i64,
    pub enabled: bool,
    pub available: bool,
    pub consecutive_failures: i64,
    pub failover_count: i64,
    pub last_failure_at: Option<String>,
    pub last_checked_at: Option<String>,
    pub created_at: String,
    pub is_current: bool, // == virtual_model.current_mapping_id
}

#[derive(Debug, Clone, Serialize)]
pub struct VirtualModelWithMappings {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub current_mapping_id: Option<String>,
    pub enabled: bool,
    pub mappings: Vec<MappingDto>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVirtualModelBody {
    pub name: String,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    /// Initial mappings to attach: each specifies `provider_model_id` and optional priority.
    pub mappings: Vec<CreateMappingInput>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMappingInput {
    pub provider_model_id: String,
    pub priority: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateVirtualModelBody {
    pub name: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMappingBody {
    pub priority: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SetAvailableBody {
    pub available: bool,
}

#[derive(Debug, Deserialize)]
pub struct SetStickyBody {
    /// `None` clears the sticky anchor (let the router auto-pick on next request);
    /// `Some(id)` forces sticky to that mapping (must be enabled & available).
    pub mapping_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RealModelOption {
    pub provider_model_id: String,
    pub provider_id: String,
    pub label: String, // "provider_name/model_name"
}

// --- Handlers ---

pub async fn list_virtual_models(
) -> Result<Json<ApiResponse<Vec<VirtualModelWithMappings>>>, Json<ApiError>> {
    match fetch_virtual_models_with_mappings().await {
        Ok(items) => Ok(ok(items)),
        Err(e) => Err(err_json(e.to_string())),
    }
}

pub async fn create_virtual_model(
    axum::Json(body): axum::Json<CreateVirtualModelBody>,
) -> Result<Json<ApiResponse<String>>, Json<ApiError>> {
    if body.name.trim().is_empty() {
        return Err(err_json("name is required"));
    }
    let pool = get_pool().await;
    let id = uuid::Uuid::new_v4().to_string();
    let enabled = if body.enabled.unwrap_or(true) { 1 } else { 0 };
    sqlx::query(
        "INSERT INTO virtual_models (id, name, description, enabled)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(body.name.trim())
    .bind(body.description.as_deref())
    .bind(enabled)
    .execute(pool)
    .await
    .map_err(|e| err_json(format!("db: {}", e)))?;

    // Attach initial mappings (best-effort: skip invalid provider_model_id).
    for m in body.mappings {
        let _ = attach_mapping(
            &id,
            &m.provider_model_id,
            m.priority.unwrap_or(100),
            m.enabled.unwrap_or(true),
        )
        .await;
    }

    Ok(ok(id))
}

pub async fn update_virtual_model(
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(body): axum::Json<UpdateVirtualModelBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    let mut sets: Vec<String> = Vec::new();
    if let Some(name) = body.name {
        sets.push("name = ?".to_string());
        // bind later; for simplicity execute per-field
        let _ = sqlx::query(
            "UPDATE virtual_models SET name = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(name)
        .bind(&id)
        .execute(pool)
        .await;
    }
    let _ = sets;
    let pool = get_pool().await;
    if let Some(desc) = body.description {
        let _ = sqlx::query(
            "UPDATE virtual_models SET description = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(desc)
        .bind(&id)
        .execute(pool)
        .await;
    }
    let pool = get_pool().await;
    if let Some(enabled) = body.enabled {
        let _ = sqlx::query(
            "UPDATE virtual_models SET enabled = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(&id)
        .execute(pool)
        .await;
    }
    Ok(ok(()))
}

pub async fn delete_virtual_model(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    sqlx::query("DELETE FROM virtual_models WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| err_json(format!("db: {}", e)))?;
    Ok(ok(()))
}

pub async fn create_mapping(
    axum::extract::Path(virtual_id): axum::extract::Path<String>,
    axum::Json(body): axum::Json<CreateMappingInput>,
) -> Result<Json<ApiResponse<String>>, Json<ApiError>> {
    match attach_mapping(
        &virtual_id,
        &body.provider_model_id,
        body.priority.unwrap_or(100),
        body.enabled.unwrap_or(true),
    )
    .await
    {
        Ok(mid) => Ok(ok(mid)),
        Err(e) => Err(err_json(e.to_string())),
    }
}

pub async fn update_mapping(
    axum::extract::Path(mid): axum::extract::Path<String>,
    axum::Json(body): axum::Json<UpdateMappingBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    if let Some(p) = body.priority {
        let _ = sqlx::query("UPDATE virtual_model_mappings SET priority = ? WHERE id = ?")
            .bind(p)
            .bind(&mid)
            .execute(pool)
            .await;
    }
    let pool = get_pool().await;
    if let Some(enabled) = body.enabled {
        let _ = sqlx::query("UPDATE virtual_model_mappings SET enabled = ? WHERE id = ?")
            .bind(if enabled { 1 } else { 0 })
            .bind(&mid)
            .execute(pool)
            .await;
    }
    Ok(ok(()))
}

pub async fn set_mapping_available(
    axum::extract::Path(mid): axum::extract::Path<String>,
    axum::Json(body): axum::Json<SetAvailableBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    crate::virtual_model::manager::VirtualRouter::set_available(&mid, body.available)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

pub async fn delete_mapping(
    axum::extract::Path(mid): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    // Clear sticky anchor if it points here.
    let _ = sqlx::query(
        "UPDATE virtual_models SET current_mapping_id = NULL WHERE current_mapping_id = ?",
    )
    .bind(&mid)
    .execute(pool)
    .await;
    let pool = get_pool().await;
    sqlx::query("DELETE FROM virtual_model_mappings WHERE id = ?")
        .bind(&mid)
        .execute(pool)
        .await
        .map_err(|e| err_json(format!("db: {}", e)))?;
    Ok(ok(()))
}

/// `PUT /api/virtual-models/:id/sticky` — manually set or clear the sticky anchor.
///
/// Body: `{ "mapping_id": "<id>" }` to stick to a specific mapping (must be
/// enabled & available), or `{ "mapping_id": null }` to clear and let the
/// router auto-pick the next healthy mapping on the next request.
pub async fn set_sticky(
    axum::extract::Path(virtual_id): axum::extract::Path<String>,
    axum::Json(body): axum::Json<SetStickyBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;

    match body.mapping_id {
        None => {
            sqlx::query(
                "UPDATE virtual_models
                 SET current_mapping_id = NULL, updated_at = datetime('now')
                 WHERE id = ?",
            )
            .bind(&virtual_id)
            .execute(pool)
            .await
            .map_err(|e| err_json(format!("db: {}", e)))?;
            Ok(ok(()))
        }
        Some(mid) => {
            // Validate the mapping belongs to this virtual model and is healthy.
            let row: Option<(i64, i64, i64)> = sqlx::query_as(
                "SELECT 1, m.enabled, m.available
                 FROM virtual_model_mappings m
                 WHERE m.id = ? AND m.virtual_model_id = ?",
            )
            .bind(&mid)
            .bind(&virtual_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| err_json(format!("db: {}", e)))?;
            let (_, enabled, available) =
                row.ok_or_else(|| err_json("mapping does not belong to this virtual model"))?;
            if enabled == 0 {
                return Err(err_json("cannot stick to a disabled mapping"));
            }
            if available == 0 {
                return Err(err_json(
                    "cannot stick to an unavailable mapping; restore it first",
                ));
            }

            let pool = get_pool().await;
            sqlx::query(
                "UPDATE virtual_models
                 SET current_mapping_id = ?, updated_at = datetime('now')
                 WHERE id = ?",
            )
            .bind(&mid)
            .bind(&virtual_id)
            .execute(pool)
            .await
            .map_err(|e| err_json(format!("db: {}", e)))?;
            Ok(ok(()))
        }
    }
}

/// Returns all real provider_models in the system, formatted as
/// `provider_name/model_name`, for the frontend "add mapping" dropdown.
pub async fn list_real_models() -> Result<Json<ApiResponse<Vec<RealModelOption>>>, Json<ApiError>> {
    let providers = match ProviderManager::list().await {
        Ok(p) => p,
        Err(e) => return Err(err_json(e.to_string())),
    };
    let mut out: Vec<RealModelOption> = Vec::new();
    for p in providers {
        for m in p.models {
            out.push(RealModelOption {
                provider_model_id: m.id,
                provider_id: p.id.clone(),
                label: format!("{}/{}", p.name, m.model_name),
            });
        }
    }
    Ok(ok(out))
}

// --- helpers ---

async fn fetch_virtual_models_with_mappings() -> Result<Vec<VirtualModelWithMappings>, ProxyError> {
    let pool = get_pool().await;
    let vrows: Vec<VirtualModelRow> = sqlx::query_as(
        "SELECT id, name, description, current_mapping_id, enabled FROM virtual_models ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(ProxyError::Database)?;

    let mut out = Vec::with_capacity(vrows.len());
    for v in vrows {
        let pool = get_pool().await;
        let mappings: Vec<DbMapping> = sqlx::query_as(
            "SELECT id, virtual_model_id, provider_id, provider_model_id, label,
                    priority, enabled, available, consecutive_failures, failover_count,
                    last_failure_at, last_checked_at, created_at
             FROM virtual_model_mappings
             WHERE virtual_model_id = ?
             ORDER BY priority ASC, created_at ASC",
        )
        .bind(&v.id)
        .fetch_all(pool)
        .await
        .map_err(ProxyError::Database)?;
        let dtos: Vec<MappingDto> = mappings
            .into_iter()
            .map(|m| MappingDto {
                is_current: v.current_mapping_id.as_deref() == Some(m.id.as_str()),
                enabled: m.enabled != 0,
                available: m.available != 0,
                id: m.id,
                virtual_model_id: m.virtual_model_id,
                provider_id: m.provider_id,
                provider_model_id: m.provider_model_id,
                label: m.label,
                priority: m.priority,
                consecutive_failures: m.consecutive_failures,
                failover_count: m.failover_count,
                last_failure_at: m.last_failure_at,
                last_checked_at: m.last_checked_at,
                created_at: m.created_at,
            })
            .collect();
        out.push(VirtualModelWithMappings {
            id: v.id,
            name: v.name,
            description: v.description,
            current_mapping_id: v.current_mapping_id,
            enabled: v.enabled != 0,
            mappings: dtos,
        });
    }
    Ok(out)
}

async fn attach_mapping(
    virtual_id: &str,
    provider_model_id: &str,
    priority: i64,
    enabled: bool,
) -> Result<String, ProxyError> {
    let pool = get_pool().await;

    // Look up the provider_id + build label "provider_name/model_name".
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT p.id, p.name
         FROM provider_models pm
         JOIN providers p ON p.id = pm.provider_id
         WHERE pm.id = ?",
    )
    .bind(provider_model_id)
    .fetch_optional(pool)
    .await
    .map_err(ProxyError::Database)?;
    let (provider_id, provider_name) =
        row.ok_or_else(|| ProxyError::Routing("provider_model not found".into()))?;

    let pool = get_pool().await;
    let model_name: String =
        sqlx::query_scalar("SELECT model_name FROM provider_models WHERE id = ?")
            .bind(provider_model_id)
            .fetch_one(pool)
            .await
            .map_err(ProxyError::Database)?;

    let label = format!("{}/{}", provider_name, model_name);
    let mid = uuid::Uuid::new_v4().to_string();
    let pool = get_pool().await;
    sqlx::query(
        "INSERT INTO virtual_model_mappings
         (id, virtual_model_id, provider_id, provider_model_id, label, priority, enabled)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&mid)
    .bind(virtual_id)
    .bind(&provider_id)
    .bind(provider_model_id)
    .bind(&label)
    .bind(priority)
    .bind(if enabled { 1 } else { 0 })
    .execute(pool)
    .await
    .map_err(ProxyError::Database)?;
    Ok(mid)
}
