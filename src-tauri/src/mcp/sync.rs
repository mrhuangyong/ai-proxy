use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::Utc;

use crate::apps::types::AppType;
use crate::apps::config::{claude_desktop_config_path, atomic_write};
use crate::db::get_pool;
use super::types::*;

#[derive(Debug, Serialize, Deserialize)]
struct McpServerEntry {
    #[serde(default)]
    r#type: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    url: Option<String>,
    headers: Option<HashMap<String, String>>,
    env: Option<HashMap<String, String>>,
}

fn config_path_for(app_type: &AppType) -> Option<PathBuf> {
    match app_type {
        AppType::ClaudeDesktop => Some(claude_desktop_config_path()),
        AppType::ClaudeCli => {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            Some(home.join(".claude.json"))
        }
        _ => None,
    }
}

pub async fn import_from_app(app_type: &AppType) -> Result<ImportResult, String> {
    let path = config_path_for(app_type)
        .ok_or_else(|| format!("{} does not support MCP import", app_type.display_name()))?;

    if !path.exists() {
        return Err(format!("Config file not found: {}", path.display()));
    }

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Failed to read config: {}", e))?;

    let config: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse config JSON: {}", e))?;

    let mcp_servers = config.get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let pool = get_pool().await;
    let now = Utc::now().to_rfc3339();
    let app_type_str = app_type.to_string();
    let mut imported = 0u32;
    let mut skipped = 0u32;

    for (name, value) in &mcp_servers {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM mcp_servers WHERE name = ?",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap_or(false);

        if exists {
            skipped += 1;
            continue;
        }

        let entry: McpServerEntry = serde_json::from_value(value.clone())
            .map_err(|e| format!("Failed to parse server '{}': {}", name, e))?;

        let transport_type = entry.r#type.as_deref().unwrap_or("stdio").to_string();
        let id = Uuid::new_v4().to_string();
        let args_json = entry.args.as_ref()
            .map(|a| serde_json::to_string(a).unwrap_or_default());
        let env_json = entry.env.as_ref()
            .map(|e| serde_json::to_string(e).unwrap_or_default());
        let headers_json = entry.headers.as_ref()
            .map(|h| serde_json::to_string(h).unwrap_or_default());

        sqlx::query(
            "INSERT INTO mcp_servers (id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(&transport_type)
        .bind(&entry.command)
        .bind(&args_json)
        .bind(&entry.url)
        .bind(&headers_json)
        .bind(&env_json)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to insert server '{}': {}", name, e))?;

        sqlx::query(
            "INSERT INTO mcp_app_bindings (mcp_server_id, app_type, enabled) VALUES (?, ?, 1)",
        )
        .bind(&id)
        .bind(&app_type_str)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to insert binding for '{}': {}", name, e))?;

        imported += 1;
    }

    Ok(ImportResult { imported, skipped })
}

pub async fn apply_to_app(app_type: &AppType) -> Result<ApplyResult, String> {
    let path = config_path_for(app_type)
        .ok_or_else(|| format!("{} does not support MCP apply", app_type.display_name()))?;

    let pool = get_pool().await;
    let app_type_str = app_type.to_string();

    let bindings: Vec<McpAppBinding> = sqlx::query_as(
        "SELECT mcp_server_id, app_type, enabled FROM mcp_app_bindings WHERE app_type = ? AND enabled = 1",
    )
    .bind(&app_type_str)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query bindings: {}", e))?;

    let mut managed_mcp = serde_json::Map::new();

    for binding in &bindings {
        let server: Option<McpServer> = sqlx::query_as(
            "SELECT id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at FROM mcp_servers WHERE id = ?",
        )
        .bind(&binding.mcp_server_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to query server: {}", e))?;

        let Some(server) = server else { continue };

        let mut entry = serde_json::Map::new();

        if server.transport_type != "stdio" {
            entry.insert("type".into(), serde_json::Value::String(server.transport_type.clone()));
        }

        if let Some(cmd) = &server.command {
            entry.insert("command".into(), serde_json::Value::String(cmd.clone()));
        }

        if let Some(args_str) = &server.args {
            if let Ok(args_val) = serde_json::from_str::<serde_json::Value>(args_str) {
                entry.insert("args".into(), args_val);
            }
        }

        if let Some(url) = &server.url {
            entry.insert("url".into(), serde_json::Value::String(url.clone()));
        }

        if let Some(headers_str) = &server.headers {
            if let Ok(headers_val) = serde_json::from_str::<serde_json::Value>(headers_str) {
                entry.insert("headers".into(), headers_val);
            }
        }

        if let Some(env_str) = &server.env {
            if let Ok(env_val) = serde_json::from_str::<serde_json::Value>(env_str) {
                entry.insert("env".into(), env_val);
            }
        }

        managed_mcp.insert(server.name.clone(), serde_json::Value::Object(entry));
    }

    // Read existing config, merge, write back
    let mut config: serde_json::Value = if path.exists() {
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read config: {}", e))?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let mcp_servers_obj = config
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

    let existing_map = mcp_servers_obj.as_object_mut().unwrap();

    // Remove stale managed entries — remove any entry whose name exists in our DB
    let all_db_names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM mcp_servers",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    existing_map.retain(|name, _| !all_db_names.contains(name));

    // Add managed entries
    for (name, value) in managed_mcp {
        existing_map.insert(name, value);
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let content = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    atomic_write(&path, &content).await?;

    Ok(ApplyResult { applied: bindings.len() as u32 })
}
