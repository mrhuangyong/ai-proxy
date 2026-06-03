use axum::Json;
use axum::extract::{Path, Query};
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::routing;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::db::get_pool;
use crate::key::store::encrypt_api_key;
use crate::provider::endpoint::Provider;
use crate::provider::manager::ProviderManager;
use crate::converter::generators::anthropic::AnthropicGenerator;
use crate::converter::generators::completions::CompletionsGenerator;
use crate::converter::generators::gemini::GeminiGenerator;
use crate::converter::generators::responses::ResponsesGenerator;
use crate::converter::ir::{ClientFormat, IrContentPart, IrMessage, IrRequest, IrRole};
use crate::converter::parsers::anthropic::AnthropicParser;
use crate::converter::parsers::completions::CompletionsParser;
use crate::converter::parsers::gemini::GeminiParser;
use crate::converter::parsers::responses::ResponsesParser;
use crate::converter::{FormatGenerator, FormatParser};
use crate::key::rotation::{KeyRotation, RotationStrategy};
use crate::key::store::decrypt_api_key;
use crate::apps::handlers;
use crate::logging::store::log_request;
use crate::get_log_layer;
use crate::usage::pricing::PricingTable;

// --- Unified response types ---

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub data: T,
}

#[derive(Serialize)]
pub struct ApiError {
    pub success: bool,
    pub error: String,
}

pub fn ok<T: Serialize>(data: T) -> Json<ApiResponse<T>> {
    Json(ApiResponse { success: true, data })
}

pub fn err_json(msg: impl Into<String>) -> Json<ApiError> {
    Json(ApiError { success: false, error: msg.into() })
}

// --- Provider handlers ---

async fn list_providers() -> Json<ApiResponse<Vec<Provider>>> {
    match ProviderManager::list().await {
        Ok(providers) => ok(providers),
        Err(e) => {
            tracing::error!("list_providers error: {}", e);
            ok(vec![])
        }
    }
}

#[derive(Deserialize)]
struct CreateProviderBody {
    name: String,
    base_url: String,
    format: String,
    endpoint_path: Option<String>,
    api_key: String,
    models: Vec<ModelInput>,
}

#[derive(Deserialize)]
struct ModelInput {
    model_name: String,
    target_model: Option<String>,
}

async fn create_provider(
    axum::Json(body): axum::Json<CreateProviderBody>,
) -> Result<Json<ApiResponse<String>>, Json<ApiError>> {
    let pool = get_pool().await;
    let id = uuid::Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO providers (id, name, base_url, format, endpoint_path) VALUES (?, ?, ?, ?, ?)")
        .bind(&id).bind(&body.name).bind(&body.base_url).bind(&body.format).bind(&body.endpoint_path)
        .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    for m in &body.models {
        let model_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO provider_models (id, provider_id, model_name, target_model) VALUES (?, ?, ?, ?)")
            .bind(&model_id).bind(&id).bind(&m.model_name).bind(&m.target_model)
            .execute(pool).await.map_err(|e| err_json(e.to_string()))?;
    }

    let (encrypted, nonce) = encrypt_api_key(&body.api_key).map_err(|e| err_json(e.to_string()))?;
    let key_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO api_keys (id, provider_id, label, encrypted_key, nonce) VALUES (?, ?, ?, ?, ?)")
        .bind(&key_id).bind(&id).bind(&body.name).bind(&encrypted).bind(&nonce.as_slice())
        .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    Ok(ok(id))
}

#[derive(Deserialize)]
struct UpdateProviderBody {
    name: Option<String>,
    base_url: Option<String>,
    format: Option<String>,
    endpoint_path: Option<Option<String>>,
    api_key: Option<String>,
    models: Option<Vec<ModelInput>>,
}

async fn update_provider(
    Path(id): Path<String>,
    axum::Json(body): axum::Json<UpdateProviderBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;

    let current: (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT name, base_url, format, endpoint_path FROM providers WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(pool)
    .await
    .map_err(|e| err_json(e.to_string()))?;

    let name = body.name.unwrap_or(current.0);
    let base_url = body.base_url.unwrap_or(current.1);
    let format = body.format.unwrap_or(current.2);
    let endpoint_path = body.endpoint_path.unwrap_or(current.3);

    sqlx::query("UPDATE providers SET name = ?, base_url = ?, format = ?, endpoint_path = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(&name).bind(&base_url).bind(&format).bind(&endpoint_path).bind(&id)
        .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    if let Some(models) = body.models {
        sqlx::query("DELETE FROM provider_models WHERE provider_id = ?")
            .bind(&id)
            .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

        for m in &models {
            let model_id = uuid::Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO provider_models (id, provider_id, model_name, target_model) VALUES (?, ?, ?, ?)")
                .bind(&model_id).bind(&id).bind(&m.model_name).bind(&m.target_model)
                .execute(pool).await.map_err(|e| err_json(e.to_string()))?;
        }
    }

    if let Some(ref plaintext_key) = body.api_key {
        if !plaintext_key.is_empty() {
            let (encrypted, nonce) = encrypt_api_key(plaintext_key).map_err(|e| err_json(e.to_string()))?;
            sqlx::query("UPDATE api_keys SET encrypted_key = ?, nonce = ?, label = ? WHERE provider_id = ?")
                .bind(&encrypted).bind(&nonce.as_slice()).bind(&name).bind(&id)
                .execute(pool).await.map_err(|e| err_json(e.to_string()))?;
        }
    }

    Ok(ok(()))
}

async fn delete_provider(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(&id)
        .execute(pool).await.map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

// --- Log handlers ---

#[derive(Debug, Clone, Serialize)]
struct LogEntry {
    id: i64,
    request_id: String,
    client_format: String,
    provider_name: String,
    provider_format: String,
    model: String,
    stream: bool,
    status_code: Option<i64>,
    duration_ms: Option<i64>,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    cached_tokens: i64,
    ttft_ms: Option<i64>,
    error_message: Option<String>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct LogQuery {
    #[serde(default = "default_page")]
    page: i64,
    #[serde(default = "default_limit")]
    limit: i64,
    model: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
}

fn default_page() -> i64 { 1 }
fn default_limit() -> i64 { 20 }

#[derive(Serialize)]
struct LogList {
    logs: Vec<LogEntry>,
    total: i64,
}

async fn list_logs(
    Query(query): Query<LogQuery>,
) -> Result<Json<ApiResponse<LogList>>, Json<ApiError>> {
    let pool = get_pool().await;
    let offset = (query.page - 1).max(0) * query.limit;

    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();

    if let Some(ref model) = query.model {
        if !model.trim().is_empty() {
            conditions.push("model LIKE ?".to_string());
            params.push(format!("%{}%", model.trim()));
        }
    }

    if let Some(ref start) = query.start_date {
        conditions.push("created_at >= ?".to_string());
        params.push(start.clone());
    }

    if let Some(ref end) = query.end_date {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d") {
            let next = (d + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
            conditions.push("created_at < ?".to_string());
            params.push(next);
        }
    }

    let select_cols = "id, request_id, client_format, provider_name, provider_format, model, stream, status_code, duration_ms, prompt_tokens, completion_tokens, total_tokens, error_message, cached_tokens, ttft_ms, created_at";
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM request_logs{}", where_clause);
    let data_sql = format!("SELECT {} FROM request_logs{} ORDER BY id DESC LIMIT ? OFFSET ?", select_cols, where_clause);

    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql);
    let mut data_q = sqlx::query_as::<_, (i64, String, String, String, String, String, i32, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<i64>, Option<i64>, String)>(&data_sql);

    for p in &params {
        count_q = count_q.bind(p);
        data_q = data_q.bind(p);
    }
    data_q = data_q.bind(query.limit).bind(offset);

    let total: (i64,) = count_q.fetch_one(pool).await.map_err(|e| err_json(e.to_string()))?;
    let rows = data_q.fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?;

    let logs = rows.into_iter().map(|(id, request_id, client_format, provider_name, provider_format, model, stream, status_code, duration_ms, prompt_tokens, completion_tokens, total_tokens, error_message, cached_tokens, ttft_ms, created_at)| {
        LogEntry {
            id, request_id, client_format, provider_name, provider_format, model,
            stream: stream != 0, status_code, duration_ms,
            prompt_tokens: prompt_tokens.unwrap_or(0),
            completion_tokens: completion_tokens.unwrap_or(0),
            total_tokens: total_tokens.unwrap_or(0),
            cached_tokens: cached_tokens.unwrap_or(0),
            ttft_ms,
            error_message, created_at,
        }
    }).collect();

    Ok(ok(LogList { logs, total: total.0 }))
}

async fn get_log(
    Path(id): Path<i64>,
) -> Result<Json<ApiResponse<LogEntry>>, Json<ApiError>> {
    let pool = get_pool().await;
    let row: (i64, String, String, String, String, String, i32, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<i64>, Option<i64>, String) = sqlx::query_as(
        "SELECT id, request_id, client_format, provider_name, provider_format, model, stream, status_code, duration_ms, prompt_tokens, completion_tokens, total_tokens, error_message, cached_tokens, ttft_ms, created_at FROM request_logs WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool).await.map_err(|e| err_json(e.to_string()))?;

    Ok(ok(LogEntry {
        id: row.0, request_id: row.1, client_format: row.2, provider_name: row.3,
        provider_format: row.4, model: row.5, stream: row.6 != 0,
        status_code: row.7, duration_ms: row.8,
        prompt_tokens: row.9.unwrap_or(0), completion_tokens: row.10.unwrap_or(0),
        total_tokens: row.11.unwrap_or(0), error_message: row.12,
        cached_tokens: row.13.unwrap_or(0), ttft_ms: row.14, created_at: row.15,
    }))
}

async fn clear_logs() -> Result<Json<ApiResponse<serde_json::Value>>, Json<ApiError>> {
    let pool = get_pool().await;
    sqlx::query("DELETE FROM request_logs")
        .execute(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(serde_json::json!({ "deleted": true })))
}

// --- Usage handlers ---

#[derive(Debug, Clone, Serialize)]
struct UsageStat {
    model: String,
    provider_name: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    cached_tokens: i64,
    cost_estimate: f64,
    request_count: i64,
}

#[derive(Debug, Clone, Serialize)]
struct UsageSummary {
    stats: Vec<UsageStat>,
    total_cost: f64,
    total_requests: i64,
}

#[derive(Deserialize)]
struct UsageQuery {
    #[serde(default = "default_days")]
    days: i64,
}

fn default_days() -> i64 { 7 }

async fn get_usage(
    Query(query): Query<UsageQuery>,
) -> Result<Json<ApiResponse<UsageSummary>>, Json<ApiError>> {
    let pool = get_pool().await;
    let (sql, param): (&str, String) = if query.days == 1 {
        ("SELECT model, provider_name, \
         SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens), COUNT(*), SUM(cached_tokens) \
         FROM request_logs \
         WHERE date(created_at) = date('now') AND status_code = 200 \
         GROUP BY model, provider_name \
         ORDER BY SUM(total_tokens) DESC", String::new())
    } else {
        ("SELECT model, provider_name, \
         SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens), COUNT(*), SUM(cached_tokens) \
         FROM request_logs \
         WHERE created_at >= datetime('now', ? || ' days') AND status_code = 200 \
         GROUP BY model, provider_name \
         ORDER BY SUM(total_tokens) DESC", format!("-{}", query.days))
    };
    let rows: Vec<(String, String, i64, i64, i64, i64, i64)> = if query.days == 1 {
        sqlx::query_as(sql).fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?
    } else {
        sqlx::query_as(sql).bind(&param).fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?
    };

    let pricing = PricingTable::default();
    let mut total_cost = 0.0;
    let mut total_requests = 0i64;
    let stats: Vec<UsageStat> = rows.into_iter().map(|(model, provider_name, prompt_tokens, completion_tokens, total_tokens, request_count, cached_tokens)| {
        let cost_estimate = pricing.get_cost(&model, prompt_tokens as u32, completion_tokens as u32);
        total_cost += cost_estimate;
        total_requests += request_count;
        UsageStat { model, provider_name, prompt_tokens, completion_tokens, total_tokens, cached_tokens, cost_estimate, request_count }
    }).collect();

    Ok(ok(UsageSummary { stats, total_cost, total_requests }))
}

#[derive(Debug, Clone, Serialize)]
struct UsageTrendPoint {
    date: String,
    model: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

async fn get_usage_trend(
    Query(query): Query<UsageQuery>,
) -> Result<Json<ApiResponse<Vec<UsageTrendPoint>>>, Json<ApiError>> {
    let pool = get_pool().await;
    let rows: Vec<(String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT DATE(created_at), model, SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens) \
         FROM request_logs WHERE created_at >= datetime('now', ? || ' days') AND status_code = 200 \
         GROUP BY DATE(created_at), model ORDER BY DATE(created_at) ASC, model ASC"
    )
    .bind(format!("-{}", query.days))
    .fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?;

    Ok(ok(rows.into_iter().map(|(date, model, prompt_tokens, completion_tokens, total_tokens)| {
        UsageTrendPoint { date, model, prompt_tokens, completion_tokens, total_tokens }
    }).collect()))
}

async fn clear_usage() -> Result<Json<ApiResponse<serde_json::Value>>, Json<ApiError>> {
    let pool = get_pool().await;
    sqlx::query("DELETE FROM request_logs")
        .execute(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(serde_json::json!({ "deleted": true })))
}

// --- Rule handlers ---

use crate::interceptor::rules::InterceptorRule;

async fn list_rules() -> Result<Json<ApiResponse<Vec<InterceptorRule>>>, Json<ApiError>> {
    let pool = get_pool().await;
    let rows: Vec<(String, String, String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, name, phase, rule_type, condition_json, action_json, priority, enabled FROM interceptor_rules ORDER BY priority DESC",
    )
    .fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?;

    use crate::interceptor::rules::{RuleAction, RuleCondition, RulePhase};
    let rules: Vec<InterceptorRule> = rows.into_iter().map(|(id, name, phase, _rule_type, condition_json, action_json, priority, enabled)| {
        let rule_phase = RulePhase::from_str(&phase).unwrap_or(RulePhase::Pre);
        let condition: RuleCondition = serde_json::from_str(&condition_json).unwrap_or(RuleCondition::Always);
        let action: RuleAction = serde_json::from_str(&action_json).unwrap_or(RuleAction::SetHeader { name: "x-no-op".into(), value: "true".into() });
        InterceptorRule { id, name, phase: rule_phase, condition, action, priority, enabled: enabled != 0 }
    }).collect();

    Ok(ok(rules))
}

#[derive(Deserialize)]
struct CreateRuleBody {
    name: String,
    phase: String,
    condition: serde_json::Value,
    action: serde_json::Value,
    priority: Option<i64>,
    enabled: Option<bool>,
}

async fn create_rule(
    axum::Json(body): axum::Json<CreateRuleBody>,
) -> Result<Json<ApiResponse<InterceptorRule>>, Json<ApiError>> {
    let pool = get_pool().await;
    let id = uuid::Uuid::new_v4().to_string();
    let priority = body.priority.unwrap_or(0);
    let enabled = body.enabled.unwrap_or(true) as i32;

    let condition_json = serde_json::to_string(&body.condition).map_err(|e| err_json(e.to_string()))?;
    let action_json = serde_json::to_string(&body.action).map_err(|e| err_json(e.to_string()))?;

    sqlx::query(
        "INSERT INTO interceptor_rules (id, name, phase, rule_type, condition_json, action_json, priority, enabled) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id).bind(&body.name).bind(&body.phase).bind("custom")
    .bind(&condition_json).bind(&action_json).bind(priority).bind(enabled)
    .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    use crate::interceptor::rules::{RuleAction, RuleCondition, RulePhase};
    let rule_phase = RulePhase::from_str(&body.phase).unwrap_or(RulePhase::Pre);
    let condition: RuleCondition = serde_json::from_value(body.condition).unwrap_or(RuleCondition::Always);
    let action: RuleAction = serde_json::from_value(body.action).unwrap_or(RuleAction::SetHeader { name: "x-no-op".into(), value: "true".into() });

    Ok(ok(InterceptorRule { id, name: body.name, phase: rule_phase, condition, action, priority, enabled: enabled != 0 }))
}

#[derive(Deserialize)]
struct UpdateRuleBody {
    name: Option<String>,
    phase: Option<String>,
    condition: Option<serde_json::Value>,
    action: Option<serde_json::Value>,
    priority: Option<i64>,
    enabled: Option<bool>,
}

async fn update_rule(
    Path(id): Path<String>,
    axum::Json(body): axum::Json<UpdateRuleBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    let current: (String, String, String, String, i64, i64) = sqlx::query_as(
        "SELECT name, phase, condition_json, action_json, priority, enabled FROM interceptor_rules WHERE id = ?",
    ).bind(&id).fetch_one(pool).await.map_err(|e| err_json(e.to_string()))?;

    let name = body.name.unwrap_or(current.0);
    let phase = body.phase.unwrap_or(current.1);
    let condition_json = body.condition.map(|c| serde_json::to_string(&c).unwrap_or_default()).unwrap_or(current.2);
    let action_json = body.action.map(|a| serde_json::to_string(&a).unwrap_or_default()).unwrap_or(current.3);
    let priority = body.priority.unwrap_or(current.4);
    let enabled = body.enabled.map(|e| e as i32).unwrap_or(current.5 as i32);

    sqlx::query(
        "UPDATE interceptor_rules SET name = ?, phase = ?, condition_json = ?, action_json = ?, priority = ?, enabled = ? WHERE id = ?",
    ).bind(&name).bind(&phase).bind(&condition_json).bind(&action_json).bind(priority).bind(enabled).bind(&id)
    .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    Ok(ok(()))
}

async fn delete_rule(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    sqlx::query("DELETE FROM interceptor_rules WHERE id = ?")
        .bind(&id)
        .execute(pool).await.map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

// --- Settings handlers ---

#[derive(Serialize)]
struct Settings {
    http_port: String,
    log_retention_days: String,
    record_request_body: String,
    proxy_auth_enabled: String,
    proxy_auth_key: String,
    request_timeout: String,
    connect_timeout: String,
}

async fn get_settings() -> Result<Json<ApiResponse<Settings>>, Json<ApiError>> {
    let pool = get_pool().await;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM settings WHERE key IN ('http_port', 'log_retention_days', 'record_request_body', 'proxy_auth_enabled', 'proxy_auth_key', 'request_timeout', 'connect_timeout')"
    ).fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?;

    let map: HashMap<String, String> = rows.into_iter().collect();
    Ok(ok(Settings {
        http_port: map.get("http_port").cloned().unwrap_or_else(|| "7860".into()),
        log_retention_days: map.get("log_retention_days").cloned().unwrap_or_else(|| "30".into()),
        record_request_body: map.get("record_request_body").cloned().unwrap_or_else(|| "false".into()),
        proxy_auth_enabled: map.get("proxy_auth_enabled").cloned().unwrap_or_else(|| "false".into()),
        proxy_auth_key: map.get("proxy_auth_key").cloned().unwrap_or_default(),
        request_timeout: map.get("request_timeout").cloned().unwrap_or_else(|| "1200".into()),
        connect_timeout: map.get("connect_timeout").cloned().unwrap_or_else(|| "30".into()),
    }))
}

#[derive(Deserialize)]
struct UpdateSettingsBody {
    http_port: Option<String>,
    log_retention_days: Option<String>,
    record_request_body: Option<String>,
    proxy_auth_enabled: Option<String>,
    proxy_auth_key: Option<String>,
    request_timeout: Option<String>,
    connect_timeout: Option<String>,
}

async fn update_settings(
    axum::Json(body): axum::Json<UpdateSettingsBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    let updates = [
        ("http_port", body.http_port),
        ("log_retention_days", body.log_retention_days),
        ("record_request_body", body.record_request_body),
        ("proxy_auth_enabled", body.proxy_auth_enabled),
        ("proxy_auth_key", body.proxy_auth_key),
        ("request_timeout", body.request_timeout),
        ("connect_timeout", body.connect_timeout),
    ];
    for (key, value) in updates {
        if let Some(v) = value {
            sqlx::query("UPDATE settings SET value = ? WHERE key = ?")
                .bind(&v).bind(key)
                .execute(pool).await.map_err(|e| err_json(e.to_string()))?;
        }
    }
    Ok(ok(()))
}

// --- Model test handlers ---

#[derive(Deserialize)]
struct TestModelBody {
    model_name: String,
}

#[derive(Serialize)]
struct TestModelResult {
    success: bool,
    message: String,
    response_text: Option<String>,
    duration_ms: Option<i64>,
    error: Option<String>,
}

fn get_generator(format: &ClientFormat) -> Box<dyn FormatGenerator> {
    match format {
        ClientFormat::Completions => Box::new(CompletionsGenerator),
        ClientFormat::Responses => Box::new(ResponsesGenerator),
        ClientFormat::Anthropic => Box::new(AnthropicGenerator),
        ClientFormat::Gemini => Box::new(GeminiGenerator),
    }
}

fn get_parser(format: &ClientFormat) -> Box<dyn FormatParser> {
    match format {
        ClientFormat::Completions => Box::new(CompletionsParser),
        ClientFormat::Responses => Box::new(ResponsesParser),
        ClientFormat::Anthropic => Box::new(AnthropicParser),
        ClientFormat::Gemini => Box::new(GeminiParser),
    }
}



async fn test_model(
    axum::Json(body): axum::Json<TestModelBody>,
) -> Result<Json<ApiResponse<TestModelResult>>, Json<ApiError>> {
    let start = std::time::Instant::now();

    let route = match ProviderManager::find_for_model(&body.model_name).await {
        Ok(r) => r,
        Err(e) => {
            return Ok(ok(TestModelResult {
                success: false,
                message: "路由解析失败".into(),
                response_text: None,
                duration_ms: None,
                error: Some(e.to_string()),
            }));
        }
    };

    let selected_key = match KeyRotation::get_next_key(&route.provider_id, &RotationStrategy::LeastUsed).await {
        Ok(k) => k,
        Err(e) => {
            return Ok(ok(TestModelResult {
                success: false,
                message: "未找到可用的 API Key".into(),
                response_text: None,
                duration_ms: None,
                error: Some(e.to_string()),
            }));
        }
    };

    let nonce_slice = selected_key.nonce;
    let mut nonce_array = [0u8; 12];
    if nonce_slice.len() == 12 {
        nonce_array.copy_from_slice(&nonce_slice);
    } else {
        return Ok(ok(TestModelResult {
            success: false,
            message: "Nonce 格式错误".into(),
            response_text: None,
            duration_ms: None,
            error: Some("invalid nonce length".into()),
        }));
    }

    let api_key = match decrypt_api_key(&selected_key.encrypted_key, &nonce_array) {
        Ok(k) => k,
        Err(e) => {
            return Ok(ok(TestModelResult {
                success: false,
                message: "API Key 解密失败".into(),
                response_text: None,
                duration_ms: None,
                error: Some(e.to_string()),
            }));
        }
    };

    let ir_request = IrRequest {
        model: route.target_model.clone(),
        messages: vec![IrMessage {
            role: IrRole::User,
            content: vec![IrContentPart::Text { text: "Hi, reply with 'OK'.".into() }],
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }],
        tools: None,
        tool_choice: None,
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        max_tokens: Some(32),
        stream: true,
        stop_sequences: None,
        response_format: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        thinking: None,
        stream_options: None,
        metadata: HashMap::new(),
        extra: HashMap::new(),
    };

    let generator = get_generator(&route.target_format);
    let target_body = match generator.generate_request(&ir_request) {
        Ok(b) => b,
        Err(e) => {
            return Ok(ok(TestModelResult {
                success: false,
                message: "请求格式转换失败".into(),
                response_text: None,
                duration_ms: None,
                error: Some(e.to_string()),
            }));
        }
    };

    let url = format!("{}{}", route.base_url.trim_end_matches('/'), route.endpoint_path);

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let mut req_builder = http_client
        .post(&url)
        .json(&target_body)
        .header("Content-Type", "application/json");

    match route.target_format {
        ClientFormat::Anthropic => {
            req_builder = req_builder.header("x-api-key", &api_key);
            req_builder = req_builder.header("anthropic-version", "2023-06-01");
        }
        _ => {
            req_builder = req_builder.bearer_auth(&api_key);
        }
    }

    let resp = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            let duration = start.elapsed().as_millis() as i64;
            let _ = log_request(
                &format!("test-{}", uuid::Uuid::new_v4()),
                "test",
                &route.provider_name,
                &format!("{:?}", route.target_format).to_lowercase(),
                &body.model_name,
                false,
                0,
                duration,
                Some(&e.to_string()),
                0, 0, 0, None,
            ).await;
            return Ok(ok(TestModelResult {
                success: false,
                message: "请求上游供应商失败".into(),
                response_text: None,
                duration_ms: Some(duration),
                error: Some(e.to_string()),
            }));
        }
    };

    let status = resp.status();
    let resp_body = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            return Ok(ok(TestModelResult {
                success: false,
                message: "读取响应失败".into(),
                response_text: None,
                duration_ms: Some(start.elapsed().as_millis() as i64),
                error: Some(e.to_string()),
            }));
        }
    };

    let duration = start.elapsed().as_millis() as i64;

    if !status.is_success() {
        let err_msg: String = resp_body.chars().take(500).collect();
        let _ = log_request(
            &format!("test-{}", uuid::Uuid::new_v4()),
            "test",
            &route.provider_name,
            &format!("{:?}", route.target_format).to_lowercase(),
            &body.model_name,
            false,
            status.as_u16(),
            duration,
            Some(&err_msg),
            0, 0, 0, None,
        ).await;
        return Ok(ok(TestModelResult {
            success: false,
            message: format!("上游返回错误状态: {}", status),
            response_text: None,
            duration_ms: Some(duration),
            error: Some(err_msg),
        }));
    }

    let response_text = if resp_body.starts_with("data:") || resp_body.starts_with("event:") {
        // SSE streaming response — extract text from delta events
        extract_text_from_sse(&resp_body, &route.target_format)
    } else if let Ok(resp_value) = serde_json::from_str::<serde_json::Value>(&resp_body) {
        // Non-streaming JSON response (fallback)
        let parser = get_parser(&route.target_format);
        match parser.parse_response(&resp_value) {
            Ok(ir_resp) => {
                let mut text_parts: Vec<String> = Vec::new();
                for part in &ir_resp.message.content {
                    if let IrContentPart::Text { text } = part {
                        text_parts.push(text.clone());
                    }
                }
                if text_parts.is_empty() { None } else { Some(text_parts.join("")) }
            }
            Err(_) => Some(resp_body.chars().take(200).collect()),
        }
    } else {
        Some(resp_body.chars().take(200).collect())
    };

    let _ = log_request(
        &format!("test-{}", uuid::Uuid::new_v4()),
        "test",
        &route.provider_name,
        &format!("{:?}", route.target_format).to_lowercase(),
        &body.model_name,
        false,
        status.as_u16(),
        duration,
        None,
        0, 0, 0, None,
    ).await;

    Ok(ok(TestModelResult {
        success: true,
        message: "测试成功".into(),
        response_text,
        duration_ms: Some(duration),
        error: None,
    }))
}

/// Extract concatenated text from SSE streaming response.
/// Handles Anthropic, OpenAI, and Gemini SSE event formats.
fn extract_text_from_sse(resp_body: &str, format: &ClientFormat) -> Option<String> {
    let mut text_parts: Vec<String> = Vec::new();

    for line in resp_body.lines() {
        let data = if let Some(d) = line.strip_prefix("data:") {
            d.trim()
        } else {
            continue;
        };

        if data == "[DONE]" || data.is_empty() {
            continue;
        }

        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };

        match format {
            ClientFormat::Anthropic => {
                // Anthropic SSE: {"type":"content_block_delta","delta":{"type":"text_delta","text":"..."}}
                if json.get("type").and_then(|v| v.as_str()) == Some("content_block_delta") {
                    if let Some(text) = json.pointer("/delta/text").and_then(|v| v.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
            }
            ClientFormat::Completions | ClientFormat::Responses => {
                // OpenAI SSE: {"choices":[{"delta":{"content":"..."}}]}
                if let Some(content) = json.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
                    text_parts.push(content.to_string());
                }
            }
            ClientFormat::Gemini => {
                // Gemini SSE: {"candidates":[{"content":{"parts":[{"text":"..."}]}}]}
                if let Some(text) = json.pointer("/candidates/0/content/parts/0/text").and_then(|v| v.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
        }
    }

    if text_parts.is_empty() { None } else { Some(text_parts.join("")) }
}

// --- Runtime log handlers ---

async fn get_runtime_logs() -> Result<Json<ApiResponse<Vec<crate::logging::layer::LogEntry>>>, Json<ApiError>> {
    let layer = get_log_layer();
    let buffer = layer.buffer();
    let entries = buffer.lock().unwrap().snapshot();
    Ok(ok(entries))
}

async fn runtime_logs_ws(ws: WebSocketUpgrade) -> axum::response::Response {
    ws.on_upgrade(handle_runtime_logs_ws)
}

async fn handle_runtime_logs_ws(mut socket: WebSocket) {
    let mut rx = get_log_layer().subscribe();
    loop {
        match rx.recv().await {
            Ok(entry) => {
                let msg = serde_json::to_string(&entry).unwrap_or_default();
                if socket.send(axum::extract::ws::Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

// --- Route registration ---

pub fn api_routes() -> axum::Router {
    axum::Router::new()
        .route("/providers", axum::routing::get(list_providers).post(create_provider))
        .route("/providers/:id", routing::put(update_provider).delete(delete_provider))
        .route("/logs", axum::routing::get(list_logs).delete(clear_logs))
        .route("/logs/:id", axum::routing::get(get_log))
        .route("/usage", axum::routing::get(get_usage).delete(clear_usage))
        .route("/usage/trend", axum::routing::get(get_usage_trend))
        .route("/models/test", axum::routing::post(test_model))
        .route("/rules", axum::routing::get(list_rules).post(create_rule))
        .route("/rules/:id", routing::put(update_rule).delete(delete_rule))
        .route("/settings", axum::routing::get(get_settings).put(update_settings))
        .route("/apps", axum::routing::get(handlers::list_apps))
        .route("/apps/launch", axum::routing::post(handlers::launch_app))
        .route("/apps/:app_type/path", axum::routing::put(handlers::set_app_path))
        .route("/runtime-logs", axum::routing::get(get_runtime_logs))
        .route("/runtime-logs/stream", axum::routing::get(runtime_logs_ws))
}
