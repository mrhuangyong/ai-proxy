use crate::converter::ir::*;
use crate::converter::FormatParser;
use crate::error::ProxyError;
use serde_json::{json, Value};

pub struct CompletionsParser;

impl FormatParser for CompletionsParser {
    fn parse_request(&self, body: &Value) -> Result<IrRequest, ProxyError> {
        let model = body["model"]
            .as_str()
            .ok_or_else(|| ProxyError::Parse("missing model".into()))?
            .to_string();

        let messages = body["messages"]
            .as_array()
            .ok_or_else(|| ProxyError::Parse("missing messages".into()))?;

        let mut ir_messages: Vec<IrMessage> = messages
            .iter()
            .map(|m| {
                let role = match m["role"].as_str().unwrap_or("user") {
                    "system" => IrRole::System,
                    "developer" => IrRole::Developer,
                    "user" => IrRole::User,
                    "assistant" => IrRole::Assistant,
                    "tool" | "function" => IrRole::Tool,
                    r => return Err(ProxyError::Parse(format!("unknown role: {}", r))),
                };

                let content = if m["content"].is_string() {
                    vec![IrContentPart::Text {
                        text: m["content"].as_str().unwrap().to_string(),
                    }]
                } else if let Some(arr) = m["content"].as_array() {
                    arr.iter()
                        .filter_map(|p| match p["type"].as_str() {
                            Some("text") => Some(IrContentPart::Text {
                                text: p.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            }),
                            Some("image_url") => {
                                let url_str = p["image_url"].get("url").and_then(|v| v.as_str()).unwrap_or("");
                                if let Some(rest) = url_str.strip_prefix("data:") {
                                    if let Some(semi) = rest.find(';') {
                                        let media_type = rest[..semi].to_string();
                                        let after = &rest[semi + 1..];
                                        if let Some(comma) = after.find(',') {
                                            let b64_data = after[comma + 1..].to_string();
                                            Some(IrContentPart::Image {
                                                url: None,
                                                data: Some(b64_data),
                                                media_type: Some(media_type),
                                            })
                                        } else {
                                            Some(IrContentPart::Image {
                                                url: Some(url_str.to_string()),
                                                data: None,
                                                media_type: None,
                                            })
                                        }
                                    } else {
                                        Some(IrContentPart::Image {
                                            url: Some(url_str.to_string()),
                                            data: None,
                                            media_type: None,
                                        })
                                    }
                                } else {
                                    Some(IrContentPart::Image {
                                        url: Some(url_str.to_string()),
                                        data: None,
                                        media_type: None,
                                    })
                                }
                            }
                            Some("refusal") => None,
                            _ => None,
                        })
                        .collect()
                } else {
                    vec![]
                };

                let tool_calls = m.get("tool_calls").and_then(|tc| {
                    let calls: Vec<IrToolCall> = tc
                        .as_array()?
                        .iter()
                        .filter_map(|tc| {
                            let func = tc.get("function")?;
                            Some(IrToolCall {
                                id: tc.get("id")?.as_str()?.to_string(),
                                name: func.get("name")?.as_str()?.to_string(),
                                arguments: func.get("arguments")?.as_str()?.to_string(),
                            })
                        })
                        .collect();
                    if calls.is_empty() { None } else { Some(calls) }
                }).or_else(|| {
                    m.get("function_call").and_then(|fc| {
                        let name = fc.get("name")?.as_str()?.to_string();
                        Some(vec![IrToolCall {
                            id: name.clone(),
                            name,
                            arguments: fc.get("arguments")?.as_str()?.to_string(),
                        }])
                    })
                });

                let tool_call_id = m.get("tool_call_id").and_then(|v| v.as_str()).map(String::from)
                    .or_else(|| {
                        if m["role"].as_str() == Some("function") {
                            m.get("name").and_then(|v| v.as_str()).map(String::from)
                        } else {
                            None
                        }
                    });

                Ok(IrMessage {
                    role,
                    content,
                    name: m.get("name").and_then(|v| v.as_str()).map(String::from),
                    tool_call_id,
                    tool_calls,
                })
            })
            .collect::<Result<_, _>>()?;

        // Post-process: fix tool_call_id for function-role-derived tool messages
        // Codex uses new-style tool_calls (id="call_abc") on assistant but old-style
        // function role (name="read_file") for responses. Map name → real id.
        let mut all_ids = std::collections::HashSet::new();
        let mut name_to_id = std::collections::HashMap::new();
        for msg in &ir_messages {
            if msg.role == IrRole::Assistant {
                if let Some(calls) = &msg.tool_calls {
                    for tc in calls {
                        all_ids.insert(tc.id.clone());
                        name_to_id.insert(tc.name.clone(), tc.id.clone());
                    }
                }
            }
        }
        for msg in &mut ir_messages {
            if msg.role == IrRole::Tool {
                tracing::warn!("TOOL PARSE: tool_call_id={:?}, name={:?}", msg.tool_call_id, msg.name);
                if let Some(ref call_id) = msg.tool_call_id {
                    if !all_ids.contains(call_id) {
                        tracing::warn!("TOOL PARSE: call_id={} not in all_ids, trying name_to_id", call_id);
                        if let Some(real_id) = name_to_id.get(call_id).cloned() {
                            tracing::warn!("TOOL PARSE: remapped {} -> {}", call_id, real_id);
                            msg.tool_call_id = Some(real_id);
                        }
                    }
                }
            }
        }

        let tools = body.get("tools").and_then(|t| {
            Some(
                t.as_array()?
                    .iter()
                    .filter_map(|tool| {
                        let func = tool.get("function")?;
                        Some(IrTool {
                            name: func.get("name")?.as_str()?.to_string(),
                            description: func.get("description").and_then(|v| v.as_str()).map(String::from),
                            input_schema: func.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                            strict: func.get("strict").and_then(|v| v.as_bool()),
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        });

        let mut metadata = std::collections::HashMap::new();
        if let Some(user) = body.get("user") {
            metadata.insert("user".into(), user.clone());
        }

        let max_tokens = body.get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let thinking = body.get("reasoning").and_then(|r| {
            let effort = r["effort"].as_str().unwrap_or("medium");
            Some(IrThinkingConfig {
                enabled: true,
                budget_tokens: match effort {
                    "low" => Some(5000),
                    "medium" => Some(10000),
                    "high" => Some(30000),
                    _ => None,
                },
            })
        });

        Ok(IrRequest {
            model,
            messages: ir_messages,
            tools,
            tool_choice: body.get("tool_choice").cloned(),
            temperature: body.get("temperature").and_then(|v| v.as_f64()).map(|v| v as f32),
            top_p: body.get("top_p").and_then(|v| v.as_f64()).map(|v| v as f32),
            top_k: None,
            max_tokens,
            stream: body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
            stop_sequences: body.get("stop").and_then(|v| {
                if v.is_string() {
                    Some(vec![v.as_str()?.to_string()])
                } else {
                    v.as_array().map(|arr| {
                        arr.iter().filter_map(|s| s.as_str().map(String::from)).collect()
                    })
                }
            }),
            response_format: body.get("response_format").cloned(),
            presence_penalty: body.get("presence_penalty").and_then(|v| v.as_f64()).map(|v| v as f32),
            frequency_penalty: body.get("frequency_penalty").and_then(|v| v.as_f64()).map(|v| v as f32),
            seed: body.get("seed").and_then(|v| v.as_u64()),
            thinking,
            stream_options: body.get("stream_options").cloned(),
            metadata,
            extra: std::collections::HashMap::new(),
        })
    }

    fn parse_stream_chunk(&self, line: &str) -> Result<Option<IrStreamChunk>, ProxyError> {
        let data = if let Some(d) = line.strip_prefix("data: ") {
            d
        } else if let Some(d) = line.strip_prefix("data:") {
            d
        } else {
            return Ok(None);
        };
        if data == "[DONE]" {
            return Ok(Some(IrStreamChunk {
                id: None,
                model: None,
                delta_content: None,
                delta_tool_calls: None,
                delta_thinking: None,
                finish_reason: Some("stop".into()),
                usage: None,
                error: None,
            }));
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|e| ProxyError::Parse(format!("SSE parse error: {}", e)))?;

        let choice = chunk["choices"].get(0);
        let delta = choice.and_then(|c| c.get("delta"));

        Ok(Some(IrStreamChunk {
            id: chunk["id"].as_str().map(String::from),
            model: chunk["model"].as_str().map(String::from),
            delta_content: delta.and_then(|d| d["content"].as_str()).map(String::from),
            delta_tool_calls: delta.and_then(|d| d["tool_calls"].as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|tc| {
                        Some(IrToolCallDelta {
                            index: tc.get("index")?.as_u64()? as u32,
                            id: tc.get("id").and_then(|v| v.as_str()).map(String::from),
                            name: tc.get("function")?.get("name").and_then(|v| v.as_str()).map(String::from),
                            arguments: tc.get("function")?.get("arguments").and_then(|v| v.as_str()).map(String::from),
                        })
                    })
                    .collect()
            }),
            delta_thinking: delta.and_then(|d| d["reasoning_content"].as_str()).map(String::from),
            finish_reason: choice.and_then(|c| c["finish_reason"].as_str()).map(String::from),
            usage: chunk.get("usage").map(|u| IrUsage {
                prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
                cached_tokens: u.get("prompt_tokens_details")
                    .and_then(|d| d["cached_tokens"].as_u64())
                    .unwrap_or(0) as u32,
            }),
            error: None,
        }))
    }

    fn parse_response(&self, body: &Value) -> Result<IrResponse, ProxyError> {
        let choice = body["choices"]
            .get(0)
            .ok_or(ProxyError::Parse("no choices".into()))?;
        let msg = &choice["message"];

        let tool_calls = msg.get("tool_calls").and_then(|tc| {
            let calls: Vec<IrToolCall> = tc.as_array()?.iter().filter_map(|tc| {
                let func = tc.get("function")?;
                Some(IrToolCall {
                    id: tc.get("id")?.as_str()?.to_string(),
                    name: func.get("name")?.as_str()?.to_string(),
                    arguments: func.get("arguments")?.as_str()?.to_string(),
                })
            }).collect();
            if calls.is_empty() { None } else { Some(calls) }
        });

        let mut content = if msg["content"].is_string() {
            let text = msg["content"].as_str().unwrap_or("").to_string();
            if text.is_empty() { vec![] } else {
                vec![IrContentPart::Text { text }]
            }
        } else if let Some(arr) = msg["content"].as_array() {
            arr.iter()
                .filter_map(|p| match p["type"].as_str() {
                    Some("text") => Some(IrContentPart::Text {
                        text: p.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    }),
                    Some("refusal") => None,
                    _ => None,
                })
                .collect()
        } else {
            vec![]
        };

        if let Some(reasoning) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
            if !reasoning.is_empty() {
                content.insert(0, IrContentPart::Thinking { text: reasoning.to_string() });
            }
        }

        Ok(IrResponse {
            id: body["id"].as_str().map(String::from),
            model: body["model"].as_str().map(String::from),
            message: IrMessage {
                role: IrRole::Assistant,
                content,
                name: None,
                tool_call_id: None,
                tool_calls,
            },
            finish_reason: choice["finish_reason"].as_str().map(String::from),
            usage: body.get("usage").map(|u| IrUsage {
                prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
                cached_tokens: u.get("prompt_tokens_details")
                    .and_then(|d| d["cached_tokens"].as_u64())
                    .unwrap_or(0) as u32,
            }).unwrap_or(IrUsage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, cached_tokens: 0 }),
        })
    }
}
