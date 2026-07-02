use ai_proxy_lib::converter::generators::anthropic::AnthropicGenerator;
use ai_proxy_lib::converter::generators::completions::CompletionsGenerator;
use ai_proxy_lib::converter::generators::gemini::GeminiGenerator;
use ai_proxy_lib::converter::generators::responses::ResponsesGenerator;
use ai_proxy_lib::converter::ir::*;
use ai_proxy_lib::converter::parsers::anthropic::AnthropicParser;
use ai_proxy_lib::converter::parsers::completions::CompletionsParser;
use ai_proxy_lib::converter::parsers::gemini::GeminiParser;
use ai_proxy_lib::converter::parsers::responses::ResponsesParser;
use ai_proxy_lib::converter::{FormatGenerator, FormatParser};
use serde_json::json;

fn sample_ir_request() -> IrRequest {
    IrRequest {
        model: "gpt-4o".into(),
        messages: vec![
            IrMessage {
                role: IrRole::System,
                content: vec![IrContentPart::Text {
                    text: "You are helpful.".into(),
                    citations: None,
                }],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
            IrMessage {
                role: IrRole::User,
                content: vec![IrContentPart::Text {
                    text: "Hello!".into(),
                    citations: None,
                }],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
        ],
        tools: None,
        tool_choice: None,
        temperature: Some(0.7),
        top_p: None,
        top_k: None,
        max_tokens: Some(1024),
        stream: false,
        stop_sequences: None,
        response_format: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        thinking: None,
        stream_options: None,
        metadata: std::collections::HashMap::new(),
        extra: std::collections::HashMap::new(),
    }
}

fn sample_ir_response() -> IrResponse {
    IrResponse {
        id: Some("resp-123".into()),
        model: Some("gpt-4o".into()),
        message: IrMessage {
            role: IrRole::Assistant,
            content: vec![IrContentPart::Text {
                text: "Hi there!".into(),
                citations: None,
            }],
            name: None,
            tool_call_id: None,
            tool_calls: None,
        },
        finish_reason: Some("stop".into()),
        stop_sequence: None,
        usage: IrUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            cached_tokens: 0,
            cache_creation_input_tokens: 0,
            thinking_tokens: 0,
            raw: None,
        },
    }
}

fn assert_request_roundtrip(parsed: &IrRequest, original: &IrRequest) {
    assert_eq!(parsed.model, original.model);
    assert_eq!(parsed.messages.len(), original.messages.len());
    for (p, o) in parsed.messages.iter().zip(original.messages.iter()) {
        assert_eq!(
            std::mem::discriminant(&p.role),
            std::mem::discriminant(&o.role)
        );
        assert_eq!(p.content.len(), o.content.len());
        for (pc, oc) in p.content.iter().zip(o.content.iter()) {
            match (pc, oc) {
                (IrContentPart::Text { text: t1, .. }, IrContentPart::Text { text: t2, .. }) => {
                    assert_eq!(t1, t2);
                }
                _ => panic!("content part type mismatch"),
            }
        }
    }
    assert_eq!(parsed.temperature, original.temperature);
    assert_eq!(parsed.max_tokens, original.max_tokens);
    assert_eq!(parsed.stream, original.stream);
}

fn assert_response_roundtrip(parsed: &IrResponse, original: &IrResponse) {
    assert_eq!(parsed.message.content.len(), original.message.content.len());
    for (p, o) in parsed
        .message
        .content
        .iter()
        .zip(original.message.content.iter())
    {
        match (p, o) {
            (IrContentPart::Text { text: t1, .. }, IrContentPart::Text { text: t2, .. }) => {
                assert_eq!(t1, t2);
            }
            _ => panic!("content type mismatch"),
        }
    }
    assert_eq!(parsed.usage.prompt_tokens, original.usage.prompt_tokens);
    assert_eq!(
        parsed.usage.completion_tokens,
        original.usage.completion_tokens
    );
}

fn run_roundtrip<P: FormatParser, G: FormatGenerator>(parser: P, generator: G) {
    let ir_req = sample_ir_request();
    let ir_resp = sample_ir_response();

    let generated_req = generator.generate_request(&ir_req).unwrap();
    let parsed_req = parser.parse_request(&generated_req).unwrap();
    assert_request_roundtrip(&parsed_req, &ir_req);

    let generated_resp = generator.generate_response(&ir_resp).unwrap();
    let parsed_resp = parser.parse_response(&generated_resp).unwrap();
    assert_response_roundtrip(&parsed_resp, &ir_resp);
}

#[test]
fn completions_roundtrip() {
    run_roundtrip(CompletionsParser, CompletionsGenerator);
}

#[test]
fn responses_roundtrip() {
    run_roundtrip(ResponsesParser, ResponsesGenerator);
}

#[test]
fn anthropic_roundtrip() {
    run_roundtrip(AnthropicParser, AnthropicGenerator);
}

#[test]
fn gemini_roundtrip() {
    let ir_req = sample_ir_request();
    let ir_resp = sample_ir_response();

    let generator = GeminiGenerator;
    let parser = GeminiParser;

    let generated_req = generator.generate_request(&ir_req).unwrap();
    let parsed_req = parser.parse_request(&generated_req).unwrap();
    assert_eq!(parsed_req.model, "");
    assert_eq!(parsed_req.messages.len(), ir_req.messages.len());

    let generated_resp = generator.generate_response(&ir_resp).unwrap();
    let parsed_resp = parser.parse_response(&generated_resp).unwrap();
    assert_response_roundtrip(&parsed_resp, &ir_resp);
}

#[test]
fn completions_stream_chunk_done() {
    let parser = CompletionsParser;
    let chunk = parser.parse_stream_chunk("data: [DONE]").unwrap();
    assert!(chunk.is_some());
    let c = chunk.unwrap();
    assert_eq!(c.finish_reason.as_deref(), Some("stop"));
}

#[test]
fn completions_stream_chunk_data() {
    let parser = CompletionsParser;
    let input = r#"data: {"id":"chatcmpl-1","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}]}"#;
    let chunk = parser.parse_stream_chunk(input).unwrap().unwrap();
    assert_eq!(chunk.id.as_deref(), Some("chatcmpl-1"));
    assert_eq!(chunk.delta_content.as_deref(), Some("Hi"));
}

#[test]
fn completions_tool_calls_request() {
    let generator = CompletionsGenerator;
    let ir = IrRequest {
        model: "gpt-4o".into(),
        messages: vec![IrMessage {
            role: IrRole::User,
            content: vec![IrContentPart::Text {
                text: "What's the weather?".into(),
                citations: None,
            }],
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }],
        tools: Some(vec![IrTool {
            name: "get_weather".into(),
            description: Some("Get current weather".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                },
                "required": ["location"]
            }),
            strict: None,
        }]),
        tool_choice: None,
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        stream: false,
        stop_sequences: None,
        response_format: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        thinking: None,
        stream_options: None,
        metadata: std::collections::HashMap::new(),
        extra: std::collections::HashMap::new(),
    };

    let body = generator.generate_request(&ir).unwrap();
    let tools = body.get("tools").unwrap().as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0]["function"]["name"].as_str().unwrap(),
        "get_weather"
    );

    let parser = CompletionsParser;
    let parsed = parser.parse_request(&body).unwrap();
    assert!(parsed.tools.is_some());
    assert_eq!(parsed.tools.unwrap().len(), 1);
}

#[test]
fn responses_json_schema_is_downgraded_to_json_object_for_completions() {
    let generator = CompletionsGenerator;
    let mut ir = sample_ir_request();
    ir.response_format = Some(json!({
        "type": "json_schema",
        "name": "codex_output",
        "schema": {
            "type": "object",
            "properties": {
                "summary": { "type": "string" }
            },
            "required": ["summary"]
        },
        "strict": true
    }));

    let body = generator.generate_request(&ir).unwrap();
    let response_format = body
        .get("response_format")
        .expect("response_format should exist");
    assert_eq!(response_format, &json!({ "type": "json_object" }));
}

#[test]
fn completions_json_schema_is_downgraded_to_json_object() {
    let generator = CompletionsGenerator;
    let mut ir = sample_ir_request();
    ir.response_format = Some(json!({
        "type": "json_schema",
        "json_schema": {
            "name": "codex_output",
            "schema": { "type": "object" },
            "strict": true
        }
    }));

    let body = generator.generate_request(&ir).unwrap();
    let response_format = body
        .get("response_format")
        .expect("response_format should exist");

    assert_eq!(response_format, &json!({ "type": "json_object" }));
}

#[test]
fn invalid_json_schema_response_format_is_downgraded() {
    let generator = CompletionsGenerator;
    let mut ir = sample_ir_request();
    ir.response_format = Some(json!({
        "type": "json_schema"
    }));

    let body = generator.generate_request(&ir).unwrap();
    let response_format = body
        .get("response_format")
        .expect("response_format should exist");
    assert_eq!(response_format, &json!({ "type": "json_object" }));
}

#[test]
fn responses_stream_field_roundtrip() {
    let mut ir = sample_ir_request();
    ir.stream = true;

    let generator = ResponsesGenerator;
    let body = generator.generate_request(&ir).unwrap();

    assert_eq!(body["stream"].as_bool(), Some(true));

    let parser = ResponsesParser;
    let parsed = parser.parse_request(&body).unwrap();
    assert!(parsed.stream);
}

#[test]
fn responses_parser_json_schema_roundtrip_to_completions() {
    let parser = ResponsesParser;
    let generator = CompletionsGenerator;
    let body = json!({
        "model": "gpt-5.4",
        "input": "hello",
        "text": {
            "format": {
                "type": "json_schema",
                "name": "codex_output",
                "schema": {
                    "type": "object",
                    "properties": {
                        "summary": { "type": "string" }
                    },
                    "required": ["summary"]
                },
                "strict": true
            }
        }
    });

    let ir = parser.parse_request(&body).unwrap();
    let generated = generator.generate_request(&ir).unwrap();
    let response_format = generated
        .get("response_format")
        .expect("response_format should exist");

    assert_eq!(response_format, &json!({ "type": "json_object" }));
}

#[test]
fn cross_format_completions_to_anthropic() {
    let ir = sample_ir_request();

    let gen = CompletionsGenerator;
    let comp_body = gen.generate_request(&ir).unwrap();
    assert!(comp_body.get("messages").is_some());

    let gen = AnthropicGenerator;
    let ant_body = gen.generate_request(&ir).unwrap();
    assert!(ant_body.get("messages").is_some());
    assert_eq!(ant_body["model"].as_str().unwrap(), "gpt-4o");
}

#[test]
fn cross_format_completions_to_gemini() {
    let ir = sample_ir_request();

    let gen = GeminiGenerator;
    let gem_body = gen.generate_request(&ir).unwrap();
    assert!(gem_body.get("contents").is_some());
    assert!(gem_body.get("systemInstruction").is_some());
}

#[test]
fn anthropic_response_with_tool_use() {
    let body = json!({
        "id": "msg-1",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "text", "text": "Let me check." },
            {
                "type": "tool_use",
                "id": "tool-1",
                "name": "get_weather",
                "input": { "location": "Tokyo" }
            }
        ],
        "model": "claude-3",
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": 20,
            "output_tokens": 30
        }
    });

    let parser = AnthropicParser;
    let ir = parser.parse_response(&body).unwrap();

    assert_eq!(ir.message.content.len(), 1);
    assert!(matches!(ir.message.content[0], IrContentPart::Text { .. }));

    let tool_calls = ir.message.tool_calls.as_ref().expect("expected tool_calls");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].name, "get_weather");
    assert_eq!(tool_calls[0].id, "tool-1");
}

#[test]
fn model_pattern_matching() {
    assert!(model_matches("gpt-4o", "gpt-4o"));
    assert!(model_matches("gpt-4o", "*"));
    assert!(model_matches("gpt-4o-mini", "gpt-4o*"));
    assert!(!model_matches("claude-3", "gpt-4o*"));
    assert!(model_matches("claude-3-opus", "*opus"));
    assert!(model_matches("gpt-4o", "gpt-4o*"));
    assert!(!model_matches("gpt-4o", "gpt-4o-mini*"));
    // contains matching with *prefix*suffix*
    assert!(model_matches("claude-sonnet-4-20250514", "*sonnet*"));
    assert!(model_matches("claude-opus-4-20250514", "*opus*"));
    assert!(model_matches("claude-haiku-3-5-20241022", "*haiku*"));
    assert!(!model_matches("gpt-4o", "*sonnet*"));
    assert!(!model_matches("claude-sonnet-4", "*sonnet-4-20250514"));
}

fn model_matches(model: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return model == pattern;
    }
    let starts_star = pattern.starts_with('*');
    let ends_star = pattern.ends_with('*');
    let parts: Vec<&str> = pattern.split('*').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }
    if !starts_star && !model.starts_with(parts[0]) {
        return false;
    }
    if !ends_star && !model.ends_with(parts.last().unwrap()) {
        return false;
    }
    let mut pos = 0;
    for part in &parts {
        match model[pos..].find(part) {
            Some(found) => pos += found + part.len(),
            None => return false,
        }
    }
    true
}

/// Compaction items (from /v1/responses/compact) must round-trip through the
/// Responses parser → IR → Responses generator without losing encrypted_content.
#[test]
fn responses_compaction_round_trip() {
    let parser = ResponsesParser;
    let generator = ResponsesGenerator;

    // Simulate a request that includes a compaction item in the input array
    // (what Codex sends after a compact, referencing the previous compaction).
    let request_body = json!({
        "model": "gpt-5-codex",
        "input": [
            {
                "type": "compaction",
                "id": "cmp_abc123",
                "encrypted_content": "gAAAAABpM0Yj-fake-encrypted-content=="
            },
            {
                "role": "user",
                "content": "Continue working"
            }
        ]
    });

    let ir = parser.parse_request(&request_body).unwrap();
    let regenerated = generator.generate_request(&ir).unwrap();

    let input = regenerated["input"].as_array().unwrap();
    // First item should be the compaction
    let compaction_item = &input[0];
    assert_eq!(compaction_item["type"], "compaction");
    assert_eq!(compaction_item["id"], "cmp_abc123");
    assert_eq!(
        compaction_item["encrypted_content"],
        "gAAAAABpM0Yj-fake-encrypted-content=="
    );
}

/// Compaction output items in a response must survive parsing and regeneration.
#[test]
fn responses_compaction_output_round_trip() {
    let parser = ResponsesParser;
    let generator = ResponsesGenerator;

    // Simulate a compact response with a compaction output item.
    let response_body = json!({
        "id": "resp_001",
        "object": "response.compaction",
        "status": "completed",
        "output": [
            {
                "type": "compaction",
                "id": "cmp_001",
                "encrypted_content": "gAAAAABpM0Yj-encrypted-data=="
            }
        ],
        "usage": {
            "input_tokens": 100,
            "output_tokens": 50,
            "total_tokens": 150
        }
    });

    let ir = parser.parse_response(&response_body).unwrap();

    // Verify compaction content was extracted
    let has_compaction = ir.message.content.iter().any(|p| {
        matches!(
            p,
            IrContentPart::Compaction { id, encrypted_content }
                if id == "cmp_001" && encrypted_content == "gAAAAABpM0Yj-encrypted-data=="
        )
    });
    assert!(has_compaction, "compaction item should be in IR content");

    // Regenerate and verify
    let regenerated = generator.generate_response(&ir).unwrap();
    let output = regenerated["output"].as_array().unwrap();
    let compaction_output = output
        .iter()
        .find(|o| o["type"] == "compaction")
        .expect("compaction output item should exist");
    assert_eq!(compaction_output["id"], "cmp_001");
    assert_eq!(
        compaction_output["encrypted_content"],
        "gAAAAABpM0Yj-encrypted-data=="
    );
}

#[test]
fn completions_extra_fields_roundtrip() {
    // Test that extra/unknown fields like chat_template_kwargs pass through
    let body = json!({
        "model": "deepseek-chat",
        "messages": [
            { "role": "user", "content": "hello" }
        ],
        "stream": false,
        "chat_template_kwargs": {
            "enable_thinking": false
        }
    });

    // Parse
    let parser = CompletionsParser;
    let ir = parser.parse_request(&body).unwrap();

    // Verify extra was captured
    let kwargs = ir.extra.get("chat_template_kwargs").expect("chat_template_kwargs should be in extra");
    assert_eq!(
        kwargs,
        &json!({ "enable_thinking": false })
    );

    // Generate back to completions format
    let generator = CompletionsGenerator;
    let output = generator.generate_request(&ir).unwrap();

    // Verify extra field is in the output
    let output_kwargs = output.get("chat_template_kwargs").expect("chat_template_kwargs should be in generated body");
    assert_eq!(
        output_kwargs,
        &json!({ "enable_thinking": false })
    );

    // Verify standard fields are still present
    assert_eq!(output["model"], "deepseek-chat");
    assert!(output.get("messages").is_some());

    // Verify the serialized JSON contains the field
    let serialized = serde_json::to_string(&output).unwrap();
    assert!(
        serialized.contains("chat_template_kwargs"),
        "serialized JSON should contain chat_template_kwargs, got: {}",
        serialized
    );
    assert!(
        serialized.contains("enable_thinking"),
        "serialized JSON should contain enable_thinking, got: {}",
        serialized
    );

    // Full end-to-end: also test that the serializer produces valid request JSON
    eprintln!("FULL JSON OUTPUT: {}", serialized);
}
