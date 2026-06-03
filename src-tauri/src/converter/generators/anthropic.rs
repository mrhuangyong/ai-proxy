use crate::converter::ir::{IrContentPart, IrRequest, IrResponse, IrRole, IrStreamChunk};
use crate::converter::FormatGenerator;
use crate::error::ProxyError;
use serde_json::{json, Value};

pub struct AnthropicGenerator;

impl FormatGenerator for AnthropicGenerator {
    fn generate_request(&self, ir: &IrRequest) -> Result<Value, ProxyError> {
        let mut system_parts: Vec<String> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();

        for msg in &ir.messages {
            match msg.role {
                IrRole::System | IrRole::Developer => {
                    let text = extract_text_parts(&msg.content);
                    if !text.is_empty() {
                        system_parts.push(text);
                    }
                }
                _ => {
                    let role_str = match msg.role {
                        IrRole::User => "user",
                        IrRole::Assistant => "assistant",
                        IrRole::Tool => "user",
                        _ => "user",
                    };

                    // Filter out ToolUse from content — handled via msg.tool_calls below
                    let content_for_conv: Vec<IrContentPart> = if msg.role == IrRole::Assistant {
                        msg.content.iter()
                            .filter(|p| !matches!(p, IrContentPart::ToolUse { .. }))
                            .cloned()
                            .collect()
                    } else {
                        msg.content.clone()
                    };

                    let mut content = convert_anthropic_content(&content_for_conv, &msg.role);

                    // Anthropic format: tool_calls become tool_use content blocks
                    if msg.role == IrRole::Assistant {
                        if let Some(tool_calls) = &msg.tool_calls {
                            let mut content_arr = match content {
                                Value::Array(arr) => arr,
                                other => {
                                    if other.as_str().map_or(false, |s| !s.is_empty()) {
                                        vec![json!({"type": "text", "text": other})]
                                    } else {
                                        vec![]
                                    }
                                }
                            };
                            for tc in tool_calls {
                                let input: Value = serde_json::from_str(&tc.arguments).unwrap_or_else(|e| {
                                    tracing::warn!("Failed to parse tool_call arguments as JSON: {}", e);
                                    Value::Object(serde_json::Map::new())
                                });
                                content_arr.push(json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.name,
                                    "input": input,
                                }));
                            }
                            content = json!(content_arr);
                        }

                        // Ensure assistant content is never empty
                        if content.is_array() && content.as_array().map_or(true, |a| a.is_empty()) {
                            content = json!([{"type": "text", "text": ""}]);
                        }
                    }

                    let mut message = json!({ "role": role_str });
                    message["content"] = content;

                    messages.push(message);
                }
            }
        }

        // Ensure every tool_use has a matching tool_result.
        // Anthropic API requires that assistant messages with tool_use be followed
        // by user messages with tool_result for each tool_use_id.
        messages = ensure_tool_results(messages);

        let mut body = json!({
            "model": ir.model,
            "messages": messages,
            "max_tokens": ir.max_tokens.unwrap_or(4096),
            "stream": ir.stream,
        });

        if !system_parts.is_empty() {
            body["system"] = json!(system_parts.join("\n\n"));
        }

        if let Some(tools) = &ir.tools {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    let schema = if t.input_schema.is_null() {
                        json!({"type": "object"})
                    } else {
                        t.input_schema.clone()
                    };
                    let mut tool = json!({
                        "name": t.name,
                        "input_schema": schema,
                    });
                    if let Some(desc) = &t.description {
                        tool["description"] = json!(desc);
                    }
                    tool
                })
                .collect();
            body["tools"] = json!(tool_defs);
        }

        if let Some(tool_choice) = &ir.tool_choice {
            let converted = match tool_choice {
                Value::String(s) => match s.as_str() {
                    "auto" => json!({"type": "auto"}),
                    "required" => json!({"type": "any"}),
                    "none" => json!({"type": "none"}),
                    _ => json!({"type": "auto"}),
                },
                Value::Object(map) => {
                    if let Some(t) = map.get("type").and_then(|v| v.as_str()) {
                        match t {
                            "function" => {
                                if let Some(name) = map.get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                {
                                    json!({"type": "tool", "name": name})
                                } else {
                                    json!({"type": "auto"})
                                }
                            }
                            other => json!({"type": other}),
                        }
                    } else {
                        tool_choice.clone()
                    }
                }
                _ => json!({"type": "auto"}),
            };
            body["tool_choice"] = converted;
        }

        if let Some(temperature) = ir.temperature {
            body["temperature"] = json!(temperature);
        }

        if let Some(top_p) = ir.top_p {
            body["top_p"] = json!(top_p);
        }

        if let Some(top_k) = ir.top_k {
            body["top_k"] = json!(top_k);
        }

        if let Some(stop) = &ir.stop_sequences {
            body["stop_sequences"] = json!(stop);
        }

        if let Some(thinking) = &ir.thinking {
            if thinking.enabled {
                let budget = thinking.budget_tokens.unwrap_or(10000);
                body["thinking"] = json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                });
                let current_max = ir.max_tokens.unwrap_or(4096);
                if current_max <= budget {
                    tracing::warn!(
                        "max_tokens ({}) <= budget_tokens ({}), adjusting to {}",
                        current_max, budget, budget + 4096
                    );
                    body["max_tokens"] = json!(budget + 4096);
                }
            }
        }

        if ir.extra.contains_key("has_cache_control") {
            // cache_control is per-element, not top-level
        }

        Ok(body)
    }

    fn generate_stream_start(&self, response_id: &str, model: &str, input_tokens: u32, output_tokens: u32, cached_tokens: u32) -> Option<String> {
        let mut usage = json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        });
        if cached_tokens > 0 {
            usage["cache_read_input_tokens"] = json!(cached_tokens);
        }
        let event = json!({
            "type": "message_start",
            "message": {
                "id": response_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage,
            }
        });
        Some(format!("event: message_start\ndata: {}\n\n", event))
    }

    fn generate_stream_chunk(&self, chunk: &IrStreamChunk) -> String {
        if let Some(reason) = &chunk.finish_reason {
            let stop_reason = match reason.as_str() {
                "stop" | "end_turn" | "completed" => "end_turn",
                "tool_calls" | "tool_use" => "tool_use",
                r => r,
            };

            let usage = chunk.usage.as_ref().map(|u| json!({
                "output_tokens": u.completion_tokens,
            })).unwrap_or(json!({ "output_tokens": 0 }));

            let message_delta = json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason,
                    "stop_sequence": null,
                },
                "usage": usage,
            });

            let message_stop = json!({
                "type": "message_stop",
            });

            return format!(
                "event: message_delta\ndata: {}\n\nevent: message_stop\ndata: {}\n\n",
                message_delta, message_stop
            );
        }

        if let Some(thinking) = &chunk.delta_thinking {
            let delta_event = json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": thinking,
                }
            });
            return format!("event: content_block_delta\ndata: {}\n\n", delta_event);
        }

        if let Some(content) = &chunk.delta_content {
            let delta_event = json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": content,
                }
            });
            return format!("event: content_block_delta\ndata: {}\n\n", delta_event);
        }

        if let Some(tool_calls) = &chunk.delta_tool_calls {
            if let Some(tc) = tool_calls.first() {
                if let Some(args) = &tc.arguments {
                    let delta_event = json!({
                        "type": "content_block_delta",
                        "index": tc.index,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": args,
                        }
                    });
                    return format!("event: content_block_delta\ndata: {}\n\n", delta_event);
                }
                if tc.id.is_some() {
                    let start_event = json!({
                        "type": "content_block_start",
                        "index": tc.index,
                        "content_block": {
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": {},
                        }
                    });
                    return format!("event: content_block_start\ndata: {}\n\n", start_event);
                }
            }
        }

        String::new()
    }

    fn generate_stream_end(&self) -> Option<String> {
        None
    }

    fn generate_response(&self, ir: &IrResponse) -> Result<Value, ProxyError> {
        let id = ir.id.as_deref().unwrap_or("msg-proxy");

        let mut content: Vec<Value> = Vec::new();

        for part in &ir.message.content {
            match part {
                IrContentPart::Text { text } => {
                    content.push(json!({
                        "type": "text",
                        "text": text,
                    }));
                }
                IrContentPart::Thinking { text } => {
                    content.push(json!({
                        "type": "thinking",
                        "thinking": text,
                    }));
                }
                IrContentPart::ToolUse { id, name, input } => {
                    content.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input,
                    }));
                }
                _ => {}
            }
        }

        if content.is_empty() && ir.message.tool_calls.is_none() {
            content.push(json!({
                "type": "text",
                "text": "",
            }));
        }

        // Add tool_calls as tool_use content blocks
        if let Some(tool_calls) = &ir.message.tool_calls {
            for tc in tool_calls {
                let input: Value = serde_json::from_str(&tc.arguments).unwrap_or(Value::Object(serde_json::Map::new()));
                content.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": input,
                }));
            }
        }

        let stop_reason = match ir.finish_reason.as_deref() {
            Some("stop") | Some("end_turn") | Some("completed") => "end_turn",
            Some("tool_calls") | Some("tool_use") => "tool_use",
            Some(r) => r,
            None => "end_turn",
        };

        let mut usage = json!({
            "input_tokens": ir.usage.prompt_tokens,
            "output_tokens": ir.usage.completion_tokens,
        });
        if ir.usage.cached_tokens > 0 {
            usage["cache_read_input_tokens"] = json!(ir.usage.cached_tokens);
        }

        Ok(json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": ir.model.as_deref().unwrap_or(""),
            "content": content,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": usage,
        }))
    }
}

fn ensure_tool_results(messages: Vec<Value>) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];
        result.push(msg.clone());

        // Check if this is an assistant message with tool_use blocks
        let role = msg["role"].as_str().unwrap_or("");
        if role == "assistant" {
            if let Some(content) = msg["content"].as_array() {
                let tool_use_ids: Vec<String> = content.iter()
                    .filter(|b| b["type"].as_str() == Some("tool_use"))
                    .filter_map(|b| b["id"].as_str().map(String::from))
                    .collect();

                if !tool_use_ids.is_empty() {
                    // Collect tool_result ids from the next message(s) that are user/tool results
                    let mut answered_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
                    let mut peek = i + 1;
                    while peek < messages.len() {
                        let next_role = messages[peek]["role"].as_str().unwrap_or("");
                        if next_role != "user" { break; }
                        if let Some(arr) = messages[peek]["content"].as_array() {
                            for block in arr {
                                if block["type"].as_str() == Some("tool_result") {
                                    if let Some(id) = block["tool_use_id"].as_str() {
                                        answered_ids.insert(id.to_string());
                                    }
                                }
                            }
                        }
                        // Only peek at consecutive user messages that contain tool_results
                        let has_tool_result = messages[peek]["content"].as_array()
                            .map(|arr| arr.iter().any(|b| b["type"].as_str() == Some("tool_result")))
                            .unwrap_or(false);
                        if !has_tool_result { break; }
                        peek += 1;
                    }

                    // Find orphaned tool_use ids
                    let orphaned: Vec<&String> = tool_use_ids.iter()
                        .filter(|id| !answered_ids.contains(*id))
                        .collect();

                    if !orphaned.is_empty() {
                        tracing::warn!(
                            "Found {} orphaned tool_use(s) without tool_result, adding placeholders: {:?}",
                            orphaned.len(),
                            orphaned
                        );
                        let tool_results: Vec<Value> = orphaned.iter().map(|id| {
                            json!({
                                "type": "tool_result",
                                "tool_use_id": id,
                                "content": "",
                            })
                        }).collect();
                        result.push(json!({
                            "role": "user",
                            "content": tool_results,
                        }));
                    }
                }
            }
        }

        i += 1;
    }

    result
}

fn extract_text_parts(parts: &[IrContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            IrContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn convert_anthropic_content(parts: &[IrContentPart], role: &IrRole) -> Value {
    if parts.len() == 1 {
        if let Some(IrContentPart::Text { text }) = parts.first() {
            return json!(text);
        }
    }

    let items: Vec<Value> = parts
        .iter()
        .map(|part| match part {
            IrContentPart::Text { text } => json!({
                "type": "text",
                "text": text,
            }),
            IrContentPart::Thinking { text } => json!({
                "type": "text",
                "text": text,
            }),
            IrContentPart::Image { url, data, media_type } => {
                let source = if let Some(image_url) = url {
                    json!({
                        "type": "url",
                        "url": image_url,
                    })
                } else if let Some(b64) = data {
                    let mt = media_type.as_deref().unwrap_or("image/png");
                    json!({
                        "type": "base64",
                        "media_type": mt,
                        "data": b64,
                    })
                } else {
                    json!({ "type": "url", "url": "" })
                };
                json!({
                    "type": "image",
                    "source": source,
                })
            }
            IrContentPart::ToolUse { id, name, input } => json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
            IrContentPart::ToolResult { tool_use_id, content, .. } => {
                let _ = role;
                json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                })
            }
        })
        .collect();

    json!(items)
}
