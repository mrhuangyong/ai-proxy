//! Failover route group: `/failover/<protocol-uri>`.
//!
//! These handlers accept a virtual model name (in `body.model` for
//! completions/responses/anthropic, or in the URL path for Gemini) and resolve
//! it through `VirtualRouter::resolve`. On upstream failure it rotates to the
//! next mapping, with two distinct failure classes:
//!
//! - Provider failure (429 / 5xx / 502): recorded against the mapping's
//!   `consecutive_failures` (may trip the threshold and mark it down).
//! - Parameter/validation 4xx (400/404/413/415/422…, excluding 401/403):
//!   rotated to the next mapping *without* penalty — a param mismatch is a
//!   request-side problem, not a provider outage.
//!
//! Per-model capability flags (`provider_models.supports_*`, migration 026)
//! drive upstream-body sanitization so that a parameter valid for one mapping
//! is stripped before being sent to a different model reached via rotation.

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

pub async fn handle_completions(request: Request) -> Response {
    run_failover(request, ClientFormat::Completions, None, false, None).await
}

pub async fn handle_responses(request: Request) -> Response {
    run_failover(request, ClientFormat::Responses, None, false, None).await
}

pub async fn handle_responses_compact(request: Request) -> Response {
    run_failover(
        request,
        ClientFormat::Responses,
        None,
        false,
        Some("/v1/responses/compact"),
    )
    .await
}

pub async fn handle_anthropic(request: Request) -> Response {
    run_failover(request, ClientFormat::Anthropic, None, false, None).await
}

pub async fn handle_gemini(Path(model_segment): Path<String>, request: Request) -> Response {
    let (virtual_name, is_stream) = handlers::parse_gemini_model_segment(&model_segment);
    run_failover(
        request,
        ClientFormat::Gemini,
        Some(virtual_name),
        is_stream,
        None,
    )
    .await
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
    // Both shapes for codex compatibility (see handlers::handle_list_models).
    axum::Json(json!({ "object": "list", "data": data, "models": data })).into_response()
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
        ProxyError::ModelNotFound(format!("virtual model '{}' not found", model_name))
            .into_response()
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
    endpoint_override: Option<&str>,
) -> Response {
    let start = std::time::Instant::now();
    let (parts, body) = request.into_parts();

    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return ProxyError::Parse(format!("failed to read failover body: {}", e))
                .into_response();
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

        let resolved = match VirtualRouter::resolve_excluding(&virtual_name, &excluded).await {
            Ok(r) => r,
            Err(e) => {
                if let Some(resp) = last_response.take() {
                    return resp;
                }
                return e.into_response();
            }
        };

        // No longer needed: resolve_excluding already skips excluded ids, so if
        // it returns a mapping we know it's a fresh candidate.

        // Pass the ORIGINAL body (with virtual model name) unchanged.
        // handle_proxy_inner will:
        //   1. parse ir_request.model = virtual_model_name
        //   2. capture client_model = virtual_model_name (for logging)
        //   3. use our pre_resolved route (correct provider + target_model)
        //   4. override ir_request.model = route.target_model (real model for upstream)
        // This ensures request logs show the virtual model name as `model`
        // and the real model as `target_model`, and the actual provider
        // matches the failover mapping — so failure counts are accurate.
        let request_body_bytes = body_bytes.to_vec();

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

        let response = handlers::handle_proxy_inner(
            new_request,
            client_format.clone(),
            override_model.clone(),
            force_stream,
            Some(resolved.route.clone()),
            endpoint_override,
            Some(handlers::FailoverContext {
                mapping_id: resolved.mapping_id.clone(),
                threshold,
            }),
        )
        .await;

        let status = response.status();
        // Two distinct failure classes, with different penalty semantics:
        //
        // - Provider failure (429 / 5xx / 502): the upstream is unhealthy.
        //   We record the failure (may trip the threshold and mark the mapping
        //   available=0) and rotate to the next mapping.
        //
        // - Parameter/validation 4xx (400/404/413/415/422…): the request is
        //   incompatible with this mapping (a param the model rejects, or a
        //   capability gap). We rotate to the next mapping so the client gets
        //   a chance on a compatible model, but we do NOT count it against
        //   the mapping's `consecutive_failures` — this is a request-side
        //   problem, not a provider outage, and counting it would let a single
        //   ill-formed client exhaust good mappings.
        //
        //   401/403 are excluded from rotation: an auth failure is almost
        //   certain to repeat on every mapping sharing that provider's keys,
        //   so we return it to the client immediately instead of burning all
        //   `max_failover` attempts.
        let is_provider_failure = status == axum::http::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
            || status == axum::http::StatusCode::BAD_GATEWAY;
        let is_param_4xx = status.is_client_error()
            && status != axum::http::StatusCode::UNAUTHORIZED
            && status != axum::http::StatusCode::FORBIDDEN;

        if !is_provider_failure && !is_param_4xx {
            if status.is_success() {
                // Determine whether this is a streaming response.
                let is_stream = response
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|c| c.contains("text/event-stream"))
                    .unwrap_or(false);

                // Non-stream 200 whose body looks like an error (e.g. upstream
                // returns HTTP 200 with {"error":...} for an internal failure,
                // or "stream ended before any business chunk" surfaced via
                // SessionOutcome::Exhausted). Treat it as a failure so failover
                // can kick in instead of passing the error through to the client.
                if !is_stream {
                    let bytes = match axum::body::to_bytes(response.into_body(), 64 * 1024).await {
                        Ok(b) => b,
                        Err(_) => {
                            // Can't read body — treat as failure to be safe.
                            let became_unavail =
                                VirtualRouter::record_failure(&resolved.mapping_id, threshold)
                                    .await;
                            warn!(
                                "[failover] virtual={} mapping={} body-unreadable became_unavail={}",
                                virtual_name, resolved.mapping_id, became_unavail
                            );
                            excluded.push(resolved.mapping_id.clone());
                            last_response = Some(
                                ProxyError::Provider("unreadable response body".into())
                                    .into_response(),
                            );
                            attempts += 1;
                            continue;
                        }
                    };
                    let body_str = String::from_utf8_lossy(&bytes);
                    let looks_like_error = body_str.contains("\"error\"")
                        || body_str.contains("\"type\":\"upstream_error\"")
                        || body_str.contains("upstream failed after")
                        || body_str.contains("\"invalid")
                        || body_str.contains("unsupported")
                        || body_str.contains("not support")
                        || body_str.contains("does not support");
                    if looks_like_error {
                        let became_unavail =
                            VirtualRouter::record_failure(&resolved.mapping_id, threshold).await;
                        warn!(
                            "[failover] virtual={} mapping={} 200-but-error body, became_unavail={}: {}",
                            virtual_name,
                            resolved.mapping_id,
                            became_unavail,
                            body_str.chars().take(200).collect::<String>()
                        );
                        excluded.push(resolved.mapping_id.clone());
                        // Reconstruct the response so the last attempt's body
                        // is preserved for the client if all mappings fail.
                        let mut resp = axum::body::Body::from(bytes).into_response();
                        *resp.status_mut() = axum::http::StatusCode::BAD_GATEWAY;
                        last_response = Some(resp);
                        attempts += 1;
                        continue;
                    }
                    // Genuine non-stream success.
                    VirtualRouter::record_success(&resolved.mapping_id).await;
                    info!(
                        "[failover] virtual={} ok via mapping={} after {} tries, {}ms",
                        virtual_name,
                        resolved.mapping_id,
                        attempts + 1,
                        start.elapsed().as_millis(),
                    );
                    // Reconstruct response from the bytes we consumed.
                    let mut resp = axum::body::Body::from(bytes).into_response();
                    *resp.status_mut() = status;
                    return resp;
                }
                // Streaming success: accounting deferred to guard drop.
                info!(
                    "[failover] virtual={} ok via mapping={} after {} tries, {}ms (stream)",
                    virtual_name,
                    resolved.mapping_id,
                    attempts + 1,
                    start.elapsed().as_millis(),
                );
            }
            return response;
        }

        // Rotate on failure — but only count provider failures against the
        // mapping's health. A param 4xx rotates silently (no penalty).
        if is_provider_failure {
            let became_unavail =
                VirtualRouter::record_failure(&resolved.mapping_id, threshold).await;
            warn!(
                "[failover] virtual={} mapping={} status={} became_unavail={}",
                virtual_name, resolved.mapping_id, status, became_unavail
            );
        } else {
            // is_param_4xx: rotate to try a compatible mapping, but do not
            // punish this mapping — a parameter mismatch is not an outage.
            info!(
                "[failover] virtual={} mapping={} param/4xx status={}, rotating without penalty",
                virtual_name, resolved.mapping_id, status
            );
        }
        excluded.push(resolved.mapping_id.clone());
        last_response = Some(response);
        attempts += 1;
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
