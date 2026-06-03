use serde_json::Value;

use crate::converter::ir::{
    IrContentPart, IrMessage, IrRequest, IrResponse, IrRole, IrStreamChunk, IrStreamError, IrTool,
    IrToolCall, IrToolCallDelta, IrUsage, IrThinkingConfig,
};
use crate::converter::FormatParser;
use crate::error::ProxyError;

pub struct ResponsesParser;

impl FormatParser for ResponsesParser {
    fn parse_request(&self, body: &Value) -> Result<IrRequest, ProxyError> {
        let model = body["model"]
            .as_str()
            .ok_or_else(|| ProxyError::Parse("missing 'model' field".into()))?
            .to_string();

        let mut messages = Vec::new();

        if let Some(instructions) = body["instructions"].as_str() {
            if !instructions.is_empty() {
                messages.push(IrMessage {
                    role: IrRole::System,
                    content: vec![IrContentPart::Text {
                        text: instructions.to_string(),
                    }],
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
        }

        if let Some(input) = body.get("input") {
            if let Some(text) = input.as_str() {
                messages.push(IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentPart::Text {
                        text: text.to_string(),
                    }],
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                });
            } else if let Some(arr) = input.as_array() {
                for item in arr {
                    if let Some(msg) = parse_input_item(item)? {
                        messages.push(msg);
                    }
                }
            }
        }

        // Responses format uses separate `function_call` items for each tool call,
        // but completions format expects all tool_calls in a single assistant message.
        // Merge consecutive assistant messages to satisfy this requirement.
        messages = merge_consecutive_assistant_messages(messages);

        let tools = body["tools"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let tool_type = t["type"].as_str().unwrap_or("");
                    if tool_type != "function" {
                        return None;
                    }
                    let name = t["name"].as_str()?.to_string();
                    Some(IrTool {
                        name,
                        description: t["description"].as_str().map(String::from),
                        input_schema: t
                            .get("parameters")
                            .cloned()
                            .unwrap_or(Value::Object(serde_json::Map::new())),
                        strict: t.get("strict").and_then(|v| v.as_bool()),
                    })
                })
                .collect::<Vec<_>>()
        });

        let temperature = body["temperature"].as_f64().map(|v| v as f32);
        let top_p = body["top_p"].as_f64().map(|v| v as f32);
        let max_tokens = body["max_output_tokens"].as_u64().map(|v| v as u32);
        let stream = body["stream"].as_bool().unwrap_or(false);

        let stop_sequences = body["stop"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        });

        let tool_choice = body.get("tool_choice").cloned();
        let response_format = body.get("text").and_then(|t| t.get("format")).cloned()
            .or_else(|| body.get("response_format").cloned());

        let thinking = body.get("reasoning").and_then(|r| {
            let effort = r.get("effort")?.as_str()?;
            Some(IrThinkingConfig {
                enabled: true,
                budget_tokens: match effort {
                    "low" => Some(5000),
                    "medium" => Some(10000),
                    "high" => Some(30000),
                    _ => Some(10000),
                },
            })
        });

        let mut extra = std::collections::HashMap::new();
        if let Some(prev_id) = body.get("previous_response_id") {
            extra.insert("previous_response_id".into(), prev_id.clone());
        }

        Ok(IrRequest {
            model,
            messages,
            tools,
            tool_choice,
            temperature,
            top_p,
            top_k: None,
            max_tokens,
            stream,
            stop_sequences,
            response_format,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            thinking,
            stream_options: None,
            metadata: std::collections::HashMap::new(),
            extra,
        })
    }

    fn parse_stream_chunk(&self, line: &str) -> Result<Option<IrStreamChunk>, ProxyError> {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            return Ok(None);
        }

        let data = if let Some(d) = trimmed.strip_prefix("data: ") {
            d.trim()
        } else if let Some(d) = trimmed.strip_prefix("data:") {
            d.trim()
        } else {
            return Ok(None);
        };

        if data == "[DONE]" {
            return Ok(None);
        }

        let event: Value = serde_json::from_str(data)
            .map_err(|e| ProxyError::Parse(format!("failed to parse SSE chunk: {}", e)))?;

        let event_type = event["type"].as_str().unwrap_or("");

        match event_type {
            "response.output_text.delta" => {
                let delta = event["delta"].as_str().unwrap_or("");
                Ok(Some(IrStreamChunk {
                    id: event["response_id"].as_str().map(String::from),
                    model: None,
                    delta_content: Some(delta.to_string()),
                    delta_tool_calls: None,
                    delta_thinking: None,
                    finish_reason: None,
                    usage: None,
                    error: None,
                }))
            }
            "response.function_call_arguments.delta" => {
                let delta = event["delta"].as_str().unwrap_or("");
                Ok(Some(IrStreamChunk {
                    id: None,
                    model: None,
                    delta_content: None,
                    delta_tool_calls: Some(vec![IrToolCallDelta {
                        index: 0,
                        id: None,
                        name: None,
                        arguments: Some(delta.to_string()),
                    }]),
                    delta_thinking: None,
                    finish_reason: None,
                    usage: None,
                    error: None,
                }))
            }
            "response.output_item.added" => {
                let item = &event["item"];
                let item_type = item["type"].as_str().unwrap_or("");

                if item_type == "function_call" {
                    let call_id = item["call_id"].as_str().unwrap_or("");
                    let name = item["name"].as_str().unwrap_or("");
                    return Ok(Some(IrStreamChunk {
                        id: None,
                        model: None,
                        delta_content: None,
                        delta_tool_calls: Some(vec![IrToolCallDelta {
                            index: 0,
                            id: Some(call_id.to_string()),
                            name: Some(name.to_string()),
                            arguments: None,
                        }]),
                        delta_thinking: None,
                        finish_reason: None,
                        usage: None,
                        error: None,
                    }));
                }

                Ok(None)
            }
            "response.output_item.done" => {
                let item = &event["item"];
                let item_type = item["type"].as_str().unwrap_or("");

                if item_type == "function_call" {
                    let call_id = item["call_id"].as_str().unwrap_or("");
                    let name = item["name"].as_str().unwrap_or("");
                    let arguments = item["arguments"].as_str().unwrap_or("{}");

                    return Ok(Some(IrStreamChunk {
                        id: event["response_id"].as_str().map(String::from),
                        model: None,
                        delta_content: None,
                        delta_tool_calls: Some(vec![IrToolCallDelta {
                            index: 0,
                            id: Some(call_id.to_string()),
                            name: Some(name.to_string()),
                            arguments: Some(arguments.to_string()),
                        }]),
                        delta_thinking: None,
                        finish_reason: None,
                        usage: None,
                        error: None,
                    }));
                }

                Ok(None)
            }
            "response.completed" => {
                let response = &event["response"];
                let usage = response.get("usage").and_then(|u| {
                    Some(IrUsage {
                        prompt_tokens: u["input_tokens"].as_u64()? as u32,
                        completion_tokens: u["output_tokens"].as_u64()? as u32,
                        total_tokens: u["input_tokens"].as_u64()? as u32
                            + u["output_tokens"].as_u64()? as u32,
                        cached_tokens: u.get("input_tokens_details")
                            .and_then(|d| d["cached_tokens"].as_u64())
                            .unwrap_or(0) as u32,
                    })
                });

                Ok(Some(IrStreamChunk {
                    id: response["id"].as_str().map(String::from),
                    model: response["model"].as_str().map(String::from),
                    delta_content: None,
                    delta_tool_calls: None,
                    delta_thinking: None,
                    finish_reason: Some("completed".to_string()),
                    usage,
                    error: None,
                }))
            }
            "response.failed" => {
                let response_obj = &event["response"];
                let error_obj = &event["error"];
                let error_message = error_obj["message"]
                    .as_str()
                    .unwrap_or("upstream response failed")
                    .to_string();
                let error_code = error_obj["code"]
                    .as_str()
                    .map(String::from);

                let usage = response_obj.get("usage").and_then(|u| {
                    Some(IrUsage {
                        prompt_tokens: u["input_tokens"].as_u64()? as u32,
                        completion_tokens: u["output_tokens"].as_u64()? as u32,
                        total_tokens: u["input_tokens"].as_u64()? as u32
                            + u["output_tokens"].as_u64()? as u32,
                        cached_tokens: u.get("input_tokens_details")
                            .and_then(|d| d["cached_tokens"].as_u64())
                            .unwrap_or(0) as u32,
                    })
                });

                Ok(Some(IrStreamChunk {
                    id: response_obj["id"].as_str().map(String::from),
                    model: response_obj["model"].as_str().map(String::from),
                    delta_content: None,
                    delta_tool_calls: None,
                    delta_thinking: None,
                    finish_reason: Some("failed".to_string()),
                    usage,
                    error: Some(IrStreamError {
                        code: error_code,
                        message: error_message,
                    }),
                }))
            }
            _ => Ok(None),
        }
    }

    fn parse_response(&self, body: &Value) -> Result<IrResponse, ProxyError> {
        let id = body["id"].as_str().map(String::from);
        let model = body["model"].as_str().map(String::from);

        let mut content_parts = Vec::new();
        let mut tool_calls = Vec::new();

        if let Some(outputs) = body["output"].as_array() {
            for output in outputs {
                let output_type = output["type"].as_str().unwrap_or("");

                match output_type {
                    "message" => {
                        if let Some(parts) = output["content"].as_array() {
                            for part in parts {
                                let part_type = part["type"].as_str().unwrap_or("");
                                match part_type {
                                    "output_text" => {
                                        if let Some(text) = part["text"].as_str() {
                                            content_parts.push(IrContentPart::Text {
                                                text: text.to_string(),
                                            });
                                        }
                                    }
                                    "refusal" => {}
                                    _ => {}
                                }
                            }
                        }
                    }
                    "function_call" => {
                        let call_id = output["call_id"].as_str().unwrap_or("");
                        let name = output["name"].as_str().unwrap_or("");
                        let arguments = output["arguments"].as_str().unwrap_or("{}");

                        tool_calls.push(IrToolCall {
                            id: call_id.to_string(),
                            name: name.to_string(),
                            arguments: arguments.to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }

        let message = IrMessage {
            role: IrRole::Assistant,
            content: content_parts,
            name: None,
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        };

        let finish_reason = body["status"]
            .as_str()
            .filter(|s| *s != "in_progress")
            .map(String::from);

        let usage = body
            .get("usage")
            .and_then(|u| {
                Some(IrUsage {
                    prompt_tokens: u["input_tokens"].as_u64()? as u32,
                    completion_tokens: u["output_tokens"].as_u64()? as u32,
                    total_tokens: u["input_tokens"].as_u64()? as u32
                        + u["output_tokens"].as_u64()? as u32,
                    cached_tokens: u.get("input_tokens_details")
                        .and_then(|d| d["cached_tokens"].as_u64())
                        .unwrap_or(0) as u32,
                })
            })
            .unwrap_or(IrUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cached_tokens: 0,
            });

        Ok(IrResponse {
            id,
            model,
            message,
            finish_reason,
            usage,
        })
    }
}

fn parse_input_item(item: &Value) -> Result<Option<IrMessage>, ProxyError> {
    let item_type = item["type"].as_str().unwrap_or("");

    match item_type {
        "function_call" => {
            let call_id = item["call_id"].as_str().unwrap_or("").to_string();
            let name = item["name"].as_str().unwrap_or("").to_string();
            let arguments = item["arguments"].as_str().unwrap_or("{}").to_string();

            Ok(Some(IrMessage {
                role: IrRole::Assistant,
                content: vec![],
                name: None,
                tool_call_id: None,
                tool_calls: Some(vec![IrToolCall {
                    id: call_id,
                    name,
                    arguments,
                }]),
            }))
        }
        "function_call_output" => {
            let call_id = item["call_id"].as_str().unwrap_or("").to_string();
            let output = item["output"].as_str().unwrap_or("").to_string();

            Ok(Some(IrMessage {
                role: IrRole::Tool,
                content: vec![IrContentPart::Text {
                    text: output,
                }],
                name: None,
                tool_call_id: Some(call_id),
                tool_calls: None,
            }))
        }
        "message" | "" => {
            let role_str = item["role"].as_str().unwrap_or("");

            let role = match role_str {
                "user" => IrRole::User,
                "assistant" => IrRole::Assistant,
                "system" => IrRole::System,
                "developer" => IrRole::Developer,
                "" => return Ok(None),
                other => {
                    return Err(ProxyError::Parse(format!(
                        "unknown role in input: {}",
                        other
                    )));
                }
            };

            let mut content_parts = Vec::new();

            if let Some(text) = item.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    content_parts.push(IrContentPart::Text {
                        text: text.to_string(),
                    });
                }
            }

            if content_parts.is_empty() {
                if let Some(arr) = item["content"].as_array() {
                    for part in arr {
                        if let Some(text) = part["text"].as_str() {
                            content_parts.push(IrContentPart::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                }
            }

            if content_parts.is_empty() {
                if let Some(text) = item["input"].as_str() {
                    content_parts.push(IrContentPart::Text {
                        text: text.to_string(),
                    });
                }
            }

            // For assistant messages, extract <thinking> tags from content
            if role == IrRole::Assistant && !content_parts.is_empty() {
                let all_text: String = content_parts.iter()
                    .filter_map(|p| match p {
                        IrContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let (thinking_opt, clean) = split_thinking_and_content(&all_text);
                content_parts.clear();
                if let Some(thinking) = thinking_opt {
                    content_parts.push(IrContentPart::Thinking { text: thinking });
                }
                let trimmed = clean.trim();
                if !trimmed.is_empty() {
                    content_parts.push(IrContentPart::Text { text: trimmed.to_string() });
                }
            }

            Ok(Some(IrMessage {
                role,
                content: content_parts,
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }))
        }
        "reasoning" => {
            let text = item.get("summary")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();

            Ok(Some(IrMessage {
                role: IrRole::Assistant,
                content: if text.is_empty() {
                    vec![]
                } else {
                    vec![IrContentPart::Thinking { text }]
                },
                name: None,
                tool_call_id: None,
                tool_calls: None,
            }))
        }
        _ => Ok(None),
    }
}

/// Extract `<thinking>...</thinking>` tagged content from text.
/// Returns (thinking_content, remaining_text).
fn split_thinking_and_content(text: &str) -> (Option<String>, String) {
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

/// Merge consecutive assistant messages into one.
///
/// The Responses API represents each function_call as a separate input item,
/// which `parse_input_item` converts to individual assistant messages.
/// The Completions API requires all tool_calls in a single assistant message.
fn merge_consecutive_assistant_messages(messages: Vec<IrMessage>) -> Vec<IrMessage> {
    let mut result: Vec<IrMessage> = Vec::new();

    for msg in messages {
        if let Some(last) = result.last_mut() {
            if last.role == IrRole::Assistant && msg.role == IrRole::Assistant {
                if !msg.content.is_empty() {
                    last.content
                        .extend(msg.content.into_iter().filter(|p| {
                            if let IrContentPart::Text { text } = p {
                                !text.is_empty()
                            } else {
                                true
                            }
                        }));
                }
                match (&mut last.tool_calls, msg.tool_calls) {
                    (Some(existing), Some(new)) => existing.extend(new),
                    (None, Some(new)) => last.tool_calls = Some(new),
                    _ => {}
                }
                continue;
            }
        }
        result.push(msg);
    }

    result
}
