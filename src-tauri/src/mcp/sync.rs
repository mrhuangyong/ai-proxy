use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::Utc;

use crate::apps::types::AppType;
use crate::apps::config::{claude_desktop_config_path, codex_config_path, atomic_write};
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
        AppType::CodexCli | AppType::CodexDesktop => Some(codex_config_path()),
    }
}

fn is_toml_config(app_type: &AppType) -> bool {
    matches!(app_type, AppType::CodexCli | AppType::CodexDesktop)
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

    let mcp_entries: Vec<(String, McpServerEntry)> = if is_toml_config(app_type) {
        parse_toml_mcp_servers(&content)?
    } else {
        parse_json_mcp_servers(&content)?
    };

    let pool = get_pool().await;
    let now = Utc::now().to_rfc3339();
    let app_type_str = app_type.to_string();
    let mut imported = 0u32;
    let mut skipped = 0u32;

    for (name, entry) in &mcp_entries {
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

fn parse_json_mcp_servers(content: &str) -> Result<Vec<(String, McpServerEntry)>, String> {
    let config: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;

    let mcp_servers = config.get("mcpServers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let mut entries = Vec::new();
    for (name, value) in &mcp_servers {
        let entry: McpServerEntry = serde_json::from_value(value.clone())
            .map_err(|e| format!("Failed to parse server '{}': {}", name, e))?;
        entries.push((name.clone(), entry));
    }
    Ok(entries)
}

fn parse_toml_mcp_servers(content: &str) -> Result<Vec<(String, McpServerEntry)>, String> {
    let config: toml::Value = toml::from_str(content)
        .map_err(|e| format!("Failed to parse TOML: {}", e))?;

    let mcp_section = config.get("mcp_servers")
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default();

    let mut entries = Vec::new();
    for (name, value) in &mcp_section {
        let entry: McpServerEntry = serde_json::from_str(
            &serde_json::to_string(value).unwrap_or_default()
        ).map_err(|e| format!("Failed to parse MCP server '{}': {}", name, e))?;
        entries.push((name.clone(), entry));
    }
    Ok(entries)
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

    // Get all DB server names for stale entry cleanup
    let all_db_names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM mcp_servers",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if is_toml_config(app_type) {
        apply_toml(&path, &bindings, &all_db_names, pool).await
    } else {
        apply_json(&path, &bindings, &all_db_names, pool).await
    }
}

async fn apply_json(
    path: &PathBuf,
    bindings: &[McpAppBinding],
    all_db_names: &[String],
    pool: &sqlx::SqlitePool,
) -> Result<ApplyResult, String> {
    let mut managed_mcp = serde_json::Map::new();

    for binding in bindings {
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

    let mut config: serde_json::Value = if path.exists() {
        let content = tokio::fs::read_to_string(path)
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
    existing_map.retain(|name, _| !all_db_names.contains(&name.to_string()));

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

    atomic_write(path, &content).await?;

    Ok(ApplyResult { applied: bindings.len() as u32 })
}

async fn apply_toml(
    path: &PathBuf,
    bindings: &[McpAppBinding],
    all_db_names: &[String],
    pool: &sqlx::SqlitePool,
) -> Result<ApplyResult, String> {
    let mut config: toml::Value = if path.exists() {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("Failed to read config: {}", e))?;
        toml::from_str(&content).unwrap_or(toml::Value::Table(toml::map::Map::new()))
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let config_table = config.as_table_mut().unwrap();

    // Get or create mcp_servers section
    let mcp_section = config_table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .unwrap();

    // Remove stale managed entries
    mcp_section.retain(|name, _| !all_db_names.contains(&name.to_string()));

    // Add managed entries
    for binding in bindings {
        let server: Option<McpServer> = sqlx::query_as(
            "SELECT id, name, transport_type, command, args, url, headers, env, description, created_at, updated_at FROM mcp_servers WHERE id = ?",
        )
        .bind(&binding.mcp_server_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to query server: {}", e))?;

        let Some(server) = server else { continue };

        let mut table = toml::map::Map::new();

        let transport = server.transport_type.as_str();
        if transport != "stdio" {
            table.insert("type".into(), toml::Value::String(server.transport_type.clone()));
        }

        if let Some(cmd) = &server.command {
            table.insert("command".into(), toml::Value::String(cmd.clone()));
        }

        if let Some(args_str) = &server.args {
            if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(args_str) {
                let toml_args: Vec<toml::Value> = arr.iter()
                    .filter_map(|v| v.as_str().map(|s| toml::Value::String(s.to_string())))
                    .collect();
                if !toml_args.is_empty() {
                    table.insert("args".into(), toml::Value::Array(toml_args));
                }
            }
        }

        if let Some(url) = &server.url {
            table.insert("url".into(), toml::Value::String(url.clone()));
        }

        if let Some(headers_str) = &server.headers {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(headers_str) {
                let mut h_table = toml::map::Map::new();
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        h_table.insert(k, toml::Value::String(s.to_string()));
                    }
                }
                if !h_table.is_empty() {
                    table.insert("headers".into(), toml::Value::Table(h_table));
                }
            }
        }

        if let Some(env_str) = &server.env {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(env_str) {
                let mut e_table = toml::map::Map::new();
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        e_table.insert(k, toml::Value::String(s.to_string()));
                    }
                }
                if !e_table.is_empty() {
                    table.insert("env".into(), toml::Value::Table(e_table));
                }
            }
        }

        mcp_section.insert(server.name.clone(), toml::Value::Table(table));
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let content = toml::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize TOML: {}", e))?;

    atomic_write(path, &content).await?;

    Ok(ApplyResult { applied: bindings.len() as u32 })
}
