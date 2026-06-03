use serde_json::Value;

use crate::converter::ir::{
    IrContentPart, IrMessage, IrRequest, IrResponse, IrRole, IrStreamChunk, IrTool,
    IrToolCall, IrToolCallDelta, IrUsage,
};
use crate::converter::FormatParser;
use crate::error::ProxyError;

pub struct GeminiParser;

impl FormatParser for GeminiParser {
    fn parse_request(&self, body: &Value) -> Result<IrRequest, ProxyError> {
        let mut messages = Vec::new();

        if let Some(system_instruction) = body.get("systemInstruction") {
            let empty_parts = Vec::new();
            let parts = system_instruction["parts"]
                .as_array()
                .unwrap_or(&empty_parts);

            let mut system_texts = Vec::new();
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    system_texts.push(text.to_string());
                }
            }

            if !system_texts.is_empty() {
                messages.push(IrMessage {
                    role: IrRole::System,
                    content: system_texts
                        .into_iter()
                        .map(|text| IrContentPart::Text { text })
                        .collect(),
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
        }

        let contents = body["contents"]
            .as_array()
            .ok_or_else(|| ProxyError::Parse("missing 'contents' field".into()))?;

        for content in contents {
            messages.push(parse_gemini_content(content)?);
        }

        let tools = body["tools"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|tool_set| tool_set["functionDeclarations"].as_array())
            .map(|funcs| {
                funcs
                    .iter()
                    .filter_map(|f| {
                        let name = f["name"].as_str()?.to_string();
                        Some(IrTool {
                            name,
                            description: f["description"].as_str().map(String::from),
                            input_schema: f
                                .get("parameters")
                                .cloned()
                                .unwrap_or(Value::Object(serde_json::Map::new())),
                            strict: None,
                        })
                    })
                    .collect::<Vec<_>>()
            });

        let temperature = body["generationConfig"]["temperature"]
            .as_f64()
            .map(|v| v as f32);
        let top_p = body["generationConfig"]["topP"]
            .as_f64()
            .map(|v| v as f32);
        let top_k = body["generationConfig"]["topK"]
            .as_u64()
            .map(|v| v as u32);
        let max_tokens = body["generationConfig"]["maxOutputTokens"]
            .as_u64()
            .map(|v| v as u32);

        let stop_sequences = body["generationConfig"]["stopSequences"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            });

        let response_format = body["generationConfig"]
            .get("responseMimeType")
            .cloned();

        let tool_choice = body.get("toolConfig")
            .and_then(|tc| tc.get("function_calling_config"))
            .and_then(|fcc| {
                let mode = fcc["mode"].as_str().unwrap_or("AUTO");
                match mode {
                    "NONE" => Some(serde_json::json!("none")),
                    "AUTO" => Some(serde_json::json!("auto")),
                    "ANY" => Some(serde_json::json!("required")),
                    _ => None,
                }
            });

        Ok(IrRequest {
            model: String::new(),
            messages,
            tools,
            tool_choice,
            temperature,
            top_p,
            top_k,
            max_tokens,
            stream: false,
            stop_sequences,
            response_format,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            thinking: None,
            stream_options: None,
            metadata: std::collections::HashMap::new(),
            extra: std::collections::HashMap::new(),
        })
    }

    fn parse_stream_chunk(&self, line: &str) -> Result<Option<IrStreamChunk>, ProxyError> {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            return Ok(None);
        }

        let data = if let Some(stripped) = trimmed.strip_prefix("data: ") {
            stripped.trim()
        } else if let Some(stripped) = trimmed.strip_prefix("data:") {
            stripped.trim()
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            return Ok(None);
        };

        if data == "[DONE]" {
            return Ok(None);
        }

        let chunk: Value = serde_json::from_str(data)
            .map_err(|e| ProxyError::Parse(format!("failed to parse SSE chunk: {}", e)))?;

        let candidate = chunk["candidates"]
            .as_array()
            .and_then(|a| a.first());

        let mut delta_content_parts: Vec<String> = Vec::new();
        let mut delta_tool_calls: Vec<IrToolCallDelta> = Vec::new();

        if let Some(parts) = candidate
            .and_then(|c| c["content"]["parts"].as_array())
        {
            for (idx, part) in parts.iter().enumerate() {
                if let Some(text) = part["text"].as_str() {
                    delta_content_parts.push(text.to_string());
                }

                if let Some(func_call) = part.get("functionCall") {
                    let name = func_call["name"].as_str().unwrap_or("").to_string();
                    let args = func_call["args"].clone();
                    delta_tool_calls.push(IrToolCallDelta {
                        index: idx as u32,
                        id: Some(format!("call_{}_{}", name, idx)),
                        name: Some(name),
                        arguments: Some(serde_json::to_string(&args).unwrap_or_default()),
                    });
                }
            }
        }

        let delta_content = if delta_content_parts.is_empty() {
            None
        } else {
            Some(delta_content_parts.join(""))
        };

        let delta_tool_calls = if delta_tool_calls.is_empty() {
            None
        } else {
            Some(delta_tool_calls)
        };

        let finish_reason = candidate
            .and_then(|c| c["finishReason"].as_str())
            .map(String::from);

        let usage = chunk.get("usageMetadata").and_then(|u| {
            Some(IrUsage {
                prompt_tokens: u["promptTokenCount"].as_u64()? as u32,
                completion_tokens: u["candidatesTokenCount"].as_u64()? as u32,
                total_tokens: u["totalTokenCount"].as_u64()? as u32,
                cached_tokens: u["cachedContentTokenCount"].as_u64().unwrap_or(0) as u32,
            })
        });

        Ok(Some(IrStreamChunk {
            id: None,
            model: None,
            delta_content,
            delta_tool_calls,
            delta_thinking: None,
            finish_reason,
            usage,
            error: None,
        }))
    }

    fn parse_response(&self, body: &Value) -> Result<IrResponse, ProxyError> {
        let candidate = body["candidates"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| ProxyError::Parse("missing 'candidates' in response".into()))?;

        let mut content_parts = Vec::new();
        let mut tool_calls = Vec::new();

        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for (idx, part) in parts.iter().enumerate() {
                if let Some(text) = part["text"].as_str() {
                    content_parts.push(IrContentPart::Text {
                        text: text.to_string(),
                    });
                }

                if let Some(func_call) = part.get("functionCall") {
                    let name = func_call["name"].as_str().unwrap_or("").to_string();
                    let args = func_call["args"].clone();

                    tool_calls.push(IrToolCall {
                        id: format!("call_{}_{}", name, idx),
                        name,
                        arguments: serde_json::to_string(&args).unwrap_or_default(),
                    });
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

        let finish_reason = candidate["finishReason"]
            .as_str()
            .map(String::from);

        let usage = body
            .get("usageMetadata")
            .map(|u| IrUsage {
                prompt_tokens: u["promptTokenCount"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["totalTokenCount"].as_u64().unwrap_or(0) as u32,
                cached_tokens: u["cachedContentTokenCount"].as_u64().unwrap_or(0) as u32,
            })
            .unwrap_or(IrUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cached_tokens: 0,
            });

        Ok(IrResponse {
            id: None,
            model: None,
            message,
            finish_reason,
            usage,
        })
    }
}

fn parse_gemini_content(content: &Value) -> Result<IrMessage, ProxyError> {
    let role_str = content["role"]
        .as_str()
        .ok_or_else(|| ProxyError::Parse("content missing 'role'".into()))?;

    let role = match role_str {
        "user" => IrRole::User,
        "model" => IrRole::Assistant,
        "system" => IrRole::System,
        "function" => IrRole::Tool,
        other => {
            return Err(ProxyError::Parse(format!("unknown role: {}", other)));
        }
    };

    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();

    if let Some(parts) = content["parts"].as_array() {
        for (idx, part) in parts.iter().enumerate() {
            if let Some(text) = part["text"].as_str() {
                content_parts.push(IrContentPart::Text {
                    text: text.to_string(),
                });
            }

            if let Some(inline_data) = part.get("inlineData") {
                content_parts.push(IrContentPart::Image {
                    url: None,
                    data: inline_data["data"].as_str().map(String::from),
                    media_type: inline_data["mimeType"].as_str().map(String::from),
                });
            }

            if let Some(file_data) = part.get("fileData") {
                content_parts.push(IrContentPart::Image {
                    url: file_data["fileUri"].as_str().map(String::from),
                    data: None,
                    media_type: file_data["mimeType"].as_str().map(String::from),
                });
            }

            if let Some(func_call) = part.get("functionCall") {
                let name = func_call["name"].as_str().unwrap_or("").to_string();
                let args = func_call["args"].clone();

                tool_calls.push(IrToolCall {
                    id: format!("call_{}_{}", name, idx),
                    name,
                    arguments: serde_json::to_string(&args).unwrap_or_default(),
                });
            }

            if let Some(func_resp) = part.get("functionResponse") {
                let name = func_resp["name"].as_str().unwrap_or("").to_string();
                let response_content =
                    serde_json::to_string(&func_resp["response"]).unwrap_or_default();

                content_parts.push(IrContentPart::ToolResult {
                    tool_use_id: name.clone(),
                    content: response_content,
                    tool_name: Some(name),
                });
            }
        }
    }

    Ok(IrMessage {
        role,
        content: content_parts,
        name: None,
        tool_call_id: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
    })
}
