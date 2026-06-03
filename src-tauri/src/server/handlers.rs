use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Cache: response_id → accumulated reasoning_content.
/// Stores reasoning from DeepSeek so it can be injected into subsequent requests
/// when Codex doesn't preserve `<thinking>` tags in multi-turn conversations.
static REASONING_CACHE: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

use axum::extract::{Request, Path};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::stream::StreamExt;
use serde_json::Value;
use tracing::{error, info, warn};

use crate::converter::generators::anthropic::AnthropicGenerator;
use crate::converter::generators::completions::CompletionsGenerator;
use crate::converter::generators::gemini::GeminiGenerator;
use crate::converter::generators::responses::ResponsesGenerator;
use crate::converter::ir::{ClientFormat, IrContentPart, IrRole};
use crate::converter::parsers::anthropic::AnthropicParser;
use crate::converter::parsers::completions::CompletionsParser;
use crate::converter::parsers::gemini::GeminiParser;
use crate::converter::parsers::responses::ResponsesParser;
use crate::converter::{FormatGenerator, FormatParser};
use crate::error::ProxyError;
use crate::interceptor::engine::InterceptorEngine;
use crate::key::rotation::{KeyRotation, RotationStrategy};
use crate::key::store::decrypt_api_key;
use crate::logging::store::log_request;
use crate::provider::manager::ProviderManager;


pub async fn handle_completions(request: Request) -> Response {
    handle_proxy(request, ClientFormat::Completions, None, false).await
}

pub async fn handle_responses(request: Request) -> Response {
    handle_proxy(request, ClientFormat::Responses, None, false).await
}

pub async fn handle_anthropic(request: Request) -> Response {
    handle_proxy(request, ClientFormat::Anthropic, None, false).await
}

pub async fn handle_gemini(Path(model_segment): Path<String>, request: Request) -> Response {
    let (model, is_stream) = parse_gemini_model_segment(&model_segment);
    handle_proxy(request, ClientFormat::Gemini, Some(model), is_stream).await
}

fn parse_gemini_model_segment(segment: &str) -> (String, bool) {
    let is_stream = segment.contains("streamGenerateContent");
    let model = segment
        .split(':')
        .next()
        .unwrap_or(segment)
        .to_string();
    (model, is_stream)
}

fn truncate_str(s: &str, max_len: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= max_len {
        std::borrow::Cow::Borrowed(s)
    } else {
        let boundary = s.char_indices()
            .take_while(|(i, _)| *i < max_len)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max_len.min(s.len()));
        std::borrow::Cow::Owned(format!("{}... (truncated, {} bytes total)", &s[..boundary], s.len()))
    }
}

async fn handle_proxy(
    request: Request,
    client_format: ClientFormat,
    override_model: Option<String>,
    force_stream: bool,
) -> Response {
    let start = std::time::Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();

    let (parts, body) = request.into_parts();

    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to read request body: {}", e);
            return ProxyError::Parse(format!("failed to read body: {}", e)).into_response();
        }
    };

    let body_value: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("[ERR] invalid request body: {}", e);
            if let Err(le) = log_request_entry(
                &request_id, &client_format, "proxy", &client_format,
                "unknown", false, 400, start.elapsed().as_millis() as i64,
                Some(&format!("invalid JSON: {}", e)), 0, 0, 0, None,
            ).await {
                tracing::error!("Early error logging failed: {}", le);
            }
            return ProxyError::Parse(format!("invalid JSON: {}", e)).into_response();
        }
    };

    let parser = get_parser(&client_format);
    let generator = get_generator(&client_format);

    {
        let model_hint = body_value["model"].as_str().unwrap_or("unknown");
        let stream = body_value["stream"].as_bool().unwrap_or(false);
        info!("[REQ] {:?} model={} stream={}", client_format, model_hint, stream);
    }

    if tracing::enabled!(tracing::Level::DEBUG) {
        if let Some(msgs) = body_value["messages"].as_array() {
            let has_tool = msgs.iter().any(|m| {
                let role = m["role"].as_str().unwrap_or("");
                role == "tool" || role == "function" || m.get("tool_calls").is_some() || m.get("function_call").is_some()
            });
            if has_tool {
                let serialized = serde_json::to_string(&body_value["messages"]).unwrap_or_default();
                tracing::debug!("RAW REQUEST messages: {}", truncate_str(&serialized, 2000));
            }
        }
    }

    let mut ir_request = match parser.parse_request(&body_value) {
        Ok(r) => {
            info!("Parsed request: model={}, stream={}, thinking={:?}", r.model, r.stream, r.thinking);
            r
        }
        Err(e) => {
            let model_hint = body_value["model"].as_str().unwrap_or("unknown");
            tracing::error!("[ERR] parse failed model={}: {}", model_hint, e);
            error!("Parse request error: {}", e);
            if let Err(le) = log_request_entry(
                &request_id, &client_format, "proxy", &client_format,
                model_hint, false, 400, start.elapsed().as_millis() as i64,
                Some(&format!("parse error: {}", e)), 0, 0, 0, None,
            ).await {
                tracing::error!("Early error logging failed: {}", le);
            }
            return e.into_response();
        }
    };

    if let Some(model) = override_model {
        ir_request.model = model;
    }
    if force_stream {
        ir_request.stream = true;
    }

    let mut extra_headers: HashMap<String, String> = HashMap::new();
    extract_headers(&parts.headers, &mut extra_headers);

    let path = parts.uri.path().to_string();

    if let Err(e) = InterceptorEngine::execute_pre_rules(&mut ir_request, &path, &mut extra_headers).await {
        error!("Interceptor error: {}", e);
    }

    let route = match ProviderManager::find_for_model(&ir_request.model).await {
        Ok(r) => {
            info!("Route found: model={} -> {} ({:?} via {})", ir_request.model, r.target_model, r.target_format, r.provider_name);
            info!("[ROUTE] {} -> {} ({})", ir_request.model, r.target_model, r.provider_name);
            // Non-standard Anthropic endpoints (e.g. Kimi coding) don't support thinking parameter.
            // Clear it to avoid upstream errors and max_tokens inflation.
            if ir_request.thinking.is_some() {
                if r.base_url.contains("kimi.com") || r.base_url.contains("moonshot.cn") {
                    tracing::info!("Clearing thinking for non-standard Anthropic endpoint: {}", r.base_url);
                    ir_request.thinking = None;
                }
            }
            r
        }
        Err(e) => {
            tracing::error!("[ERR] no route for model={}: {}", ir_request.model, e);
            error!("Route not found for model '{}': {}", ir_request.model, e);
            if let Err(le) = log_request_entry(
                &request_id, &client_format, "proxy", &client_format,
                &ir_request.model, ir_request.stream, 404, start.elapsed().as_millis() as i64,
                Some(&format!("route not found: {}", e)), 0, 0, 0, None,
            ).await {
                tracing::error!("Early error logging failed: {}", le);
            }
            return e.into_response();
        }
    };

    let selected_key = match KeyRotation::get_next_key(&route.provider_id, &RotationStrategy::LeastUsed).await {
        Ok(k) => k,
        Err(e) => {
            let err_msg = format!("key rotation error: {}", e);
            if let Err(le) = log_request_entry(
                &request_id, &client_format, &route.provider_name, &route.target_format,
                &ir_request.model, ir_request.stream, 500, start.elapsed().as_millis() as i64,
                Some(&err_msg), 0, 0, 0, None,
            ).await {
                tracing::error!("Early error logging failed: {}", le);
            }
            return e.into_response();
        }
    };

    let nonce_slice: Vec<u8> = selected_key.nonce;
    let mut nonce_array = [0u8; 12];
    if nonce_slice.len() == 12 {
        nonce_array.copy_from_slice(&nonce_slice);
    } else {
        if let Err(le) = log_request_entry(
            &request_id, &client_format, &route.provider_name, &route.target_format,
            &ir_request.model, ir_request.stream, 500, start.elapsed().as_millis() as i64,
            Some("invalid nonce length"), 0, 0, 0, None,
        ).await {
            tracing::error!("Early error logging failed: {}", le);
        }
        return ProxyError::KeyManagement("invalid nonce length".into()).into_response();
    }

    let api_key = match decrypt_api_key(&selected_key.encrypted_key, &nonce_array) {
        Ok(k) => k,
        Err(e) => {
            let err_msg = format!("key decryption error: {}", e);
            if let Err(le) = log_request_entry(
                &request_id, &client_format, &route.provider_name, &route.target_format,
                &ir_request.model, ir_request.stream, 500, start.elapsed().as_millis() as i64,
                Some(&err_msg), 0, 0, 0, None,
            ).await {
                tracing::error!("Early error logging failed: {}", le);
            }
            return e.into_response();
        }
    };

    let target_model = route.target_model.clone();
    ir_request.model = target_model.clone();

    let target_generator = get_generator(&route.target_format);

    let mut ir_request_for_upstream = ir_request.clone();

    // Inject cached reasoning_content into assistant messages that lack it.
    // DeepSeek requires reasoning_content on assistant messages in thinking mode.
    // Codex may strip <thinking> tags, so we rely on a proxy-side cache.
    {
        let cache = REASONING_CACHE.lock().unwrap();
        inject_cached_reasoning_into_assistant_messages(
            &mut ir_request_for_upstream.messages,
            ir_request_for_upstream
                .extra
                .get("previous_response_id")
                .and_then(|v| v.as_str()),
            &cache,
        );
    }

    if client_format == ClientFormat::Gemini && ir_request.stream {
        ir_request_for_upstream.stream = true;
    }

    let target_body = match target_generator.generate_request(&ir_request_for_upstream) {
        Ok(b) => b,
        Err(e) => {
            let err_msg = format!("request generation error: {}", e);
            if let Err(le) = log_request_entry(
                &request_id, &client_format, &route.provider_name, &route.target_format,
                &ir_request.model, ir_request.stream, 500, start.elapsed().as_millis() as i64,
                Some(&err_msg), 0, 0, 0, None,
            ).await {
                tracing::error!("Early error logging failed: {}", le);
            }
            return e.into_response();
        }
    };

    if tracing::enabled!(tracing::Level::DEBUG) {
        if ir_request_for_upstream.messages.iter().any(|m| m.role == IrRole::Tool) {
            let serialized = serde_json::to_string(&target_body["messages"]).unwrap_or_default();
            tracing::debug!("TOOL DEBUG messages for model={}: {}",
                ir_request_for_upstream.model,
                truncate_str(&serialized, 2000)
            );
        }
    }

    let mut url = format!("{}{}", route.base_url.trim_end_matches('/'), route.endpoint_path);

    if client_format == ClientFormat::Gemini && ir_request.stream {
        url = url.replace(":generateContent", ":streamGenerateContent");
    }

    info!("Upstream request: {} {}", "POST", url);

    let request_timeout_secs: u64 = {
        let pool_ref = crate::db::pool::get_pool().await;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT key, value FROM settings WHERE key = 'request_timeout'"
        ).fetch_all(pool_ref).await.unwrap_or_default();
        let map: HashMap<String, String> = rows.into_iter().collect();
        map.get("request_timeout").and_then(|v| v.parse().ok()).unwrap_or(1200)
    };

    let http_client = crate::http::SHARED_HTTP_CLIENT.clone();
    let mut req_builder = http_client
        .post(&url)
        .json(&target_body)
        .header("Content-Type", "application/json");

    // Streaming: 24h max for long agent tasks. Non-streaming: 2h from DB.
    if ir_request.stream {
        req_builder = req_builder.timeout(std::time::Duration::from_secs(86400));
    } else {
        let timeout_secs = if request_timeout_secs < 7200 { 7200 } else { request_timeout_secs };
        req_builder = req_builder.timeout(std::time::Duration::from_secs(timeout_secs));
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

    for (key, value) in &extra_headers {
        req_builder = req_builder.header(key.as_str(), value.as_str());
    }

    let resp = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            let err = ProxyError::Network(format!("request to provider failed: {}", e));
            tracing::error!("[ERR] upstream network error model={}: {}", target_model, e);
            if let Err(le) = log_request_entry(
                &request_id,
                &client_format,
                &route.provider_name,
                &route.target_format,
                &ir_request.model,
                ir_request.stream,
                502,
                start.elapsed().as_millis() as i64,
                Some(err.to_string().as_str()),
                0,
                0,
                0,
                None,
            )
            .await
            {
                tracing::error!("Network error logging failed: {}", le);
            }
            return err.into_response();
        }
    };

    let status = resp.status();
    let is_stream = ir_request.stream;

    if !status.is_success() {
        let status_code = status.as_u16();
        let resp_body = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return ProxyError::Network(format!("failed to read error response: {}", e)).into_response();
            }
        };
        let body_text = String::from_utf8_lossy(&resp_body).into_owned();
        tracing::error!("[ERR] upstream status={} model={}", status_code, target_model);
        error!("Upstream error {}: {}", status_code, body_text);

        let err_msg = if body_text.trim_start().starts_with("<") {
            extract_text_from_html(&body_text, 4000)
        } else {
            body_text
        };
        if let Err(le) = log_request_entry(
            &request_id,
            &client_format,
            &route.provider_name,
            &route.target_format,
            &ir_request.model,
            ir_request.stream,
            status_code,
            start.elapsed().as_millis() as i64,
            Some(&err_msg),
            0,
            0,
            0,
            None,
        )
        .await
        {
            tracing::error!("Upstream error logging failed: {}", le);
        }

        let error_body = serde_json::json!({
            "error": {
                "message": err_msg,
                "type": "upstream_error",
                "code": status_code,
            }
        });
        let mut response = axum::Json(error_body).into_response();
        *response.status_mut() = status;
        return response;
    }

    if !is_stream {
        let resp_body = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return ProxyError::Network(format!("failed to read response: {}", e)).into_response();
            }
        };

        if resp_body.is_empty() {
            tracing::error!("Upstream returned empty body with status {}", status);
            return ProxyError::Parse("upstream returned empty response body".into()).into_response();
        }

        // Handle upstream returning SSE despite stream:false (e.g. provider bugs)
        let resp_body_str = String::from_utf8_lossy(&resp_body);
        let resp_value: Value = if resp_body_str.starts_with("data:") || resp_body_str.starts_with("event:") {
            // Upstream returned SSE — parse it as a streaming response and extract text
            tracing::warn!("Upstream returned SSE for non-streaming request, parsing as stream");
            let text = extract_text_from_sse_body(&resp_body_str, &route.target_format);
            // Build a minimal valid response in the upstream format
            match route.target_format {
                ClientFormat::Anthropic => {
                    serde_json::json!({
                        "id": "msg-proxy-fallback",
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "text", "text": text.unwrap_or_default()}],
                        "model": target_model,
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    })
                }
                _ => {
                    serde_json::json!({
                        "id": "chatcmpl-proxy-fallback",
                        "object": "chat.completion",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": text.unwrap_or_default()}, "finish_reason": "stop"}],
                        "model": target_model,
                        "usage": {"prompt_tokens": 0, "completion_tokens": 0}
                    })
                }
            }
        } else {
            match serde_json::from_slice(&resp_body) {
                Ok(v) => v,
                Err(e) => {
                    let preview: String = resp_body_str.chars().take(200).collect();
                    tracing::error!("Invalid response JSON: {} | body preview: {}", e, preview);
                    return ProxyError::Parse(format!("invalid response JSON: {}", e)).into_response();
                }
            }
        };

        let target_parser = get_parser(&route.target_format);
        let ir_response = match target_parser.parse_response(&resp_value) {
            Ok(r) => r,
            Err(e) => {
                return e.into_response();
            }
        };

        let client_response = match generator.generate_response(&ir_response) {
            Ok(r) => r,
            Err(e) => {
                return e.into_response();
            }
        };

        let prompt_tokens = ir_response.usage.prompt_tokens as i64;
        let completion_tokens = ir_response.usage.completion_tokens as i64;
        let cached_tokens = ir_response.usage.cached_tokens as i64;

        // Cache reasoning_content for multi-turn (non-streaming path)
        if let Some(ref resp_id) = ir_response.id {
            let reasoning: String = ir_response.message.content.iter()
                .filter_map(|p| match p {
                    IrContentPart::Thinking { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if !reasoning.is_empty() {
                if let Ok(mut cache) = REASONING_CACHE.lock() {
                    cache.insert(resp_id.clone(), reasoning);
                }
            }
        }

        if let Err(e) = log_request_entry(
            &request_id,
            &client_format,
            &route.provider_name,
            &route.target_format,
            &ir_request.model,
            false,
            200,
            start.elapsed().as_millis() as i64,
            None,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            Some(start.elapsed().as_millis() as i64),
        )
        .await
        {
            tracing::error!("Non-stream logging failed: {}", e);
        }

        info!("[DONE] {} status=200 duration={}ms tokens={}/{}",
            target_model, start.elapsed().as_millis(), prompt_tokens, completion_tokens);

        let mut response = axum::Json(client_response).into_response();
        *response.status_mut() = status;
        response
    } else {
        let target_parser = get_parser(&route.target_format);
        let client_generator = get_generator(&client_format);

        let stream = resp.bytes_stream();

        let response_id = uuid::Uuid::new_v4().to_string();
        let model_name = ir_request.model.clone();
        let client_format_clone = client_format.clone();

        let stream_state = Arc::new(StreamLogState {
            request_id: request_id.clone(),
            client_format: client_format.clone(),
            provider_name: route.provider_name.clone(),
            provider_format: route.target_format.clone(),
            model: ir_request.model.clone(),
            start: start.clone(),
            prompt_tokens: AtomicU32::new(0),
            completion_tokens: AtomicU32::new(0),
            cached_tokens: AtomicU32::new(0),
            ttft_ms: Mutex::new(None),
            logged: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
        });
        let stream_state_ref = stream_state.clone();

        let sse_stream = async_stream::stream! {
            let _guard = StreamLoggingGuard { state: stream_state };

            let mut total_prompt = 0u32;
            let mut total_completion = 0u32;
            let mut total_cached = 0u32;
            let mut reader = stream;
            let mut ttft_ms: Option<i64> = None;
            let mut buffer: Vec<u8> = Vec::new();
            let mut started = false;
            let mut finished = false;

            // Anthropic content block state
            let mut content_block_index: u32 = 0;
            let mut text_block_open = false;
            let mut tool_block_open = false;
            let mut had_tool_calls = false;
            let is_anthropic = matches!(client_format, ClientFormat::Anthropic);

            // Responses output item state
            let mut resp_output_index: u32 = 0;
            let mut resp_message_open = false;
            let mut resp_text_part_open = false;
            let mut resp_func_open = false;
            let mut resp_call_id = String::new();
            let mut resp_func_name = String::new();
            let mut resp_accumulated_args = String::new();
            let mut resp_accumulated_text = String::new();
            let mut resp_thinking_started = false;
            let mut resp_accumulated_reasoning = String::new(); // pure reasoning without tags, for cache
            let is_responses = matches!(client_format, ClientFormat::Responses);

            let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(15));
            heartbeat_interval.tick().await; // skip first immediate tick

            let base_idle_timeout = std::time::Duration::from_secs(600);
            let mut upstream_idle = tokio::time::Instant::now();
            let mut chunk_count: u64 = 0;

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
                                    "Stream error after {} chunks, {}s elapsed: {}",
                                    chunk_count,
                                    start.elapsed().as_secs(),
                                    e
                                );
                                stream_state_ref.interrupted.store(true, Ordering::SeqCst);
                                break;
                            }
                            None => {
                                info!(
                                    "Stream ended normally after {} chunks, {}s elapsed",
                                    chunk_count,
                                    start.elapsed().as_secs()
                                );
                                break;
                            }
                        }
                    }
                    _ = heartbeat_interval.tick() => {
                        let idle_elapsed = upstream_idle.elapsed();
                        // Adaptive: streams that already received data get 2x timeout
                        // to tolerate long reasoning pauses from models like mimo-v2.5-pro
                        let effective_timeout = if chunk_count > 0 {
                            base_idle_timeout.saturating_mul(2)
                        } else {
                            base_idle_timeout
                        };
                        if idle_elapsed > effective_timeout {
                            error!(
                                "Upstream stall detected: no data for {}s ({} chunks received, {}s total)",
                                idle_elapsed.as_secs(),
                                chunk_count,
                                start.elapsed().as_secs()
                            );
                            stream_state_ref.interrupted.store(true, Ordering::SeqCst);
                            break;
                        }
                        // SSE heartbeat: keep client and intermediaries alive during upstream silence
                        yield Ok::<_, std::convert::Infallible>(Bytes::from(": ping\n\n"));
                        continue;
                    }
                };

                chunk_count += 1;
                buffer.extend_from_slice(&chunk);

                while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                    let line_bytes: Vec<u8> = buffer[..newline_pos].to_vec();
                    buffer = buffer[newline_pos + 1..].to_vec();

                    let line = match std::str::from_utf8(&line_bytes) {
                        Ok(s) => s.to_string(),
                        Err(_) => {
                            // Skip lines with invalid UTF-8 (shouldn't happen in well-formed SSE)
                            continue;
                        }
                    };

                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    if trimmed.starts_with("data: ") {
                        tracing::debug!("Upstream SSE: {}", trimmed);
                    }

                    let ir_chunk = match target_parser.parse_stream_chunk(trimmed) {
                        Ok(Some(c)) => c,
                        Ok(None) => continue,
                        Err(e) => {
                            error!("Stream chunk parse error: {}", e);
                            continue;
                        }
                    };

                    if let Some(usage) = &ir_chunk.usage {
                        // Upstream APIs report cumulative (not incremental) usage:
                        // - Anthropic: message_start has input+cached, message_delta has output
                        // - OpenAI: only the final chunk carries usage (full totals)
                        // - Gemini: each chunk carries cumulative totals
                        // - Responses: response.completed carries full totals
                        // Use "latest non-zero" instead of accumulation to avoid double-counting.
                        if usage.prompt_tokens > 0 {
                            total_prompt = usage.prompt_tokens;
                        }
                        if usage.completion_tokens > 0 {
                            total_completion = usage.completion_tokens;
                        }
                        if usage.cached_tokens > 0 {
                            total_cached = usage.cached_tokens;
                        }
                        stream_state_ref.prompt_tokens.store(total_prompt, Ordering::SeqCst);
                        stream_state_ref.completion_tokens.store(total_completion, Ordering::SeqCst);
                        stream_state_ref.cached_tokens.store(total_cached, Ordering::SeqCst);
                    }

                    if ttft_ms.is_none() && (ir_chunk.delta_content.is_some() || ir_chunk.delta_tool_calls.is_some() || ir_chunk.delta_thinking.is_some()) {
                        ttft_ms = Some(start.elapsed().as_millis() as i64);
                        *stream_state_ref.ttft_ms.lock().unwrap() = ttft_ms;
                    }

                    // Emit stream start on first real content
                    // (Responses format manages its own start lifecycle)
                    if !started && !is_responses {
                        let has_content = ir_chunk.delta_content.is_some()
                            || ir_chunk.delta_tool_calls.is_some()
                            || ir_chunk.finish_reason.is_some();
                        if has_content {
                            if let Some(start_event) = client_generator.generate_stream_start(&response_id, &model_name, total_prompt, total_completion, total_cached) {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(start_event));
                            }
                            started = true;
                        }
                    }

                    // Skip content after finish (avoid duplicate finish events)
                    if finished {
                        continue;
                    }

                    // Handle Anthropic content block lifecycle
                    if is_anthropic {
                        // Tool call start: close text block first, open tool_use block
                        if let Some(tool_calls) = &ir_chunk.delta_tool_calls {
                            if let Some(tc) = tool_calls.first() {
                                if tc.id.is_some() && tc.name.is_some() {
                                    // Close text block if open
                                    if text_block_open {
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", content_block_index - 1)
                                        ));
                                        text_block_open = false;
                                    }
                                    // Close previous tool block if open (multiple tool calls)
                                    if tool_block_open {
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", content_block_index - 1)
                                        ));
                                    }
                                    // Emit content_block_start for tool_use
                                    let block_start = serde_json::json!({
                                        "type": "content_block_start",
                                        "index": content_block_index,
                                        "content_block": {
                                            "type": "tool_use",
                                            "id": tc.id,
                                            "name": tc.name,
                                            "input": {},
                                        }
                                    });
                                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                        format!("event: content_block_start\ndata: {}\n\n", block_start)
                                    ));
                                    tool_block_open = true;
                                    had_tool_calls = true;
                                    content_block_index += 1;
                                    continue;
                                }
                                // Argument delta for tool call
                                if let Some(args) = &tc.arguments {
                                    if !args.is_empty() {
                                        let delta_event = serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": content_block_index - 1,
                                            "delta": {
                                                "type": "input_json_delta",
                                                "partial_json": args,
                                            }
                                        });
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("event: content_block_delta\ndata: {}\n\n", delta_event)
                                        ));
                                    }
                                    continue;
                                }
                            }
                        }

                        // Text content
                        if let Some(content) = &ir_chunk.delta_content {
                            if !content.is_empty() && !tool_block_open {
                                // Open text block if not open
                                if !text_block_open {
                                    let block_start = serde_json::json!({
                                        "type": "content_block_start",
                                        "index": content_block_index,
                                        "content_block": {
                                            "type": "text",
                                            "text": "",
                                        }
                                    });
                                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                        format!("event: content_block_start\ndata: {}\n\n", block_start)
                                    ));
                                    text_block_open = true;
                                    content_block_index += 1;
                                }
                                let delta_event = serde_json::json!({
                                    "type": "content_block_delta",
                                    "index": content_block_index - 1,
                                    "delta": {
                                        "type": "text_delta",
                                        "text": content,
                                    }
                                });
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("event: content_block_delta\ndata: {}\n\n", delta_event)
                                ));
                            }
                            continue;
                        }

                        // Finish
                        if ir_chunk.finish_reason.is_some() {
                            // Close any open content blocks
                            if text_block_open {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", content_block_index - 1)
                                ));
                                text_block_open = false;
                            }
                            if tool_block_open {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", content_block_index - 1)
                                ));
                                tool_block_open = false;
                            }

                            let stop_reason = match ir_chunk.finish_reason.as_deref() {
                                Some("stop") | Some("end_turn") | Some("completed") => {
                                    if had_tool_calls { "tool_use" } else { "end_turn" }
                                }
                                Some("tool_calls") | Some("tool_use") => "tool_use",
                                other => other.unwrap_or("end_turn"),
                            };

                            let mut usage = serde_json::json!({
                                "input_tokens": total_prompt,
                                "output_tokens": total_completion,
                            });
                            if total_cached > 0 {
                                usage["cache_read_input_tokens"] = serde_json::json!(total_cached);
                            }

                            let message_delta = serde_json::json!({
                                "type": "message_delta",
                                "delta": {
                                    "stop_reason": stop_reason,
                                    "stop_sequence": null,
                                },
                                "usage": usage,
                            });

                            yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                format!("event: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n", message_delta)
                            ));
                            finished = true;
                            continue;
                        }

                        continue;
                    }

                    // Handle Responses API output item lifecycle
                    if is_responses {
                        // Tool call start
                        if let Some(tool_calls) = &ir_chunk.delta_tool_calls {
                            if let Some(tc) = tool_calls.first() {
                                if tc.id.is_some() && tc.name.is_some() {
                                    // Emit response.created if not started
                                    if !started {
                                        let created = serde_json::json!({
                                            "type": "response.created",
                                            "response": {
                                                "id": response_id,
                                                "object": "response",
                                                "status": "in_progress",
                                                "model": model_name,
                                                "output": [],
                                            }
                                        });
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\n", created)
                                        ));
                                        started = true;
                                    }
                                    // Close reasoning summary before starting a tool call.
                                    if let Some(done_sse) = close_responses_thinking_if_needed(
                                        &mut resp_thinking_started,
                                        &mut resp_accumulated_reasoning,
                                        true,
                                        &response_id,
                                        resp_output_index,
                                    ) {
                                        resp_output_index += 1;
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(done_sse));
                                    }

                                    // Close text part + message if open
                                    if resp_text_part_open {
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\n", serde_json::json!({
                                                "type": "response.output_text.done",
                                                "output_index": resp_output_index - 1,
                                                "content_index": 0,
                                                "text": resp_accumulated_text,
                                            }))
                                        ));
                                        resp_text_part_open = false;
                                    }
                                    if resp_message_open {
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\n", serde_json::json!({
                                                "type": "response.output_item.done",
                                                "output_index": resp_output_index - 1,
                                                "item": {
                                                    "type": "message",
                                                    "id": "msg_proxy",
                                                    "role": "assistant",
                                                    "content": [{"type": "output_text", "text": resp_accumulated_text}],
                                                    "status": "completed",
                                                }
                                            }))
                                        ));
                                        resp_message_open = false;
                                    }
                                    // Close previous func_call if open
                                    if resp_func_open {
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\ndata: {}\n\n",
                                                serde_json::json!({
                                                    "type": "response.function_call_arguments.done",
                                                    "output_index": resp_output_index - 1,
                                                    "item_id": format!("fc_{}", resp_call_id),
                                                    "call_id": resp_call_id,
                                                    "arguments": resp_accumulated_args,
                                                }),
                                                serde_json::json!({
                                                    "type": "response.output_item.done",
                                                    "output_index": resp_output_index - 1,
                                                    "item": {
                                                        "type": "function_call",
                                                        "id": format!("fc_{}", resp_call_id),
                                                        "call_id": resp_call_id,
                                                        "name": resp_func_name,
                                                        "arguments": resp_accumulated_args,
                                                    }
                                                })
                                            )
                                        ));
                                    }

                                    resp_call_id = tc.id.as_deref().unwrap_or("").to_string();
                                    resp_func_name = tc.name.as_deref().unwrap_or("").to_string();
                                    resp_accumulated_args.clear();

                                    let added = serde_json::json!({
                                        "type": "response.output_item.added",
                                        "output_index": resp_output_index,
                                        "item": {
                                            "type": "function_call",
                                            "id": format!("fc_{}", resp_call_id),
                                            "call_id": resp_call_id,
                                            "name": resp_func_name,
                                            "arguments": "",
                                        }
                                    });
                                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                        format!("data: {}\n\n", added)
                                    ));
                                    resp_func_open = true;
                                    had_tool_calls = true;
                                    resp_output_index += 1;
                                    continue;
                                }
                                // Argument delta
                                if let Some(args) = &tc.arguments {
                                    if !args.is_empty() {
                                        resp_accumulated_args.push_str(args);
                                        let delta_event = serde_json::json!({
                                            "type": "response.function_call_arguments.delta",
                                            "output_index": resp_output_index - 1,
                                            "item_id": format!("fc_{}", resp_call_id),
                                            "call_id": resp_call_id,
                                            "delta": args,
                                        });
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\n", delta_event)
                                        ));
                                    }
                                    continue;
                                }
                            }
                        }

                        // Thinking / reasoning_content — output as reasoning summary events
                        if let Some(thinking) = &ir_chunk.delta_thinking {
                            if !thinking.is_empty() && !resp_func_open {
                                if !resp_message_open && !resp_thinking_started {
                                    // Emit response.created if not started
                                    if !started {
                                        let created = serde_json::json!({
                                            "type": "response.created",
                                            "response": {
                                                "id": response_id,
                                                "object": "response",
                                                "status": "in_progress",
                                                "model": model_name,
                                                "output": [],
                                            }
                                        });
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\n", created)
                                        ));
                                        started = true;
                                    }
                                }
                                // First thinking chunk: emit reasoning summary part added
                                if !resp_thinking_started {
                                    let part_added = serde_json::json!({
                                        "type": "response.reasoning_summary_part.added",
                                        "output_index": resp_output_index,
                                        "content_index": 0,
                                        "part": {"type": "summary_text", "text": ""},
                                        "response_id": response_id,
                                    });
                                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                        format!("data: {}\n\n", part_added)
                                    ));
                                    resp_thinking_started = true;
                                }
                                resp_accumulated_reasoning.push_str(thinking);
                                let delta_event = serde_json::json!({
                                    "type": "response.reasoning_summary_text.delta",
                                    "output_index": resp_output_index,
                                    "content_index": 0,
                                    "delta": thinking,
                                    "response_id": response_id,
                                });
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\n", delta_event)
                                ));
                            }
                            continue;
                        }

                        // Text content
                        if let Some(content) = &ir_chunk.delta_content {
                            if !content.is_empty() && !resp_func_open {
                                // Close reasoning summary if we were in thinking mode
                                if let Some(done_sse) = close_responses_thinking_if_needed(
                                    &mut resp_thinking_started,
                                    &mut resp_accumulated_reasoning,
                                    true,
                                    &response_id,
                                    resp_output_index,
                                ) {
                                    resp_output_index += 1;
                                    yield Ok::<_, std::convert::Infallible>(Bytes::from(done_sse));
                                }

                                if !resp_message_open {
                                    // Emit response.created if not started
                                    if !started {
                                        let created = serde_json::json!({
                                            "type": "response.created",
                                            "response": {
                                                "id": response_id,
                                                "object": "response",
                                                "status": "in_progress",
                                                "model": model_name,
                                                "output": [],
                                            }
                                        });
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\n", created)
                                        ));
                                    }
                                    let item_added = serde_json::json!({
                                        "type": "response.output_item.added",
                                        "output_index": resp_output_index,
                                        "item": {
                                            "type": "message",
                                            "id": "msg_proxy",
                                            "role": "assistant",
                                            "content": [],
                                            "status": "in_progress",
                                        }
                                    });
                                    let part_added = serde_json::json!({
                                        "type": "response.content_part.added",
                                        "output_index": resp_output_index,
                                        "content_index": 0,
                                        "part": {"type": "output_text", "text": ""},
                                    });
                                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                        format!("data: {}\n\ndata: {}\n\n", item_added, part_added)
                                    ));
                                    resp_message_open = true;
                                    resp_text_part_open = true;
                                    resp_output_index += 1;
                                    started = true;
                                }
                                resp_accumulated_text.push_str(content);
                                let delta_event = serde_json::json!({
                                    "type": "response.output_text.delta",
                                    "output_index": resp_output_index - 1,
                                    "content_index": 0,
                                    "delta": content,
                                    "response_id": response_id,
                                });
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\n", delta_event)
                                ));
                            }
                            continue;
                        }

                        // Handle upstream failure
                        if ir_chunk.finish_reason.as_deref() == Some("failed") {
                            if resp_text_part_open {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\n", serde_json::json!({
                                        "type": "response.output_text.done",
                                        "output_index": resp_output_index - 1,
                                        "content_index": 0,
                                        "text": resp_accumulated_text,
                                    }))
                                ));
                                resp_text_part_open = false;
                            }
                            if resp_message_open {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\n", serde_json::json!({
                                        "type": "response.output_item.done",
                                        "output_index": resp_output_index - 1,
                                        "item": {
                                            "type": "message",
                                            "id": "msg_proxy",
                                            "role": "assistant",
                                            "content": [{"type": "output_text", "text": resp_accumulated_text}],
                                            "status": "incomplete",
                                        }
                                    }))
                                ));
                                resp_message_open = false;
                            }
                            if resp_func_open {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\n", serde_json::json!({
                                        "type": "response.output_item.done",
                                        "output_index": resp_output_index - 1,
                                        "item": {
                                            "type": "function_call",
                                            "id": format!("fc_{}", resp_call_id),
                                            "call_id": resp_call_id,
                                            "name": resp_func_name,
                                            "arguments": resp_accumulated_args,
                                            "status": "incomplete",
                                        }
                                    }))
                                ));
                                resp_func_open = false;
                            }

                            let err_code = ir_chunk.error.as_ref()
                                .and_then(|e| e.code.clone())
                                .unwrap_or_else(|| "server_error".to_string());
                            let err_message = ir_chunk.error.as_ref()
                                .map(|e| e.message.clone())
                                .unwrap_or_else(|| "upstream response failed".to_string());

                            let failed_event = serde_json::json!({
                                "type": "response.failed",
                                "response": {
                                    "id": response_id,
                                    "object": "response",
                                    "status": "failed",
                                    "model": model_name,
                                    "output": build_responses_output_array(
                                        &resp_accumulated_text,
                                        &resp_accumulated_reasoning,
                                        resp_thinking_started,
                                        resp_func_open,
                                        &resp_call_id,
                                        &resp_func_name,
                                        &resp_accumulated_args,
                                    ),
                                },
                                "error": {
                                    "code": err_code,
                                    "message": err_message,
                                }
                            });
                            yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                format!("data: {}\n\n", failed_event)
                            ));
                            finished = true;
                            continue;
                        }

                        // Finish (normal completion)
                        if ir_chunk.finish_reason.is_some() {
                            // Close reasoning summary if still open
                            if let Some(done_sse) = close_responses_thinking_if_needed(
                                &mut resp_thinking_started,
                                &mut resp_accumulated_reasoning,
                                false,
                                &response_id,
                                resp_output_index,
                            ) {
                                resp_output_index += 1;
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(done_sse));
                            }

                            // Close func_call if open
                            if resp_func_open {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\ndata: {}\n\n",
                                        serde_json::json!({
                                            "type": "response.function_call_arguments.done",
                                            "output_index": resp_output_index - 1,
                                            "item_id": format!("fc_{}", resp_call_id),
                                            "call_id": resp_call_id,
                                            "arguments": resp_accumulated_args,
                                        }),
                                        serde_json::json!({
                                            "type": "response.output_item.done",
                                            "output_index": resp_output_index - 1,
                                            "item": {
                                                "type": "function_call",
                                                "id": format!("fc_{}", resp_call_id),
                                                "call_id": resp_call_id,
                                                "name": resp_func_name,
                                                "arguments": resp_accumulated_args,
                                            }
                                        })
                                    )
                                ));
                                resp_func_open = false;
                            }
                            // Close message if open
                            if resp_message_open {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\ndata: {}\n\n",
                                        serde_json::json!({
                                            "type": "response.output_text.done",
                                            "output_index": resp_output_index - 1,
                                            "content_index": 0,
                                            "text": resp_accumulated_text,
                                        }),
                                        serde_json::json!({
                                            "type": "response.output_item.done",
                                            "output_index": resp_output_index - 1,
                                            "item": {
                                                "type": "message",
                                                "id": "msg_proxy",
                                                "role": "assistant",
                                                "content": [{"type": "output_text", "text": resp_accumulated_text}],
                                                "status": "completed",
                                            }
                                        })
                                    )
                                ));
                                resp_message_open = false;
                            }

                            let completed = serde_json::json!({
                                "type": "response.completed",
                                "response": {
                                    "id": response_id,
                                    "object": "response",
                                    "status": "completed",
                                    "model": model_name,
                                    "output": build_responses_output_array(
                                        &resp_accumulated_text,
                                        &resp_accumulated_reasoning,
                                        resp_thinking_started,
                                        resp_func_open,
                                        &resp_call_id,
                                        &resp_func_name,
                                        &resp_accumulated_args,
                                    ),
                                    "usage": {
                                        "input_tokens": total_prompt,
                                        "output_tokens": total_completion,
                                        "total_tokens": total_prompt + total_completion,
                                    }
                                }
                            });
                            yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                format!("data: {}\n\n", completed)
                            ));

                            // Store accumulated reasoning in cache for multi-turn
                            if !resp_accumulated_reasoning.is_empty() {
                                if let Ok(mut cache) = REASONING_CACHE.lock() {
                                    cache.insert(response_id.to_string(), resp_accumulated_reasoning.clone());
                                    // Evict old entries (keep last 50)
                                    if cache.len() > 50 {
                                        let keys: Vec<String> = cache.keys().take(cache.len() - 50).cloned().collect();
                                        for k in keys { cache.remove(&k); }
                                    }
                                }
                            }

                            finished = true;
                            continue;
                        }

                        continue;
                    }

                    // Other formats (Completions, Gemini): delegate to generator
                    if !started {
                        started = true;
                    }
                    let sse_data = client_generator.generate_stream_chunk(&ir_chunk);
                    if !sse_data.is_empty() {
                        yield Ok::<_, std::convert::Infallible>(Bytes::from(sse_data));
                    }
                    if ir_chunk.finish_reason.is_some() && !finished {
                        // Send [DONE] marker for Completions format
                        if matches!(client_format, ClientFormat::Completions) {
                            // Emit final usage chunk with accumulated totals
                            if total_prompt > 0 || total_completion > 0 {
                                let usage_chunk = serde_json::json!({
                                    "id": response_id,
                                    "object": "chat.completion.chunk",
                                    "created": chrono::Utc::now().timestamp(),
                                    "model": model_name,
                                    "choices": [],
                                    "usage": {
                                        "prompt_tokens": total_prompt,
                                        "completion_tokens": total_completion,
                                        "total_tokens": total_prompt + total_completion,
                                    }
                                });
                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                    format!("data: {}\n\n", usage_chunk)
                                ));
                            }
                            yield Ok::<_, std::convert::Infallible>(Bytes::from("data: [DONE]\n\n"));
                        }
                        finished = true;
                    }
                }
            }

            // Process remaining buffer data after stream ended
            if !buffer.is_empty() {
                if let Ok(remaining) = std::str::from_utf8(&buffer) {
                    let trimmed = remaining.trim();
                    if !trimmed.is_empty() && !finished {
                        info!("Processing {} bytes remaining in buffer after stream end", buffer.len());
                        match target_parser.parse_stream_chunk(trimmed) {
                            Ok(Some(ir_chunk)) => {
                                if let Some(usage) = &ir_chunk.usage {
                                    if usage.prompt_tokens > 0 { total_prompt = usage.prompt_tokens; }
                                    if usage.completion_tokens > 0 { total_completion = usage.completion_tokens; }
                                    if usage.cached_tokens > 0 { total_cached = usage.cached_tokens; }
                                }
                                if is_responses {
                                    // For Responses format, only handle finish events from remaining buffer
                                    if ir_chunk.finish_reason.as_deref() == Some("failed") {
                                        let err_code = ir_chunk.error.as_ref()
                                            .and_then(|e| e.code.clone())
                                            .unwrap_or_else(|| "server_error".to_string());
                                        let err_message = ir_chunk.error.as_ref()
                                            .map(|e| e.message.clone())
                                            .unwrap_or_else(|| "upstream response failed".to_string());
                                        let failed_event = serde_json::json!({
                                            "type": "response.failed",
                                            "response": {
                                                "id": response_id,
                                                "object": "response",
                                                "status": "failed",
                                                "model": model_name,
                                                "output": build_responses_output_array(
                                                    &resp_accumulated_text,
                                                    &resp_accumulated_reasoning,
                                                    resp_thinking_started,
                                                    resp_func_open,
                                                    &resp_call_id,
                                                    &resp_func_name,
                                                    &resp_accumulated_args,
                                                ),
                                            },
                                            "error": {
                                                "code": err_code,
                                                "message": err_message,
                                            }
                                        });
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                            format!("data: {}\n\n", failed_event)
                                        ));
                                        finished = true;
                                    }
                                } else {
                                    let sse_data = client_generator.generate_stream_chunk(&ir_chunk);
                                    if !sse_data.is_empty() {
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(sse_data));
                                    }
                                    if ir_chunk.finish_reason.is_some() {
                                        if matches!(client_format, ClientFormat::Completions) {
                                            if total_prompt > 0 || total_completion > 0 {
                                                let usage_chunk = serde_json::json!({
                                                    "id": response_id,
                                                    "object": "chat.completion.chunk",
                                                    "created": chrono::Utc::now().timestamp(),
                                                    "model": model_name,
                                                    "choices": [],
                                                    "usage": {
                                                        "prompt_tokens": total_prompt,
                                                        "completion_tokens": total_completion,
                                                        "total_tokens": total_prompt + total_completion,
                                                    }
                                                });
                                                yield Ok::<_, std::convert::Infallible>(Bytes::from(
                                                    format!("data: {}\n\n", usage_chunk)
                                                ));
                                            }
                                            yield Ok::<_, std::convert::Infallible>(Bytes::from("data: [DONE]\n\n"));
                                        }
                                        finished = true;
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse remaining buffer data: {}", e);
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Safety: close any open content blocks if stream ended unexpectedly
            if is_anthropic && started && !finished {
                if text_block_open {
                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                        format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", content_block_index - 1)
                    ));
                }
                if tool_block_open {
                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                        format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{}}}\n\n", content_block_index - 1)
                    ));
                }
            }
            if is_responses && started && !finished {
                // Kimi 等非标准端点可能不发送 message_stop/message_delta，
                // 但已经发送了实际内容。如果有内容，视为正常完成。
                let has_content = !resp_accumulated_text.is_empty()
                    || !resp_accumulated_args.is_empty()
                    || resp_thinking_started;

                // Close open items first
                if resp_func_open {
                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                        format!("data: {}\n\n", serde_json::json!({
                            "type": "response.output_item.done",
                            "output_index": resp_output_index - 1,
                            "item": {
                                "type": "function_call",
                                "id": format!("fc_{}", resp_call_id),
                                "call_id": resp_call_id,
                                "name": resp_func_name,
                                "arguments": resp_accumulated_args,
                            }
                        }))
                    ));
                    resp_func_open = false;
                }
                if resp_message_open {
                    if resp_text_part_open {
                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                            format!("data: {}\n\n", serde_json::json!({
                                "type": "response.output_text.done",
                                "output_index": resp_output_index - 1,
                                "content_index": 0,
                                "text": resp_accumulated_text,
                            }))
                        ));
                        resp_text_part_open = false;
                    }
                    let status = if has_content { "completed" } else { "incomplete" };
                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                        format!("data: {}\n\n", serde_json::json!({
                            "type": "response.output_item.done",
                            "output_index": resp_output_index - 1,
                            "item": {
                                "type": "message",
                                "id": "msg_proxy",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": resp_accumulated_text}],
                                "status": status,
                            }
                        }))
                    ));
                    resp_message_open = false;
                }

                if has_content {
                    // Graceful completion: upstream sent content but no proper termination
                    tracing::info!("Stream completed without proper termination, synthesizing response.completed from accumulated content");
                    let completed = serde_json::json!({
                        "type": "response.completed",
                        "response": {
                            "id": response_id,
                            "object": "response",
                            "status": "completed",
                            "model": model_name,
                            "output": build_responses_output_array(
                                &resp_accumulated_text,
                                &resp_accumulated_reasoning,
                                resp_thinking_started,
                                resp_func_open,
                                &resp_call_id,
                                &resp_func_name,
                                &resp_accumulated_args,
                            ),
                            "usage": {
                                "input_tokens": total_prompt,
                                "output_tokens": total_completion,
                                "total_tokens": total_prompt + total_completion,
                            }
                        }
                    });
                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                        format!("data: {}\n\n", completed)
                    ));
                    finished = true;
                } else {
                    // No content at all — report failure
                    let failed_output = build_responses_output_array(
                        &resp_accumulated_text,
                        &resp_accumulated_reasoning,
                        resp_thinking_started,
                        resp_func_open,
                        &resp_call_id,
                        &resp_func_name,
                        &resp_accumulated_args,
                    );
                    let failed_event = serde_json::json!({
                        "type": "response.failed",
                        "response": {
                            "id": response_id,
                            "object": "response",
                            "status": "failed",
                            "model": model_name,
                            "output": failed_output,
                        },
                        "error": {
                            "code": "server_error",
                            "message": "stream disconnected before completion",
                        }
                    });
                    yield Ok::<_, std::convert::Infallible>(Bytes::from(
                        format!("data: {}\n\n", failed_event)
                    ));
                    finished = true;
                }
            }

            // Emit error events for interrupted streams so clients can detect the failure
            if started && !finished {
                match client_format {
                    ClientFormat::Completions | ClientFormat::Gemini => {
                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                            format!("data: {{\"error\": {{\"message\": \"stream interrupted by proxy\", \"type\": \"server_error\"}}}}\n\ndata: [DONE]\n\n")
                        ));
                    }
                    ClientFormat::Anthropic => {
                        yield Ok::<_, std::convert::Infallible>(Bytes::from(
                            format!("event: error\ndata: {{\"type\": \"error\", \"error\": {{\"type\": \"api_error\", \"message\": \"stream interrupted by proxy\"}}}}\n\n")
                        ));
                    }
                    ClientFormat::Responses => {
                        // Already handled above
                    }
                }
            }

            let elapsed = start.elapsed().as_millis() as i64;
            let pt = total_prompt as i64;
            let ct = total_completion as i64;
            let cache_t = total_cached as i64;

            stream_state_ref.logged.store(true, Ordering::SeqCst);

            if let Err(e) = log_request_entry(
                &request_id,
                &client_format_clone,
                &route.provider_name,
                &route.target_format,
                &target_model,
                true,
                200,
                elapsed,
                None,
                pt,
                ct,
                cache_t,
                ttft_ms,
            )
            .await
            {
                tracing::error!("Stream logging failed: {}", e);
            }

            info!("[DONE] {} status=200 duration={}ms tokens={}/{} ttft={}ms",
                target_model, elapsed, pt, ct, ttft_ms.unwrap_or(0));
        };

        let body_stream = axum::body::Body::from_stream(sse_stream);

        Response::builder()
            .status(status)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(body_stream)
            .unwrap()
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

/// Extract concatenated text from an SSE response body.
fn extract_text_from_sse_body(body: &str, format: &ClientFormat) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for line in body.lines() {
        let data = match line.strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        match format {
            ClientFormat::Anthropic => {
                if json.get("type").and_then(|v| v.as_str()) == Some("content_block_delta") {
                    if let Some(t) = json.pointer("/delta/text").and_then(|v| v.as_str()) {
                        parts.push(t.to_string());
                    }
                }
            }
            ClientFormat::Completions | ClientFormat::Responses => {
                if let Some(c) = json.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
                    parts.push(c.to_string());
                }
            }
            ClientFormat::Gemini => {
                if let Some(t) = json.pointer("/candidates/0/content/parts/0/text").and_then(|v| v.as_str()) {
                    parts.push(t.to_string());
                }
            }
        }
    }
    if parts.is_empty() { None } else { Some(parts.join("")) }
}

fn get_generator(format: &ClientFormat) -> Box<dyn FormatGenerator> {
    match format {
        ClientFormat::Completions => Box::new(CompletionsGenerator),
        ClientFormat::Responses => Box::new(ResponsesGenerator),
        ClientFormat::Anthropic => Box::new(AnthropicGenerator),
        ClientFormat::Gemini => Box::new(GeminiGenerator),
    }
}

use serde::Serialize;
use serde_json::json;

pub async fn handle_list_models() -> Response {
    let models = match query_model_routes().await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let data: Vec<Value> = models
        .iter()
        .map(|m| {
            json!({
                "id": m.model_name,
                "object": "model",
                "created": 0,
                "owned_by": m.provider_name,
            })
        })
        .collect();

    let body = json!({
        "object": "list",
        "data": data,
    });

    axum::Json(body).into_response()
}

pub async fn handle_get_model(Path(model): Path<String>) -> Response {
    let models = match query_model_routes().await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let found = models.iter().find(|m| m.model_name == model);

    match found {
        Some(m) => {
            let body = json!({
                "id": m.model_name,
                "object": "model",
                "created": 0,
                "owned_by": m.provider_name,
            });
            axum::Json(body).into_response()
        }
        None => ProxyError::ModelNotFound(format!("model '{}' not found", model)).into_response(),
    }
}

pub async fn handle_gemini_list_models() -> Response {
    let models = match query_model_routes().await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let gemini_models: Vec<Value> = models
        .iter()
        .map(|m| {
            json!({
                "name": format!("models/{}", m.model_name),
                "displayName": m.model_name,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            })
        })
        .collect();

    let body = json!({
        "models": gemini_models,
    });

    axum::Json(body).into_response()
}

pub async fn handle_gemini_get_model(Path(model): Path<String>) -> Response {
    let models = match query_model_routes().await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };

    let model_name = model.split(':').next().unwrap_or(&model);

    let found = models.iter().find(|m| m.model_name == model_name);

    match found {
        Some(m) => {
            let body = json!({
                "name": format!("models/{}", m.model_name),
                "displayName": m.model_name,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            });
            axum::Json(body).into_response()
        }
        None => ProxyError::ModelNotFound(format!("model '{}' not found", model_name)).into_response(),
    }
}

#[derive(Serialize)]
struct ModelRouteInfo {
    model_name: String,
    provider_name: String,
    target_model: Option<String>,
    format: String,
}

async fn query_model_routes() -> Result<Vec<ModelRouteInfo>, ProxyError> {
    let pool = crate::db::get_pool().await;

    let rows = sqlx::query_as::<_, (String, String, Option<String>, String)>(
        "SELECT pm.model_name, p.name, pm.target_model, p.format \
         FROM provider_models pm \
         JOIN providers p ON pm.provider_id = p.id \
         WHERE pm.enabled = 1 \
         ORDER BY pm.model_name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ProxyError::Database(e))?;

    Ok(rows
        .into_iter()
        .map(|(model_name, provider_name, target_model, format)| {
            ModelRouteInfo {
                model_name,
                provider_name,
                target_model,
                format,
            }
        })
        .collect())
}

fn extract_text_from_html(html: &str, max_len: usize) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    let mut prev_ws = true;
    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            continue;
        }
        if in_tag {
            continue;
        }
        if ch.is_whitespace() {
            if !prev_ws {
                text.push(' ');
            }
            prev_ws = true;
        } else {
            text.push(ch);
            prev_ws = false;
        }
    }
    let text = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .trim()
        .to_string();
    text.chars().take(max_len).collect()
}

fn extract_headers(header_map: &axum::http::HeaderMap, headers: &mut HashMap<String, String>) {
    let skip = [
        "content-length",
        "content-type",
        "host",
        "transfer-encoding",
        "connection",
        "authorization",
    ];
    for (name, value) in header_map.iter() {
        let key = name.as_str().to_lowercase();
        if skip.contains(&key.as_str()) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            headers.insert(key, v.to_string());
        }
    }
}

async fn log_request_entry(
    request_id: &str,
    client_format: &ClientFormat,
    provider_name: &str,
    provider_format: &ClientFormat,
    model: &str,
    stream: bool,
    status_code: u16,
    duration_ms: i64,
    error_message: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    ttft_ms: Option<i64>,
) -> Result<(), ProxyError> {
    log_request(
        request_id,
        &format!("{:?}", client_format).to_lowercase(),
        provider_name,
        &format!("{:?}", provider_format).to_lowercase(),
        model,
        stream,
        status_code,
        duration_ms,
        error_message,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        ttft_ms,
    )
    .await
}

struct StreamLogState {
    request_id: String,
    client_format: ClientFormat,
    provider_name: String,
    provider_format: ClientFormat,
    model: String,
    start: std::time::Instant,
    prompt_tokens: AtomicU32,
    completion_tokens: AtomicU32,
    cached_tokens: AtomicU32,
    ttft_ms: Mutex<Option<i64>>,
    logged: AtomicBool,
    interrupted: AtomicBool,
}

struct StreamLoggingGuard {
    state: Arc<StreamLogState>,
}

impl Drop for StreamLoggingGuard {
    fn drop(&mut self) {
        if self.state.logged.load(Ordering::SeqCst) {
            return;
        }

        let state = self.state.clone();
        let interrupted = state.interrupted.load(Ordering::SeqCst);
        tokio::spawn(async move {
            let pt = state.prompt_tokens.load(Ordering::SeqCst) as i64;
            let ct = state.completion_tokens.load(Ordering::SeqCst) as i64;
            let cache_t = state.cached_tokens.load(Ordering::SeqCst) as i64;
            let elapsed = state.start.elapsed().as_millis() as i64;
            let ttft = *state.ttft_ms.lock().unwrap();

            let (status_code, error_msg) = if interrupted {
                (502, Some("stream interrupted".to_string()))
            } else {
                (200, None)
            };

            if let Err(e) = log_request_entry(
                &state.request_id,
                &state.client_format,
                &state.provider_name,
                &state.provider_format,
                &state.model,
                true,
                status_code,
                elapsed,
                error_msg.as_deref(),
                pt,
                ct,
                cache_t,
                ttft,
            )
            .await
            {
                tracing::error!("Stream guard logging failed: {}", e);
            }

            if interrupted {
                tracing::warn!(
                    "[INTERRUPTED] {} duration={}ms tokens={}/{} - stream was interrupted",
                    state.model, elapsed, pt, ct
                );
            }
        });
    }
}

/// Split `<thinking>...</thinking>` from text.
/// Returns (thinking_content, remaining_text).
/// Emit reasoning summary done event when thinking ends.
fn close_responses_thinking_if_needed(
    thinking_started: &mut bool,
    accumulated_reasoning: &mut String,
    _append_newline: bool,
    response_id: &str,
    output_index: u32,
) -> Option<String> {
    if !*thinking_started {
        return None;
    }

    *thinking_started = false;
    let done_event = serde_json::json!({
        "type": "response.reasoning_summary_part.done",
        "output_index": output_index,
        "content_index": 0,
        "part": {
            "type": "summary_text",
            "text": accumulated_reasoning.clone(),
        },
        "response_id": response_id,
    });
    Some(format!("data: {}\n\n", done_event))
}

fn build_responses_output_array(
    accumulated_text: &str,
    accumulated_reasoning: &str,
    reasoning_started: bool,
    func_open: bool,
    call_id: &str,
    func_name: &str,
    accumulated_args: &str,
) -> Vec<serde_json::Value> {
    let mut output: Vec<serde_json::Value> = Vec::new();

    if reasoning_started && !accumulated_reasoning.is_empty() {
        output.push(serde_json::json!({
            "type": "reasoning",
            "id": "rs_proxy",
            "summary": [{"type": "summary_text", "text": accumulated_reasoning}],
        }));
    }

    if !accumulated_text.is_empty() {
        output.push(serde_json::json!({
            "type": "message",
            "id": "msg_proxy",
            "role": "assistant",
            "content": [{"type": "output_text", "text": accumulated_text}],
            "status": "completed",
        }));
    }

    if func_open && !call_id.is_empty() {
        output.push(serde_json::json!({
            "type": "function_call",
            "id": format!("fc_{}", call_id),
            "call_id": call_id,
            "name": func_name,
            "arguments": accumulated_args,
        }));
    }

    output
}

fn inject_cached_reasoning_into_assistant_messages(
    messages: &mut [crate::converter::ir::IrMessage],
    previous_response_id: Option<&str>,
    cache: &HashMap<String, String>,
) {
    // Only inject into the last assistant message that lacks thinking
    let last_assistant_idx = messages.iter().rposition(|m| {
        m.role == IrRole::Assistant
            && !m.content.iter().any(|p| matches!(p, IrContentPart::Thinking { .. }))
    });

    let Some(idx) = last_assistant_idx else { return };
    let msg = &mut messages[idx];

    // Prefer exact match by previous_response_id
    if let Some(reasoning) = previous_response_id.and_then(|id| cache.get(id)) {
        msg.content.insert(0, IrContentPart::Thinking { text: reasoning.clone() });
        return;
    }

    // Try extracting <thinking> tags from text content
    let text_content: String = msg
        .content
        .iter()
        .filter_map(|p| match p {
            IrContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let (thinking_opt, remaining) = split_thinking_tags(&text_content);
    if let Some(thinking) = thinking_opt {
        msg.content.clear();
        msg.content.push(IrContentPart::Thinking { text: thinking });
        let trimmed = remaining.trim();
        if !trimmed.is_empty() {
            msg.content.push(IrContentPart::Text { text: trimmed.to_string() });
        }
    }
}

fn split_thinking_tags(text: &str) -> (Option<String>, String) {
    let tag_start = "<thinking>";
    let tag_end = "</thinking>";
    let mut thinking = String::new();
    let mut remaining = text.to_string();
    while let Some(start_idx) = remaining.find(tag_start) {
        let after_start = start_idx + tag_start.len();
        if let Some(rel_end) = remaining[after_start..].find(tag_end) {
            thinking.push_str(&remaining[after_start..after_start + rel_end]);
            let end_abs = after_start + rel_end + tag_end.len();
            remaining = format!("{}{}", &remaining[..start_idx], &remaining[end_abs..]);
        } else {
            break;
        }
    }
    (if thinking.is_empty() { None } else { Some(thinking) }, remaining)
}

#[cfg(test)]
mod tests {
    use super::{
        close_responses_thinking_if_needed, inject_cached_reasoning_into_assistant_messages,
    };
    use crate::converter::ir::{IrContentPart, IrMessage, IrRole};
    use std::collections::HashMap;

    #[test]
    fn closes_thinking_before_tool_call_boundary() {
        let mut thinking_started = true;
        let mut accumulated_reasoning = "repo analysis".to_string();

        let done_sse = close_responses_thinking_if_needed(
            &mut thinking_started,
            &mut accumulated_reasoning,
            true,
            "resp_test",
            0,
        );

        assert!(done_sse.is_some());
        assert!(!thinking_started);
        let sse = done_sse.unwrap();
        assert!(sse.contains("response.reasoning_summary_part.done"));
        assert!(sse.contains("repo analysis"));
    }

    #[test]
    fn injects_cached_reasoning_when_previous_response_id_missing() {
        let mut messages = vec![IrMessage {
            role: IrRole::Assistant,
            content: vec![IrContentPart::Text {
                text: "最终答案".to_string(),
            }],
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut cache = HashMap::new();
        cache.insert("resp_1".to_string(), "已缓存推理".to_string());

        // No previous_response_id → no injection (fallback removed)
        inject_cached_reasoning_into_assistant_messages(&mut messages, None, &cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.len(), 1);
        match &messages[0].content[0] {
            IrContentPart::Text { text } => assert_eq!(text, "最终答案"),
            other => panic!("expected text content, got {:?}", other),
        }

        // With matching previous_response_id → inject into last assistant message
        inject_cached_reasoning_into_assistant_messages(&mut messages, Some("resp_1"), &cache);
        assert_eq!(messages[0].content.len(), 2);
        match &messages[0].content[0] {
            IrContentPart::Thinking { text } => assert_eq!(text, "已缓存推理"),
            other => panic!("expected thinking content, got {:?}", other),
        }
    }
}
