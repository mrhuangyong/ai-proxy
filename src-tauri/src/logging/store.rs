use crate::db::get_pool;
use crate::error::ProxyError;

pub async fn log_request(
    request_id: &str,
    client_format: &str,
    provider_name: &str,
    provider_format: &str,
    model: &str,
    target_model: &str,
    stream: bool,
    status_code: u16,
    duration_ms: i64,
    error_message: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    ttft_ms: Option<i64>,
    final_usage_json: Option<&str>,
    upstream_usage_events_json: Option<&str>,
    upstream_retry_count: i64,
    upstream_last_error: Option<&str>,
    client_user_agent: Option<&str>,
) -> Result<(), ProxyError> {
    let pool = get_pool().await;
    let total = prompt_tokens + completion_tokens;

    sqlx::query(
        "INSERT INTO request_logs (request_id, client_format, provider_name, provider_format, model, target_model, stream, status_code, duration_ms, prompt_tokens, completion_tokens, total_tokens, error_message, cached_tokens, ttft_ms, final_usage_json, upstream_usage_events_json, upstream_retry_count, upstream_last_error, client_user_agent) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(request_id)
    .bind(client_format)
    .bind(provider_name)
    .bind(provider_format)
    .bind(model)
    .bind(target_model)
    .bind(stream as i64)
    .bind(status_code as i64)
    .bind(duration_ms)
    .bind(prompt_tokens)
    .bind(completion_tokens)
    .bind(total)
    .bind(error_message)
    .bind(cached_tokens)
    .bind(ttft_ms)
    .bind(final_usage_json)
    .bind(upstream_usage_events_json)
    .bind(upstream_retry_count)
    .bind(upstream_last_error)
    .bind(client_user_agent)
    .execute(pool)
    .await?;

    Ok(())
}
