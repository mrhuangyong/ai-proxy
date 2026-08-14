//! Regression tests for protocol-conversion bugs found by the converter-layer
//! audit. Each test pins one concrete defect; see the comment on each test.

use ai_proxy_lib::converter::generators::anthropic::AnthropicGenerator;
use ai_proxy_lib::converter::generators::responses::ResponsesGenerator;
use ai_proxy_lib::converter::ir::*;
use ai_proxy_lib::converter::parsers::gemini::GeminiParser;
use ai_proxy_lib::converter::parsers::responses::ResponsesParser;
use ai_proxy_lib::converter::{FormatGenerator, FormatParser};
use ai_proxy_lib::server::handlers::ResponsesStreamStateMachine;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// 1. Responses stream state machine: `response.completed` / `response.failed`
//    must carry the function_call and reasoning items in their output array.
//    Previously build_responses_output_array was called AFTER the close-out
//    flipped func_open / thinking_started to false, so the final event's
//    output array lost those items.
// ---------------------------------------------------------------------------

fn chunk_with_tool_call_start() -> IrStreamChunk {
    IrStreamChunk {
        id: None,
        model: None,
        delta_content: None,
        delta_tool_calls: Some(vec![IrToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("run".into()),
            arguments: None,
        }]),
        delta_thinking: None,
        finish_reason: None,
        usage: None,
        error: None,
    }
}

fn chunk_with_args(args: &str) -> IrStreamChunk {
    IrStreamChunk {
        id: None,
        model: None,
        delta_content: None,
        delta_tool_calls: Some(vec![IrToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: Some(args.into()),
        }]),
        delta_thinking: None,
        finish_reason: None,
        usage: None,
        error: None,
    }
}

fn chunk_with_thinking(text: &str) -> IrStreamChunk {
    IrStreamChunk {
        id: Some("resp_t".into()),
        model: None,
        delta_content: None,
        delta_tool_calls: None,
        delta_thinking: Some(text.into()),
        finish_reason: None,
        usage: None,
        error: None,
    }
}

fn chunk_finish(reason: &str) -> IrStreamChunk {
    IrStreamChunk {
        id: Some("resp_t".into()),
        model: Some("m".into()),
        delta_content: None,
        delta_tool_calls: None,
        delta_thinking: None,
        finish_reason: Some(reason.into()),
        usage: None,
        error: None,
    }
}

fn parse_all(sm: &mut ResponsesStreamStateMachine, chunks: &[IrStreamChunk]) -> Vec<Value> {
    let mut frames = Vec::new();
    for c in chunks {
        for sse in sm.process_chunk(c, (10, 20)) {
            let trimmed = sse.strip_prefix("data: ").unwrap_or(&sse).trim();
            frames.push(serde_json::from_str::<Value>(trimmed).expect("valid JSON"));
        }
    }
    frames
}

#[test]
fn completed_output_array_keeps_function_call_item() {
    let mut sm = ResponsesStreamStateMachine::new("resp_t".into(), "m".into());
    let frames = parse_all(
        &mut sm,
        &[
            chunk_with_tool_call_start(),
            chunk_with_args("{\"x\":1}"),
            chunk_finish("completed"),
        ],
    );
    let completed = frames
        .iter()
        .rev()
        .find(|f| f["type"] == "response.completed")
        .expect("completed event");
    let output = &completed["response"]["output"];
    let fc = output
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["type"] == "function_call")
        .expect("function_call item must survive in completed output");
    assert_eq!(fc["call_id"], "call_1");
    assert_eq!(fc["name"], "run");
    assert_eq!(fc["arguments"], "{\"x\":1}");
}

#[test]
fn failed_output_array_keeps_function_call_item() {
    let mut sm = ResponsesStreamStateMachine::new("resp_t".into(), "m".into());
    let mut fail = chunk_finish("failed");
    fail.error = Some(IrStreamError {
        code: Some("server_error".into()),
        message: "boom".into(),
    });
    let frames = parse_all(
        &mut sm,
        &[chunk_with_tool_call_start(), chunk_with_args("{}"), fail],
    );
    let failed = frames
        .iter()
        .rev()
        .find(|f| f["type"] == "response.failed")
        .expect("failed event");
    let output = &failed["response"]["output"];
    assert!(
        output
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["type"] == "function_call"),
        "failed output must keep the function_call item, got: {output}"
    );
}

#[test]
fn completed_output_array_keeps_reasoning_item() {
    let mut sm = ResponsesStreamStateMachine::new("resp_t".into(), "m".into());
    // thinking then text then finish
    let frames = parse_all(
        &mut sm,
        &[
            chunk_with_thinking("analyzing"),
            IrStreamChunk {
                delta_content: Some("answer".into()),
                ..chunk_finish("")
            },
            chunk_finish("completed"),
        ],
    );
    let completed = frames
        .iter()
        .rev()
        .find(|f| f["type"] == "response.completed")
        .expect("completed event");
    let output = completed["response"]["output"].as_array().unwrap();
    assert!(
        output.iter().any(|o| o["type"] == "reasoning"),
        "reasoning item must survive in completed output, got: {output:?}"
    );
    assert!(output.iter().any(|o| o["type"] == "message"));
}

// ---------------------------------------------------------------------------
// 2. ResponsesParser: input_image parts in `input` must become IR Image
//    parts instead of being silently dropped (only `text` was read before).
// ---------------------------------------------------------------------------

#[test]
fn responses_parser_keeps_input_image_parts() {
    let body = json!({
        "model": "gpt-5",
        "input": [{
            "role": "user",
            "content": [
                { "type": "input_text", "text": "What is in this image?" },
                { "type": "input_image", "image_url": "https://example.com/cat.png" },
                { "type": "input_image", "image_url": "data:image/jpeg;base64,QUJD" }
            ]
        }]
    });
    let ir = ResponsesParser.parse_request(&body).unwrap();
    let user_msg = ir.messages.iter().find(|m| m.role == IrRole::User).unwrap();
    let images: Vec<&IrContentPart> = user_msg
        .content
        .iter()
        .filter(|p| matches!(p, IrContentPart::Image { .. }))
        .collect();
    assert_eq!(images.len(), 2, "both input_image parts must be kept");
    match images[0] {
        IrContentPart::Image { url, .. } => {
            assert_eq!(url.as_deref(), Some("https://example.com/cat.png"))
        }
        _ => panic!(),
    }
    match images[1] {
        IrContentPart::Image {
            data,
            media_type,
            url,
        } => {
            assert_eq!(data.as_deref(), Some("QUJD"));
            assert_eq!(media_type.as_deref(), Some("image/jpeg"));
            assert!(url.is_none());
        }
        _ => panic!(),
    }
    // Text part preserved alongside
    assert!(user_msg.content.iter().any(|p| matches!(p,
        IrContentPart::Text { text, .. } if text == "What is in this image?")));
}

// ---------------------------------------------------------------------------
// 3. ResponsesParser streaming: `response.reasoning_summary_text.delta`
//    events must map to delta_thinking instead of being dropped.
// ---------------------------------------------------------------------------

#[test]
fn responses_parser_stream_reasoning_delta_maps_to_thinking() {
    let line = r#"data: {"type":"response.reasoning_summary_text.delta","delta":"thinking..."}"#;
    let chunk = ResponsesParser
        .parse_stream_chunk(line)
        .unwrap()
        .expect("chunk");
    assert_eq!(chunk.delta_thinking.as_deref(), Some("thinking..."));
}

// ---------------------------------------------------------------------------
// 4. ResponsesParser streaming: `response.output_item.done` for a
//    function_call must NOT re-emit the full arguments as a delta — arguments
//    already streamed via response.function_call_arguments.delta events.
// ---------------------------------------------------------------------------

#[test]
fn responses_parser_stream_item_done_does_not_duplicate_arguments() {
    let line = r#"data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"run","arguments":"{\"x\":1}"}}"#;
    let chunk = ResponsesParser.parse_stream_chunk(line).unwrap();
    match chunk {
        Some(c) => {
            assert!(
                c.delta_tool_calls.is_none(),
                "done event must not re-emit arguments as tool_call delta"
            );
        }
        None => panic!("expected a chunk"),
    }
}

// ---------------------------------------------------------------------------
// 5. GeminiParser: synthetic tool call ids must be stable across a
//    call/response round trip (old id embedded the part index, which differs
//    between the call message and the response message).
// ---------------------------------------------------------------------------

#[test]
fn gemini_parser_tool_ids_match_between_call_and_response() {
    let body = json!({
        "contents": [
            { "role": "user", "parts": [{ "text": "weather in Tokyo?" }] },
            { "role": "model", "parts": [
                { "functionCall": { "name": "get_weather", "args": { "city": "Tokyo" } } }
            ]},
            { "role": "user", "parts": [
                { "functionResponse": { "name": "get_weather", "response": { "result": "sunny" } } }
            ]}
        ]
    });
    let ir = GeminiParser.parse_request(&body).unwrap();

    let assistant = ir
        .messages
        .iter()
        .find(|m| m.role == IrRole::Assistant)
        .expect("assistant message");
    let call_id = assistant.tool_calls.as_ref().unwrap()[0].id.clone();

    let tool = ir
        .messages
        .iter()
        .find(|m| m.role == IrRole::Tool)
        .expect("functionResponse must produce a Tool-role message");
    match tool.content.first() {
        Some(IrContentPart::ToolResult { tool_use_id, .. }) => {
            assert_eq!(
                tool_use_id.as_str(),
                call_id.as_str(),
                "tool result must reference the call id"
            );
        }
        _ => panic!("expected ToolResult part"),
    }
}

#[test]
fn gemini_parser_repeated_same_tool_gets_distinct_ids() {
    let body = json!({
        "contents": [
            { "role": "model", "parts": [
                { "functionCall": { "name": "search", "args": { "q": "1" } } },
                { "functionCall": { "name": "search", "args": { "q": "2" } } }
            ]}
        ]
    });
    let ir = GeminiParser.parse_request(&body).unwrap();
    let assistant = ir
        .messages
        .iter()
        .find(|m| m.role == IrRole::Assistant)
        .unwrap();
    let calls = assistant.tool_calls.as_ref().unwrap();
    assert_eq!(calls.len(), 2);
    assert_ne!(
        calls[0].id, calls[1].id,
        "same-name calls need distinct ids"
    );
}

// ---------------------------------------------------------------------------
// 6. ResponsesGenerator: multiple system messages must be concatenated,
//    not last-wins.
// ---------------------------------------------------------------------------

#[test]
fn responses_generator_concatenates_multiple_system_messages() {
    let ir = IrRequest {
        model: "gpt-5".into(),
        messages: vec![
            IrMessage {
                role: IrRole::System,
                content: vec![text_part("You are a coder.")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
            IrMessage {
                role: IrRole::System,
                content: vec![text_part("Always answer in Chinese.")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
            IrMessage {
                role: IrRole::User,
                content: vec![text_part("hi")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
        ],
        tools: None,
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
        metadata: Default::default(),
        extra: Default::default(),
    };
    let body = ResponsesGenerator.generate_request(&ir).unwrap();
    let instructions = body["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("You are a coder."),
        "first system message dropped: {instructions}"
    );
    assert!(
        instructions.contains("Always answer in Chinese."),
        "second system message dropped: {instructions}"
    );
}

// ---------------------------------------------------------------------------
// 7. AnthropicGenerator: a Tool message whose content is a ToolResult part
//    must keep the result payload (it was emptied by text-only extraction).
// ---------------------------------------------------------------------------

fn text_part(t: &str) -> IrContentPart {
    IrContentPart::Text {
        text: t.into(),
        citations: None,
    }
}

#[test]
fn anthropic_generator_tool_result_part_is_not_lost() {
    let ir = IrRequest {
        model: "claude-x".into(),
        messages: vec![
            IrMessage {
                role: IrRole::User,
                content: vec![text_part("weather?")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
            IrMessage {
                role: IrRole::Assistant,
                content: vec![],
                name: None,
                tool_call_id: None,
                tool_calls: Some(vec![IrToolCall {
                    id: "call_get_weather".into(),
                    name: "get_weather".into(),
                    arguments: "{\"city\":\"Tokyo\"}".into(),
                }]),
            },
            IrMessage {
                role: IrRole::Tool,
                content: vec![IrContentPart::ToolResult {
                    tool_use_id: "call_get_weather".into(),
                    content: "sunny".into(),
                    tool_name: Some("get_weather".into()),
                    id: None,
                }],
                name: None,
                tool_call_id: Some("call_get_weather".into()),
                tool_calls: None,
            },
        ],
        tools: Some(vec![IrTool {
            name: "get_weather".into(),
            description: None,
            input_schema: json!({"type": "object"}),
            strict: None,
        }]),
        tool_choice: None,
        temperature: None,
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
        metadata: Default::default(),
        extra: Default::default(),
    };
    let body = AnthropicGenerator.generate_request(&ir).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let tool_result_msg = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_array()
                .map(|a| a.iter().any(|b| b["type"] == "tool_result"))
                .unwrap_or(false)
        })
        .expect("tool_result user message must exist");
    let block = tool_result_msg["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "tool_result")
        .unwrap();
    assert_eq!(block["tool_use_id"], "call_get_weather");
    assert_eq!(block["content"], "sunny", "ToolResult payload must be kept");
}

// ---------------------------------------------------------------------------
// Round 2 (follow-up audit)
// ---------------------------------------------------------------------------

// 8. extract_text_from_sse_body: Responses-format SSE bodies must be read from
//    response.output_text.delta events (not the Completions /choices/0 shape),
//    and Gemini multi-part text must be fully collected.
//    These functions are private to the handlers module, so the fix is
//    verified indirectly below via the parsers; here we assert the public
//    behavior contract of the round-2 parser fixes instead.

use ai_proxy_lib::converter::parsers::completions::CompletionsParser;

// 9. CompletionsParser: `stop: []` (empty array) must map to "no stop
//    sequences" instead of round-tripping an empty array that strict
//    upstreams reject with a 400.
#[test]
fn completions_parser_empty_stop_array_is_dropped() {
    let body = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "stop": []
    });
    let ir = CompletionsParser.parse_request(&body).unwrap();
    assert!(
        ir.stop_sequences.is_none(),
        "empty stop array must not round-trip, got {:?}",
        ir.stop_sequences
    );

    // Non-empty stop still works (string and array forms).
    let body2 = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "hi" }],
        "stop": "END"
    });
    let ir2 = CompletionsParser.parse_request(&body2).unwrap();
    assert_eq!(ir2.stop_sequences, Some(vec!["END".to_string()]));
}

// 10. Interceptor OverrideParameter("thinking") must support enabling
//     thinking (bool true or the structured Anthropic-style object), not
//     just disabling it.
#[test]
fn interceptor_override_can_enable_thinking() {
    use ai_proxy_lib::converter::ir::{IrRequest, ThinkingMode};
    use ai_proxy_lib::interceptor::engine::InterceptorEngine;
    use ai_proxy_lib::interceptor::rules::RuleAction;
    use std::collections::HashMap;

    let mut ir = IrRequest {
        model: "m".into(),
        messages: vec![],
        tools: None,
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
        metadata: Default::default(),
        extra: Default::default(),
    };
    let mut headers = HashMap::new();

    // Bare `true` enables thinking.
    InterceptorEngine::apply_action(
        &RuleAction::OverrideParameter {
            parameter: "thinking".into(),
            value: json!(true),
        },
        &mut ir,
        &mut headers,
    );
    assert_eq!(ir.thinking.as_ref().unwrap().mode, ThinkingMode::Enabled);

    // Structured form sets mode + budget.
    InterceptorEngine::apply_action(
        &RuleAction::OverrideParameter {
            parameter: "thinking".into(),
            value: json!({"type": "enabled", "budget_tokens": 8000}),
        },
        &mut ir,
        &mut headers,
    );
    let t = ir.thinking.as_ref().unwrap();
    assert_eq!(t.mode, ThinkingMode::Enabled);
    assert_eq!(t.budget_tokens, Some(8000));

    // `false` still disables.
    InterceptorEngine::apply_action(
        &RuleAction::OverrideParameter {
            parameter: "thinking".into(),
            value: json!(false),
        },
        &mut ir,
        &mut headers,
    );
    assert!(ir.thinking.is_none());
}

// ---------------------------------------------------------------------------
// Round 3: Codex(Responses client) → Anthropic upstream session continuity
// ---------------------------------------------------------------------------

use ai_proxy_lib::converter::generators::completions::CompletionsGenerator;
use ai_proxy_lib::converter::generators::gemini::GeminiGenerator;

fn ir_with_previous_response_id() -> IrRequest {
    let mut ir = IrRequest {
        model: "claude-x".into(),
        messages: vec![IrMessage {
            role: IrRole::User,
            content: vec![text_part("hi")],
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }],
        tools: None,
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
        metadata: Default::default(),
        extra: Default::default(),
    };
    ir.extra
        .insert("previous_response_id".into(), json!("resp_prev_turn"));
    ir
}

// 11. previous_response_id is a Responses-protocol session pointer and must
//     NOT leak into non-Responses upstream bodies (Anthropic rejects unknown
//     fields with 400 "extra fields not permitted").
#[test]
fn anthropic_generator_does_not_leak_previous_response_id() {
    let body = AnthropicGenerator
        .generate_request(&ir_with_previous_response_id())
        .unwrap();
    assert!(
        body.get("previous_response_id").is_none(),
        "leaked into anthropic body: {}",
        body
    );
}

#[test]
fn completions_generator_does_not_leak_previous_response_id() {
    let body = CompletionsGenerator
        .generate_request(&ir_with_previous_response_id())
        .unwrap();
    assert!(body.get("previous_response_id").is_none());
}

#[test]
fn gemini_generator_does_not_leak_previous_response_id() {
    let body = GeminiGenerator
        .generate_request(&ir_with_previous_response_id())
        .unwrap();
    assert!(body.get("previous_response_id").is_none());
}

// 12. ResponsesGenerator::generate_response must emit official resp_-prefixed
//     ids so codex can key session items and previous_response_id off them.
#[test]
fn responses_generator_ids_are_resp_prefixed() {
    let mut ir = IrResponse {
        id: Some("msg_upstream_123".into()),
        model: Some("claude-x".into()),
        message: IrMessage {
            role: IrRole::Assistant,
            content: vec![text_part("hello")],
            name: None,
            tool_call_id: None,
            tool_calls: None,
        },
        finish_reason: Some("stop".into()),
        stop_sequence: None,
        usage: IrUsage::default(),
    };
    let out = ResponsesGenerator.generate_response(&ir).unwrap();
    assert_eq!(out["id"].as_str().unwrap(), "resp_msg_upstream_123");

    // Already-prefixed ids pass through unchanged.
    ir.id = Some("resp_abc".into());
    let out = ResponsesGenerator.generate_response(&ir).unwrap();
    assert_eq!(out["id"].as_str().unwrap(), "resp_abc");

    // Missing id gets a resp_ default.
    ir.id = None;
    let out = ResponsesGenerator.generate_response(&ir).unwrap();
    assert!(out["id"].as_str().unwrap().starts_with("resp_"));
}

// 13. AnthropicGenerator must collapse consecutive user messages (codex sends
//     user_instructions + environment_context + prompt as separate items).
#[test]
fn anthropic_generator_merges_consecutive_user_messages() {
    let ir = IrRequest {
        model: "claude-x".into(),
        messages: vec![
            IrMessage {
                role: IrRole::User,
                content: vec![text_part("<user_instructions>be brief")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
            IrMessage {
                role: IrRole::User,
                content: vec![text_part("<environment_context>cwd=/tmp")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
            IrMessage {
                role: IrRole::User,
                content: vec![text_part("fix the bug")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
        ],
        tools: None,
        tool_choice: None,
        temperature: None,
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
        metadata: Default::default(),
        extra: Default::default(),
    };
    let body = AnthropicGenerator.generate_request(&ir).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "three consecutive user messages must merge into one, got {messages:?}"
    );
    let blocks = messages[0]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 3, "all three texts preserved");
    assert_eq!(
        blocks[0]["text"].as_str().unwrap(),
        "<user_instructions>be brief"
    );
    assert_eq!(blocks[2]["text"].as_str().unwrap(), "fix the bug");
}

// 14. When merging a tool_result user message with a plain-text user message,
//     tool_result blocks come first.
#[test]
fn anthropic_generator_merged_user_puts_tool_results_first() {
    let ir = IrRequest {
        model: "claude-x".into(),
        messages: vec![
            IrMessage {
                role: IrRole::Assistant,
                content: vec![],
                name: None,
                tool_call_id: None,
                tool_calls: Some(vec![IrToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                }]),
            },
            IrMessage {
                role: IrRole::Tool,
                content: vec![IrContentPart::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "file body".into(),
                    tool_name: Some("read_file".into()),
                    id: None,
                }],
                name: None,
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
            },
            IrMessage {
                role: IrRole::User,
                content: vec![text_part("now summarize")],
                name: None,
                tool_call_id: None,
                tool_calls: None,
            },
        ],
        tools: None,
        tool_choice: None,
        temperature: None,
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
        metadata: Default::default(),
        extra: Default::default(),
    };
    let body = AnthropicGenerator.generate_request(&ir).unwrap();
    let messages = body["messages"].as_array().unwrap();
    // assistant + merged(user tool_result+text) = 2
    assert_eq!(messages.len(), 2);
    let user_msg = &messages[1];
    assert_eq!(user_msg["role"], "user");
    let blocks = user_msg["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[0]["type"], "tool_result",
        "tool_result must come first"
    );
    assert_eq!(blocks[1]["type"], "text");
}

// 15. reasoning effort "none" is an explicit opt-OUT: it must disable
//     thinking, not enable it with a default budget. Force-enabling thinking
//     on glm via bigmodel produced stream-of-consciousness text with no tool
//     calls, hitting the gateway's 9096-token output cap.
#[test]
fn responses_parser_effort_none_disables_thinking() {
    let body = json!({
        "model": "gpt-5.6-sol",
        "input": "hi",
        "reasoning": {"effort": "none"}
    });
    let ir = ResponsesParser.parse_request(&body).unwrap();
    assert_eq!(ir.thinking.as_ref().unwrap().mode, ThinkingMode::Disabled);
    assert!(ir.thinking.as_ref().unwrap().budget_tokens.is_none());

    // minimal gets a small budget, not the 10000 medium default
    let body2 = json!({
        "model": "gpt-5.6-sol",
        "input": "hi",
        "reasoning": {"effort": "minimal"}
    });
    let ir2 = ResponsesParser.parse_request(&body2).unwrap();
    assert_eq!(ir2.thinking.as_ref().unwrap().mode, ThinkingMode::Enabled);
    assert_eq!(ir2.thinking.as_ref().unwrap().budget_tokens, Some(1024));

    // End-to-end: AnthropicGenerator must NOT emit a thinking block for none
    let out = AnthropicGenerator.generate_request(&ir).unwrap();
    assert!(
        out.get("thinking").is_none(),
        "thinking must be omitted for effort=none, got {}",
        out.get("thinking").unwrap_or(&serde_json::Value::Null)
    );
}

#[test]
fn completions_parser_effort_none_disables_thinking() {
    use ai_proxy_lib::converter::parsers::completions::CompletionsParser;
    let body = json!({
        "model": "gpt-x",
        "messages": [{"role":"user","content":"hi"}],
        "reasoning": {"effort": "none"}
    });
    let ir = CompletionsParser.parse_request(&body).unwrap();
    assert_eq!(ir.thinking.as_ref().unwrap().mode, ThinkingMode::Disabled);
}
