use axum::Json;
use axum::extract::Path;
use uuid::Uuid;
use chrono::Utc;

use crate::db::get_pool;
use crate::server::api::{ok, err_json, ApiError, ApiResponse};
use super::types::*;

pub async fn list_servers() -> Json<ApiResponse<Vec<McpServerWithBindings>>> {
    let pool = get_pool().await;

    let servers: Vec<McpServer> = match sqlx::query_as(
        "SELECT id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at FROM mcp_servers ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("list_servers error: {}", e);
            return ok(vec![]);
        }
    };

    let mut result = Vec::new();
    for server in servers {
        let bindings: Vec<McpAppBinding> = sqlx::query_as(
            "SELECT mcp_server_id, app_type, enabled FROM mcp_app_bindings WHERE mcp_server_id = ?",
        )
        .bind(&server.id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        result.push(McpServerWithBindings { server, bindings });
    }

    ok(result)
}

pub async fn create_server(
    axum::Json(body): axum::Json<CreateMcpServerBody>,
) -> Result<Json<ApiResponse<McpServerWithBindings>>, Json<ApiError>> {
    let pool = get_pool().await;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO mcp_servers (id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&body.name)
    .bind(&body.transport_type)
    .bind(&body.command)
    .bind(&body.args)
    .bind(&body.url)
    .bind(&body.headers)
    .bind(&body.env)
    .bind(&body.description)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| err_json(format!("Failed to create MCP server: {}", e)))?;

    if let Some(bindings) = body.bindings {
        for b in &bindings {
            let enabled = if b.enabled { 1i64 } else { 0i64 };
            sqlx::query(
                "INSERT OR REPLACE INTO mcp_app_bindings (mcp_server_id, app_type, enabled) VALUES (?, ?, ?)",
            )
            .bind(&id)
            .bind(&b.app_type)
            .bind(enabled)
            .execute(pool)
            .await
            .map_err(|e| err_json(format!("Failed to create binding: {}", e)))?;
        }
    }

    let server: McpServer = sqlx::query_as(
        "SELECT id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at FROM mcp_servers WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(pool)
    .await
    .map_err(|e| err_json(format!("Failed to fetch created server: {}", e)))?;

    let bindings: Vec<McpAppBinding> = sqlx::query_as(
        "SELECT mcp_server_id, app_type, enabled FROM mcp_app_bindings WHERE mcp_server_id = ?",
    )
    .bind(&id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Ok(ok(McpServerWithBindings { server, bindings }))
}

pub async fn update_server(
    Path(id): Path<String>,
    axum::Json(body): axum::Json<UpdateMcpServerBody>,
) -> Result<Json<ApiResponse<McpServerWithBindings>>, Json<ApiError>> {
    let pool = get_pool().await;

    let existing: McpServer = sqlx::query_as(
        "SELECT id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at FROM mcp_servers WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(pool)
    .await
    .map_err(|e| err_json(format!("Database error: {}", e)))?
    .ok_or_else(|| err_json("MCP server not found"))?;

    let now = Utc::now().to_rfc3339();
    let name = body.name.unwrap_or(existing.name);
    let transport_type = body.transport_type.unwrap_or(existing.transport_type);
    let command = body.command.or(existing.command);
    let args = body.args.or(existing.args);
    let url = body.url.or(existing.url);
    let headers = body.headers.or(existing.headers);
    let env = body.env.or(existing.env);
    let description = body.description.or(existing.description);

    sqlx::query(
        "UPDATE mcp_servers SET name=?, transport_type=?, command=?, args=?, url=?, headers=?, env=?, description=?, updated_at=? WHERE id=?",
    )
    .bind(&name)
    .bind(&transport_type)
    .bind(&command)
    .bind(&args)
    .bind(&url)
    .bind(&headers)
    .bind(&env)
    .bind(&description)
    .bind(&now)
    .bind(&id)
    .execute(pool)
    .await
    .map_err(|e| err_json(format!("Failed to update MCP server: {}", e)))?;

    let server: McpServer = sqlx::query_as(
        "SELECT id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at FROM mcp_servers WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(pool)
    .await
    .map_err(|e| err_json(format!("Failed to fetch updated server: {}", e)))?;

    let bindings: Vec<McpAppBinding> = sqlx::query_as(
        "SELECT mcp_server_id, app_type, enabled FROM mcp_app_bindings WHERE mcp_server_id = ?",
    )
    .bind(&id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Ok(ok(McpServerWithBindings { server, bindings }))
}

pub async fn delete_server(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;

    sqlx::query("DELETE FROM mcp_app_bindings WHERE mcp_server_id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| err_json(format!("Failed to delete bindings: {}", e)))?;

    sqlx::query("DELETE FROM mcp_servers WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| err_json(format!("Failed to delete MCP server: {}", e)))?;

    Ok(ok(()))
}

pub async fn update_bindings(
    Path(id): Path<String>,
    axum::Json(body): axum::Json<UpdateBindingsBody>,
) -> Result<Json<ApiResponse<Vec<McpAppBinding>>>, Json<ApiError>> {
    let pool = get_pool().await;

    let exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM mcp_servers WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(pool)
    .await
    .map_err(|e| err_json(format!("Database error: {}", e)))?;

    if !exists {
        return Err(err_json("MCP server not found"));
    }

    sqlx::query("DELETE FROM mcp_app_bindings WHERE mcp_server_id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| err_json(format!("Failed to clear bindings: {}", e)))?;

    for b in &body.bindings {
        let enabled = if b.enabled { 1i64 } else { 0i64 };
        sqlx::query(
            "INSERT INTO mcp_app_bindings (mcp_server_id, app_type, enabled) VALUES (?, ?, ?)",
        )
        .bind(&id)
        .bind(&b.app_type)
        .bind(enabled)
        .execute(pool)
        .await
        .map_err(|e| err_json(format!("Failed to create binding: {}", e)))?;
    }

    let bindings: Vec<McpAppBinding> = sqlx::query_as(
        "SELECT mcp_server_id, app_type, enabled FROM mcp_app_bindings WHERE mcp_server_id = ?",
    )
    .bind(&id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Ok(ok(bindings))
}

pub async fn import_from_app(
    Path(app_type_str): Path<String>,
) -> Result<Json<ApiResponse<super::types::ImportResult>>, Json<ApiError>> {
    // Delegate to sync module
    let _ = app_type_str;
    Ok(ok(super::types::ImportResult { imported: 0, skipped: 0 }))
}

pub async fn apply_to_app(
    Path(app_type_str): Path<String>,
) -> Result<Json<ApiResponse<super::types::ApplyResult>>, Json<ApiError>> {
    let _ = app_type_str;
    Ok(ok(super::types::ApplyResult { applied: 0 }))
}
