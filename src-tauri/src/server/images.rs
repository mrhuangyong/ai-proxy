//! Images API relay (`/v1/images/generations`, `/v1/images/edits`).
//!
//! gpt-image generation goes through the standard OpenAI Images paths on the
//! Codex ChatGPT backend (`{base_url}/images/generations|edits`, same OAuth
//! bearer + account headers as the Responses endpoint). Requests and
//! responses are relayed byte-for-byte — generations are JSON, edits are
//! multipart, so the original Content-Type must survive untouched.
//!
//! Provider selection order: explicit `x-aiproxy-provider` header → JSON
//! `model` field routed through normal model routing (`provider/model` or a
//! registered image model) → first enabled codex_oauth provider.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tracing::info;

use crate::error::ProxyError;

const PROVIDER_HINT_HEADER: &str = "x-aiproxy-provider";

/// Select the codex provider an images request goes to.
///
/// Returns (provider_id, provider_name, base_url, resolved_model).
async fn resolve_images_provider(
    hint: Option<&str>,
    json_model: Option<&str>,
) -> Result<(String, String, String, String), Response> {
    let pool = crate::db::get_pool().await;

    let enabled_codex = || async {
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id, name, base_url FROM providers \
             WHERE provider_kind = 'codex_oauth' AND enabled = 1 \
             ORDER BY name LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        row
    };

    // 1. Explicit provider hint (name or id).
    if let Some(h) = hint.map(str::trim).filter(|h| !h.is_empty()) {
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id, name, base_url FROM providers \
             WHERE (id = ?1 OR name = ?1 COLLATE NOCASE) AND enabled = 1 LIMIT 1",
        )
        .bind(h)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        if let Some((id, name, base_url)) = row {
            return Ok((id, name, base_url, json_model.unwrap_or_default().to_string()));
        }
    }

    // 2. Normal model routing (supports `provider/model` and provider_models
    //    entries the user may have added for image models).
    if let Some(model) = json_model.filter(|m| !m.is_empty()) {
        if let Ok(route) =
            crate::provider::manager::ProviderManager::find_for_model(model).await
        {
            if route.provider_kind == crate::codex::PROVIDER_KIND_CODEX {
                return Ok((route.provider_id, route.provider_name, route.base_url, model.to_string()));
            }
        }
    }

    // 3. First enabled codex provider.
    match enabled_codex().await {
        Some((id, name, base_url)) => Ok((
            id,
            name,
            base_url,
            json_model.unwrap_or("gpt-image-1").to_string(),
        )),
        None => Err((
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": {
                    "message": "没有可用的 Codex 渠道：请先在供应商管理中添加并启用一个 Codex 官方渠道",
                    "type": "proxy_error"
                }
            })),
        )
            .into_response()),
    }
}

/// Shared relay: forward the raw body with its content-type, inject the Codex
/// OAuth headers, and return the upstream response verbatim.
async fn relay(
    State(_): State<()>,
    headers: HeaderMap,
    path_segment: &'static str,
    body: axum::body::Bytes,
) -> Response {
    let start = std::time::Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();

    let hint = headers
        .get(PROVIDER_HINT_HEADER)
        .and_then(|v| v.to_str().ok());

    // Generation bodies are JSON and carry `model`; edit bodies are multipart
    // where peeking the model out of raw bytes is unreliable — the hint header
    // or first-enabled-provider fallback covers those.
    let json_model: Option<String> = if path_segment.contains("generations") {
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("model")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
    } else {
        None
    };

    let (provider_id, provider_name, base_url, model) = match resolve_images_provider(hint, json_model.as_deref()).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let mut extra_headers = std::collections::HashMap::new();
    let bearer = match crate::codex::store::get_valid_access_token(&provider_id).await {
        Ok((token, account_id)) => {
            if let Some(acc) = account_id.filter(|a| !a.is_empty()) {
                extra_headers.insert(
                    crate::codex::HEADER_CHATGPT_ACCOUNT_ID.to_string(),
                    acc,
                );
            }
            extra_headers.insert(
                crate::codex::HEADER_OPENAI_BETA.to_string(),
                crate::codex::OPENAI_BETA_VALUE.to_string(),
            );
            extra_headers.insert(
                crate::codex::HEADER_ORIGINATOR.to_string(),
                crate::codex::ORIGINATOR.to_string(),
            );
            token
        }
        Err(e) => {
            log_images_request(
                &request_id,
                &provider_name,
                &model,
                500,
                start.elapsed().as_millis() as i64,
                Some(&e.to_string()),
            )
            .await;
            return ProxyError::Provider(e.to_string()).into_response();
        }
    };

    let upstream_path = if path_segment.contains("edits") {
        "/images/edits"
    } else {
        "/images/generations"
    };
    let url = crate::provider::manager::join_base_url_and_path(&base_url, upstream_path);
    info!(
        "[images] POST {} model={} via provider {}",
        url, model, provider_name
    );

    let mut req = crate::http::SHARED_HTTP_CLIENT
        .post(&url)
        .bearer_auth(&bearer)
        .timeout(std::time::Duration::from_secs(600));
    if let Some(ct) = headers.get("content-type") {
        req = req.header("content-type", ct);
    }
    for (k, v) in &extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req = req.body(body);

    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            log_images_request(
                &request_id,
                &provider_name,
                &model,
                502,
                start.elapsed().as_millis() as i64,
                Some(&msg),
            )
            .await;
            return ProxyError::Network(msg).into_response();
        }
    };

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| "application/json".parse().unwrap());
    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            let msg = e.to_string();
            log_images_request(
                &request_id,
                &provider_name,
                &model,
                502,
                start.elapsed().as_millis() as i64,
                Some(&msg),
            )
            .await;
            return ProxyError::Network(msg).into_response();
        }
    };

    log_images_request(
        &request_id,
        &provider_name,
        &model,
        status.as_u16(),
        start.elapsed().as_millis() as i64,
        None,
    )
    .await;

    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Images requests land in the request log as non-streaming Responses traffic
/// so the Logs page shows them alongside everything else.
async fn log_images_request(
    request_id: &str,
    provider_name: &str,
    model: &str,
    status: u16,
    duration_ms: i64,
    error_message: Option<&str>,
) {
    if let Err(e) = crate::logging::store::log_request(
        request_id,
        "responses",
        provider_name,
        "responses",
        model,
        model,
        false,
        status,
        duration_ms,
        error_message,
        0,
        0,
        0,
        None,
        None,
        None,
        0,
        None,
        None,
        true,
    )
    .await
    {
        tracing::warn!("[images] failed to write request log: {}", e);
    }
}

pub async fn handle_image_generation(
    state: State<()>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    relay(state, headers, "generations", body).await
}

pub async fn handle_image_edit(
    state: State<()>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    relay(state, headers, "edits", body).await
}
