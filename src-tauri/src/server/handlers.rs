use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Cache: response_id → accumulated reasoning_content.
/// Stores reasoning from DeepSeek so it can be injected into subsequent requests
/// when Codex doesn't preserve `<thinking>` tags in multi-turn conversations.
static REASONING_CACHE: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

use axum::extract::{Path, Request};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::stream::StreamExt;
use futures::TryStreamExt;
use serde_json::Value;
#[allow(unused_imports)]
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::converter::generators::anthropic::AnthropicGenerator;
use crate::converter::generators::completions::CompletionsGenerator;
use crate::converter::generators::gemini::GeminiGenerator;
use crate::converter::generators::responses::ResponsesGenerator;
use crate::converter::ir::{ClientFormat, IrContentPart, IrRole, IrStreamChunk};
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

#[allow(dead_code)]
fn parse_retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            if let Ok(secs) = v.trim().parse::<u64>() {
                return Some(secs);
            }
            None
        })
}

use crate::provider::manager::{ProviderManager, ResolvedRoute};

pub async fn handle_completions(request: Request) -> Response {
    handle_proxy(request, ClientFormat::Completions, None, false, None).await
}

pub async fn handle_responses(request: Request) -> Response {
    handle_proxy(request, ClientFormat::Responses, None, false, None).await
}

/// POST /v1/responses/compact — OpenAI conversation compaction endpoint.
/// Forwards to upstream's /v1/responses/compact with the same routing,
/// key rotation, and logging as a normal responses request.
pub async fn handle_responses_compact(request: Request) -> Response {
    handle_proxy(
        request,
        ClientFormat::Responses,
        None,
        false,
        Some("/v1/responses/compact"),
    )
    .await
}

pub async fn handle_anthropic(request: Request) -> Response {
    handle_proxy(request, ClientFormat::Anthropic, None, false, None).await
}

pub async fn handle_anthropic_count_tokens(request: Request) -> Response {
    let start = std::time::Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();

    let (_parts, body) = request.into_parts();
    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to read count_tokens request body: {}", e);
            return ProxyError::Parse(format!("failed to read body: {}", e)).into_response();
        }
    };

    let body_value: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            error!("Invalid JSON in count_tokens request: {}", e);
            return ProxyError::Parse(format!("invalid JSON: {}", e)).into_response();
        }
    };

    let model = body_value["model"].as_str().unwrap_or("");

    let route = match ProviderManager::find_for_model(model).await {
        Ok(r) => {
            info!(
                "[count_tokens] Route: {} -> {} via {}",
                model, r.target_model, r.provider_name
            );
            r
        }
        Err(e) => {
            error!("[count_tokens] No route for model '{}': {}", model, e);
            return e.into_response();
        }
    };

    let selected_key =
        match KeyRotation::get_next_key(&route.provider_id, &RotationStrategy::LeastUsed).await {
            Ok(k) => k,
            Err(e) => {
                error!("[count_tokens] Key rotation error: {}", e);
                return e.into_response();
            }
        };

    let nonce_slice: Vec<u8> = selected_key.nonce;
    let mut nonce_array = [0u8; 12];
    if nonce_slice.len() == 12 {
        nonce_array.copy_from_slice(&nonce_slice);
    } else {
        return ProxyError::KeyManagement("invalid nonce length".into()).into_response();
    }

    let api_key = match decrypt_api_key(&selected_key.encrypted_key, &nonce_array) {
        Ok(k) => k,
        Err(e) => {
            error!("[count_tokens] Key decryption error: {}", e);
            return e.into_response();
        }
    };

    // Replace model with target model if configured
    let mut forward_body = body_value.clone();
    if !route.target_model.is_empty() && route.target_model != model {
        forward_body["model"] = Value::String(route.target_model.clone());
    }

    let url = format!(
        "{}/v1/messages/count_tokens",
        route.base_url.trim_end_matches('/')
    );
    info!("[count_tokens] Upstream: POST {}", url);

    let http_client = crate::http::SHARED_HTTP_CLIENT.clone();
    let mut req_builder = http_client
        .post(&url)
        .json(&forward_body)
        .header("Content-Type", "application/json")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(300));

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
            error!("[count_tokens] Upstream request failed: {}", e);
            return ProxyError::Network(format!("upstream request failed: {}", e)).into_response();
        }
    };

    let status = resp.status();
    let resp_body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            error!("[count_tokens] Failed to read upstream response: {}", e);
            return ProxyError::Network(format!("failed to read response: {}", e)).into_response();
        }
    };

    info!(
        "[count_tokens] Completed in {}ms, status={}",
        start.elapsed().as_millis(),
        status
    );

    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        resp_body.to_vec(),
    )
        .into_response()
}

pub async fn handle_gemini(Path(model_segment): Path<String>, request: Request) -> Response {
    let (model, is_stream) = parse_gemini_model_segment(&model_segment);
    handle_proxy(request, ClientFormat::Gemini, Some(model), is_stream, None).await
}

pub(crate) fn parse_gemini_model_segment(segment: &str) -> (String, bool) {
    let is_stream = segment.contains("streamGenerateContent");
    let model = segment.split(':').next().unwrap_or(segment).to_string();
    (model, is_stream)
}

fn truncate_str(s: &str, max_len: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= max_len {
        std::borrow::Cow::Borrowed(s)
    } else {
        let boundary = s
            .char_indices()
            .take_while(|(i, _)| *i < max_len)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max_len.min(s.len()));
        std::borrow::Cow::Owned(format!(
            "{}... (truncated, {} bytes total)",
            &s[..boundary],
            s.len()
        ))
    }
}

pub(crate) async fn handle_proxy(
    request: Request,
    client_format: ClientFormat,
    override_model: Option<String>,
    force_stream: bool,
    endpoint_override: Option<&str>,
) -> Response {
    handle_proxy_inner(
        request,
        client_format,
        override_model,
        force_stream,
        None,
        endpoint_override,
    )
    .await
}

pub(crate) async fn handle_proxy_inner(
    request: Request,
    client_format: ClientFormat,
    override_model: Option<String>,
    force_stream: bool,
    pre_resolved: Option<ResolvedRoute>,
    endpoint_override: Option<&str>,
) -> Response {
    let start = std::time::Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();

    let (parts, body) = request.into_parts();

    // Capture the downstream (client) User-Agent for request logging.
    let client_user_agent = parts
        .headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to read request body: {}", e);
            return ProxyError::Parse(format!("failed to read body: {}", e)).into_response();
        }
    };

    let mut body_value: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("[ERR] invalid request body: {}", e);
            if let Err(le) = log_request_entry(
                &request_id,
                &client_format,
                "proxy",
                &client_format,
                "unknown",
                "",
                false,
                400,
                start.elapsed().as_millis() as i64,
                Some(&format!("invalid JSON: {}", e)),
                0,
                0,
                0,
                None,
                None,
                None,
                0,
                None,
                client_user_agent.as_deref(),
            )
            .await
            {
                tracing::error!("Early error logging failed: {}", le);
            }
            return ProxyError::Parse(format!("invalid JSON: {}", e)).into_response();
        }
    };

    let parser = get_parser(&client_format);
    let generator = get_generator(&client_format);

    // Pre-process: extract system-role messages from messages array into top-level system field
    // when the setting is enabled. This fixes Claude Code v2.1.153+ which puts role:"system"
    // inside messages instead of the top-level system parameter.
    {
        let pool_ref = crate::db::pool::get_pool().await;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT key, value FROM settings WHERE key = 'extract_system_from_messages'",
        )
        .fetch_all(pool_ref)
        .await
        .unwrap_or_default();
        let map: HashMap<String, String> = rows.into_iter().collect();
        let enabled = map
            .get("extract_system_from_messages")
            .map(|v| v == "true")
            .unwrap_or(true);

        if enabled {
            if let Some(msgs) = body_value
                .get_mut("messages")
                .and_then(|m| m.as_array_mut())
            {
                let mut extra_systems: Vec<String> = Vec::new();
                let mut i = 0;
                while i < msgs.len() {
                    if msgs[i]["role"].as_str() == Some("system")
                        || msgs[i]["role"].as_str() == Some("developer")
                    {
                        let msg = msgs.remove(i);
                        let content = &msg["content"];
                        let text = if let Some(s) = content.as_str() {
                            s.to_string()
                        } else if let Some(arr) = content.as_array() {
                            arr.iter()
                                .filter_map(|p| {
                                    if p["type"].as_str() == Some("text") {
                                        p["text"].as_str().map(String::from)
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        } else {
                            String::new()
                        };
                        if !text.is_empty() {
                            extra_systems.push(text);
                        }
                    } else {
                        i += 1;
                    }
                }
                if !extra_systems.is_empty() {
                    let existing = if let Some(sys) = body_value.get("system") {
                        if let Some(s) = sys.as_str() {
                            s.to_string()
                        } else if let Some(arr) = sys.as_array() {
                            arr.iter()
                                .filter_map(|p| {
                                    if p["type"].as_str() == Some("text") {
                                        p["text"].as_str().map(String::from)
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };
                    let combined = if existing.is_empty() {
                        extra_systems.join("\n\n")
                    } else {
                        format!("{}\n\n{}", existing, extra_systems.join("\n\n"))
                    };
                    body_value["system"] = serde_json::Value::String(combined);
                }
            }
        }
    }

    {
        let model_hint = body_value["model"].as_str().unwrap_or("unknown");
        let stream = body_value["stream"].as_bool().unwrap_or(false);
        info!(
            "[REQ] {:?} model={} stream={}",
            client_format, model_hint, stream
        );
    }

    if tracing::enabled!(tracing::Level::DEBUG) {
        if let Some(msgs) = body_value["messages"].as_array() {
            let has_tool = msgs.iter().any(|m| {
                let role = m["role"].as_str().unwrap_or("");
                role == "tool"
                    || role == "function"
                    || m.get("tool_calls").is_some()
                    || m.get("function_call").is_some()
            });
            if has_tool {
                let serialized = serde_json::to_string(&body_value["messages"]).unwrap_or_default();
                tracing::debug!("RAW REQUEST messages: {}", truncate_str(&serialized, 2000));
            }
        }
    }

    let mut ir_request = match parser.parse_request(&body_value) {
        Ok(r) => {
            info!(
                "Parsed request: model={}, stream={}, thinking={:?}",
                r.model, r.stream, r.thinking
            );
            r
        }
        Err(e) => {
            let model_hint = body_value["model"].as_str().unwrap_or("unknown");
            tracing::error!("[ERR] parse failed model={}: {}", model_hint, e);
            error!("Parse request error: {}", e);
            if let Err(le) = log_request_entry(
                &request_id,
                &client_format,
                "proxy",
                &client_format,
                model_hint,
                "",
                false,
                400,
                start.elapsed().as_millis() as i64,
                Some(&format!("parse error: {}", e)),
                0,
                0,
                0,
                None,
                None,
                None,
                0,
                None,
                client_user_agent.as_deref(),
            )
            .await
            {
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
    let client_model = ir_request.model.clone();

    if let Err(e) =
        InterceptorEngine::execute_pre_rules(&mut ir_request, &path, &mut extra_headers).await
    {
        error!("Interceptor error: {}", e);
    }

    let mut route = if let Some(r) = pre_resolved {
        info!(
            "[ROUTE] (pre-resolved) {} -> {} ({})",
            client_model, r.target_model, r.provider_name
        );
        // Non-standard Anthropic endpoints (e.g. Kimi coding) don't support thinking parameter.
        if ir_request.thinking.is_some() {
            if r.base_url.contains("kimi.com") || r.base_url.contains("moonshot.cn") {
                tracing::info!(
                    "Clearing thinking for non-standard Anthropic endpoint: {}",
                    r.base_url
                );
                ir_request.thinking = None;
            }
        }
        r
    } else {
        match ProviderManager::find_for_model(&ir_request.model).await {
            Ok(r) => {
                info!(
                    "Route found: model={} -> {} ({:?} via {})",
                    ir_request.model, r.target_model, r.target_format, r.provider_name
                );
                info!(
                    "[ROUTE] {} -> {} ({})",
                    ir_request.model, r.target_model, r.provider_name
                );
                // Non-standard Anthropic endpoints (e.g. Kimi coding) don't support thinking parameter.
                // Clear it to avoid upstream errors and max_tokens inflation.
                if ir_request.thinking.is_some() {
                    if r.base_url.contains("kimi.com") || r.base_url.contains("moonshot.cn") {
                        tracing::info!(
                            "Clearing thinking for non-standard Anthropic endpoint: {}",
                            r.base_url
                        );
                        ir_request.thinking = None;
                    }
                }
                r
            }
            Err(e) => {
                tracing::error!("[ERR] no route for model={}: {}", ir_request.model, e);
                error!("Route not found for model '{}': {}", ir_request.model, e);
                if let Err(le) = log_request_entry(
                    &request_id,
                    &client_format,
                    "proxy",
                    &client_format,
                    &client_model,
                    "",
                    ir_request.stream,
                    404,
                    start.elapsed().as_millis() as i64,
                    Some(&format!("route not found: {}", e)),
                    0,
                    0,
                    0,
                    None,
                    None,
                    None,
                    0,
                    None,
                    client_user_agent.as_deref(),
                )
                .await
                {
                    tracing::error!("Early error logging failed: {}", le);
                }
                return e.into_response();
            }
        }
    };

    // Override endpoint path (e.g. /v1/responses/compact for the compact endpoint).
    if let Some(ep) = endpoint_override {
        route.endpoint_path = ep.to_string();
    }

    let selected_key =
        match KeyRotation::get_next_key(&route.provider_id, &RotationStrategy::LeastUsed).await {
            Ok(k) => k,
            Err(e) => {
                let err_msg = format!("key rotation error: {}", e);
                if let Err(le) = log_request_entry(
                    &request_id,
                    &client_format,
                    &route.provider_name,
                    &route.target_format,
                    &client_model,
                    &route.target_model,
                    ir_request.stream,
                    500,
                    start.elapsed().as_millis() as i64,
                    Some(&err_msg),
                    0,
                    0,
                    0,
                    None,
                    None,
                    None,
                    0,
                    None,
                    client_user_agent.as_deref(),
                )
                .await
                {
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
            &request_id,
            &client_format,
            &route.provider_name,
            &route.target_format,
            &client_model,
            &route.target_model,
            ir_request.stream,
            500,
            start.elapsed().as_millis() as i64,
            Some("invalid nonce length"),
            0,
            0,
            0,
            None,
            None,
            None,
            0,
            None,
            client_user_agent.as_deref(),
        )
        .await
        {
            tracing::error!("Early error logging failed: {}", le);
        }
        return ProxyError::KeyManagement("invalid nonce length".into()).into_response();
    }

    let api_key = match decrypt_api_key(&selected_key.encrypted_key, &nonce_array) {
        Ok(k) => k,
        Err(e) => {
            let err_msg = format!("key decryption error: {}", e);
            if let Err(le) = log_request_entry(
                &request_id,
                &client_format,
                &route.provider_name,
                &route.target_format,
                &client_model,
                &route.target_model,
                ir_request.stream,
                500,
                start.elapsed().as_millis() as i64,
                Some(&err_msg),
                0,
                0,
                0,
                None,
                None,
                None,
                0,
                None,
                client_user_agent.as_deref(),
            )
            .await
            {
                tracing::error!("Early error logging failed: {}", le);
            }
            return e.into_response();
        }
    };

    // Inject custom upstream User-Agent (provider override > global > passthrough client UA).
    {
        let global_ua = get_setting("upstream_user_agent").await.unwrap_or_default();
        let final_ua: &str = if !route.upstream_user_agent.is_empty() {
            &route.upstream_user_agent
        } else if !global_ua.is_empty() {
            &global_ua
        } else {
            ""
        };
        if !final_ua.is_empty() {
            extra_headers.insert("user-agent".to_string(), final_ua.to_string());
        }
    }

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
        Ok(b) => {
            // Log extra fields for debugging
            if !ir_request_for_upstream.extra.is_empty() {
                tracing::info!(
                    "[UPSTREAM] extra fields present in IR: {:?}, body has chat_template_kwargs: {}",
                    ir_request_for_upstream.extra.keys().collect::<Vec<_>>(),
                    b.get("chat_template_kwargs").is_some()
                );
            }
            b
        }
        Err(e) => {
            let err_msg = format!("request generation error: {}", e);
            if let Err(le) = log_request_entry(
                &request_id,
                &client_format,
                &route.provider_name,
                &route.target_format,
                &client_model,
                &target_model,
                ir_request.stream,
                500,
                start.elapsed().as_millis() as i64,
                Some(&err_msg),
                0,
                0,
                0,
                None,
                None,
                None,
                0,
                None,
                client_user_agent.as_deref(),
            )
            .await
            {
                tracing::error!("Early error logging failed: {}", le);
            }
            return e.into_response();
        }
    };

    if tracing::enabled!(tracing::Level::DEBUG) {
        if ir_request_for_upstream
            .messages
            .iter()
            .any(|m| m.role == IrRole::Tool)
        {
            let serialized = serde_json::to_string(&target_body["messages"]).unwrap_or_default();
            tracing::debug!(
                "TOOL DEBUG messages for model={}: {}",
                ir_request_for_upstream.model,
                truncate_str(&serialized, 2000)
            );
        }
    }

    let mut url = format!(
        "{}{}",
        route.base_url.trim_end_matches('/'),
        route.endpoint_path
    );

    if client_format == ClientFormat::Gemini && ir_request.stream {
        url = url.replace(":generateContent", ":streamGenerateContent");
    }

    info!("Upstream request: {} {}", "POST", url);

    let http_client = crate::http::SHARED_HTTP_CLIENT.clone();

    // Load retry config from DB
    let retry_config =
        crate::server::retry_session::load_config_from_db(client_format.clone()).await;

    // Build a request factory that re-injects the right auth header per attempt.
    let url_clone = url.clone();
    let target_body_clone = target_body.clone();
    let extra_headers_clone = extra_headers.clone();
    let target_format_clone = route.target_format.clone();
    let http_client_clone = http_client.clone();
    let is_stream_for_timeout = ir_request.stream;

    // Decrypt all available keys for rotation
    let mut decrypted_keys: Vec<String> = Vec::new();
    let all_keys = crate::key::store::list_active_keys(&route.provider_id)
        .await
        .unwrap_or_default();
    for k in all_keys {
        // SelectedKey.nonce is Vec<u8>; decrypt_api_key expects &[u8; 12].
        if k.nonce.len() != 12 {
            continue;
        }
        let mut nonce_arr = [0u8; 12];
        nonce_arr.copy_from_slice(&k.nonce);
        if let Ok(d) = decrypt_api_key(&k.encrypted_key, &nonce_arr) {
            decrypted_keys.push(d);
        }
    }
    if decrypted_keys.is_empty() {
        // fallback: legacy single-key path (uses the already-resolved selected_key)
        decrypted_keys.push(api_key.clone());
    }

    let factory = move |key: &str| {
        let client = http_client_clone.clone();
        let url = url_clone.clone();
        let body = target_body_clone.clone();
        let extra = extra_headers_clone.clone();
        let fmt = target_format_clone.clone();
        let key_owned = key.to_string();
        async move {
            let mut b = client
                .post(&url)
                .json(&body)
                .header("Content-Type", "application/json");
            if is_stream_for_timeout {
                b = b.timeout(std::time::Duration::from_secs(86400));
            } else {
                b = b.timeout(std::time::Duration::from_secs(7200));
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

    let session_outcome = crate::server::retry_session::run_upstream_session(
        factory,
        decrypted_keys,
        retry_config,
        route.target_format.clone(),
        ir_request.stream,
    )
    .await;

    use crate::server::retry_session::SessionOutcome;

    let (final_status, retry_count_for_log, last_error_for_log, body_or_stream): (
        reqwest::StatusCode,
        u32,
        Option<String>,
        EitherBody,
    ) = match session_outcome {
        SessionOutcome::CompletedBuffer {
            status,
            bytes,
            retry_count,
        } => (status, retry_count, None, EitherBody::Bytes(bytes)),
        SessionOutcome::StartedStreaming {
            status,
            buffered_bytes,
            remaining,
            retry_count,
        } => (
            status,
            retry_count,
            None,
            EitherBody::Stream {
                buffered: buffered_bytes,
                remaining,
            },
        ),
        SessionOutcome::Exhausted {
            last_status,
            last_error,
            retry_count,
            partial_buffer,
        } => {
            // If we have buffered partial content on a stream request, replay it as SSE
            // with an error trailer so the client can still consume partial results.
            if let Some(ref buffered) = partial_buffer {
                if ir_request.stream {
                    let trailer = crate::server::retry_invisible::error_trailer_event(
                        client_format.clone(),
                        &format!("upstream buffer cap exceeded after {} retries", retry_count),
                    );
                    let body_stream = futures::stream::iter(vec![
                        Ok::<_, std::io::Error>(bytes::Bytes::from(buffered.clone())),
                        Ok(bytes::Bytes::from(trailer)),
                    ]);
                    let body = axum::body::Body::from_stream(body_stream);
                    let mut response = axum::response::Response::new(body);
                    *response.status_mut() = axum::http::StatusCode::OK;
                    response.headers_mut().insert(
                        axum::http::header::CONTENT_TYPE,
                        axum::http::header::HeaderValue::from_static("text/event-stream"),
                    );
                    let _ = log_request_entry(
                        &request_id,
                        &client_format,
                        &route.provider_name,
                        &route.target_format,
                        &client_model,
                        &target_model,
                        ir_request.stream,
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
            if let Err(le) = log_request_entry(
                &request_id,
                &client_format,
                &route.provider_name,
                &route.target_format,
                &client_model,
                &target_model,
                ir_request.stream,
                status.as_u16(),
                start.elapsed().as_millis() as i64,
                Some(&err_msg),
                0,
                0,
                0,
                None,
                None,
                None,
                retry_count as i64,
                Some(&last_error),
                client_user_agent.as_deref(),
            )
            .await
            {
                tracing::error!("Upstream exhausted logging failed: {}", le);
            }
            let error_body = serde_json::json!({
                "error": { "message": err_msg, "type": "upstream_error", "code": status.as_u16() }
            });
            let mut response = axum::Json(error_body).into_response();
            *response.status_mut() = status;
            return response;
        }
        SessionOutcome::Fatal { status, body } => {
            if let Err(le) = log_request_entry(
                &request_id,
                &client_format,
                &route.provider_name,
                &route.target_format,
                &client_model,
                &target_model,
                ir_request.stream,
                status.as_u16(),
                start.elapsed().as_millis() as i64,
                Some(&body),
                0,
                0,
                0,
                None,
                None,
                None,
                0,
                None,
                client_user_agent.as_deref(),
            )
            .await
            {
                tracing::error!("Upstream fatal logging failed: {}", le);
            }
            let mut response = axum::Json(serde_json::json!({
                "error": { "message": body, "type": "upstream_error", "code": status.as_u16() }
            }))
            .into_response();
            *response.status_mut() = status;
            return response;
        }
    };

    let status = final_status;
    let is_stream = ir_request.stream;

    // Defensive: SessionOutcome normally pre-returns on non-success / fatal /
    // exhausted. If we somehow still have a non-success status, surface an error.
    if !status.is_success() {
        let status_code = status.as_u16();
        let resp_body = match &body_or_stream {
            EitherBody::Bytes(b) => b.clone(),
            EitherBody::Stream { .. } => Vec::new(),
        };
        let body_text = String::from_utf8_lossy(&resp_body).into_owned();
        tracing::error!(
            "[ERR] upstream status={} model={}",
            status_code,
            target_model
        );
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
            &client_model,
            &target_model,
            ir_request.stream,
            status_code,
            start.elapsed().as_millis() as i64,
            Some(&err_msg),
            0,
            0,
            0,
            None,
            None,
            None,
            0,
            None,
            client_user_agent.as_deref(),
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
        // Non-streaming: SessionOutcome::CompletedBuffer is the only path that
        // reaches here for non-streaming requests. Extract bytes from EitherBody.
        let resp_body = match body_or_stream {
            EitherBody::Bytes(b) => b,
            EitherBody::Stream { .. } => {
                return ProxyError::Parse(
                    "internal: stream body returned for non-streaming request".into(),
                )
                .into_response();
            }
        };

        if resp_body.is_empty() {
            tracing::error!("Upstream returned empty body with status {}", status);
            return ProxyError::Parse("upstream returned empty response body".into())
                .into_response();
        }

        // Handle upstream returning SSE despite stream:false (e.g. provider bugs)
        let resp_body_str = String::from_utf8_lossy(&resp_body);
        let resp_value: Value = if resp_body_str.starts_with("data:")
            || resp_body_str.starts_with("event:")
        {
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
                    return ProxyError::Parse(format!("invalid response JSON: {}", e))
                        .into_response();
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
        let final_usage_json = serde_json::to_string(&ir_response.usage).ok();
        let upstream_usage_events_json = ir_response.usage.raw.as_ref().and_then(|raw| {
            serde_json::to_string(&serde_json::Value::Array(vec![raw.clone()])).ok()
        });

        // Cache reasoning_content for multi-turn (non-streaming path)
        if let Some(ref resp_id) = ir_response.id {
            let reasoning: String = ir_response
                .message
                .content
                .iter()
                .filter_map(|p| match p {
                    IrContentPart::Thinking { text, .. } => Some(text.as_str()),
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
            &client_model,
            &target_model,
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
            retry_count_for_log as i64,
            last_error_for_log.as_deref(),
            client_user_agent.as_deref(),
        )
        .await
        {
            tracing::error!("Non-stream logging failed: {}", e);
        }

        info!(
            "[DONE] {} status=200 duration={}ms tokens={}/{}",
            target_model,
            start.elapsed().as_millis(),
            prompt_tokens,
            completion_tokens
        );

        let mut response = axum::Json(client_response).into_response();
        *response.status_mut() = status;
        response
    } else {
        let target_parser = get_parser(&route.target_format);
        let client_generator = get_generator(&client_format);

        // Build a unified byte stream from EitherBody.
        // - StartedStreaming (pre_first_token mode): forward buffered bytes first,
        //   then continue with the remaining upstream stream.
        // - CompletedBuffer (full_buffer mode): replay the whole buffer as a single chunk.
        let (buffered_bytes, remaining_stream) = match body_or_stream {
            EitherBody::Bytes(b) => {
                // full_buffer mode: replay the buffered bytes as a single chunk stream
                let s =
                    futures::stream::once(
                        async move { Ok::<_, std::io::Error>(bytes::Bytes::from(b)) },
                    );
                (Vec::new(), s.boxed())
            }
            EitherBody::Stream {
                buffered,
                remaining,
            } => {
                let remaining_mapped = remaining
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
                (buffered, remaining_mapped.boxed())
            }
        };

        let stream: futures::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>> =
            if buffered_bytes.is_empty() {
                remaining_stream
            } else {
                let first = bytes::Bytes::from(buffered_bytes);
                futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
                    .chain(remaining_stream)
                    .boxed()
            };

        let response_id = uuid::Uuid::new_v4().to_string();
        let model_name = ir_request.model.clone();
        let client_format_clone = client_format.clone();

        let stream_state = Arc::new(StreamLogState {
            request_id: request_id.clone(),
            client_format: client_format.clone(),
            provider_name: route.provider_name.clone(),
            provider_format: route.target_format.clone(),
            model: client_model.clone(),
            target_model: target_model.clone(),
            start: start.clone(),
            prompt_tokens: AtomicU32::new(0),
            completion_tokens: AtomicU32::new(0),
            cached_tokens: AtomicU32::new(0),
            ttft_ms: Mutex::new(None),
            usage_events: Mutex::new(Vec::new()),
            final_usage: Mutex::new(None),
            logged: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
            upstream_retry_count: retry_count_for_log as i64,
            upstream_last_error: last_error_for_log.clone(),
            client_user_agent: client_user_agent.clone(),
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

            // Responses output-item lifecycle is driven by the state machine below;
            // sequence_number stamping + lifecycle events live there.
            let is_responses = matches!(client_format, ClientFormat::Responses);
            let mut responses_sm = if is_responses {
                Some(ResponsesStreamStateMachine::new(
                    response_id.to_string(),
                    model_name.clone(),
                ))
            } else {
                None
            };

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
                        // NOTE: cached_tokens is tied to prompt_tokens so both always come
                        // from the same event, preventing cross-event mismatch.
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

                    // Handle Responses API output-item lifecycle via the state machine
                    // (sequence_number stamping + created/added/done/completed events).
                    if let Some(sm) = responses_sm.as_mut() {
                        for sse in sm.process_chunk(&ir_chunk, (total_prompt, total_completion)) {
                            yield Ok::<_, std::convert::Infallible>(Bytes::from(sse));
                        }
                        if sm.is_finished() {
                            // Store accumulated reasoning in cache for multi-turn reuse.
                            let reasoning = sm.accumulated_reasoning().to_string();
                            if !reasoning.is_empty() {
                                if let Ok(mut cache) = REASONING_CACHE.lock() {
                                    cache.insert(response_id.to_string(), reasoning);
                                    // Evict old entries (keep last 50)
                                    if cache.len() > 50 {
                                        let keys: Vec<String> =
                                            cache.keys().take(cache.len() - 50).cloned().collect();
                                        for k in keys {
                                            cache.remove(&k);
                                        }
                                    }
                                }
                            }
                            finished = true;
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
                                        "prompt_tokens_details": {
                                            "cached_tokens": total_cached,
                                        }
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
                                    if usage.prompt_tokens > 0 { total_prompt = usage.prompt_tokens; total_cached = usage.cached_tokens; }
                                    if usage.completion_tokens > 0 { total_completion = usage.completion_tokens; }
                                }
                                if let Some(sm) = responses_sm.as_mut() {
                                    // Delegate any remaining Responses lifecycle event
                                    // (failed / completed / trailing content) to the state machine.
                                    for sse in sm.process_chunk(&ir_chunk, (total_prompt, total_completion)) {
                                        yield Ok::<_, std::convert::Infallible>(Bytes::from(sse));
                                    }
                                    if sm.is_finished() {
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
                                                        "prompt_tokens_details": {
                                                            "cached_tokens": total_cached,
                                                        }
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
            if let Some(sm) = responses_sm.as_mut() {
                if sm.is_started() && !sm.is_finished() {
                    // Kimi 等非标准端点可能不发送 message_stop/message_delta，
                    // 但已经发送了实际内容。如果有内容，视为正常完成。
                    if sm.had_content() {
                        tracing::info!(
                            "Stream completed without proper termination, synthesizing response.completed from accumulated content"
                        );
                    }
                    for sse in sm.finish_on_stream_end(sm.had_content(), (total_prompt, total_completion)) {
                        yield Ok::<_, std::convert::Infallible>(Bytes::from(sse));
                    }
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

            // Snapshot final usage and raw events for diagnostics storage.
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

            if let Err(e) = log_request_entry(
                &request_id,
                &client_format_clone,
                &route.provider_name,
                &route.target_format,
                &client_model,
                &target_model,
                true,
                200,
                elapsed,
                None,
                pt,
                ct,
                cache_t,
                ttft_ms,
                final_usage_json.as_deref(),
                upstream_usage_events_json.as_deref(),
                retry_count_for_log as i64,
                last_error_for_log.as_deref(),
                stream_state_ref.client_user_agent.as_deref(),
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
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        match format {
            ClientFormat::Anthropic => {
                if json.get("type").and_then(|v| v.as_str()) == Some("content_block_delta") {
                    if let Some(t) = json.pointer("/delta/text").and_then(|v| v.as_str()) {
                        parts.push(t.to_string());
                    }
                }
            }
            ClientFormat::Completions | ClientFormat::Responses => {
                if let Some(c) = json
                    .pointer("/choices/0/delta/content")
                    .and_then(|v| v.as_str())
                {
                    parts.push(c.to_string());
                }
            }
            ClientFormat::Gemini => {
                if let Some(t) = json
                    .pointer("/candidates/0/content/parts/0/text")
                    .and_then(|v| v.as_str())
                {
                    parts.push(t.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(""))
    }
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
        None => {
            ProxyError::ModelNotFound(format!("model '{}' not found", model_name)).into_response()
        }
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
         WHERE pm.enabled = 1 AND p.enabled = 1 \
         ORDER BY pm.model_name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ProxyError::Database(e))?;

    Ok(rows
        .into_iter()
        .map(
            |(model_name, provider_name, target_model, format)| ModelRouteInfo {
                model_name,
                provider_name,
                target_model,
                format,
            },
        )
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

/// Read a single setting value from the settings table (returns None if missing).
async fn get_setting(key: &str) -> Option<String> {
    let pool = crate::db::get_pool().await;
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
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
    log_request(
        request_id,
        &format!("{:?}", client_format).to_lowercase(),
        provider_name,
        &format!("{:?}", provider_format).to_lowercase(),
        model,
        target_model,
        stream,
        status_code,
        duration_ms,
        error_message,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        ttft_ms,
        final_usage_json,
        upstream_usage_events_json,
        upstream_retry_count,
        upstream_last_error,
        client_user_agent,
    )
    .await
}

struct StreamLogState {
    request_id: String,
    client_format: ClientFormat,
    provider_name: String,
    provider_format: ClientFormat,
    model: String,
    target_model: String,
    start: std::time::Instant,
    prompt_tokens: AtomicU32,
    completion_tokens: AtomicU32,
    cached_tokens: AtomicU32,
    ttft_ms: Mutex<Option<i64>>,
    /// Raw upstream usage events captured during streaming (in arrival order).
    usage_events: Mutex<Vec<serde_json::Value>>,
    /// Final accumulated upstream usage snapshot (set at stream end).
    final_usage: Mutex<Option<serde_json::Value>>,
    logged: AtomicBool,
    interrupted: AtomicBool,
    upstream_retry_count: i64,
    upstream_last_error: Option<String>,
    /// Downstream (client) User-Agent, captured at request entry for logging.
    client_user_agent: Option<String>,
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
            let events_vec = state.usage_events.lock().unwrap().clone();
            let final_usage = serde_json::json!({
                "prompt_tokens": pt,
                "completion_tokens": ct,
                "cached_tokens": cache_t,
            });
            let final_usage_json = serde_json::to_string(&final_usage).ok();
            let upstream_usage_events_json = if events_vec.is_empty() {
                None
            } else {
                serde_json::to_string(&serde_json::Value::Array(events_vec)).ok()
            };

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
                &state.target_model,
                true,
                status_code,
                elapsed,
                error_msg.as_deref(),
                pt,
                ct,
                cache_t,
                ttft,
                final_usage_json.as_deref(),
                upstream_usage_events_json.as_deref(),
                state.upstream_retry_count,
                state.upstream_last_error.as_deref(),
                state.client_user_agent.as_deref(),
            )
            .await
            {
                tracing::error!("Stream guard logging failed: {}", e);
            }

            if interrupted {
                tracing::warn!(
                    "[INTERRUPTED] {} duration={}ms tokens={}/{} - stream was interrupted",
                    state.model,
                    elapsed,
                    pt,
                    ct
                );
            }
        });
    }
}

/// Split `<thinking>...</thinking>` from text.
/// Returns (thinking_content, remaining_text).
/// Emit reasoning summary done event when thinking ends.
/// Responses API streaming state machine.
///
/// Encapsulates the conversion of incoming `IrStreamChunk`s into the OpenAI
/// Responses SSE lifecycle (response.created → output_item.added →
/// content_part.added → ...delta... → output_text.done → output_item.done →
/// response.completed), including reasoning-summary and function-call items.
///
/// Every emitted event carries a strictly-increasing `sequence_number`
/// (required by the Responses streaming protocol; clients like codex rely on
/// it to order/dedupe events). Extracted from the inline `async_stream` body
/// so the lifecycle can be unit-tested in isolation.
pub struct ResponsesStreamStateMachine {
    response_id: String,
    model: String,
    /// Unique message item id derived from response_id, so multiple responses
    /// in the same turn never collide on a fixed id like "msg_proxy".
    message_id: String,
    /// Unique reasoning item id derived from response_id.
    reasoning_id: String,
    seq: u64,
    started: bool,
    finished: bool,
    // Responses output item state
    output_index: u32,
    message_open: bool,
    text_part_open: bool,
    func_open: bool,
    call_id: String,
    func_name: String,
    accumulated_args: String,
    accumulated_text: String,
    thinking_started: bool,
    /// whether a reasoning output item has been added but not yet done
    reasoning_item_open: bool,
    /// pure reasoning without tags, for cache
    accumulated_reasoning: String,
}

impl ResponsesStreamStateMachine {
    pub fn new(response_id: String, model: String) -> Self {
        let message_id = format!("msg_{}", &response_id);
        let reasoning_id = format!("rs_{}", &response_id);
        Self {
            response_id,
            model,
            message_id,
            reasoning_id,
            seq: 0,
            started: false,
            finished: false,
            output_index: 0,
            message_open: false,
            text_part_open: false,
            func_open: false,
            call_id: String::new(),
            func_name: String::new(),
            accumulated_args: String::new(),
            accumulated_text: String::new(),
            thinking_started: false,
            reasoning_item_open: false,
            accumulated_reasoning: String::new(),
        }
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Whether any content (text / tool args / reasoning) was accumulated.
    pub fn had_content(&self) -> bool {
        !self.accumulated_text.is_empty()
            || !self.accumulated_args.is_empty()
            || self.thinking_started
    }

    /// Accumulated reasoning text (without tags), for the reasoning cache.
    pub fn accumulated_reasoning(&self) -> &str {
        &self.accumulated_reasoning
    }

    /// Build an SSE `data: {...}\n\n` line, stamping a strictly-increasing
    /// `sequence_number` onto the event.
    fn ev(&mut self, mut v: serde_json::Value) -> String {
        v["sequence_number"] = serde_json::json!(self.seq);
        self.seq += 1;
        format!("data: {}\n\n", v)
    }

    fn ensure_created(&mut self, out: &mut Vec<String>) {
        if !self.started {
            out.push(self.ev(serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "in_progress",
                    "model": self.model,
                    "output": [],
                }
            })));
            self.started = true;
        }
    }

    /// Close an open reasoning item: emit `reasoning_summary_part.done` then
    /// `output_item.done` for the reasoning item. Returns empty vec if no
    /// reasoning item was open. Caller advances output_index after these events.
    fn close_thinking(&mut self) -> Vec<String> {
        if !self.thinking_started {
            return Vec::new();
        }
        self.thinking_started = false;
        // The reasoning item lives at the current output_index (its `added`
        // event does not advance output_index; the caller advances it after
        // close_thinking, same convention as the original design).
        let idx = self.output_index;
        let mut out = Vec::new();
        out.push(self.ev(serde_json::json!({
            "type": "response.reasoning_summary_part.done",
            "output_index": idx,
            "content_index": 0,
            "item_id": self.reasoning_id,
            "part": {
                "type": "summary_text",
                "text": self.accumulated_reasoning.clone(),
            },
            "response_id": self.response_id,
        })));
        // Close the reasoning output item itself.
        if self.reasoning_item_open {
            out.push(self.ev(serde_json::json!({
                "type": "response.output_item.done",
                "output_index": idx,
                "item": {
                    "type": "reasoning",
                    "id": self.reasoning_id,
                    "summary": [{"type": "summary_text", "text": self.accumulated_reasoning.clone()}],
                    "status": "completed",
                }
            })));
            self.reasoning_item_open = false;
        }
        out
    }

    /// Process one IR chunk into 0..N SSE events.
    ///
    /// Mirrors the previous inline `is_responses` branch in `handle_proxy`.
    /// `usage_totals` is stamped onto the terminal `response.completed` event.
    pub fn process_chunk(
        &mut self,
        chunk: &IrStreamChunk,
        usage_totals: (u32, u32),
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        // --- Tool call start (id + name) ---
        if let Some(tool_calls) = &chunk.delta_tool_calls {
            if let Some(tc) = tool_calls.first() {
                if tc.id.is_some() && tc.name.is_some() {
                    self.ensure_created(&mut out);

                    // Close reasoning summary before starting a tool call.
                    let thinking_done = self.close_thinking();
                    if !thinking_done.is_empty() {
                        out.extend(thinking_done);
                        self.output_index += 1;
                    }

                    // Close text part + message if open
                    if self.text_part_open {
                        out.push(self.ev(serde_json::json!({
                            "type": "response.output_text.done",
                            "output_index": self.output_index - 1,
                            "content_index": 0,
                            "item_id": self.message_id,
                            "text": self.accumulated_text,
                        })));
                        self.text_part_open = false;
                    }
                    if self.message_open {
                        out.push(self.ev(serde_json::json!({
                            "type": "response.output_item.done",
                            "output_index": self.output_index - 1,
                            "item": {
                                "type": "message",
                                "id": self.message_id,
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": self.accumulated_text}],
                                "status": "completed",
                            }
                        })));
                        self.message_open = false;
                    }
                    // Close previous func_call if open
                    if self.func_open {
                        let done_args = self.ev(serde_json::json!({
                            "type": "response.function_call_arguments.done",
                            "output_index": self.output_index - 1,
                            "item_id": format!("fc_{}", self.call_id),
                            "call_id": self.call_id,
                            "arguments": self.accumulated_args,
                        }));
                        let item_done = self.ev(serde_json::json!({
                            "type": "response.output_item.done",
                            "output_index": self.output_index - 1,
                            "item": {
                                "type": "function_call",
                                "id": format!("fc_{}", self.call_id),
                                "call_id": self.call_id,
                                "name": self.func_name,
                                "arguments": self.accumulated_args,
                            }
                        }));
                        out.push(done_args);
                        out.push(item_done);
                    }

                    self.call_id = tc.id.as_deref().unwrap_or("").to_string();
                    self.func_name = tc.name.as_deref().unwrap_or("").to_string();
                    self.accumulated_args.clear();

                    out.push(self.ev(serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": self.output_index,
                        "item": {
                            "type": "function_call",
                            "id": format!("fc_{}", self.call_id),
                            "call_id": self.call_id,
                            "name": self.func_name,
                            "arguments": "",
                        }
                    })));
                    self.func_open = true;
                    self.output_index += 1;
                    return out;
                }
                // Argument delta
                if let Some(args) = &tc.arguments {
                    if !args.is_empty() {
                        self.accumulated_args.push_str(args);
                        out.push(self.ev(serde_json::json!({
                            "type": "response.function_call_arguments.delta",
                            "output_index": self.output_index - 1,
                            "item_id": format!("fc_{}", self.call_id),
                            "call_id": self.call_id,
                            "delta": args,
                        })));
                    }
                    return out;
                }
            }
        }

        // --- Thinking / reasoning_content → reasoning summary events ---
        if let Some(thinking) = &chunk.delta_thinking {
            if !thinking.is_empty() && !self.func_open {
                if !self.message_open && !self.thinking_started {
                    self.ensure_created(&mut out);
                }
                if !self.thinking_started {
                    // Open the reasoning output item first.
                    if !self.reasoning_item_open {
                        out.push(self.ev(serde_json::json!({
                            "type": "response.output_item.added",
                            "output_index": self.output_index,
                            "item": {
                                "type": "reasoning",
                                "id": self.reasoning_id,
                                "summary": [],
                                "status": "in_progress",
                            }
                        })));
                        self.reasoning_item_open = true;
                    }
                    out.push(self.ev(serde_json::json!({
                        "type": "response.reasoning_summary_part.added",
                        "output_index": self.output_index,
                        "content_index": 0,
                        "item_id": self.reasoning_id,
                        "part": {"type": "summary_text", "text": ""},
                        "response_id": self.response_id,
                    })));
                    self.thinking_started = true;
                }
                self.accumulated_reasoning.push_str(thinking);
                out.push(self.ev(serde_json::json!({
                    "type": "response.reasoning_summary_text.delta",
                    "output_index": self.output_index,
                    "content_index": 0,
                    "item_id": self.reasoning_id,
                    "delta": thinking,
                    "response_id": self.response_id,
                })));
            }
            return out;
        }

        // --- Text content ---
        if let Some(content) = &chunk.delta_content {
            if !content.is_empty() && !self.func_open {
                let thinking_done = self.close_thinking();
                if !thinking_done.is_empty() {
                    out.extend(thinking_done);
                    self.output_index += 1;
                }

                if !self.message_open {
                    self.ensure_created(&mut out);
                    out.push(self.ev(serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": self.output_index,
                        "item": {
                            "type": "message",
                            "id": self.message_id,
                            "role": "assistant",
                            "content": [],
                            "status": "in_progress",
                        }
                    })));
                    out.push(self.ev(serde_json::json!({
                        "type": "response.content_part.added",
                        "output_index": self.output_index,
                        "content_index": 0,
                        "item_id": self.message_id,
                        "part": {"type": "output_text", "text": ""},
                    })));
                    self.message_open = true;
                    self.text_part_open = true;
                    self.output_index += 1;
                }
                self.accumulated_text.push_str(content);
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_text.delta",
                    "output_index": self.output_index - 1,
                    "content_index": 0,
                    "item_id": self.message_id,
                    "delta": content,
                    "response_id": self.response_id,
                })));
            }
            return out;
        }

        // --- Upstream failure ---
        if chunk.finish_reason.as_deref() == Some("failed") {
            if self.text_part_open {
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_text.done",
                    "output_index": self.output_index - 1,
                    "content_index": 0,
                    "item_id": self.message_id,
                    "text": self.accumulated_text,
                })));
                self.text_part_open = false;
            }
            if self.message_open {
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": self.output_index - 1,
                    "item": {
                        "type": "message",
                        "id": self.message_id,
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": self.accumulated_text}],
                        "status": "incomplete",
                    }
                })));
                self.message_open = false;
            }
            if self.func_open {
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": self.output_index - 1,
                    "item": {
                        "type": "function_call",
                        "id": format!("fc_{}", self.call_id),
                        "call_id": self.call_id,
                        "name": self.func_name,
                        "arguments": self.accumulated_args,
                        "status": "incomplete",
                    }
                })));
                self.func_open = false;
            }

            let err_code = chunk
                .error
                .as_ref()
                .and_then(|e| e.code.clone())
                .unwrap_or_else(|| "server_error".to_string());
            let err_message = chunk
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "upstream response failed".to_string());

            out.push(self.ev(serde_json::json!({
                "type": "response.failed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "failed",
                    "model": self.model,
                    "output": build_responses_output_array(
                        &self.message_id,
                        &self.reasoning_id,
                        &self.accumulated_text,
                        &self.accumulated_reasoning,
                        self.thinking_started,
                        self.func_open,
                        &self.call_id,
                        &self.func_name,
                        &self.accumulated_args,
                    ),
                },
                "error": {
                    "code": err_code,
                    "message": err_message,
                }
            })));
            self.finished = true;
            return out;
        }

        // --- Normal completion ---
        if chunk.finish_reason.is_some() {
            // Close reasoning summary if still open
            let thinking_done = self.close_thinking();
            if !thinking_done.is_empty() {
                out.extend(thinking_done);
                self.output_index += 1;
            }

            // Close func_call if open
            if self.func_open {
                out.push(self.ev(serde_json::json!({
                    "type": "response.function_call_arguments.done",
                    "output_index": self.output_index - 1,
                    "item_id": format!("fc_{}", self.call_id),
                    "call_id": self.call_id,
                    "arguments": self.accumulated_args,
                })));
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": self.output_index - 1,
                    "item": {
                        "type": "function_call",
                        "id": format!("fc_{}", self.call_id),
                        "call_id": self.call_id,
                        "name": self.func_name,
                        "arguments": self.accumulated_args,
                    }
                })));
                self.func_open = false;
            }
            // Close message if open
            if self.message_open {
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_text.done",
                    "output_index": self.output_index - 1,
                    "content_index": 0,
                    "item_id": self.message_id,
                    "text": self.accumulated_text,
                })));
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": self.output_index - 1,
                    "item": {
                        "type": "message",
                        "id": self.message_id,
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": self.accumulated_text}],
                        "status": "completed",
                    }
                })));
                self.message_open = false;
            }

            out.push(self.ev(serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "completed",
                    "model": self.model,
                    "output": build_responses_output_array(
                        &self.message_id,
                        &self.reasoning_id,
                        &self.accumulated_text,
                        &self.accumulated_reasoning,
                        self.thinking_started,
                        self.func_open,
                        &self.call_id,
                        &self.func_name,
                        &self.accumulated_args,
                    ),
                    "usage": {
                        "input_tokens": usage_totals.0,
                        "output_tokens": usage_totals.1,
                        "total_tokens": usage_totals.0 + usage_totals.1,
                    }
                }
            })));
            self.finished = true;
            return out;
        }

        out
    }

    /// Synthesize close/completed events when the upstream stream ended
    /// without a proper termination event (or with one left in the buffer).
    ///
    /// `had_content` controls the final message status (completed vs incomplete)
    /// when no proper finish event was observed.
    pub fn finish_on_stream_end(
        &mut self,
        had_content: bool,
        usage_totals: (u32, u32),
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        if !self.started {
            return out;
        }
        if self.finished {
            return out;
        }

        // Close open items first
        if self.func_open {
            out.push(self.ev(serde_json::json!({
                "type": "response.output_item.done",
                "output_index": self.output_index - 1,
                "item": {
                    "type": "function_call",
                    "id": format!("fc_{}", self.call_id),
                    "call_id": self.call_id,
                    "name": self.func_name,
                    "arguments": self.accumulated_args,
                }
            })));
            self.func_open = false;
        }
        if self.message_open {
            if self.text_part_open {
                out.push(self.ev(serde_json::json!({
                    "type": "response.output_text.done",
                    "output_index": self.output_index - 1,
                    "content_index": 0,
                    "item_id": self.message_id,
                    "text": self.accumulated_text,
                })));
                self.text_part_open = false;
            }
            let status = if had_content {
                "completed"
            } else {
                "incomplete"
            };
            out.push(self.ev(serde_json::json!({
                "type": "response.output_item.done",
                "output_index": self.output_index - 1,
                "item": {
                    "type": "message",
                    "id": self.message_id,
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": self.accumulated_text}],
                    "status": status,
                }
            })));
            self.message_open = false;
        }

        if had_content {
            out.push(self.ev(serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "completed",
                    "model": self.model,
                    "output": build_responses_output_array(
                        &self.message_id,
                        &self.reasoning_id,
                        &self.accumulated_text,
                        &self.accumulated_reasoning,
                        self.thinking_started,
                        self.func_open,
                        &self.call_id,
                        &self.func_name,
                        &self.accumulated_args,
                    ),
                    "usage": {
                        "input_tokens": usage_totals.0,
                        "output_tokens": usage_totals.1,
                        "total_tokens": usage_totals.0 + usage_totals.1,
                    }
                }
            })));
        } else {
            out.push(self.ev(serde_json::json!({
                "type": "response.failed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "failed",
                    "model": self.model,
                    "output": build_responses_output_array(
                        &self.message_id,
                        &self.reasoning_id,
                        &self.accumulated_text,
                        &self.accumulated_reasoning,
                        self.thinking_started,
                        self.func_open,
                        &self.call_id,
                        &self.func_name,
                        &self.accumulated_args,
                    ),
                },
                "error": {
                    "code": "server_error",
                    "message": "stream disconnected before completion",
                }
            })));
        }
        self.finished = true;
        out
    }
}

fn build_responses_output_array(
    message_id: &str,
    reasoning_id: &str,
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
            "id": reasoning_id,
            "summary": [{"type": "summary_text", "text": accumulated_reasoning}],
        }));
    }

    if !accumulated_text.is_empty() {
        output.push(serde_json::json!({
            "type": "message",
            "id": message_id,
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
            && !m
                .content
                .iter()
                .any(|p| matches!(p, IrContentPart::Thinking { .. }))
    });

    let Some(idx) = last_assistant_idx else {
        return;
    };
    let msg = &mut messages[idx];

    // Prefer exact match by previous_response_id
    if let Some(reasoning) = previous_response_id.and_then(|id| cache.get(id)) {
        msg.content.insert(
            0,
            IrContentPart::Thinking {
                text: reasoning.clone(),
                signature: None,
            },
        );
        return;
    }

    // Try extracting <thinking> tags from text content
    let text_content: String = msg
        .content
        .iter()
        .filter_map(|p| match p {
            IrContentPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let (thinking_opt, remaining) = split_thinking_tags(&text_content);
    if let Some(thinking) = thinking_opt {
        msg.content.clear();
        msg.content.push(IrContentPart::Thinking {
            text: thinking,
            signature: None,
        });
        let trimmed = remaining.trim();
        if !trimmed.is_empty() {
            msg.content.push(IrContentPart::Text {
                text: trimmed.to_string(),
                citations: None,
            });
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
    (
        if thinking.is_empty() {
            None
        } else {
            Some(thinking)
        },
        remaining,
    )
}

/// Either a fully buffered byte response (non-streaming or full_buffer mode) or
/// a streaming body that has already buffered some initial bytes before being
/// handed off to the SSE forwarding loop.
enum EitherBody {
    Bytes(Vec<u8>),
    Stream {
        buffered: Vec<u8>,
        remaining: futures::stream::BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
    },
}

#[cfg(test)]
mod tests {
    use super::inject_cached_reasoning_into_assistant_messages;
    use crate::converter::ir::{IrContentPart, IrMessage, IrRole};
    use std::collections::HashMap;

    #[test]
    fn injects_cached_reasoning_when_previous_response_id_missing() {
        let mut messages = vec![IrMessage {
            role: IrRole::Assistant,
            content: vec![IrContentPart::Text {
                text: "最终答案".to_string(),
                citations: None,
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
            IrContentPart::Text { text, .. } => assert_eq!(text, "最终答案"),
            other => panic!("expected text content, got {:?}", other),
        }

        // With matching previous_response_id → inject into last assistant message
        inject_cached_reasoning_into_assistant_messages(&mut messages, Some("resp_1"), &cache);
        assert_eq!(messages[0].content.len(), 2);
        match &messages[0].content[0] {
            IrContentPart::Thinking { text, .. } => assert_eq!(text, "已缓存推理"),
            other => panic!("expected thinking content, got {:?}", other),
        }
    }
}
