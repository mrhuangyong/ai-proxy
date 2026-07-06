use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::routing;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(feature = "desktop")]
use crate::apps::handlers;
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
use crate::db::get_pool;
use crate::get_log_layer;
use crate::key::rotation::{KeyRotation, RotationStrategy};
use crate::key::store::decrypt_api_key;
use crate::key::store::encrypt_api_key;
use crate::logging::store::log_request;
use crate::provider::endpoint::Provider;
use crate::provider::manager::ProviderManager;
use crate::usage::pricing::PricingTable;
use sqlx::Row;

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
    Json(ApiResponse {
        success: true,
        data,
    })
}

pub fn err_json(msg: impl Into<String>) -> Json<ApiError> {
    Json(ApiError {
        success: false,
        error: msg.into(),
    })
}

// --- Provider handlers ---

async fn list_providers() -> Result<Json<ApiResponse<Vec<Provider>>>, Json<ApiError>> {
    match ProviderManager::list().await {
        Ok(providers) => Ok(ok(providers)),
        Err(e) => {
            tracing::error!("list_providers error: {}", e);
            Err(err_json(e.to_string()))
        }
    }
}

#[derive(Deserialize)]
struct CreateProviderBody {
    name: String,
    base_url: String,
    format: String,
    endpoint_path: Option<String>,
    upstream_user_agent: Option<String>,
    api_key: String,
    models: Vec<ModelInput>,
}

#[derive(Deserialize)]
struct ModelInput {
    /// Provider-model row id. Present when editing an existing provider's
    /// models so we can UPDATE in place and preserve the row id (which
    /// `virtual_model_mappings` references via FK).
    id: Option<String>,
    model_name: String,
    target_model: Option<String>,
    context_window: Option<i64>,
}

async fn create_provider(
    axum::Json(body): axum::Json<CreateProviderBody>,
) -> Result<Json<ApiResponse<String>>, Json<ApiError>> {
    let pool = get_pool().await;
    let id = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO providers (id, name, base_url, format, endpoint_path, upstream_user_agent) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&body.name)
    .bind(&body.base_url)
    .bind(&body.format)
    .bind(&body.endpoint_path)
    .bind(body.upstream_user_agent.as_deref().unwrap_or(""))
    .execute(pool)
    .await
    .map_err(|e| err_json(e.to_string()))?;

    for m in &body.models {
        let model_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO provider_models (id, provider_id, model_name, target_model, context_window) VALUES (?, ?, ?, ?, ?)")
            .bind(&model_id).bind(&id).bind(&m.model_name).bind(&m.target_model).bind(m.context_window.unwrap_or(272000i64))
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
    upstream_user_agent: Option<String>,
    api_key: Option<String>,
    models: Option<Vec<ModelInput>>,
}

async fn update_provider(
    Path(id): Path<String>,
    axum::Json(body): axum::Json<UpdateProviderBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;

    let current: (String, String, String, Option<String>, String) =
        sqlx::query_as("SELECT name, base_url, format, endpoint_path, upstream_user_agent FROM providers WHERE id = ?")
            .bind(&id)
            .fetch_one(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?;

    let name = body.name.unwrap_or(current.0);
    let base_url = body.base_url.unwrap_or(current.1);
    let format = body.format.unwrap_or(current.2);
    let endpoint_path = body.endpoint_path.unwrap_or(current.3);
    let upstream_user_agent = body.upstream_user_agent.unwrap_or(current.4);

    sqlx::query("UPDATE providers SET name = ?, base_url = ?, format = ?, endpoint_path = ?, upstream_user_agent = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(&name).bind(&base_url).bind(&format).bind(&endpoint_path).bind(&upstream_user_agent).bind(&id)
        .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    if let Some(models) = body.models {
        update_provider_models(pool, &id, &models).await?;
    }

    if let Some(ref plaintext_key) = body.api_key {
        if !plaintext_key.is_empty() {
            let (encrypted, nonce) =
                encrypt_api_key(plaintext_key).map_err(|e| err_json(e.to_string()))?;
            sqlx::query(
                "UPDATE api_keys SET encrypted_key = ?, nonce = ?, label = ? WHERE provider_id = ?",
            )
            .bind(&encrypted)
            .bind(&nonce.as_slice())
            .bind(&name)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?;
        }
    }

    Ok(ok(()))
}

/// Diff/upsert a provider's models while preserving existing row ids.
///
/// `virtual_model_mappings.provider_model_id` references `provider_models.id`
/// with `ON DELETE CASCADE`, so we must NOT delete+re-insert rows that are
/// merely being edited — that would wipe their failover mappings. Instead we
/// UPDATE matched rows in place, INSERT genuinely new rows, and only DELETE
/// rows the user actually removed.
async fn update_provider_models(
    pool: &sqlx::SqlitePool,
    provider_id: &str,
    models: &[ModelInput],
) -> Result<(), Json<ApiError>> {
    use std::collections::HashSet;

    // Snapshot existing rows (id, model_name) for this provider.
    let existing: Vec<(String, String)> =
        sqlx::query_as("SELECT id, model_name FROM provider_models WHERE provider_id = ?")
            .bind(provider_id)
            .fetch_all(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?;

    // Validate that any incoming `id` actually belongs to this provider;
    // a foreign id is ignored (treated as a new row) to avoid cross-provider
    // row hijacking.
    let own_ids: HashSet<&str> = existing.iter().map(|(id, _)| id.as_str()).collect();

    // Track which existing rows the client still references; anything not
    // touched here gets deleted below.
    let mut seen_ids: HashSet<&str> = HashSet::new();

    for m in models {
        // Only honour `id` when it belongs to this provider.
        if let Some(ref mid) = m.id {
            if own_ids.contains(mid.as_str()) {
                seen_ids.insert(mid.as_str());
                sqlx::query(
                    "UPDATE provider_models SET model_name = ?, target_model = ?, context_window = ? WHERE id = ? AND provider_id = ?",
                )
                .bind(&m.model_name)
                .bind(&m.target_model)
                .bind(m.context_window.unwrap_or(272000i64))
                .bind(mid)
                .bind(provider_id)
                .execute(pool)
                .await
                .map_err(|e| err_json(e.to_string()))?;
                continue;
            }
        }

        // New row. Use ON CONFLICT(provider_id, model_name) so a duplicate
        // model_name (e.g. client omitted id for an existing row) becomes an
        // in-place UPDATE instead of a constraint error.
        let new_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO provider_models (id, provider_id, model_name, target_model, context_window) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(provider_id, model_name) DO UPDATE SET \
               target_model = excluded.target_model, \
               context_window = excluded.context_window",
        )
        .bind(&new_id)
        .bind(provider_id)
        .bind(&m.model_name)
        .bind(&m.target_model)
        .bind(m.context_window.unwrap_or(272000i64))
        .execute(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    }

    // Delete rows the user removed. Mappings referencing them are cleaned up
    // by the existing ON DELETE CASCADE — that's the intended behaviour.
    let to_delete: Vec<&str> = own_ids.difference(&seen_ids).copied().collect();
    for did in to_delete {
        sqlx::query("DELETE FROM provider_models WHERE id = ? AND provider_id = ?")
            .bind(did)
            .bind(provider_id)
            .execute(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?;
    }

    Ok(())
}

async fn delete_provider(Path(id): Path<String>) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

async fn toggle_provider(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, Json<ApiError>> {
    match ProviderManager::toggle_enabled(&id).await {
        Ok(new_enabled) => Ok(ok(serde_json::json!({ "enabled": new_enabled }))),
        Err(e) => Err(err_json(e.to_string())),
    }
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
    target_model: String,
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
    final_usage_json: Option<String>,
    upstream_usage_events_json: Option<String>,
    client_user_agent: Option<String>,
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

fn default_page() -> i64 {
    1
}
fn default_limit() -> i64 {
    20
}

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
            conditions.push("(model LIKE ? OR target_model LIKE ?)".to_string());
            let like = format!("%{}%", model.trim());
            params.push(like.clone());
            params.push(like);
        }
    }

    if let Some(ref start) = query.start_date {
        conditions.push("created_at >= ?".to_string());
        params.push(start.clone());
    }

    if let Some(ref end) = query.end_date {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d") {
            let next = (d + chrono::Duration::days(1))
                .format("%Y-%m-%d")
                .to_string();
            conditions.push("created_at < ?".to_string());
            params.push(next);
        }
    }

    let select_cols = "id, request_id, client_format, provider_name, provider_format, model, target_model, stream, status_code, duration_ms, prompt_tokens, completion_tokens, total_tokens, error_message, cached_tokens, ttft_ms, created_at, final_usage_json, upstream_usage_events_json, client_user_agent";
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM request_logs{}", where_clause);
    let data_sql = format!(
        "SELECT {} FROM request_logs{} ORDER BY id DESC LIMIT ? OFFSET ?",
        select_cols, where_clause
    );

    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql);
    let mut data_q = sqlx::query(&data_sql);

    for p in &params {
        count_q = count_q.bind(p);
        data_q = data_q.bind(p);
    }
    data_q = data_q.bind(query.limit).bind(offset);

    let total: (i64,) = count_q
        .fetch_one(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let rows = data_q
        .fetch_all(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;

    let logs = rows
        .into_iter()
        .map(|row| LogEntry {
            id: row.get(0),
            request_id: row.get(1),
            client_format: row.get(2),
            provider_name: row.get(3),
            provider_format: row.get(4),
            model: row.get(5),
            target_model: row.get(6),
            stream: row.get::<i32, _>(7) != 0,
            status_code: row.get(8),
            duration_ms: row.get(9),
            prompt_tokens: row.get::<Option<i64>, _>(10).unwrap_or(0),
            completion_tokens: row.get::<Option<i64>, _>(11).unwrap_or(0),
            total_tokens: row.get::<Option<i64>, _>(12).unwrap_or(0),
            error_message: row.get(13),
            cached_tokens: row.get::<Option<i64>, _>(14).unwrap_or(0),
            ttft_ms: row.get(15),
            created_at: row.get(16),
            final_usage_json: row.get(17),
            upstream_usage_events_json: row.get(18),
            client_user_agent: row.get(19),
        })
        .collect();

    Ok(ok(LogList {
        logs,
        total: total.0,
    }))
}

async fn get_log(Path(id): Path<i64>) -> Result<Json<ApiResponse<LogEntry>>, Json<ApiError>> {
    let pool = get_pool().await;
    let row = sqlx::query(
        "SELECT id, request_id, client_format, provider_name, provider_format, model, target_model, stream, status_code, duration_ms, prompt_tokens, completion_tokens, total_tokens, error_message, cached_tokens, ttft_ms, created_at, final_usage_json, upstream_usage_events_json, client_user_agent FROM request_logs WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool).await.map_err(|e| err_json(e.to_string()))?;

    Ok(ok(LogEntry {
        id: row.get(0),
        request_id: row.get(1),
        client_format: row.get(2),
        provider_name: row.get(3),
        provider_format: row.get(4),
        model: row.get(5),
        target_model: row.get(6),
        stream: row.get::<i32, _>(7) != 0,
        status_code: row.get(8),
        duration_ms: row.get(9),
        prompt_tokens: row.get::<Option<i64>, _>(10).unwrap_or(0),
        completion_tokens: row.get::<Option<i64>, _>(11).unwrap_or(0),
        total_tokens: row.get::<Option<i64>, _>(12).unwrap_or(0),
        error_message: row.get(13),
        cached_tokens: row.get::<Option<i64>, _>(14).unwrap_or(0),
        ttft_ms: row.get(15),
        created_at: row.get(16),
        final_usage_json: row.get(17),
        upstream_usage_events_json: row.get(18),
        client_user_agent: row.get(19),
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
    target_model: String,
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
    /// Day offset for the `days == 1` branch: 0 = today, 1 = yesterday, etc.
    #[serde(default)]
    offset: i64,
}

fn default_days() -> i64 {
    7
}

async fn get_usage(
    Query(query): Query<UsageQuery>,
) -> Result<Json<ApiResponse<UsageSummary>>, Json<ApiError>> {
    let pool = get_pool().await;
    let (sql, param): (&str, String) = if query.days == 1 {
        // Single-day branch supports an offset (0 = today, 1 = yesterday, ...).
        ("SELECT model, target_model, provider_name, \
         SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens), COUNT(*), SUM(cached_tokens) \
         FROM request_logs \
         WHERE date(created_at, 'localtime') = date('now', 'localtime', ?) AND status_code = 200 \
         GROUP BY model, target_model, provider_name \
         ORDER BY SUM(total_tokens) DESC", format!("-{} day", query.offset))
    } else {
        ("SELECT model, target_model, provider_name, \
         SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens), COUNT(*), SUM(cached_tokens) \
         FROM request_logs \
         WHERE created_at >= datetime('now', 'localtime', ? || ' days') AND status_code = 200 \
         GROUP BY model, target_model, provider_name \
         ORDER BY SUM(total_tokens) DESC", format!("-{}", query.days))
    };
    let rows: Vec<(String, String, String, i64, i64, i64, i64, i64)> = sqlx::query_as(sql)
        .bind(&param)
        .fetch_all(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;

    let pricing = PricingTable::default();
    let mut total_cost = 0.0;
    let mut total_requests = 0i64;
    let stats: Vec<UsageStat> = rows
        .into_iter()
        .map(
            |(
                model,
                target_model,
                provider_name,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                request_count,
                cached_tokens,
            )| {
                // Price by the actual model consumed upstream (target_model), falling back to model.
                let cost_model = if target_model.is_empty() {
                    &model
                } else {
                    &target_model
                };
                let cost_estimate =
                    pricing.get_cost(cost_model, prompt_tokens as u32, completion_tokens as u32);
                total_cost += cost_estimate;
                total_requests += request_count;
                UsageStat {
                    model,
                    target_model,
                    provider_name,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    cached_tokens,
                    cost_estimate,
                    request_count,
                }
            },
        )
        .collect();

    Ok(ok(UsageSummary {
        stats,
        total_cost,
        total_requests,
    }))
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
    let (sql, param): (&str, String) = if query.days == 1 {
        ("SELECT strftime('%H:00', created_at, 'localtime'), target_model, SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens) \
         FROM request_logs \
         WHERE date(created_at, 'localtime') = date('now', 'localtime') AND status_code = 200 \
         GROUP BY strftime('%H:00', created_at, 'localtime'), target_model \
         ORDER BY strftime('%H:00', created_at, 'localtime') ASC, target_model ASC", String::new())
    } else {
        ("SELECT DATE(created_at, 'localtime'), target_model, SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens) \
         FROM request_logs WHERE created_at >= datetime('now', 'localtime', ? || ' days') AND status_code = 200 \
         GROUP BY DATE(created_at, 'localtime'), target_model ORDER BY DATE(created_at, 'localtime') ASC, target_model ASC", format!("-{}", query.days))
    };
    let rows: Vec<(String, String, i64, i64, i64)> = if query.days == 1 {
        sqlx::query_as(sql)
            .fetch_all(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?
    } else {
        sqlx::query_as(sql)
            .bind(&param)
            .fetch_all(pool)
            .await
            .map_err(|e| err_json(e.to_string()))?
    };

    Ok(ok(rows
        .into_iter()
        .map(
            |(date, model, prompt_tokens, completion_tokens, total_tokens)| UsageTrendPoint {
                date,
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
            },
        )
        .collect()))
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
    let rules: Vec<InterceptorRule> = rows
        .into_iter()
        .map(
            |(id, name, phase, _rule_type, condition_json, action_json, priority, enabled)| {
                let rule_phase = RulePhase::from_str(&phase).unwrap_or(RulePhase::Pre);
                let condition: RuleCondition =
                    serde_json::from_str(&condition_json).unwrap_or(RuleCondition::Always);
                let action: RuleAction =
                    serde_json::from_str(&action_json).unwrap_or(RuleAction::SetHeader {
                        name: "x-no-op".into(),
                        value: "true".into(),
                    });
                InterceptorRule {
                    id,
                    name,
                    phase: rule_phase,
                    condition,
                    action,
                    priority,
                    enabled: enabled != 0,
                }
            },
        )
        .collect();

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

    let condition_json =
        serde_json::to_string(&body.condition).map_err(|e| err_json(e.to_string()))?;
    let action_json = serde_json::to_string(&body.action).map_err(|e| err_json(e.to_string()))?;

    sqlx::query(
        "INSERT INTO interceptor_rules (id, name, phase, rule_type, condition_json, action_json, priority, enabled) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id).bind(&body.name).bind(&body.phase).bind("custom")
    .bind(&condition_json).bind(&action_json).bind(priority).bind(enabled)
    .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    use crate::interceptor::rules::{RuleAction, RuleCondition, RulePhase};
    let rule_phase = RulePhase::from_str(&body.phase).unwrap_or(RulePhase::Pre);
    let condition: RuleCondition =
        serde_json::from_value(body.condition).unwrap_or(RuleCondition::Always);
    let action: RuleAction = serde_json::from_value(body.action).unwrap_or(RuleAction::SetHeader {
        name: "x-no-op".into(),
        value: "true".into(),
    });

    Ok(ok(InterceptorRule {
        id,
        name: body.name,
        phase: rule_phase,
        condition,
        action,
        priority,
        enabled: enabled != 0,
    }))
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
    let condition_json = body
        .condition
        .map(|c| serde_json::to_string(&c).unwrap_or_default())
        .unwrap_or(current.2);
    let action_json = body
        .action
        .map(|a| serde_json::to_string(&a).unwrap_or_default())
        .unwrap_or(current.3);
    let priority = body.priority.unwrap_or(current.4);
    let enabled = body.enabled.map(|e| e as i32).unwrap_or(current.5 as i32);

    sqlx::query(
        "UPDATE interceptor_rules SET name = ?, phase = ?, condition_json = ?, action_json = ?, priority = ?, enabled = ? WHERE id = ?",
    ).bind(&name).bind(&phase).bind(&condition_json).bind(&action_json).bind(priority).bind(enabled).bind(&id)
    .execute(pool).await.map_err(|e| err_json(e.to_string()))?;

    Ok(ok(()))
}

async fn delete_rule(Path(id): Path<String>) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    sqlx::query("DELETE FROM interceptor_rules WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

// --- Settings handlers ---

#[derive(Serialize)]
struct Settings {
    http_port: String,
    upstream_max_retries: String,
    upstream_retry_backoff_base_ms: String,
    log_retention_days: String,
    record_request_body: String,
    proxy_auth_enabled: String,
    proxy_auth_key: String,
    request_timeout: String,
    connect_timeout: String,
    codex_preserve_auth: String,
    extract_system_from_messages: String,
    upstream_invisible_retry_mode: String,
    upstream_invisible_retry_total_timeout_secs: String,
    upstream_invisible_retry_buffer_limit_mb: String,
    upstream_user_agent: String,
}

async fn get_settings() -> Result<Json<ApiResponse<Settings>>, Json<ApiError>> {
    let pool = get_pool().await;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM settings WHERE key IN ('http_port', 'log_retention_days', 'record_request_body', 'proxy_auth_enabled', 'proxy_auth_key', 'request_timeout', 'connect_timeout', 'codex_preserve_auth', 'upstream_max_retries', 'upstream_retry_backoff_base_ms', 'extract_system_from_messages', 'upstream_invisible_retry_mode', 'upstream_invisible_retry_total_timeout_secs', 'upstream_invisible_retry_buffer_limit_mb', 'upstream_user_agent')"
    ).fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?;

    let map: HashMap<String, String> = rows.into_iter().collect();
    Ok(ok(Settings {
        http_port: map
            .get("http_port")
            .cloned()
            .unwrap_or_else(|| "7860".into()),
        log_retention_days: map
            .get("log_retention_days")
            .cloned()
            .unwrap_or_else(|| "30".into()),
        record_request_body: map
            .get("record_request_body")
            .cloned()
            .unwrap_or_else(|| "false".into()),
        proxy_auth_enabled: map
            .get("proxy_auth_enabled")
            .cloned()
            .unwrap_or_else(|| "false".into()),
        proxy_auth_key: map.get("proxy_auth_key").cloned().unwrap_or_default(),
        request_timeout: map
            .get("request_timeout")
            .cloned()
            .unwrap_or_else(|| "1200".into()),
        connect_timeout: map
            .get("connect_timeout")
            .cloned()
            .unwrap_or_else(|| "30".into()),
        codex_preserve_auth: map
            .get("codex_preserve_auth")
            .cloned()
            .unwrap_or_else(|| "false".into()),
        upstream_max_retries: map
            .get("upstream_max_retries")
            .cloned()
            .unwrap_or_else(|| "10".into()),
        upstream_retry_backoff_base_ms: map
            .get("upstream_retry_backoff_base_ms")
            .cloned()
            .unwrap_or_else(|| "500".into()),
        extract_system_from_messages: map
            .get("extract_system_from_messages")
            .cloned()
            .unwrap_or_else(|| "true".into()),
        upstream_invisible_retry_mode: map
            .get("upstream_invisible_retry_mode")
            .cloned()
            .unwrap_or_else(|| "pre_first_token".into()),
        upstream_invisible_retry_total_timeout_secs: map
            .get("upstream_invisible_retry_total_timeout_secs")
            .cloned()
            .unwrap_or_else(|| "600".into()),
        upstream_invisible_retry_buffer_limit_mb: map
            .get("upstream_invisible_retry_buffer_limit_mb")
            .cloned()
            .unwrap_or_else(|| "32".into()),
        upstream_user_agent: map.get("upstream_user_agent").cloned().unwrap_or_default(),
    }))
}

#[derive(Deserialize)]
struct UpdateSettingsBody {
    upstream_max_retries: Option<String>,
    upstream_retry_backoff_base_ms: Option<String>,
    http_port: Option<String>,
    log_retention_days: Option<String>,
    record_request_body: Option<String>,
    proxy_auth_enabled: Option<String>,
    proxy_auth_key: Option<String>,
    request_timeout: Option<String>,
    connect_timeout: Option<String>,
    codex_preserve_auth: Option<String>,
    extract_system_from_messages: Option<String>,
    upstream_invisible_retry_mode: Option<String>,
    upstream_invisible_retry_total_timeout_secs: Option<String>,
    upstream_invisible_retry_buffer_limit_mb: Option<String>,
    upstream_user_agent: Option<String>,
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
        ("codex_preserve_auth", body.codex_preserve_auth),
        ("upstream_max_retries", body.upstream_max_retries),
        (
            "upstream_retry_backoff_base_ms",
            body.upstream_retry_backoff_base_ms,
        ),
        (
            "extract_system_from_messages",
            body.extract_system_from_messages,
        ),
        (
            "upstream_invisible_retry_mode",
            body.upstream_invisible_retry_mode,
        ),
        (
            "upstream_invisible_retry_total_timeout_secs",
            body.upstream_invisible_retry_total_timeout_secs,
        ),
        (
            "upstream_invisible_retry_buffer_limit_mb",
            body.upstream_invisible_retry_buffer_limit_mb,
        ),
        ("upstream_user_agent", body.upstream_user_agent),
    ];
    for (key, value) in updates {
        if let Some(v) = value {
            sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
                .bind(key).bind(&v)
                .execute(pool).await.map_err(|e| err_json(e.to_string()))?;
        }
    }
    Ok(ok(()))
}

// --- Model test handlers ---

#[derive(Deserialize)]
struct TestModelBody {
    model_name: String,
    provider_id: Option<String>,
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

    let route = match ProviderManager::find_for_model_on_provider(
        &body.model_name,
        body.provider_id.as_deref(),
    )
    .await
    {
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

    let selected_key =
        match KeyRotation::get_next_key(&route.provider_id, &RotationStrategy::LeastUsed).await {
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
            content: vec![IrContentPart::Text {
                text: "Hi, reply with 'OK'.".into(),
                citations: None,
            }],
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

    let url = format!(
        "{}{}",
        route.base_url.trim_end_matches('/'),
        route.endpoint_path
    );

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let mut req_builder = http_client
        .post(&url)
        .json(&target_body)
        .header("Content-Type", "application/json");

    // Inject custom upstream User-Agent so the test request honors the same UA config
    // (provider override > global > none).
    {
        let pool = crate::db::get_pool().await;
        let global_ua: Option<String> = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'upstream_user_agent'",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        let final_ua: &str = if !route.upstream_user_agent.is_empty() {
            &route.upstream_user_agent
        } else if let Some(ref g) = global_ua {
            if !g.is_empty() {
                g.as_str()
            } else {
                ""
            }
        } else {
            ""
        };
        if !final_ua.is_empty() {
            req_builder = req_builder.header("User-Agent", final_ua);
        }
    }

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
                &route.target_model,
                false,
                0,
                duration,
                Some(&e.to_string()),
                0,
                0,
                0,
                None,
                None,
                None,
                0,
                None,
                None,
            )
            .await;
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
            &route.target_model,
            false,
            status.as_u16(),
            duration,
            Some(&err_msg),
            0,
            0,
            0,
            None,
            None,
            None,
            0,
            None,
            None,
        )
        .await;
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
                    if let IrContentPart::Text { text, .. } = part {
                        text_parts.push(text.clone());
                    }
                }
                if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join(""))
                }
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
        &route.target_model,
        false,
        status.as_u16(),
        duration,
        None,
        0,
        0,
        0,
        None,
        None,
        None,
        0,
        None,
        None,
    )
    .await;

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
                if let Some(content) = json
                    .pointer("/choices/0/delta/content")
                    .and_then(|v| v.as_str())
                {
                    text_parts.push(content.to_string());
                }
            }
            ClientFormat::Gemini => {
                // Gemini SSE: {"candidates":[{"content":{"parts":[{"text":"..."}]}}]}
                if let Some(text) = json
                    .pointer("/candidates/0/content/parts/0/text")
                    .and_then(|v| v.as_str())
                {
                    text_parts.push(text.to_string());
                }
            }
        }
    }

    if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(""))
    }
}

// --- Runtime log handlers ---

async fn get_runtime_logs(
) -> Result<Json<ApiResponse<Vec<crate::logging::layer::LogEntry>>>, Json<ApiError>> {
    let layer = get_log_layer();
    let buffer = layer.buffer();
    let entries = buffer.lock().unwrap().snapshot();
    Ok(ok(entries))
}

async fn clear_runtime_logs() -> Result<Json<ApiResponse<serde_json::Value>>, Json<ApiError>> {
    let layer = get_log_layer();
    let buffer = layer.buffer();
    buffer.lock().unwrap().clear();
    Ok(ok(serde_json::json!({ "cleared": true })))
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
                if socket
                    .send(axum::extract::ws::Message::Text(msg.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

// --- Backup handlers ---

#[derive(Serialize)]
struct BackupStatus {
    passphrase_set: bool,
}

async fn backup_status() -> Result<Json<ApiResponse<BackupStatus>>, Json<ApiError>> {
    let pool = get_pool().await;
    let set = crate::backup::export::read_stored_passphrase(pool)
        .await
        .is_ok();
    Ok(ok(BackupStatus {
        passphrase_set: set,
    }))
}

#[derive(Deserialize)]
struct SetPassphraseBody {
    new_passphrase: String,
    old_passphrase: Option<String>,
}

async fn set_passphrase(
    axum::Json(body): axum::Json<SetPassphraseBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    if body.new_passphrase.len() < 8 {
        return Err(err_json("口令至少需要 8 位"));
    }
    let pool = get_pool().await;
    // If a passphrase already exists, require correct old_passphrase.
    if let Ok(existing) = crate::backup::export::read_stored_passphrase(pool).await {
        let old = body.old_passphrase.as_deref().unwrap_or("");
        if old != existing {
            return Err(err_json("当前口令不正确"));
        }
    }
    crate::backup::export::store_passphrase(pool, &body.new_passphrase)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

#[derive(Serialize)]
struct ExportResult {
    data: String, // base64-encoded JSON bytes
}

async fn export_backup() -> Result<Json<ApiResponse<ExportResult>>, Json<ApiError>> {
    let pool = get_pool().await;
    let _guard = crate::backup::backup_lock().lock().await;
    let bytes = crate::backup::export::export_bundle(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    Ok(ok(ExportResult {
        data: B64.encode(&bytes),
    }))
}

#[derive(Deserialize)]
struct ImportBody {
    file_bytes: String, // base64-encoded
    passphrase: Option<String>,
}

#[derive(Serialize)]
struct ImportResult {
    snapshot_saved: bool,
}

async fn import_backup(
    axum::Json(body): axum::Json<ImportBody>,
) -> Result<Json<ApiResponse<ImportResult>>, Json<ApiError>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let bytes = B64
        .decode(&body.file_bytes)
        .map_err(|e| err_json(e.to_string()))?;
    let pool = get_pool().await;
    let _guard = crate::backup::backup_lock().lock().await;
    crate::backup::import::import_bundle(pool, &bytes, body.passphrase.as_deref())
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(ImportResult {
        snapshot_saved: true,
    }))
}

// --- Sync handlers ---

#[derive(Serialize)]
struct SyncConfigResponse {
    enabled: bool,
    webdav_url: String,
    webdav_username: String,
    webdav_path: String,
    auto_enabled: bool,
    auto_interval_minutes: u32,
    sync_on_change: bool,
}

impl From<crate::sync::config::SyncConfig> for SyncConfigResponse {
    fn from(c: crate::sync::config::SyncConfig) -> Self {
        Self {
            enabled: c.enabled,
            webdav_url: c.webdav_url,
            webdav_username: c.webdav_username,
            webdav_path: c.webdav_path,
            auto_enabled: c.auto_enabled,
            auto_interval_minutes: c.auto_interval_minutes,
            sync_on_change: c.sync_on_change,
        }
    }
}

async fn get_sync_config() -> Result<Json<ApiResponse<SyncConfigResponse>>, Json<ApiError>> {
    let pool = get_pool().await;
    let cfg = crate::sync::config::load_config(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(SyncConfigResponse::from(cfg)))
}

#[derive(Deserialize)]
struct UpdateSyncConfigBody {
    enabled: Option<bool>,
    webdav_url: Option<String>,
    webdav_username: Option<String>,
    webdav_password: Option<String>,
    webdav_path: Option<String>,
    auto_enabled: Option<bool>,
    auto_interval_minutes: Option<u32>,
    sync_on_change: Option<bool>,
}

async fn update_sync_config(
    axum::Json(body): axum::Json<UpdateSyncConfigBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    let mut cfg = crate::sync::config::load_config(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    if let Some(v) = body.enabled {
        cfg.enabled = v;
    }
    if let Some(v) = body.webdav_url {
        cfg.webdav_url = v;
    }
    if let Some(v) = body.webdav_username {
        cfg.webdav_username = v;
    }
    if let Some(v) = body.webdav_password {
        cfg.webdav_password = v;
    }
    if let Some(v) = body.webdav_path {
        cfg.webdav_path = v;
    }
    if let Some(v) = body.auto_enabled {
        cfg.auto_enabled = v;
    }
    if let Some(v) = body.auto_interval_minutes {
        cfg.auto_interval_minutes = v;
    }
    if let Some(v) = body.sync_on_change {
        cfg.sync_on_change = v;
    }
    crate::sync::config::save_config(pool, &cfg)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

async fn test_sync_connection() -> Result<Json<ApiResponse<serde_json::Value>>, Json<ApiError>> {
    let pool = get_pool().await;
    let cfg = crate::sync::config::load_config(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let client = match crate::sync::webdav::WebDavClient::from_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            return Ok(ok(
                serde_json::json!({"success": false, "error": e.to_string()}),
            ))
        }
    };
    match client.test_connection().await {
        Ok(()) => Ok(ok(serde_json::json!({"success": true}))),
        Err(e) => Ok(ok(
            serde_json::json!({"success": false, "error": e.to_string()}),
        )),
    }
}

#[derive(Serialize)]
struct UploadResult {
    filename: String,
    size: usize,
}

async fn sync_upload() -> Result<Json<ApiResponse<UploadResult>>, Json<ApiError>> {
    let pool = get_pool().await;
    let _guard = crate::backup::backup_lock().lock().await;
    let cfg = crate::sync::config::load_config(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let bytes = crate::backup::export::export_bundle(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let size = bytes.len();
    let filename = format!(
        "ai-proxy-backup-{}.json",
        chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ")
    );
    let client = crate::sync::webdav::WebDavClient::from_config(&cfg)
        .map_err(|e| err_json(e.to_string()))?;
    client
        .upload(&filename, &bytes)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    crate::sync::config::update_sync_status(pool, "success", "")
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(UploadResult { filename, size }))
}

async fn sync_versions(
) -> Result<Json<ApiResponse<Vec<crate::sync::webdav::RemoteBackup>>>, Json<ApiError>> {
    let pool = get_pool().await;
    let cfg = crate::sync::config::load_config(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let client = crate::sync::webdav::WebDavClient::from_config(&cfg)
        .map_err(|e| err_json(e.to_string()))?;
    let versions = client
        .list_versions()
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(versions))
}

#[derive(Deserialize)]
struct SyncRestoreBody {
    filename: String,
    passphrase: Option<String>,
}

async fn sync_restore(
    axum::Json(body): axum::Json<SyncRestoreBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    let cfg = crate::sync::config::load_config(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let client = crate::sync::webdav::WebDavClient::from_config(&cfg)
        .map_err(|e| err_json(e.to_string()))?;
    let bytes = client
        .download(&body.filename)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let _guard = crate::backup::backup_lock().lock().await;
    crate::backup::import::import_bundle(pool, &bytes, body.passphrase.as_deref())
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

async fn delete_sync_version(
    axum::extract::Path(filename): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    let cfg = crate::sync::config::load_config(pool)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    let client = crate::sync::webdav::WebDavClient::from_config(&cfg)
        .map_err(|e| err_json(e.to_string()))?;
    client
        .delete(&filename)
        .await
        .map_err(|e| err_json(e.to_string()))?;
    Ok(ok(()))
}

#[derive(Serialize)]
struct SyncLastStatus {
    last_upload_at: String,
    last_upload_status: String,
    last_error: String,
}

async fn sync_last_status() -> Result<Json<ApiResponse<SyncLastStatus>>, Json<ApiError>> {
    let pool = get_pool().await;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings WHERE key IN ('sync_last_upload_at','sync_last_upload_status','sync_last_error')")
            .fetch_all(pool).await.map_err(|e| err_json(e.to_string()))?;
    let map: HashMap<String, String> = rows.into_iter().collect();
    Ok(ok(SyncLastStatus {
        last_upload_at: map.get("sync_last_upload_at").cloned().unwrap_or_default(),
        last_upload_status: map
            .get("sync_last_upload_status")
            .cloned()
            .unwrap_or_default(),
        last_error: map.get("sync_last_error").cloned().unwrap_or_default(),
    }))
}

// --- Route registration ---

pub fn api_routes() -> axum::Router {
    let mut router = axum::Router::new()
        .route(
            "/providers",
            axum::routing::get(list_providers).post(create_provider),
        )
        .route(
            "/providers/:id",
            routing::put(update_provider).delete(delete_provider),
        )
        .route("/providers/:id/toggle", routing::put(toggle_provider))
        .route("/logs", axum::routing::get(list_logs).delete(clear_logs))
        .route("/logs/:id", axum::routing::get(get_log))
        .route("/usage", axum::routing::get(get_usage).delete(clear_usage))
        .route("/usage/trend", axum::routing::get(get_usage_trend))
        .route("/models/test", axum::routing::post(test_model))
        .route("/rules", axum::routing::get(list_rules).post(create_rule))
        .route("/rules/:id", routing::put(update_rule).delete(delete_rule))
        .route(
            "/settings",
            axum::routing::get(get_settings).put(update_settings),
        )
        .route(
            "/runtime-logs",
            axum::routing::get(get_runtime_logs).delete(clear_runtime_logs),
        )
        .route("/runtime-logs/stream", axum::routing::get(runtime_logs_ws))
        .route(
            "/skills-marketplace/search",
            axum::routing::get(search_skills_marketplace),
        )
        // Virtual models (failover routing)
        .route(
            "/virtual-models",
            axum::routing::get(crate::virtual_model::api::list_virtual_models)
                .post(crate::virtual_model::api::create_virtual_model),
        )
        .route(
            "/virtual-models/:id",
            routing::put(crate::virtual_model::api::update_virtual_model)
                .delete(crate::virtual_model::api::delete_virtual_model),
        )
        .route(
            "/virtual-models/:id/sticky",
            routing::put(crate::virtual_model::api::set_sticky),
        )
        .route(
            "/virtual-models/:id/mappings",
            axum::routing::post(crate::virtual_model::api::create_mapping),
        )
        .route(
            "/virtual-models/mappings/:mid",
            routing::put(crate::virtual_model::api::update_mapping)
                .delete(crate::virtual_model::api::delete_mapping),
        )
        .route(
            "/virtual-models/mappings/:mid/available",
            routing::put(crate::virtual_model::api::set_mapping_available),
        )
        .route(
            "/virtual-models/real-models",
            axum::routing::get(crate::virtual_model::api::list_real_models),
        )
        // Backup & sync
        .route("/backup/status", axum::routing::get(backup_status))
        .route("/backup/passphrase", axum::routing::put(set_passphrase))
        .route("/backup/export", axum::routing::post(export_backup))
        .route("/backup/import", axum::routing::post(import_backup))
        .route(
            "/sync/config",
            axum::routing::get(get_sync_config).put(update_sync_config),
        )
        .route("/sync/test", axum::routing::post(test_sync_connection))
        .route("/sync/upload", axum::routing::post(sync_upload))
        .route("/sync/versions", axum::routing::get(sync_versions))
        .route("/sync/restore", axum::routing::post(sync_restore))
        .route(
            "/sync/versions/:filename",
            axum::routing::delete(delete_sync_version),
        )
        .route("/sync/last", axum::routing::get(sync_last_status));

    // Desktop mode: app launcher routes
    #[cfg(feature = "desktop")]
    {
        router = router
            .route("/apps", axum::routing::get(handlers::list_apps))
            .route("/apps/launch", axum::routing::post(handlers::launch_app))
            .route(
                "/apps/:app_type/path",
                axum::routing::put(handlers::set_app_path),
            );
    }

    // Server mode: add JWT auth middleware to all API routes
    #[cfg(feature = "server")]
    {
        router = router.layer(axum::middleware::from_fn(
            crate::auth::middleware::jwt_auth_middleware,
        ));
    }

    router
}

#[derive(Debug, Deserialize)]
struct MarketplaceSearchQuery {
    q: String,
    #[serde(default = "marketplace_default_limit")]
    limit: u32,
}

fn marketplace_default_limit() -> u32 {
    20
}

async fn search_skills_marketplace(
    Query(query): Query<MarketplaceSearchQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, Json<ApiError>> {
    let url = format!(
        "https://www.skills.sh/api/search?q={}&limit={}",
        query.q.replace(' ', "+"),
        query.limit
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| err_json(format!("搜索请求失败: {}", e)))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| err_json(format!("解析响应失败: {}", e)))?;

    Ok(ok(body))
}
