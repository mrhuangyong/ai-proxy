use axum::extract::{Json, Path};
use std::collections::HashMap;

use crate::apps::config;
use crate::apps::launcher;
use crate::apps::types::{AppConfig, AppType, DbAppConfig, LaunchRequest, SetPathRequest};
use crate::db::get_pool;
use crate::server::api::{err_json, ok, ApiError, ApiResponse};

fn build_model_config(body: &LaunchRequest) -> Option<String> {
    if body.model_haiku.is_none()
        && body.model_sonnet.is_none()
        && body.model_opus.is_none()
        && body.models.is_none()
    {
        return None;
    }
    let mut map = serde_json::Map::new();
    if let Some(ref v) = body.model_haiku {
        map.insert("haiku".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(ref v) = body.model_sonnet {
        map.insert("sonnet".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(ref v) = body.model_opus {
        map.insert("opus".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(ref v) = body.models {
        let arr: Vec<serde_json::Value> = v
            .iter()
            .map(|m| serde_json::Value::String(m.clone()))
            .collect();
        map.insert("models".into(), serde_json::Value::Array(arr));
    }
    Some(serde_json::Value::Object(map).to_string())
}

fn parse_model_config(
    json: Option<&str>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Vec<String>>,
) {
    json.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .map(|v| {
            let obj = v.as_object();
            let haiku = obj
                .and_then(|o| o.get("haiku"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let sonnet = obj
                .and_then(|o| o.get("sonnet"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let opus = obj
                .and_then(|o| o.get("opus"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let models = obj
                .and_then(|o| o.get("models"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                });
            (haiku, sonnet, opus, models)
        })
        .unwrap_or((None, None, None, None))
}

// ── Virtual model detection ────────────────────────────────────────────────

/// Check whether any of the given model names is an enabled virtual model.
async fn any_virtual_model(names: &[&str]) -> bool {
    if names.is_empty() {
        return false;
    }
    let pool = get_pool().await;
    let placeholders: Vec<&str> = names.iter().map(|_| "?").collect();
    let sql = format!(
        "SELECT 1 FROM virtual_models WHERE enabled = 1 AND name IN ({}) COLLATE NOCASE LIMIT 1",
        placeholders.join(", ")
    );
    let mut q = sqlx::query_scalar::<_, i64>(&sql);
    for n in names {
        q = q.bind(n);
    }
    q.fetch_optional(pool).await.ok().flatten().unwrap_or(0) != 0
}

// ── App management ─────────────────────────────────────────────────────────

pub async fn list_apps() -> Json<ApiResponse<Vec<AppConfig>>> {
    let pool = get_pool().await;

    let rows: Vec<DbAppConfig> = sqlx::query_as(
        "SELECT app_type, model, proxy_url, launched_at, config_path, install_path, status, work_dir, model_config FROM app_configs",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let db_map: HashMap<String, DbAppConfig> =
        rows.into_iter().map(|r| (r.app_type.clone(), r)).collect();

    let mut result: Vec<AppConfig> = Vec::new();

    for app_type in AppType::all() {
        let key = app_type.to_string();
        let db_rec = db_map.get(&key);

        let custom_path = db_rec.and_then(|r| r.install_path.as_deref());
        let detected_path = launcher::detect_path(&app_type).await;
        let install_path = custom_path
            .filter(|p| !p.trim().is_empty())
            .map(|p| p.to_string())
            .or(detected_path);

        let installed = install_path.is_some();
        let config_path_str = config::config_path_for(&app_type)
            .to_string_lossy()
            .to_string();
        let (model_haiku, model_sonnet, model_opus, opencode_models) =
            parse_model_config(db_rec.as_ref().and_then(|r| r.model_config.as_deref()));

        let app_config = AppConfig {
            app_type,
            installed,
            install_path,
            config_path: Some(config_path_str),
            model: db_rec
                .map(|r| {
                    if r.model.is_empty() {
                        None
                    } else {
                        Some(r.model.clone())
                    }
                })
                .unwrap_or(None),
            proxy_url: db_rec
                .map(|r| {
                    if r.proxy_url.is_empty() {
                        None
                    } else {
                        Some(r.proxy_url.clone())
                    }
                })
                .unwrap_or(None),
            launched_at: db_rec
                .map(|r| {
                    if r.launched_at.is_empty() {
                        None
                    } else {
                        Some(r.launched_at.clone())
                    }
                })
                .unwrap_or(None),
            status: db_rec
                .map(|r| {
                    if r.status.is_empty() {
                        None
                    } else {
                        Some(r.status.clone())
                    }
                })
                .unwrap_or(None),
            model_haiku,
            model_sonnet,
            model_opus,
            opencode_models,
            work_dir: db_rec.as_ref().and_then(|r| {
                if r.work_dir.as_ref().map_or(true, |s| s.is_empty()) {
                    None
                } else {
                    r.work_dir.clone()
                }
            }),
        };

        result.push(app_config);
    }

    ok(result)
}

pub async fn launch_app(
    Json(body): Json<LaunchRequest>,
) -> Result<Json<ApiResponse<AppConfig>>, Json<ApiError>> {
    let app_type = AppType::from_str(&body.app_type)
        .ok_or_else(|| err_json(format!("Unknown app type: {}", body.app_type)))?;

    let pool = get_pool().await;

    // Resolve install path: custom from DB -> auto-detect -> error
    let db_rec: Option<DbAppConfig> = sqlx::query_as(
        "SELECT app_type, model, proxy_url, launched_at, config_path, install_path, status, work_dir, model_config FROM app_configs WHERE app_type = ?",
    )
    .bind(&body.app_type)
    .fetch_optional(pool)
    .await
    .map_err(|e| err_json(e.to_string()))?;

    let custom_path = db_rec.as_ref().and_then(|r| r.install_path.as_deref());
    let detected_path = launcher::detect_path(&app_type).await;
    let install_path = custom_path
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.to_string())
        .or(detected_path)
        .ok_or_else(|| {
            err_json(format!(
                "{} is not installed or path not detected",
                app_type.display_name()
            ))
        })?;

    // Get proxy base URL from settings
    let proxy_settings: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM settings WHERE key IN ('http_port', 'codex_preserve_auth')",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| err_json(e.to_string()))?;

    let settings_map: HashMap<String, String> = proxy_settings.into_iter().collect();
    let port = settings_map
        .get("http_port")
        .cloned()
        .unwrap_or_else(|| "7860".into());
    let preserve_auth = settings_map
        .get("codex_preserve_auth")
        .map(|v| v == "true")
        .unwrap_or(false);
    let proxy_base = format!("http://127.0.0.1:{}", port);

    // Detect whether any of the configured models are virtual models.
    // If so, the proxy_url must use the /failover/ prefix so downstream
    // requests hit the failover route group.
    let all_model_names: Vec<&str> = [
        Some(body.model.as_str()),
        body.model_haiku.as_deref(),
        body.model_sonnet.as_deref(),
        body.model_opus.as_deref(),
    ]
    .into_iter()
    .flatten()
    .chain(body.models.iter().flat_map(|v| v.iter().map(|s| s.as_str())))
    .collect();
    let any_virtual = any_virtual_model(&all_model_names).await;

    let suffix = if any_virtual {
        if app_type.is_codex() || matches!(app_type, AppType::OpenCodeCli) {
            "/failover/v1"
        } else {
            "/failover"
        }
    } else {
        app_type.proxy_url_suffix()
    };
    let proxy_url = format!("{}{}", proxy_base, suffix);
    let now = chrono::Utc::now().to_rfc3339();

    // Write config file
    let model_haiku = body.model_haiku.as_deref();
    let model_sonnet = body.model_sonnet.as_deref();
    let model_opus = body.model_opus.as_deref();
    let model_config_json = build_model_config(&body);

    // Resolve context window from provider_models for the selected model.
    // For virtual models, take the MAX across all its enabled+available mappings.
    let context_window: u64 = if any_virtual && !body.model.is_empty() {
        sqlx::query_scalar::<_, i64>(
            "SELECT MAX(pm.context_window)
             FROM virtual_model_mappings m
             JOIN provider_models pm ON pm.id = m.provider_model_id
             JOIN virtual_models v ON v.id = m.virtual_model_id
             WHERE v.name = ? COLLATE NOCASE AND v.enabled = 1
               AND m.enabled = 1 AND m.available = 1 AND pm.enabled = 1",
        )
        .bind(&body.model)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(272000) as u64
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(context_window, 272000) FROM provider_models WHERE model_name = ? COLLATE NOCASE AND enabled = 1 LIMIT 1",
        )
        .bind(&body.model)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(272000) as u64
    };

    // Resolve API key: all apps use the proxy auth key for authentication against the proxy.
    // The proxy then handles format conversion and upstream provider key rotation transparently.
    let api_key = resolve_proxy_auth_key().await.map_err(|e| {
        err_json(format!(
            "{}: 代理认证密钥解析失败 - 请确认已在设置中配置 API Key ({})",
            app_type.display_name(),
            e
        ))
    })?;

    let write_result = if app_type == AppType::OpenCodeCli {
        let models = body.models.as_deref().unwrap_or(&[]);
        config::write_opencode_config(models, &proxy_url, &api_key).await
    } else {
        config::write_config(
            &app_type,
            &body.model,
            model_haiku,
            model_sonnet,
            model_opus,
            &proxy_url,
            &api_key,
            preserve_auth,
            context_window,
        )
        .await
    };

    if let Err(e) = write_result {
        // Save error status to DB
        let _ = sqlx::query(
            "INSERT OR REPLACE INTO app_configs (app_type, model, proxy_url, launched_at, config_path, install_path, status, work_dir, model_config) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&body.app_type)
        .bind(&body.model)
        .bind(&proxy_url)
        .bind(&now)
        .bind(Option::<String>::None)
        .bind(&install_path)
        .bind("config_error")
        .bind(&body.work_dir)
        .bind(&model_config_json)
        .execute(pool)
        .await;

        return Err(err_json(format!("Failed to write config: {}", e)));
    }

    let config_path = config::config_path_for(&app_type)
        .to_string_lossy()
        .to_string();

    // Launch the app
    let work_dir = body.work_dir.as_deref();
    if let Err(e) = launcher::launch(&app_type, &install_path, work_dir).await {
        // Save error status to DB
        let _ = sqlx::query(
            "INSERT OR REPLACE INTO app_configs (app_type, model, proxy_url, launched_at, config_path, install_path, status, work_dir, model_config) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&body.app_type)
        .bind(&body.model)
        .bind(&proxy_url)
        .bind(&now)
        .bind(&config_path)
        .bind(&install_path)
        .bind("launch_error")
        .bind(&body.work_dir)
        .bind(&model_config_json)
        .execute(pool)
        .await;

        return Err(err_json(format!("Failed to launch: {}", e)));
    }

    // Success — save to DB
    sqlx::query(
        "INSERT OR REPLACE INTO app_configs (app_type, model, proxy_url, launched_at, config_path, install_path, status, work_dir, model_config) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&body.app_type)
    .bind(&body.model)
    .bind(&proxy_url)
    .bind(&now)
    .bind(&config_path)
    .bind(&install_path)
    .bind("success")
    .bind(&body.work_dir)
    .bind(&model_config_json)
    .execute(pool)
    .await
    .map_err(|e| err_json(e.to_string()))?;

    if app_type.is_codex() {
        sync_codex_route_rule(&body.model).await;
    }

    if app_type == AppType::ClaudeDesktop {
        sync_claude_desktop_route_rules(&body.model, model_haiku, model_sonnet, model_opus).await;
    }

    let app_config = AppConfig {
        app_type,
        installed: true,
        install_path: Some(install_path),
        config_path: Some(config_path),
        model: Some(body.model),
        model_haiku: body.model_haiku,
        model_sonnet: body.model_sonnet,
        model_opus: body.model_opus,
        opencode_models: body.models,
        work_dir: body.work_dir,
        proxy_url: Some(proxy_url),
        launched_at: Some(now),
        status: Some("success".to_string()),
    };

    Ok(ok(app_config))
}

pub async fn set_app_path(
    Path(app_type_str): Path<String>,
    Json(body): Json<SetPathRequest>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let _app_type = AppType::from_str(&app_type_str)
        .ok_or_else(|| err_json(format!("Unknown app type: {}", app_type_str)))?;

    let pool = get_pool().await;

    let exists: bool =
        sqlx::query_scalar("SELECT COUNT(*) > 0 FROM app_configs WHERE app_type = ?")
            .bind(&app_type_str)
            .fetch_one(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?;

    if exists {
        sqlx::query("UPDATE app_configs SET install_path = ? WHERE app_type = ?")
            .bind(&body.install_path)
            .bind(&app_type_str)
            .execute(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?;
    } else {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO app_configs (app_type, model, proxy_url, launched_at, config_path, install_path, status) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&app_type_str)
        .bind("")
        .bind("")
        .bind(&now)
        .bind(Option::<String>::None)
        .bind(&body.install_path)
        .bind("pending")
        .execute(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    }

    Ok(ok(()))
}

async fn sync_codex_route_rule(model: &str) {
    let pool = get_pool().await;

    if model.is_empty() {
        let _ = sqlx::query("DELETE FROM interceptor_rules WHERE id = 'auto_codex_model_route'")
            .execute(pool)
            .await;
        return;
    }

    let condition_json = r#"{"type":"path_contains","substring":"/responses"}"#;
    let action_json = format!(r#"{{"type":"replace_model","model":"{}"}}"#, model);

    let _ = sqlx::query(
        "INSERT OR REPLACE INTO interceptor_rules (id, name, phase, rule_type, condition_json, action_json, priority, enabled) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("auto_codex_model_route")
    .bind("Codex 模型自动路由")
    .bind("pre")
    .bind("model_route")
    .bind(condition_json)
    .bind(&action_json)
    .bind(100i64)
    .bind(1i64)
    .execute(pool)
    .await;

    tracing::info!("Synced codex auto-route rule: path=/responses -> {}", model);
}

async fn resolve_proxy_auth_key() -> Result<String, String> {
    let pool = get_pool().await;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'proxy_auth_key'")
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to query proxy_auth_key: {}", e))?;

    row.and_then(|(v,)| if v.is_empty() { None } else { Some(v) })
        .ok_or_else(|| "proxy_auth_key not configured".to_string())
}

/// Claude Desktop sends requests with claude model IDs (claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7).
/// These models don't exist in the proxy, so we create interceptor rules to route them to the actual models.
async fn sync_claude_desktop_route_rules(
    default_model: &str,
    model_haiku: Option<&str>,
    model_sonnet: Option<&str>,
    model_opus: Option<&str>,
) {
    let pool = get_pool().await;

    let rules: Vec<(&str, &str, &str)> = vec![
        (
            "auto_claude_haiku_route",
            "claude-haiku-4-5",
            model_haiku.unwrap_or(default_model),
        ),
        (
            "auto_claude_sonnet_route",
            "claude-sonnet-4-6",
            model_sonnet.unwrap_or(default_model),
        ),
        (
            "auto_claude_opus_route",
            "claude-opus-4-7",
            model_opus.unwrap_or(default_model),
        ),
    ];

    for (rule_id, claude_model, target_model) in &rules {
        let condition_json = format!(r#"{{"type":"model_matches","pattern":"{}"}}"#, claude_model);
        let action_json = format!(r#"{{"type":"replace_model","model":"{}"}}"#, target_model);
        let rule_name = format!(
            "Claude Desktop {} 路由",
            claude_model.strip_prefix("claude-").unwrap_or(claude_model)
        );

        let _ = sqlx::query(
            "INSERT OR REPLACE INTO interceptor_rules (id, name, phase, rule_type, condition_json, action_json, priority, enabled) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(rule_id)
        .bind(&rule_name)
        .bind("pre")
        .bind("model_route")
        .bind(&condition_json)
        .bind(&action_json)
        .bind(100i64)
        .bind(1i64)
        .execute(pool)
        .await;

        tracing::info!(
            "Synced Claude Desktop route rule: {} -> {}",
            claude_model,
            target_model
        );
    }
}
