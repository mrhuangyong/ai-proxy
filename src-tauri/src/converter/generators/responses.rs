use crate::converter::ir::{round2, IrContentPart, IrRequest, IrResponse, IrRole, IrStreamChunk};
use crate::converter::FormatGenerator;
use crate::error::ProxyError;
use serde_json::{json, Value};

pub struct ResponsesGenerator;

impl FormatGenerator for ResponsesGenerator {
    fn generate_request(&self, ir: &IrRequest) -> Result<Value, ProxyError> {
        let mut instructions: Option<String> = None;
        let mut input_items: Vec<Value> = Vec::new();

        for msg in &ir.messages {
            match msg.role {
                IrRole::System | IrRole::Developer => {
                    let text = extract_text_content(&msg.content);
                    instructions = Some(text);
                }
                IrRole::User => {
                    let content = convert_message_content(&msg.content);
                    input_items.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                IrRole::Assistant => {
                    // Extract compaction items first (opaque encrypted_content passthrough).
                    for part in &msg.content {
                        if let IrContentPart::Compaction {
                            id,
                            encrypted_content,
                        } = part
                        {
                            input_items.push(json!({
                                "type": "compaction",
                                "id": id,
                                "encrypted_content": encrypted_content,
                            }));
                        }
                    }
                    let mut item = json!({
                        "role": "assistant",
                    });
                    if let Some(tool_calls) = &msg.tool_calls {
                        for tc in tool_calls {
                            input_items.push(json!({
                                "type": "function_call",
                                "call_id": tc.id,
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }));
                        }
                    }
                    let content = convert_message_content(&msg.content);
                    if content != json!("") && content != json!([]) {
                        item["content"] = content;
                        input_items.push(item);
                    } else if msg.tool_calls.is_none() {
                        input_items.push(item);
                    }
                }
                IrRole::Tool => {
                    if let Some(IrContentPart::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    }) = msg.content.first()
                    {
                        input_items.push(json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": content,
                        }));
                    } else if let Some(call_id) = &msg.tool_call_id {
                        let output: String = msg
                            .content
                            .iter()
                            .filter_map(|p| match p {
                                IrContentPart::Text { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        input_items.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output,
                        }));
                    }
                }
            }
        }

        let mut body = json!({
            "model": ir.model,
            "input": input_items,
            "stream": ir.stream,
        });

        if let Some(instructions) = instructions {
            body["instructions"] = json!(instructions);
        }

        if let Some(tools) = &ir.tools {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    let mut tool = json!({
                        "type": "function",
                        "name": t.name,
                        "parameters": t.input_schema,
                    });
                    if let Some(desc) = &t.description {
                        tool["description"] = json!(desc);
                    }
                    if let Some(strict) = t.strict {
                        tool["strict"] = json!(strict);
                    }
                    tool
                })
                .collect();
            body["tools"] = json!(tool_defs);
        }

        if let Some(temperature) = ir.temperature {
            body["temperature"] = json!(round2(temperature));
        }

        if let Some(top_p) = ir.top_p {
            body["top_p"] = json!(round2(top_p));
        }

        if let Some(max_tokens) = ir.max_tokens {
            body["max_output_tokens"] = json!(max_tokens);
        }

        if let Some(stop) = &ir.stop_sequences {
            body["stop"] = json!(stop);
        }

        if let Some(tool_choice) = &ir.tool_choice {
            body["tool_choice"] = tool_choice.clone();
        }

        if let Some(response_format) = &ir.response_format {
            body["text"] = json!({ "format": response_format });
        }

        if let Some(thinking) = &ir.thinking {
            if thinking.enabled {
                if let Some(budget) = thinking.budget_tokens {
                    let effort = if budget <= 5000 {
                        "low"
                    } else if budget <= 15000 {
                        "medium"
                    } else {
                        "high"
                    };
                    body["reasoning"] = json!({ "effort": effort });
                }
            }
        }

        if let Some(prev_id) = ir.extra.get("previous_response_id") {
            body["previous_response_id"] = prev_id.clone();
        }

        Ok(body)
    }

    fn generate_stream_chunk(&self, chunk: &IrStreamChunk) -> String {
        let response_id = chunk.id.as_deref().unwrap_or("resp-proxy");

        if let Some(tool_calls) = &chunk.delta_tool_calls {
            if let Some(tc) = tool_calls.first() {
                if tc.id.is_some() && tc.name.is_some() {
                    let event = json!({
                        "type": "response.output_item.added",
                        "output_index": 0,
                        "item": {
                            "type": "function_call",
                            "id": format!("fc_{}", tc.id.as_deref().unwrap_or("")),
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": "",
                        }
                    });
                    return format!("data: {}\n\n", event);
                }
                if let Some(args) = &tc.arguments {
                    let event = json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": 0,
                        "item_id": format!("fc_{}", tc.id.as_deref().unwrap_or("0")),
                        "call_id": tc.id,
                        "delta": args,
                    });
                    return format!("data: {}\n\n", event);
                }
            }
        }

        if let Some(_reason) = &chunk.finish_reason {
            let text_done = json!({
                "type": "response.output_text.done",
                "output_index": 0,
                "content_index": 0,
                "text": "",
            });
            let item_done = json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_proxy",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "",
                    }],
                    "status": "completed",
                }
            });
            let completed = json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "object": "response",
                    "status": "completed",
                    "output": [],
                }
            });
            return format!(
                "data: {}\n\ndata: {}\n\ndata: {}\n\n",
                text_done, item_done, completed
            );
        }

        if let Some(thinking) = &chunk.delta_thinking {
            let delta_event = json!({
                "type": "response.reasoning_summary_text.delta",
                "output_index": 0,
                "content_index": 0,
                "delta": thinking,
                "response_id": response_id,
            });
            return format!("data: {}\n\n", delta_event);
        }

        if let Some(content) = &chunk.delta_content {
            let delta_event = json!({
                "type": "response.output_text.delta",
                "delta": content,
                "response_id": response_id,
            });
            return format!("data: {}\n\n", delta_event);
        }

        String::new()
    }

    fn generate_stream_start(
        &self,
        response_id: &str,
        model: &str,
        _input_tokens: u32,
        _output_tokens: u32,
        _cached_tokens: u32,
    ) -> Option<String> {
        let created = json!({
            "type": "response.created",
            "response": {
                "id": response_id,
                "object": "response",
                "status": "in_progress",
                "model": model,
                "output": [],
            }
        });
        let item_added = json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_proxy",
                "role": "assistant",
                "content": [],
                "status": "in_progress",
            }
        });
        let content_added = json!({
            "type": "response.content_part.added",
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "",
            }
        });
        Some(format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\n",
            created, item_added, content_added
        ))
    }

    fn generate_response(&self, ir: &IrResponse) -> Result<Value, ProxyError> {
        let id = ir.id.as_deref().unwrap_or("resp-proxy");

        let mut output: Vec<Value> = Vec::new();

        let thinking_text = extract_thinking_content(&ir.message.content);
        if !thinking_text.is_empty() {
            output.push(json!({
                "type": "reasoning",
                "id": "rs_proxy",
                "summary": [{"type": "summary_text", "text": thinking_text}],
            }));
        }

        let text = extract_text_content(&ir.message.content);
        if !text.is_empty() {
            output.push(json!({
                "type": "message",
                "id": "msg_proxy",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": text,
                }],
            }));
        }

        if let Some(tool_calls) = &ir.message.tool_calls {
            for tc in tool_calls {
                output.push(json!({
                    "type": "function_call",
                    "id": format!("fc_{}", tc.id),
                    "call_id": tc.id,
                    "name": tc.name,
                    "arguments": tc.arguments,
                }));
            }
        }

        for part in &ir.message.content {
            if let IrContentPart::Compaction {
                id,
                encrypted_content,
            } = part
            {
                output.push(json!({
                    "type": "compaction",
                    "id": id,
                    "encrypted_content": encrypted_content,
                }));
            }
        }

        Ok(json!({
            "id": id,
            "object": "response",
            "status": "completed",
            "output": output,
            "usage": {
                "input_tokens": ir.usage.prompt_tokens,
                "output_tokens": ir.usage.completion_tokens,
                "total_tokens": ir.usage.total_tokens,
                "input_tokens_details": {
                    "cached_tokens": ir.usage.cached_tokens,
                }
            },
            "model": ir.model.as_deref().unwrap_or(""),
        }))
    }
}

fn extract_text_content(parts: &[IrContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            IrContentPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn extract_thinking_content(parts: &[IrContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            IrContentPart::Thinking { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn convert_message_content(parts: &[IrContentPart]) -> Value {
    if parts.len() == 1 {
        if let Some(IrContentPart::Text { text, .. }) = parts.first() {
            return json!(text);
        }
    }

    let items: Vec<Value> = parts
        .iter()
        .map(|part| match part {
            IrContentPart::Text { text, .. } => json!({
                "type": "input_text",
                "text": text,
            }),
            IrContentPart::Thinking { text, .. } => json!({
                "type": "input_text",
                "text": text,
            }),
            IrContentPart::Image {
                url,
                data,
                media_type,
            } => {
                if let Some(image_url) = url {
                    json!({
                        "type": "input_image",
                        "image_url": image_url,
                    })
                } else if let Some(b64) = data {
                    let mt = media_type.as_deref().unwrap_or("image/png");
                    json!({
                        "type": "input_image",
                        "image_url": format!("data:{};base64,{}", mt, b64),
                    })
                } else {
                    json!({ "type": "input_image", "image_url": "" })
                }
            }
            IrContentPart::ToolUse { id, name, input } => json!({
                "type": "function_call",
                "call_id": id,
                "name": name,
                "arguments": input.to_string(),
            }),
            IrContentPart::ToolResult {
                tool_use_id,
                content,
                ..
            } => json!({
                "type": "function_call_output",
                "call_id": tool_use_id,
                "output": content,
            }),
            IrContentPart::Compaction {
                id,
                encrypted_content,
            } => json!({
                "type": "compaction",
                "id": id,
                "encrypted_content": encrypted_content,
            }),
        })
        .collect();

    json!(items)
}
