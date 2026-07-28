use crate::converter::ir::{round2, IrContentPart, IrRequest, IrResponse, IrRole, IrStreamChunk};
use crate::converter::FormatGenerator;
use crate::error::ProxyError;
use serde_json::{json, Value};

pub struct CompletionsGenerator;

impl FormatGenerator for CompletionsGenerator {
    fn generate_request(&self, ir: &IrRequest) -> Result<Value, ProxyError> {
        // Collect all tool_call_ids from tool messages to verify completeness
        let responded_ids: std::collections::HashSet<String> = ir
            .messages
            .iter()
            .filter(|m| m.role == IrRole::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();

        let mut messages = Vec::new();

        for msg in &ir.messages {
            let mut message = json!({
                "role": match msg.role {
                    IrRole::System => "system",
                    IrRole::Developer => "system",
                    IrRole::User => "user",
                    IrRole::Assistant => "assistant",
                    IrRole::Tool => "tool",
                },
            });

            if let Some(name) = &msg.name {
                if msg.role != IrRole::Tool {
                    message["name"] = json!(name);
                }
            }

            if let Some(tool_call_id) = &msg.tool_call_id {
                message["tool_call_id"] = json!(tool_call_id);
            }

            // For assistant messages, separate Thinking content for reasoning_content
            let content_parts: Vec<IrContentPart> = if msg.role == IrRole::Assistant {
                let thinking_text: String = msg
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        IrContentPart::Thinking { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                if !thinking_text.is_empty() {
                    message["reasoning_content"] = json!(thinking_text);
                }

                msg.content
                    .iter()
                    .filter(|p| !matches!(p, IrContentPart::Thinking { .. }))
                    .cloned()
                    .collect()
            } else {
                msg.content.clone()
            };

            if content_parts.len() == 1 {
                if let Some(IrContentPart::Text { text, .. }) = content_parts.first() {
                    message["content"] = json!(text);
                } else {
                    message["content"] = json!(serialize_content_parts(&content_parts));
                }
            } else if content_parts.is_empty() {
                message["content"] = json!(null);
            } else {
                message["content"] = json!(serialize_content_parts(&content_parts));
            }

            if let Some(tool_calls) = &msg.tool_calls {
                // Only include tool_calls that have matching tool responses
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .enumerate()
                    .filter(|(_, tc)| responded_ids.contains(&tc.id))
                    .map(|(i, tc)| {
                        json!({
                            "index": i,
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }
                        })
                    })
                    .collect();
                if !calls.is_empty() {
                    message["tool_calls"] = json!(calls);
                }
            }

            messages.push(message);
        }

        let mut body = json!({
            "model": ir.model,
            "messages": messages,
            "stream": ir.stream,
        });

        if let Some(tools) = &ir.tools {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    let mut func = json!({
                        "name": t.name,
                        "parameters": t.input_schema,
                    });
                    if let Some(desc) = &t.description {
                        func["description"] = json!(desc);
                    }
                    if let Some(strict) = t.strict {
                        func["strict"] = json!(strict);
                    }
                    json!({
                        "type": "function",
                        "function": func,
                    })
                })
                .collect();
            body["tools"] = json!(tool_defs);
        }

        if let Some(tool_choice) = &ir.tool_choice {
            let converted = match tool_choice {
                Value::String(s) => json!(s),
                Value::Object(map) => {
                    if let Some(t) = map.get("type").and_then(|v| v.as_str()) {
                        match t {
                            "auto" => json!("auto"),
                            "required" => json!("required"),
                            "none" => json!("none"),
                            "function" => {
                                if let Some(name) = map
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                {
                                    json!({ "type": "function", "function": { "name": name } })
                                } else {
                                    json!("auto")
                                }
                            }
                            _ => json!("auto"),
                        }
                    } else {
                        tool_choice.clone()
                    }
                }
                _ => json!("auto"),
            };
            body["tool_choice"] = converted;
        }

        if let Some(temperature) = ir.temperature {
            body["temperature"] = json!(round2(temperature));
        }

        if let Some(top_p) = ir.top_p {
            body["top_p"] = json!(round2(top_p));
        }

        if let Some(max_tokens) = ir.max_tokens {
            if ir.thinking.as_ref().map_or(false, |t| {
                t.mode == crate::converter::ir::ThinkingMode::Enabled
            }) {
                body["max_completion_tokens"] = json!(max_tokens);
            } else {
                body["max_tokens"] = json!(max_tokens);
            }
        }

        if let Some(stop) = &ir.stop_sequences {
            body["stop"] = json!(stop);
        }

        if let Some(response_format) = &ir.response_format {
            if let Some(normalized) = normalize_completions_response_format(response_format) {
                body["response_format"] = normalized;
            }
        }

        if let Some(pp) = ir.presence_penalty {
            body["presence_penalty"] = json!(pp);
        }

        if let Some(fp) = ir.frequency_penalty {
            body["frequency_penalty"] = json!(fp);
        }

        if let Some(seed) = ir.seed {
            body["seed"] = json!(seed);
        }

        if let Some(stream_options) = &ir.stream_options {
            body["stream_options"] = stream_options.clone();
        }

        // Skip reasoning/thinking for providers that don't support it

        // Pass through any extra parameters (e.g. chat_template_kwargs)
        if !ir.extra.is_empty() {
            tracing::debug!(
                "CompletionsGenerator injecting {} extra field(s) into upstream body",
                ir.extra.len()
            );
        }
        for (key, val) in &ir.extra {
            tracing::trace!("CompletionsGenerator extra: {} = {}", key, val);
            body[key] = val.clone();
        }

        Ok(body)
    }

    fn generate_stream_chunk(&self, chunk: &IrStreamChunk) -> String {
        let id = chunk.id.as_deref().unwrap_or("chatcmpl-proxy");

        let mut delta = json!({});

        if let Some(content) = &chunk.delta_content {
            delta["content"] = json!(content);
        }

        if let Some(thinking) = &chunk.delta_thinking {
            delta["reasoning_content"] = json!(thinking);
        }

        if let Some(tool_calls) = &chunk.delta_tool_calls {
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|tc| {
                    let mut call = json!({
                        "index": tc.index,
                    });
                    if let Some(id) = &tc.id {
                        call["id"] = json!(id);
                        call["type"] = json!("function");
                    }
                    if let Some(name) = &tc.name {
                        call["function"] = json!({});
                        call["function"]["name"] = json!(name);
                    }
                    if let Some(args) = &tc.arguments {
                        if call.get("function").is_none() {
                            call["function"] = json!({});
                        }
                        call["function"]["arguments"] = json!(args);
                    }
                    call
                })
                .collect();
            delta["tool_calls"] = json!(calls);
        }

        let normalized_finish = chunk
            .finish_reason
            .as_ref()
            .map(|reason| match reason.as_str() {
                "stop" | "end_turn" | "completed" => "stop",
                "tool_calls" | "tool_use" => "tool_calls",
                "length" | "max_tokens" => "length",
                "content_filter" => "content_filter",
                _ => "stop",
            });

        let mut choice = json!({
            "index": 0,
            "delta": delta,
        });

        if let Some(reason) = normalized_finish {
            choice["finish_reason"] = json!(reason);
        } else {
            choice["finish_reason"] = json!(null);
        }

        let mut chunk_data = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "choices": [choice],
        });

        if let Some(model) = &chunk.model {
            chunk_data["model"] = json!(model);
        }

        if let Some(usage) = &chunk.usage {
            let mut usage_json = json!({
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
            });
            if usage.cached_tokens > 0 {
                usage_json["prompt_tokens_details"] = json!({
                    "cached_tokens": usage.cached_tokens,
                });
            }
            chunk_data["usage"] = usage_json;
        }

        format!("data: {}\n\n", chunk_data)
    }

    fn generate_response(&self, ir: &IrResponse) -> Result<Value, ProxyError> {
        let id = ir.id.as_deref().unwrap_or("chatcmpl-proxy");

        let mut message = json!({
            "role": "assistant",
        });

        if ir.message.content.len() == 1 {
            if let Some(IrContentPart::Text { text, .. }) = ir.message.content.first() {
                message["content"] = json!(text);
            } else {
                message["content"] = json!(serialize_content_parts(&ir.message.content));
            }
        } else if ir.message.content.is_empty() {
            message["content"] = json!(null);
        } else {
            message["content"] = json!(serialize_content_parts(&ir.message.content));
        }

        if let Some(tool_calls) = &ir.message.tool_calls {
            let calls: Vec<Value> = tool_calls
                .iter()
                .enumerate()
                .map(|(i, tc)| {
                    json!({
                        "index": i,
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments,
                        }
                    })
                })
                .collect();
            message["tool_calls"] = json!(calls);
        }

        Ok(json!({
            "id": id,
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": ir.model.as_deref().unwrap_or(""),
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": match ir.finish_reason.as_deref() {
                    Some("stop") | Some("end_turn") | Some("completed") => "stop",
                    Some("tool_calls") | Some("tool_use") => "tool_calls",
                    Some("length") | Some("max_tokens") => "length",
                    Some("content_filter") => "content_filter",
                    Some(r) => r,
                    None => "stop",
                },
            }],
            "usage": {
                "prompt_tokens": ir.usage.prompt_tokens,
                "completion_tokens": ir.usage.completion_tokens,
                "total_tokens": ir.usage.total_tokens,
                "prompt_tokens_details": {
                    "cached_tokens": ir.usage.cached_tokens,
                }
            }
        }))
    }
}

fn normalize_completions_response_format(response_format: &Value) -> Option<Value> {
    let response_type = response_format.get("type")?.as_str()?;

    match response_type {
        "json_object" => Some(json!({ "type": "json_object" })),
        // DeepSeek chat/completions currently supports `json_object`,
        // but rejects `json_schema` with "This response_format type is unavailable now".
        // Keep the JSON intent by downgrading schema-based requests to json_object.
        "json_schema" => Some(json!({ "type": "json_object" })),
        _ => None,
    }
}

fn serialize_content_parts(parts: &[IrContentPart]) -> Vec<Value> {
    parts
        .iter()
        .map(|part| match part {
            IrContentPart::Text { text, .. } => json!({
                "type": "text",
                "text": text,
            }),
            IrContentPart::Thinking { text, .. } => json!({
                "type": "text",
                "text": text,
            }),
            IrContentPart::Image {
                url,
                data,
                media_type,
            } => {
                if let Some(image_url) = url {
                    json!({
                        "type": "image_url",
                        "image_url": { "url": image_url },
                    })
                } else if let Some(b64) = data {
                    let mt = media_type.as_deref().unwrap_or("image/png");
                    json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", mt, b64),
                        },
                    })
                } else {
                    json!({ "type": "image_url", "image_url": { "url": "" } })
                }
            }
            IrContentPart::Compaction { .. } => serde_json::Value::Null,
            IrContentPart::ToolUse { id, name, input } => json!({
                "type": "text",
                "text": format!("[Tool call: {} ({}) args={}]", name, id, input),
            }),
            IrContentPart::ToolResult {
                tool_use_id,
                content,
                ..
            } => json!({
                "type": "text",
                "text": format!("[Tool result for {}: {}]", tool_use_id, content),
            }),
        })
        .filter(|v| !v.is_null())
        .collect()
}
