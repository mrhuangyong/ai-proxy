use crate::converter::ir::{round2, IrContentPart, IrRequest, IrResponse, IrRole, IrStreamChunk};
use crate::converter::FormatGenerator;
use crate::error::ProxyError;
use serde_json::{json, Value};

pub struct GeminiGenerator;

impl FormatGenerator for GeminiGenerator {
    fn generate_request(&self, ir: &IrRequest) -> Result<Value, ProxyError> {
        let mut system_instruction: Option<Value> = None;
        let mut contents: Vec<Value> = Vec::new();

        for msg in &ir.messages {
            match msg.role {
                IrRole::System | IrRole::Developer => {
                    let text = extract_text_parts(&msg.content);
                    system_instruction = Some(json!({
                        "parts": [{ "text": text }]
                    }));
                }
                _ => {
                    let role = match msg.role {
                        IrRole::User => "user",
                        IrRole::Assistant => "model",
                        IrRole::Tool => "user",
                        _ => "user",
                    };

                    let parts = convert_gemini_parts(&msg.content);

                    contents.push(json!({
                        "role": role,
                        "parts": parts,
                    }));
                }
            }
        }

        let mut body = json!({
            "contents": contents,
        });

        if let Some(si) = system_instruction {
            body["systemInstruction"] = si;
        }

        let mut generation_config = json!({});
        let mut has_config = false;

        if let Some(temperature) = ir.temperature {
            generation_config["temperature"] = json!(round2(temperature));
            has_config = true;
        }

        if let Some(top_p) = ir.top_p {
            generation_config["topP"] = json!(round2(top_p));
            has_config = true;
        }

        if let Some(top_k) = ir.top_k {
            generation_config["topK"] = json!(top_k);
            has_config = true;
        }

        if let Some(max_tokens) = ir.max_tokens {
            generation_config["maxOutputTokens"] = json!(max_tokens);
            has_config = true;
        }

        if let Some(stop) = &ir.stop_sequences {
            generation_config["stopSequences"] = json!(stop);
            has_config = true;
        }

        if let Some(response_format) = &ir.response_format {
            if let Some(rt) = response_format.get("type").and_then(|t| t.as_str()) {
                match rt {
                    "json_object" => {
                        generation_config["responseMimeType"] = json!("application/json");
                    }
                    "json_schema" => {
                        generation_config["responseMimeType"] = json!("application/json");
                        if let Some(schema) = response_format.get("json_schema") {
                            if let Some(schema_value) = schema.get("schema") {
                                generation_config["responseSchema"] = schema_value.clone();
                            }
                        }
                    }
                    _ => {}
                }
                has_config = true;
            }
        }

        if has_config {
            body["generationConfig"] = generation_config;
        }

        if let Some(tools) = &ir.tools {
            let function_declarations: Vec<Value> = tools
                .iter()
                .map(|t| {
                    let mut decl = json!({
                        "name": t.name,
                        "parameters": t.input_schema,
                    });
                    if let Some(desc) = &t.description {
                        decl["description"] = json!(desc);
                    }
                    decl
                })
                .collect();
            body["tools"] = json!([{
                "function_declarations": function_declarations,
            }]);
        }

        if let Some(tool_choice) = &ir.tool_choice {
            let mode = match tool_choice {
                Value::String(s) => match s.as_str() {
                    "auto" => "AUTO",
                    "none" => "NONE",
                    "required" => "ANY",
                    _ => "AUTO",
                },
                Value::Object(obj) => {
                    if obj.get("type").and_then(|v| v.as_str()) == Some("function") {
                        "ANY"
                    } else {
                        "AUTO"
                    }
                }
                _ => "AUTO",
            };
            body["toolConfig"] = json!({
                "function_calling_config": { "mode": mode }
            });
        }

        // Pass through any extra parameters
        if !ir.extra.is_empty() {
            tracing::debug!("GeminiGenerator injecting {} extra field(s) into upstream body", ir.extra.len());
        }
        for (key, val) in &ir.extra {
            tracing::trace!("GeminiGenerator extra: {} = {}", key, val);
            body[key] = val.clone();
        }

        Ok(body)
    }

    fn generate_stream_chunk(&self, chunk: &IrStreamChunk) -> String {
        let mut parts: Vec<Value> = Vec::new();

        if let Some(content) = &chunk.delta_content {
            parts.push(json!({ "text": content }));
        }

        if let Some(tool_calls) = &chunk.delta_tool_calls {
            for tc in tool_calls {
                if let Some(args_str) = &tc.arguments {
                    let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                    parts.push(json!({
                        "functionCall": {
                            "name": tc.name.as_deref().unwrap_or(""),
                            "args": args,
                        }
                    }));
                }
            }
        }

        if parts.is_empty() && chunk.delta_thinking.is_none() {
            parts.push(json!({ "text": "" }));
        }

        let mut candidate = json!({
            "content": {
                "role": "model",
                "parts": parts,
            },
        });

        if let Some(reason) = &chunk.finish_reason {
            let gemini_reason = match reason.as_str() {
                "stop" | "end_turn" | "completed" => "STOP",
                "max_tokens" | "length" => "MAX_TOKENS",
                "tool_calls" | "tool_use" => "STOP",
                _ => "STOP",
            };
            candidate["finishReason"] = json!(gemini_reason);
        }

        let mut chunk_data = json!({
            "candidates": [candidate],
        });

        if let Some(usage) = &chunk.usage {
            let mut meta = json!({
                "promptTokenCount": usage.prompt_tokens,
                "candidatesTokenCount": usage.completion_tokens,
                "totalTokenCount": usage.total_tokens,
            });
            if usage.cached_tokens > 0 {
                meta["cachedContentTokenCount"] = json!(usage.cached_tokens);
            }
            chunk_data["usageMetadata"] = meta;
        }

        format!("data: {}\n\n", chunk_data)
    }

    fn generate_response(&self, ir: &IrResponse) -> Result<Value, ProxyError> {
        let text = extract_text_parts(&ir.message.content);

        let mut parts: Vec<Value> = Vec::new();

        if !text.is_empty() {
            parts.push(json!({ "text": text }));
        }

        if let Some(tool_calls) = &ir.message.tool_calls {
            for tc in tool_calls {
                parts.push(json!({
                    "functionCall": {
                        "name": tc.name,
                        "args": serde_json::from_str::<Value>(&tc.arguments)
                            .unwrap_or(json!({})),
                    }
                }));
            }
        }

        if parts.is_empty() {
            parts.push(json!({ "text": "" }));
        }

        let mut candidate = json!({
            "content": {
                "role": "model",
                "parts": parts,
            },
        });

        if let Some(reason) = &ir.finish_reason {
            let gemini_reason = match reason.as_str() {
                "stop" | "end_turn" | "completed" => "STOP",
                "max_tokens" | "length" => "MAX_TOKENS",
                "tool_calls" | "tool_use" => "STOP",
                _ => "STOP",
            };
            candidate["finishReason"] = json!(gemini_reason);
        } else {
            candidate["finishReason"] = json!("STOP");
        }

        let mut usage_meta = json!({
            "promptTokenCount": ir.usage.prompt_tokens,
            "candidatesTokenCount": ir.usage.completion_tokens,
            "totalTokenCount": ir.usage.total_tokens,
        });
        if ir.usage.cached_tokens > 0 {
            usage_meta["cachedContentTokenCount"] = json!(ir.usage.cached_tokens);
        }

        Ok(json!({
            "candidates": [candidate],
            "usageMetadata": usage_meta,
        }))
    }
}

fn extract_text_parts(parts: &[IrContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            IrContentPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn convert_gemini_parts(parts: &[IrContentPart]) -> Vec<Value> {
    parts
        .iter()
        .map(|part| match part {
            IrContentPart::Text { text, .. } => json!({ "text": text }),
            IrContentPart::Thinking { text, .. } => json!({ "text": text }),
            IrContentPart::Image {
                url,
                data,
                media_type,
            } => {
                if let Some(image_url) = url {
                    json!({
                        "file_data": {
                            "file_uri": image_url,
                            "mime_type": media_type.as_deref().unwrap_or("image/png"),
                        }
                    })
                } else if let Some(b64) = data {
                    json!({
                        "inline_data": {
                            "mime_type": media_type.as_deref().unwrap_or("image/png"),
                            "data": b64,
                        }
                    })
                } else {
                    json!({ "text": "" })
                }
            }
            IrContentPart::Compaction { .. } => serde_json::Value::Null,
            IrContentPart::ToolUse { name, input, .. } => {
                json!({
                    "functionCall": {
                        "name": name,
                        "args": input,
                    }
                })
            }
            IrContentPart::ToolResult {
                tool_use_id,
                content,
                tool_name,
                ..
            } => {
                let name = tool_name.as_deref().unwrap_or(tool_use_id);
                json!({
                    "functionResponse": {
                        "name": name,
                        "response": {
                            "result": content,
                        }
                    }
                })
            }
        })
        .filter(|v| !v.is_null())
        .collect()
}
