//! Protocol passthrough: forward the client request body as-is when the
//! upstream provider speaks the client's protocol (one of the provider's
//! configured `provider_protocols` rows, migration 028).
//!
//! Unlike the conversion path (parse → IR → generate), the request body is
//! relayed unchanged — the only rewrite is the `model` field when routing
//! mapped the client model to a different target model (JSON-level, mirroring
//! the count_tokens precedent). Responses (SSE and JSON) are also relayed
//! byte-for-byte; the per-format parsers are reused read-only to extract
//! usage and detect upstream error events for logging / failover accounting.
//!
//! Skipped by design (that is what "no parameter mediation" means):
//! capability sanitization, DeepSeek reasoning-cache injection, IR-level
//! parameter handling. Requests that hit body-level interceptor rules fall
//! back to the conversion path before reaching here.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::stream::{StreamExt, TryStreamExt};
use tracing::{error, info, warn};

use crate::converter::ir::ClientFormat;
use crate::provider::manager::{ResolvedProtocol, ResolvedRoute};
use crate::server::handlers::{
    get_parser, log_request_entry, FailoverContext, StreamLogState, StreamLoggingGuard,
};
use crate::server::retry_session::{self, SessionOutcome};

/// Everything `forward` needs; assembled by `handle_proxy_inner` right after
/// key selection and UA injection, before the conversion path would start.
pub(crate) struct PassthroughContext {
    pub route: ResolvedRoute,
    pub protocol: ResolvedProtocol,
    /// Client protocol == protocol.format (caller guarantees the match).
    pub client_format: ClientFormat,
    /// Original client JSON body; only `model` may be rewritten before send.
    pub body: serde_json::Value,
    pub stream: bool,
    /// Client-facing model name (post interceptor/override), for logging.
    pub client_model: String,
    /// Headers to forward + injected UA / interceptor header rules.
    pub extra_headers: HashMap<String, String>,
    /// Primary decrypted API key (fallback when the rotation pool is empty).
    pub api_key: String,
    /// e.g. `/v1/responses/compact` — only applied for Responses upstreams.
    pub endpoint_override: Option<String>,
    pub failover_ctx: Option<FailoverContext>,
    pub request_id: String,
    pub start: std::time::Instant,
    pub client_user_agent: Option<String>,
}

pub(crate) async fn forward(ctx: PassthroughContext) -> Response {
    let PassthroughContext {
        route,
        protocol,
        client_format,
        mut body,
        stream,
        client_model,
        extra_headers,
        api_key,
        endpoint_override,
        failover_ctx,
        request_id,
        start,
        client_user_agent,
    } = ctx;

    let target_model = route.target_model.clone();

    let (url, body) = prepare_upstream(
        &protocol,
        &client_format,
        &body,
        &target_model,
        stream,
        endpoint_override.as_deref(),
    );

    info!(
        "[passthrough] POST {} model={} stream={} ({} -> {} as-is)",
        url,
        target_model,
        stream,
        route.provider_name,
        client_format_dbg(&client_format)
    );

    // Same retry policy as the conversion path, including the failover
    // collapse so a dead upstream surfaces fast to run_failover.
    let mut retry_config = retry_session::load_config_from_db(client_format.clone()).await;
    let is_failover = failover_ctx.is_some();
    if is_failover {
        retry_config.max_attempts = 1;
        retry_config.total_timeout = std::time::Duration::from_secs(30);
    }

    // Decrypt all keys for rotation (mirrors the conversion path).
    let mut decrypted_keys: Vec<String> = Vec::new();
    let all_keys = crate::key::store::list_active_keys(&route.provider_id)
        .await
        .unwrap_or_default();
    for k in all_keys {
        if k.nonce.len() != 12 {
            continue;
        }
        let mut nonce_arr = [0u8; 12];
        nonce_arr.copy_from_slice(&k.nonce);
        if let Ok(d) = crate::key::store::decrypt_api_key(&k.encrypted_key, &nonce_arr) {
            decrypted_keys.push(d);
        }
    }
    if decrypted_keys.is_empty() {
        decrypted_keys.push(api_key);
    }

    let http_client = crate::http::SHARED_HTTP_CLIENT.clone();
    let url_clone = url.clone();
    let body_clone = body.clone();
    let extra_clone = extra_headers.clone();
    let fmt_clone = client_format.clone();

    let factory = move |key: &str| {
        let client = http_client.clone();
        let url = url_clone.clone();
        let body = body_clone.clone();
        let extra = extra_clone.clone();
        let fmt = fmt_clone.clone();
        let key_owned = key.to_string();
        async move {
            // `.json()` sets Content-Type; adding it again appends a second
            // value which strict upstreams reject with 415.
            let mut b = client.post(&url).json(&body);
            if stream {
                // Streaming bodies may run for minutes; a short .timeout()
                // would attach to the whole body lifecycle and cut mid-stream.
                b = b.timeout(std::time::Duration::from_secs(86400));
            } else {
                let per_req_timeout = if is_failover {
                    std::time::Duration::from_secs(30)
                } else {
                    std::time::Duration::from_secs(7200)
                };
                b = b.timeout(per_req_timeout);
            }
            match fmt {
                ClientFormat::Anthropic => {
                    b = b
                        .header("x-api-key", &key_owned)
                        .header("anthropic-version", "2023-06-01");
                }
                _ => {
                    b = b.bearer_auth(&key_owned);
                }
            }
            for (k, v) in &extra {
                b = b.header(k.as_str(), v.as_str());
            }
            b
        }
    };

    let session_outcome = retry_session::run_upstream_session(
        factory,
        decrypted_keys,
        retry_config,
        client_format.clone(),
        stream,
    )
    .await;

    match session_outcome {
        SessionOutcome::CompletedBuffer {
            status,
            bytes,
            retry_count,
        } => {
            non_stream_response(
                &request_id,
                &client_format,
                &route,
                &client_model,
                &target_model,
                status,
                bytes,
                retry_count,
                &start,
                client_user_agent.as_deref(),
            )
            .await
        }
        SessionOutcome::StartedStreaming {
            status,
            buffered_bytes,
            remaining,
            retry_count,
        } => {
            stream_response(
                &request_id,
                &client_format,
                &route,
                &client_model,
                &target_model,
                status,
                buffered_bytes,
                remaining,
                retry_count,
                &start,
                client_user_agent,
                failover_ctx,
            )
            .await
        }
        SessionOutcome::Exhausted {
            last_status,
            last_error,
            retry_count,
            partial_buffer,
        } => {
            // Replay any buffered partial SSE plus an error trailer, mirroring
            // the conversion path so clients can still consume partial output.
            if let Some(ref buffered) = partial_buffer {
                if stream {
                    let trailer = crate::server::retry_invisible::error_trailer_event(
                        client_format.clone(),
                        &format!("upstream buffer cap exceeded after {} retries", retry_count),
                    );
                    let body_stream = futures::stream::iter(vec![
                        Ok::<_, std::io::Error>(Bytes::from(buffered.clone())),
                        Ok(Bytes::from(trailer)),
                    ]);
                    let body = axum::body::Body::from_stream(body_stream);
                    let mut response = Response::new(body);
                    *response.status_mut() = axum::http::StatusCode::OK;
                    response.headers_mut().insert(
                        axum::http::header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_static("text/event-stream"),
                    );
                    let _ = log_request_entry(
                        &request_id,
                        &client_format,
                        &route.provider_name,
                        &client_format,
                        &client_model,
                        &target_model,
                        stream,
                        200,
                        start.elapsed().as_millis() as i64,
                        Some(&format!(
                            "buffer cap hit, partial stream emitted after {} retries",
                            retry_count
                        )),
                        0,
                        0,
                        0,
                        None,
                        None,
                        None,
                        retry_count as i64,
                        Some(&last_error),
                        client_user_agent.as_deref(),
                        true,
                    )
                    .await;
                    return response;
                }
            }

            let status = last_status.unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
            let err_msg = if partial_buffer.is_some() {
                format!(
                    "upstream buffer cap exceeded after {} retries: {}",
                    retry_count, last_error
                )
            } else {
                format!(
                    "upstream failed after {} retries: {}",
                    retry_count, last_error
                )
            };
            log_passthrough_error(
                &request_id,
                &client_format,
                &route,
                &client_model,
                &target_model,
                stream,
                status.as_u16(),
                &err_msg,
                retry_count,
                Some(last_error.as_str()),
                &start,
                client_user_agent.as_deref(),
            )
            .await;
            let error_body = serde_json::json!({
                "error": { "message": err_msg, "type": "upstream_error", "code": status.as_u16() }
            });
            let mut response = axum::Json(error_body).into_response();
            *response.status_mut() = status;
            response
        }
        SessionOutcome::Fatal { status, body } => {
            log_passthrough_error(
                &request_id,
                &client_format,
                &route,
                &client_model,
                &target_model,
                stream,
                status.as_u16(),
                &body,
                0,
                None,
                &start,
                client_user_agent.as_deref(),
            )
            .await;
            let mut response = axum::Json(serde_json::json!({
                "error": { "message": body, "type": "upstream_error", "code": status.as_u16() }
            }))
            .into_response();
            *response.status_mut() = status;
            response
        }
    }
}

fn client_format_dbg(f: &ClientFormat) -> String {
    format!("{:?}", f).to_lowercase()
}

async fn log_passthrough_error(
    request_id: &str,
    client_format: &ClientFormat,
    route: &ResolvedRoute,
    client_model: &str,
    target_model: &str,
    stream: bool,
    status_code: u16,
    err_msg: &str,
    retry_count: u32,
    last_error: Option<&str>,
    start: &std::time::Instant,
    client_user_agent: Option<&str>,
) {
    if let Err(le) = log_request_entry(
        request_id,
        client_format,
        &route.provider_name,
        client_format,
        client_model,
        target_model,
        stream,
        status_code,
        start.elapsed().as_millis() as i64,
        Some(err_msg),
        0,
        0,
        0,
        None,
        None,
        None,
        retry_count as i64,
        last_error,
        client_user_agent,
        true,
    )
    .await
    {
        error!("Passthrough upstream logging failed: {}", le);
    }
}

/// Non-streaming: relay the upstream body byte-for-byte. The same-format
/// parser is used read-only to pull usage numbers for the request log.
async fn non_stream_response(
    request_id: &str,
    client_format: &ClientFormat,
    route: &ResolvedRoute,
    client_model: &str,
    target_model: &str,
    status: reqwest::StatusCode,
    bytes: Vec<u8>,
    retry_count: u32,
    start: &std::time::Instant,
    client_user_agent: Option<&str>,
) -> Response {
    if !status.is_success() {
        let body_text = String::from_utf8_lossy(&bytes).into_owned();
        error!(
            "[passthrough] upstream status={} model={}",
            status.as_u16(),
            target_model
        );
        log_passthrough_error(
            request_id,
            client_format,
            route,
            client_model,
            target_model,
            false,
            status.as_u16(),
            &body_text,
            retry_count,
            None,
            start,
            client_user_agent,
        )
        .await;
        // Relay the upstream error body unchanged — same protocol downstream.
        let mut response = Response::new(axum::body::Body::from(bytes));
        *response.status_mut() = status;
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        return response;
    }

    // Usage extraction only; parse failures degrade to zero-token logging and
    // never block the relay. Responses bodies also get the reasoning dialect
    // fix-up (`normalize_responses_body`) before being relayed, so clients
    // see the standard `summary` shape instead of the upstream's `content`.
    let mut relay_bytes = bytes.clone();
    let mut prompt_tokens: i64 = 0;
    let mut completion_tokens: i64 = 0;
    let mut cached_tokens: i64 = 0;
    let mut final_usage_json: Option<String> = None;
    let mut upstream_usage_events_json: Option<String> = None;
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        let value = if *client_format == ClientFormat::Responses {
            let normalized = normalize_responses_body(&value);
            if normalized != value {
                relay_bytes = serde_json::to_vec(&normalized).unwrap_or_else(|_| bytes.clone());
            }
            normalized
        } else {
            value
        };
        if let Ok(ir_response) = get_parser(client_format).parse_response(&value) {
            prompt_tokens = ir_response.usage.prompt_tokens as i64;
            completion_tokens = ir_response.usage.completion_tokens as i64;
            cached_tokens = ir_response.usage.cached_tokens as i64;
            final_usage_json = serde_json::to_string(&ir_response.usage).ok();
            if let Some(raw) = ir_response.usage.raw.as_ref() {
                upstream_usage_events_json =
                    serde_json::to_string(&serde_json::Value::Array(vec![raw.clone()])).ok();
            }
        }
    }

    if let Err(le) = log_request_entry(
        request_id,
        client_format,
        &route.provider_name,
        client_format,
        client_model,
        target_model,
        false,
        200,
        start.elapsed().as_millis() as i64,
        None,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        Some(start.elapsed().as_millis() as i64),
        final_usage_json.as_deref(),
        upstream_usage_events_json.as_deref(),
        retry_count as i64,
        None,
        client_user_agent,
        true,
    )
    .await
    {
        error!("Passthrough non-stream logging failed: {}", le);
    }

    info!(
        "[passthrough DONE] {} status=200 duration={}ms tokens={}/{}",
        target_model,
        start.elapsed().as_millis(),
        prompt_tokens,
        completion_tokens
    );

    let mut response = Response::new(axum::body::Body::from(relay_bytes));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response
}

/// Streaming: relay upstream SSE bytes (with Responses dialect fix-ups, see
/// `normalize_sse_line`) while scanning lines via the same-format parser,
/// read-only, for usage totals and upstream error events. Heartbeat /
/// idle-timeout / logging mirror the conversion-path stream loop.
#[allow(clippy::too_many_arguments)]
async fn stream_response(
    request_id: &str,
    client_format: &ClientFormat,
    route: &ResolvedRoute,
    client_model: &str,
    target_model: &str,
    status: reqwest::StatusCode,
    buffered_bytes: Vec<u8>,
    remaining: futures::stream::BoxStream<'static, Result<Bytes, reqwest::Error>>,
    retry_count: u32,
    start: &std::time::Instant,
    client_user_agent: Option<String>,
    failover_ctx: Option<FailoverContext>,
) -> Response {
    let stream_state = Arc::new(StreamLogState {
        request_id: request_id.to_string(),
        client_format: client_format.clone(),
        provider_name: route.provider_name.clone(),
        provider_format: client_format.clone(),
        model: client_model.to_string(),
        target_model: target_model.to_string(),
        start: *start,
        prompt_tokens: std::sync::atomic::AtomicU32::new(0),
        completion_tokens: std::sync::atomic::AtomicU32::new(0),
        cached_tokens: std::sync::atomic::AtomicU32::new(0),
        ttft_ms: Mutex::new(None),
        usage_events: Mutex::new(Vec::new()),
        final_usage: Mutex::new(None),
        logged: std::sync::atomic::AtomicBool::new(false),
        interrupted: std::sync::atomic::AtomicBool::new(false),
        upstream_retry_count: retry_count as i64,
        upstream_last_error: None,
        client_user_agent,
        is_passthrough: true,
        failover_mapping_id: failover_ctx.as_ref().map(|c| c.mapping_id.clone()),
        failover_threshold: failover_ctx.as_ref().map(|c| c.threshold).unwrap_or(0),
    });
    let stream_state_ref = stream_state.clone();

    // The stream! body must own everything it captures ('static).
    let client_format_inner = client_format.clone();
    let provider_name = route.provider_name.clone();
    let client_model_owned = client_model.to_string();
    let target_model_owned = target_model.to_string();
    let request_id_owned = request_id.to_string();
    let start_owned = *start;
    let retry_count_owned = retry_count;
    let sse_stream = async_stream::stream! {
        let _guard = StreamLoggingGuard { state: stream_state };
        let parser = get_parser(&client_format_inner);

        let mut total_prompt: u32 = 0;
        let mut total_completion: u32 = 0;
        let mut total_cached: u32 = 0;

        let remaining_mapped = remaining
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
        let stream: futures::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>> =
            if buffered_bytes.is_empty() {
                remaining_mapped.boxed()
            } else {
                let first = bytes::Bytes::from(buffered_bytes);
                futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
                    .chain(remaining_mapped)
                    .boxed()
            };
        let mut reader = stream;

        let mut ttft_ms: Option<i64> = None;
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk_count: u64 = 0;

        let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        heartbeat_interval.tick().await; // skip first immediate tick

        let base_idle_timeout = std::time::Duration::from_secs(600);
        let mut upstream_idle = tokio::time::Instant::now();

        loop {
            let chunk = tokio::select! {
                chunk_result = reader.next() => {
                    match chunk_result {
                        Some(Ok(c)) => {
                            upstream_idle = tokio::time::Instant::now();
                            c
                        }
                        Some(Err(e)) => {
                            error!(
                                "[passthrough] stream error after {} chunks, {}s elapsed: {}",
                                chunk_count,
                                start_owned.elapsed().as_secs(),
                                e
                            );
                            stream_state_ref.interrupted.store(true, Ordering::SeqCst);
                            break;
                        }
                        None => {
                            info!(
                                "[passthrough] stream ended normally after {} chunks, {}s elapsed",
                                chunk_count,
                                start_owned.elapsed().as_secs()
                            );
                            break;
                        }
                    }
                }
                _ = heartbeat_interval.tick() => {
                    let idle_elapsed = upstream_idle.elapsed();
                    let effective_timeout = if chunk_count > 0 {
                        base_idle_timeout.saturating_mul(2)
                    } else {
                        base_idle_timeout
                    };
                    if idle_elapsed > effective_timeout {
                        error!(
                            "[passthrough] upstream stall: no data for {}s ({} chunks, {}s total)",
                            idle_elapsed.as_secs(),
                            chunk_count,
                            start_owned.elapsed().as_secs()
                        );
                        stream_state_ref.interrupted.store(true, Ordering::SeqCst);
                        break;
                    }
                    yield Ok::<_, std::convert::Infallible>(Bytes::from(": ping\n\n"));
                    continue;
                }
            };

            if ttft_ms.is_none() {
                *stream_state_ref.ttft_ms.lock().unwrap() =
                    Some(start_owned.elapsed().as_millis() as i64);
                ttft_ms = Some(start_owned.elapsed().as_millis() as i64);
            }
            chunk_count += 1;

            // Relay the bytes, normalized per complete line (Responses dialect
            // fix-ups; other formats are byte-for-byte). An incomplete trailing
            // line stays buffered until its newline arrives.
            buffer.extend_from_slice(&chunk);
            let mut out: Vec<u8> = Vec::with_capacity(chunk.len());
            while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = buffer[..=newline_pos].to_vec();
                buffer = buffer[newline_pos + 1..].to_vec();
                let line = match std::str::from_utf8(&line_bytes) {
                    Ok(s) => s.to_string(),
                    Err(_) => {
                        // Non-UTF-8 line: forward as-is.
                        out.extend_from_slice(&line_bytes);
                        continue;
                    }
                };
                let normalized = normalize_sse_line(&client_format_inner, &line);
                out.extend_from_slice(normalized.as_bytes());

                let trimmed = normalized.trim();
                if trimmed.is_empty() || !trimmed.starts_with("data:") {
                    continue;
                }
                // The per-format parsers expect the full SSE line (they strip
                // the `data:` prefix themselves).
                let ir_chunk = match parser.parse_stream_chunk(trimmed) {
                    Ok(Some(c)) => c,
                    Ok(None) => continue,
                    Err(_) => continue,
                };
                if ir_chunk.error.is_some() {
                    warn!(
                        "[passthrough] upstream SSE error event: {:?}, marking stream interrupted",
                        ir_chunk.error
                    );
                    stream_state_ref.interrupted.store(true, Ordering::SeqCst);
                    break;
                }
                if let Some(usage) = &ir_chunk.usage {
                    if usage.prompt_tokens > 0 {
                        total_prompt = usage.prompt_tokens;
                        total_cached = usage.cached_tokens;
                    }
                    if usage.completion_tokens > 0 {
                        total_completion = usage.completion_tokens;
                    }
                    stream_state_ref.prompt_tokens.store(total_prompt, Ordering::SeqCst);
                    stream_state_ref.completion_tokens.store(total_completion, Ordering::SeqCst);
                    stream_state_ref.cached_tokens.store(total_cached, Ordering::SeqCst);
                    if let Some(raw) = &usage.raw {
                        stream_state_ref.usage_events.lock().unwrap().push(raw.clone());
                    }
                }
            }
            if !out.is_empty() {
                yield Ok::<_, std::convert::Infallible>(Bytes::from(out));
            }
        }

        // Flush any trailing bytes that never got a newline (defensive; SSE
        // writers almost always end lines, but do not drop what's left).
        if !buffer.is_empty() {
            let rest = String::from_utf8_lossy(&buffer).into_owned();
            let normalized = normalize_sse_line(&client_format_inner, &rest);
            yield Ok::<_, std::convert::Infallible>(Bytes::from(normalized));
            buffer.clear();
        }

        // Emit an error event for interrupted streams so clients can detect it
        // (same convention as the conversion path; the bytes so far were raw
        // upstream output, so the trailer is the only proxy-generated frame).
        if stream_state_ref.interrupted.load(Ordering::SeqCst) {
            let trailer = crate::server::retry_invisible::error_trailer_event(
                client_format_inner.clone(),
                "stream interrupted by proxy",
            );
            yield Ok::<_, std::convert::Infallible>(Bytes::from(trailer));
        }

        let elapsed = start_owned.elapsed().as_millis() as i64;
        let pt = total_prompt as i64;
        let ct = total_completion as i64;
        let cache_t = total_cached as i64;

        let final_usage = serde_json::json!({
            "prompt_tokens": total_prompt,
            "completion_tokens": total_completion,
            "cached_tokens": total_cached,
        });
        *stream_state_ref.final_usage.lock().unwrap() = Some(final_usage.clone());
        let final_usage_json = serde_json::to_string(&final_usage).ok();
        let events_vec = stream_state_ref.usage_events.lock().unwrap().clone();
        let upstream_usage_events_json = if events_vec.is_empty() {
            None
        } else {
            serde_json::to_string(&serde_json::Value::Array(events_vec)).ok()
        };

        stream_state_ref.logged.store(true, Ordering::SeqCst);
        let interrupted = stream_state_ref.interrupted.load(Ordering::SeqCst);
        let (status_code, error_msg) = if interrupted {
            (502, Some("stream interrupted".to_string()))
        } else {
            (200, None)
        };
        if let Err(e) = log_request_entry(
            &request_id_owned,
            &client_format_inner,
            &provider_name,
            &client_format_inner,
            &client_model_owned,
            &target_model_owned,
            true,
            status_code,
            elapsed,
            error_msg.as_deref(),
            pt,
            ct,
            cache_t,
            ttft_ms,
            final_usage_json.as_deref(),
            upstream_usage_events_json.as_deref(),
            retry_count_owned as i64,
            None,
            stream_state_ref.client_user_agent.as_deref(),
            true,
        )
        .await
        {
            error!("Passthrough stream logging failed: {}", e);
        }

        info!(
            "[passthrough DONE] {} status={} duration={}ms tokens={}/{} ttft={}ms",
            target_model_owned,
            status_code,
            elapsed,
            pt,
            ct,
            ttft_ms.unwrap_or(0)
        );
    };

    let body_stream = axum::body::Body::from_stream(sse_stream);

    let mut response = Response::new(body_stream);
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response
}

/// Global kill-switch for the passthrough fast-path (settings key
/// `passthrough_enabled`, default on). Off → every request goes through the
/// IR conversion path, restoring pre-028 behaviour.
pub(crate) async fn passthrough_enabled() -> bool {
    crate::server::handlers::get_setting_pub("passthrough_enabled")
        .await
        .map(|v| v != "false")
        .unwrap_or(true)
}

/// Pure URL/body preparation for a passthrough request:
/// - endpoint: protocol path, optionally overridden by the caller (only
///   meaningful for Responses upstreams, e.g. /v1/responses/compact)
/// - Gemini: stream flag flips `:generateContent` → `:streamGenerateContent`
///   and the model lives in the URL, never the body
/// - other formats: `model` is rewritten to the routing target when they
///   differ; everything else passes through untouched
pub(crate) fn prepare_upstream(
    protocol: &ResolvedProtocol,
    client_format: &ClientFormat,
    body: &serde_json::Value,
    target_model: &str,
    stream: bool,
    endpoint_override: Option<&str>,
) -> (String, serde_json::Value) {
    let mut endpoint_path = protocol.endpoint_path.clone();
    if let Some(ep) = endpoint_override {
        // Only Responses upstreams have /v1/responses/compact; forwarding it
        // to another protocol endpoint 404s (same rule as the conversion path).
        if protocol.format == ClientFormat::Responses {
            endpoint_path = ep.to_string();
        } else {
            info!(
                "[passthrough] endpoint override {} ignored for non-Responses upstream, using {}",
                ep, endpoint_path
            );
        }
    }
    if *client_format == ClientFormat::Gemini && stream {
        endpoint_path = endpoint_path.replace(":generateContent", ":streamGenerateContent");
    }

    let url = crate::provider::manager::join_base_url_and_path(&protocol.base_url, &endpoint_path);

    let mut body = body.clone();
    if *client_format != ClientFormat::Gemini {
        let current = body.get("model").and_then(|m| m.as_str()).unwrap_or("");
        if current != target_model {
            body["model"] = serde_json::Value::String(target_model.to_string());
        }
    }

    (url, body)
}

/// Responses dialect fix-up on the SSE wire. Some upstreams (verified on
/// bigmodel / Z.ai, 2026-09-07) stream reasoning under non-standard
/// `response.reasoning_text.*` event names, while the OpenAI Responses
/// protocol — and strict clients like codex — only render the
/// `reasoning_summary_text` family; unrenamed events are silently dropped by
/// the client ("upstream thinking disappears"). Renames the event in both
/// `event:` and `data:` lines; any other format or untouched line is returned
/// byte-for-byte.
pub(crate) fn normalize_sse_line(format: &ClientFormat, line: &str) -> String {
    if *format != ClientFormat::Responses || !line.contains("response.reasoning_text") {
        return line.to_string();
    }
    line.replace(
        "response.reasoning_text.delta",
        "response.reasoning_summary_text.delta",
    )
    .replace(
        "response.reasoning_text.done",
        "response.reasoning_summary_text.done",
    )
}

/// Non-streaming counterpart of `normalize_sse_line`: rewrites a reasoning
/// output item's non-standard `content: [{type: "reasoning_text"}]` into the
/// protocol-standard `summary: [{type: "summary_text"}]`. Returns the input
/// unchanged when there is nothing to fix.
pub(crate) fn normalize_responses_body(body: &serde_json::Value) -> serde_json::Value {
    let mut body = body.clone();
    let output = match body.get_mut("output").and_then(|o| o.as_array_mut()) {
        Some(o) => o,
        None => return body,
    };
    for item in output.iter_mut() {
        if item.get("type").and_then(|t| t.as_str()) != Some("reasoning") {
            continue;
        }
        let content = match item.get("content").and_then(|c| c.as_array()) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => continue,
        };
        let summary: Vec<serde_json::Value> = content
            .iter()
            .map(|part| {
                let mut part = part.clone();
                if part.get("type").and_then(|t| t.as_str()) == Some("reasoning_text") {
                    part["type"] = serde_json::Value::String("summary_text".into());
                }
                part
            })
            .collect();
        if let Some(obj) = item.as_object_mut() {
            obj.remove("content");
        }
        item["summary"] = serde_json::Value::Array(summary);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proto(format: ClientFormat, base_url: &str, endpoint_path: &str) -> ResolvedProtocol {
        ResolvedProtocol {
            format,
            base_url: base_url.to_string(),
            endpoint_path: endpoint_path.to_string(),
            is_primary: false,
        }
    }

    #[test]
    fn url_uses_protocol_base_url_with_trailing_slash_trimmed() {
        let p = proto(
            ClientFormat::Anthropic,
            "https://upstream.example/api/",
            "/v1/messages",
        );
        let body = serde_json::json!({"model": "claude-x", "stream": false});
        let (url, out) =
            prepare_upstream(&p, &ClientFormat::Anthropic, &body, "claude-x", false, None);
        assert_eq!(url, "https://upstream.example/api/v1/messages");
        assert_eq!(out["model"], "claude-x");
    }

    #[test]
    fn model_is_rewritten_only_when_target_differs() {
        let p = proto(
            ClientFormat::Completions,
            "https://u",
            "/v1/chat/completions",
        );
        let body = serde_json::json!({"model": "alias", "temperature": 0.7, "custom": [1, 2]});
        let (url, out) = prepare_upstream(
            &p,
            &ClientFormat::Completions,
            &body,
            "real-model",
            false,
            None,
        );
        assert_eq!(out["model"], "real-model");
        // Everything except `model` is untouched.
        assert_eq!(out["temperature"], 0.7);
        assert_eq!(out["custom"], serde_json::json!([1, 2]));

        // No rewrite when the names already match.
        let (_, out2) =
            prepare_upstream(&p, &ClientFormat::Completions, &body, "alias", false, None);
        assert_eq!(out2, body);
    }

    #[test]
    fn gemini_stream_flips_url_and_never_touches_body_model() {
        let p = proto(
            ClientFormat::Gemini,
            "https://u",
            "/v1beta/models/gemini-x:generateContent",
        );
        let body = serde_json::json!({"contents": []});
        let (url, out) = prepare_upstream(&p, &ClientFormat::Gemini, &body, "gemini-x", true, None);
        assert_eq!(
            url,
            "https://u/v1beta/models/gemini-x:streamGenerateContent"
        );
        assert_eq!(out, body, "gemini body must pass through untouched");

        // Non-stream keeps :generateContent.
        let (url2, _) = prepare_upstream(&p, &ClientFormat::Gemini, &body, "gemini-x", false, None);
        assert_eq!(url2, "https://u/v1beta/models/gemini-x:generateContent");
    }

    #[test]
    fn endpoint_override_applies_only_to_responses_upstreams() {
        let responses = proto(ClientFormat::Responses, "https://u", "/v1/responses");
        let body = serde_json::json!({"model": "gpt-x"});
        let (url, _) = prepare_upstream(
            &responses,
            &ClientFormat::Responses,
            &body,
            "gpt-x",
            false,
            Some("/v1/responses/compact"),
        );
        assert_eq!(url, "https://u/v1/responses/compact");

        let anthropic = proto(ClientFormat::Anthropic, "https://u", "/v1/messages");
        let (url2, _) = prepare_upstream(
            &anthropic,
            &ClientFormat::Anthropic,
            &body,
            "gpt-x",
            false,
            Some("/v1/responses/compact"),
        );
        assert_eq!(url2, "https://u/v1/messages");
    }

    #[test]
    fn duplicated_version_segment_is_collapsed() {
        // Base URL already ends in /v1 (common for OpenAI-compatible configs)
        // while the default responses path also starts with /v1.
        let p = proto(ClientFormat::Responses, "https://u/v1", "/v1/responses");
        let body = serde_json::json!({"model": "gpt-x"});
        let (url, _) = prepare_upstream(&p, &ClientFormat::Responses, &body, "gpt-x", false, None);
        assert_eq!(url, "https://u/v1/responses");
    }
}

#[cfg(test)]
mod reasoning_dialect_tests {
    use super::*;

    #[test]
    fn sse_line_renames_bigmodel_reasoning_events() {
        // event: line
        assert_eq!(
            normalize_sse_line(
                &ClientFormat::Responses,
                "event: response.reasoning_text.delta\n"
            ),
            "event: response.reasoning_summary_text.delta\n"
        );
        // data: line (both the type field and the event echo)
        let data = "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"思考\"}\n";
        let out = normalize_sse_line(&ClientFormat::Responses, data);
        assert!(out.contains("response.reasoning_summary_text.delta"));
        assert!(!out.contains("response.reasoning_text.delta"));
        // done event
        assert!(normalize_sse_line(
            &ClientFormat::Responses,
            "event: response.reasoning_text.done"
        )
        .contains("reasoning_summary_text.done"));
    }

    #[test]
    fn sse_line_untouched_for_other_formats_and_lines() {
        // Anthropic / other formats: byte-for-byte.
        assert_eq!(
            normalize_sse_line(
                &ClientFormat::Anthropic,
                "data: {\"type\":\"content_block_delta\"}\n"
            ),
            "data: {\"type\":\"content_block_delta\"}\n"
        );
        // Responses but no dialect markers: unchanged.
        assert_eq!(
            normalize_sse_line(
                &ClientFormat::Responses,
                "event: response.output_text.delta\n"
            ),
            "event: response.output_text.delta\n"
        );
        assert_eq!(
            normalize_sse_line(&ClientFormat::Responses, ": ping\n\n"),
            ": ping\n\n"
        );
    }

    #[test]
    fn responses_body_rewrites_reasoning_content_to_summary() {
        let body = serde_json::json!({
            "id": "resp_1",
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "content": [{"type": "reasoning_text", "text": "让我想想"}],
                    "summary": []
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "答案"}]
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });
        let out = normalize_responses_body(&body);
        let reasoning = &out["output"][0];
        assert!(
            reasoning.get("content").is_none(),
            "content must be removed"
        );
        assert_eq!(reasoning["summary"][0]["type"], "summary_text");
        assert_eq!(reasoning["summary"][0]["text"], "让我想想");
        // message item untouched.
        assert_eq!(out["output"][1]["content"][0]["type"], "output_text");
    }

    #[test]
    fn responses_body_unchanged_without_dialect() {
        let body = serde_json::json!({
            "output": [
                {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "ok"}]}
            ]
        });
        assert_eq!(normalize_responses_body(&body), body);

        let no_output = serde_json::json!({"error": {"message": "x"}});
        assert_eq!(normalize_responses_body(&no_output), no_output);
    }
}
