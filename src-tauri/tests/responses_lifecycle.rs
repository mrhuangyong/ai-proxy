//! Regression tests for the OpenAI Responses streaming lifecycle emitted by
//! `ResponsesStreamStateMachine`.
//!
//! Background: when proxying non-Responses upstreams (Completions / Anthropic)
//! down to a Responses client (e.g. codex with `wire_api = "responses"`), the
//! proxy must emit a well-formed Responses SSE event sequence:
//!
//!   response.created
//!     → response.output_item.added
//!     → response.content_part.added
//!     → response.output_text.delta (×N)
//!     → response.output_text.done
//!     → response.output_item.done
//!     → response.completed
//!
//! Each event must carry a strictly-increasing `sequence_number`. Omitting the
//! lifecycle envelope (created/added/done) previously caused codex to discard
//! the incremental deltas and reset to the final `response.completed` text,
//! which surfaced as "text appears, vanishes, then re-appears from the start"
//! in the UI.

use ai_proxy_lib::converter::ir::{IrStreamChunk, IrToolCallDelta};
use ai_proxy_lib::server::handlers::ResponsesStreamStateMachine;
use serde_json::Value;

/// Parse one `data: {...}\n\n` SSE frame into its JSON payload.
fn parse_frame(sse: &str) -> Value {
    let trimmed = sse
        .strip_prefix("data: ")
        .unwrap_or(sse)
        .trim()
        .trim_end_matches('\n');
    serde_json::from_str(trimmed).expect("SSE frame must be valid JSON")
}

/// Drive the state machine with a sequence of chunks and collect every emitted
/// SSE frame as a JSON value, in order.
fn run(mut sm: ResponsesStreamStateMachine, chunks: &[IrStreamChunk]) -> Vec<Value> {
    let mut frames = Vec::new();
    for chunk in chunks {
        for sse in sm.process_chunk(chunk, (10, 20)) {
            frames.push(parse_frame(&sse));
        }
    }
    frames
}

/// Assert that every frame carries a `sequence_number`, and that the values are
/// strictly increasing starting from 0.
fn assert_strict_sequence(frames: &[Value]) {
    let mut prev: Option<i64> = None;
    for (i, f) in frames.iter().enumerate() {
        let seq = f
            .get("sequence_number")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| panic!("frame #{i} missing sequence_number: {f}"));
        match prev {
            None => assert_eq!(seq, 0, "first sequence_number must be 0"),
            Some(p) => assert_eq!(seq, p + 1, "sequence_number must be strictly increasing"),
        }
        prev = Some(seq);
    }
}

fn delta_content(text: &str) -> IrStreamChunk {
    IrStreamChunk {
        id: Some("resp_test".into()),
        model: None,
        delta_content: Some(text.into()),
        delta_tool_calls: None,
        delta_thinking: None,
        finish_reason: None,
        usage: None,
        error: None,
    }
}

fn finish_completed() -> IrStreamChunk {
    IrStreamChunk {
        id: Some("resp_test".into()),
        model: Some("m".into()),
        delta_content: None,
        delta_tool_calls: None,
        delta_thinking: None,
        finish_reason: Some("completed".into()),
        usage: None,
        error: None,
    }
}

#[test]
fn case_a_plain_text_lifecycle_is_complete_and_ordered() {
    let sm = ResponsesStreamStateMachine::new("resp_test".into(), "m".into());
    let frames = run(
        sm,
        &[
            delta_content("Hello, "),
            delta_content("world!"),
            finish_completed(),
        ],
    );

    // Sanity: we must have emitted the full lifecycle envelope.
    let types: Vec<&str> = frames
        .iter()
        .map(|f| f["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        types,
        vec![
            "response.created",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.output_item.done",
            "response.completed",
        ],
        "lifecycle event sequence must be complete and ordered, got: {:?}",
        types
    );

    // created: in_progress
    assert_eq!(
        frames[0]["response"]["status"].as_str(),
        Some("in_progress")
    );

    // output_item.added: a message, in_progress
    assert_eq!(frames[1]["item"]["type"].as_str(), Some("message"));
    assert_eq!(frames[1]["item"]["status"].as_str(), Some("in_progress"));

    // content_part.added: output_text
    assert_eq!(frames[2]["part"]["type"].as_str(), Some("output_text"));

    // deltas carry the fragments
    assert_eq!(frames[3]["delta"].as_str(), Some("Hello, "));
    assert_eq!(frames[4]["delta"].as_str(), Some("world!"));

    // output_text.done carries the *full accumulated* text (not a fragment)
    assert_eq!(frames[5]["text"].as_str(), Some("Hello, world!"));
    // output_item.done carries the full text in its content too
    assert_eq!(
        frames[6]["item"]["content"][0]["text"].as_str(),
        Some("Hello, world!")
    );
    assert_eq!(frames[6]["item"]["status"].as_str(), Some("completed"));

    // response.completed carries the full text in its output array
    let completed_output = &frames[7]["response"]["output"];
    assert_eq!(
        completed_output[0]["content"][0]["text"].as_str(),
        Some("Hello, world!")
    );

    // item_id must be present on all message delta/done/part events so codex
    // can associate them with the active message item.
    assert_eq!(frames[2]["item_id"].as_str(), Some("msg_proxy")); // content_part.added
    assert_eq!(frames[3]["item_id"].as_str(), Some("msg_proxy")); // output_text.delta
    assert_eq!(frames[5]["item_id"].as_str(), Some("msg_proxy")); // output_text.done

    assert_strict_sequence(&frames);
}

#[test]
fn case_b_reasoning_then_text_keeps_indices_disjoint() {
    let sm = ResponsesStreamStateMachine::new("resp_test".into(), "m".into());
    let mut thinking_chunk = delta_content(""); // placeholder, replaced below
    thinking_chunk.delta_content = None;
    thinking_chunk.delta_thinking = Some("analyzing...".into());

    let frames = run(
        sm,
        &[thinking_chunk, delta_content("answer"), finish_completed()],
    );

    let types: Vec<&str> = frames
        .iter()
        .map(|f| f["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        types,
        vec![
            // reasoning output item (its own added/done envelope)
            "response.created",
            "response.output_item.added",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_part.done",
            "response.output_item.done",
            // text message part
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.output_item.done",
            "response.completed",
        ],
        "reasoning→text lifecycle, got: {:?}",
        types
    );

    // reasoning item added/done envelope is type=reasoning
    assert_eq!(frames[1]["item"]["type"].as_str(), Some("reasoning"));
    assert_eq!(frames[1]["item"]["id"].as_str(), Some("rs_proxy"));
    assert_eq!(frames[5]["item"]["type"].as_str(), Some("reasoning"));

    // reasoning summary events carry item_id so codex can bind them
    assert_eq!(frames[2]["item_id"].as_str(), Some("rs_proxy"));
    assert_eq!(frames[3]["delta"].as_str(), Some("analyzing..."));
    assert_eq!(frames[3]["item_id"].as_str(), Some("rs_proxy"));
    assert_eq!(frames[4]["part"]["text"].as_str(), Some("analyzing..."));
    assert_eq!(frames[4]["item_id"].as_str(), Some("rs_proxy"));

    // reasoning item done carries full summary
    assert_eq!(
        frames[5]["item"]["summary"][0]["text"].as_str(),
        Some("analyzing...")
    );

    // text part still correct + carries msg_proxy item_id
    assert_eq!(frames[6]["item"]["type"].as_str(), Some("message"));
    assert_eq!(frames[8]["delta"].as_str(), Some("answer"));
    assert_eq!(frames[8]["item_id"].as_str(), Some("msg_proxy"));
    assert_eq!(frames[9]["text"].as_str(), Some("answer"));
    assert_eq!(frames[9]["item_id"].as_str(), Some("msg_proxy"));

    // reasoning output_index (0..5) disjoint from message output_index (6..10)
    assert_eq!(frames[1]["output_index"].as_i64(), Some(0));
    assert_eq!(frames[6]["output_index"].as_i64(), Some(1));

    assert_strict_sequence(&frames);
}

#[test]
fn case_c_function_call_lifecycle() {
    let sm = ResponsesStreamStateMachine::new("resp_test".into(), "m".into());

    let start_chunk = IrStreamChunk {
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
    };
    let args_chunk = IrStreamChunk {
        id: None,
        model: None,
        delta_content: None,
        delta_tool_calls: Some(vec![IrToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: Some("{\"x\":".into()),
        }]),
        delta_thinking: None,
        finish_reason: None,
        usage: None,
        error: None,
    };
    let args_chunk2 = IrStreamChunk {
        id: None,
        model: None,
        delta_content: None,
        delta_tool_calls: Some(vec![IrToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: Some("1}".into()),
        }]),
        delta_thinking: None,
        finish_reason: None,
        usage: None,
        error: None,
    };

    let frames = run(
        sm,
        &[start_chunk, args_chunk, args_chunk2, finish_completed()],
    );

    let types: Vec<&str> = frames
        .iter()
        .map(|f| f["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        types,
        vec![
            "response.created",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ],
        "function_call lifecycle, got: {:?}",
        types
    );

    // the added item is a function_call, not a message
    assert_eq!(frames[1]["item"]["type"].as_str(), Some("function_call"));
    assert_eq!(frames[1]["item"]["name"].as_str(), Some("run"));

    // argument deltas stream and the final done carries the full accumulated args
    assert_eq!(frames[2]["delta"].as_str(), Some("{\"x\":"));
    assert_eq!(frames[3]["delta"].as_str(), Some("1}"));
    assert_eq!(frames[4]["arguments"].as_str(), Some("{\"x\":1}"));

    assert_strict_sequence(&frames);
}
