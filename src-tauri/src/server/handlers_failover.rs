//! Failover route group: `/failover/<protocol-uri>`.
//!
//! These handlers accept a virtual model name (in `body.model` for
//! completions/responses/anthropic, or in the URL path for Gemini) and resolve
//! it through `VirtualRouter::resolve`. On upstream failure (5xx / 429 / proxy
//! error), they record the failure and retry the next mapping — preserving
//! "sticky" semantics by honouring `virtual_models.current_mapping_id`.

use axum::body::Body;
use axum::extract::{Path, Request};
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::converter::ir::ClientFormat;
use crate::db::get_pool;
use crate::error::ProxyError;
use crate::server::handlers;
use crate::virtual_model::manager::VirtualRouter;
use crate::virtual_model::ResolvedFailover;

pub async fn handle_completions(request: Request) -> Response {
    run_failover(request, ClientFormat::Completions, None, false).await
}

pub async fn handle_responses(request: Request) -> Response {
    run_failover(request, ClientFormat::Responses, None, false).await
}

pub async fn handle_anthropic(request: Request) -> Response {
    run_failover(request, ClientFormat::Anthropic, None, false).await
}

pub async fn handle_gemini(Path(model_segment): Path<String>, request: Request) -> Response {
    let (virtual_name, is_stream) = handlers::parse_gemini_model_segment(&model_segment);
    run_failover(request, ClientFormat::Gemini, Some(virtual_name), is_stream).await
}

/// `GET /failover/v1/models` — OpenAI-style list of virtual models.
pub async fn handle_list_models() -> Response {
    let names = match list_virtual_names().await {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    let data: Vec<Value> = names
        .iter()
        .map(|n| json!({ "id": n, "object": "model", "created": 0, "owned_by": "failover" }))
        .collect();
    axum::Json(json!({ "object": "list", "data": data })).into_response()
}

/// `GET /failover/v1/models/:model` — OpenAI-style get a single virtual model.
pub async fn handle_get_model(Path(model): Path<String>) -> Response {
    let names = match list_virtual_names().await {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    if names.iter().any(|n| n.eq_ignore_ascii_case(&model)) {
        axum::Json(json!({
            "id": model,
            "object": "model",
            "created": 0,
            "owned_by": "failover",
        }))
        .into_response()
    } else {
        ProxyError::ModelNotFound(format!("virtual model '{}' not found", model)).into_response()
    }
}

/// `GET /failover/v1beta/models` — Gemini-style list of virtual models.
pub async fn handle_gemini_list_models() -> Response {
    let names = match list_virtual_names().await {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    let models: Vec<Value> = names
        .iter()
        .map(|n| {
            json!({
                "name": format!("models/{}", n),
                "displayName": n,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            })
        })
        .collect();
    axum::Json(json!({ "models": models })).into_response()
}

/// `GET /failover/v1beta/models/:model` — Gemini-style get a single virtual model.
pub async fn handle_gemini_get_model(Path(model): Path<String>) -> Response {
    let model_name = model.split(':').next().unwrap_or(&model).to_string();
    let names = match list_virtual_names().await {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    if names.iter().any(|n| n.eq_ignore_ascii_case(&model_name)) {
        axum::Json(json!({
            "name": format!("models/{}", model_name),
            "displayName": model_name,
            "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
        }))
        .into_response()
    } else {
        ProxyError::ModelNotFound(format!("virtual model '{}' not found", model_name)).into_response()
    }
}

/// Fetch the names of all enabled virtual models that have at least one
/// enabled+available mapping (so the model is actually usable downstream).
async fn list_virtual_names() -> Result<Vec<String>, ProxyError> {
    let pool = get_pool().await;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT v.name
         FROM virtual_models v
         WHERE v.enabled = 1
           AND EXISTS (
             SELECT 1 FROM virtual_model_mappings m
             JOIN providers p ON p.id = m.provider_id
             JOIN provider_models pm ON pm.id = m.provider_model_id
             WHERE m.virtual_model_id = v.id
               AND m.enabled = 1 AND m.available = 1
               AND p.enabled = 1 AND pm.enabled = 1
           )
         ORDER BY v.name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .map_err(ProxyError::Database)?;
    Ok(rows)
}

async fn run_failover(
    request: Request,
    client_format: ClientFormat,
    override_model: Option<String>,
    force_stream: bool,
) -> Response {
    let start = std::time::Instant::now();
    let (parts, body) = request.into_parts();

    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return ProxyError::Parse(format!("failed to read failover body: {}", e)).into_response();
        }
    };

    // Determine virtual model name: from override (Gemini) or body JSON (others).
    let virtual_name = if let Some(m) = override_model.as_ref() {
        m.clone()
    } else {
        let v: Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                return ProxyError::Parse(format!("invalid JSON in failover request: {}", e))
                    .into_response();
            }
        };
        match v.get("model").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => {
                return ProxyError::Parse("missing 'model' field in failover request".into())
                    .into_response();
            }
        }
    };

    let max_failover = load_max_failover().await;
    let threshold = load_failure_threshold().await;
    info!(
        "[failover] virtual={} fmt={:?} max_failover={} threshold={}",
        virtual_name, client_format, max_failover, threshold
    );

    let mut last_response: Option<Response> = None;
    let mut excluded: Vec<String> = Vec::new();
    let mut attempts = 0u32;

    loop {
        if attempts >= max_failover {
            warn!(
                "[failover] exhausted {} attempts for virtual={}",
                attempts, virtual_name
            );
            return last_response.unwrap_or_else(|| {
                ProxyError::Routing(format!(
                    "all failover mappings exhausted for virtual model '{}'",
                    virtual_name
                ))
                .into_response()
            });
        }

        let resolved = match VirtualRouter::resolve(&virtual_name).await {
            Ok(r) => r,
            Err(e) => {
                if let Some(resp) = last_response.take() {
                    return resp;
                }
                return e.into_response();
            }
        };

        if excluded.iter().any(|m| m == &resolved.mapping_id) && attempts > 0 {
            return last_response.unwrap_or_else(|| {
                ProxyError::Routing(format!(
                    "no healthy mapping left for virtual model '{}'",
                    virtual_name
                ))
                .into_response()
            });
        }

        let request_body_bytes = build_request_body(&body_bytes, &resolved, &client_format);

        // Construct a fresh `http::Request<axum::body::Body>` (= axum's Request)
        // to feed into the standard handle_proxy.
        let mut req_builder = axum::http::Request::builder()
            .method(parts.method.clone())
            .uri(parts.uri.clone());
        for (name, value) in parts.headers.iter() {
            req_builder = req_builder.header(name, value);
        }
        let new_request: Request = req_builder
            .body(Body::from(request_body_bytes))
            .unwrap_or_else(|_| {
                axum::http::Request::builder()
                    .method(Method::POST)
                    .body(Body::from(Vec::new()))
                    .unwrap()
            });

        let response = handlers::handle_proxy(
            new_request,
            client_format.clone(),
            override_for_inner(&client_format, &resolved),
            force_stream,
        )
        .await;

        let status = response.status();
        let is_failure = status == axum::http::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
            || status == axum::http::StatusCode::BAD_GATEWAY;

        if !is_failure {
            if status.is_success() {
                VirtualRouter::record_success(&resolved.mapping_id).await;
                info!(
                    "[failover] virtual={} ok via mapping={} after {} tries, {}ms",
                    virtual_name,
                    resolved.mapping_id,
                    attempts + 1,
                    start.elapsed().as_millis()
                );
            }
            return response;
        }

        let became_unavail = VirtualRouter::record_failure(&resolved.mapping_id, threshold).await;
        warn!(
            "[failover] virtual={} mapping={} status={} became_unavail={}",
            virtual_name, resolved.mapping_id, status, became_unavail
        );
        excluded.push(resolved.mapping_id.clone());
        last_response = Some(response);
        attempts += 1;
    }
}

/// Rewrite the incoming body's `model` field so it carries the real upstream
/// target model. Gemini has no JSON `model` key — the real model is passed via
/// the `override_model` param to `handle_proxy`.
fn build_request_body(
    original: &[u8],
    resolved: &ResolvedFailover,
    client_format: &ClientFormat,
) -> Vec<u8> {
    if matches!(client_format, ClientFormat::Gemini) {
        return original.to_vec();
    }
    let mut v: Value = match serde_json::from_slice(original) {
        Ok(v) => v,
        Err(_) => return original.to_vec(),
    };
    v["model"] = Value::String(resolved.route.target_model.clone());
    serde_json::to_vec(&v).unwrap_or_else(|_| original.to_vec())
}

fn override_for_inner(
    client_format: &ClientFormat,
    resolved: &ResolvedFailover,
) -> Option<String> {
    if matches!(client_format, ClientFormat::Gemini) {
        Some(resolved.route.target_model.clone())
    } else {
        None
    }
}

async fn load_max_failover() -> u32 {
    load_setting("virtual_model_max_failover", 3).await
}

async fn load_failure_threshold() -> u32 {
    load_setting("virtual_model_failure_threshold", 3).await
}

async fn load_setting(key: &str, default: u32) -> u32 {
    let pool = crate::db::get_pool().await;
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}