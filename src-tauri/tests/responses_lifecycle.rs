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
//! Each event must carry a strictly-increasing `sequence_number`, and each SSE
//! frame must include an `event:` line matching the JSON `type` (OpenResponses
//! / OpenAI wire shape). Omitting the lifecycle envelope previously caused
//! clients that route on `event:` to drop deltas.

use ai_proxy_lib::converter::ir::{IrStreamChunk, IrToolCallDelta};
use ai_proxy_lib::server::handlers::ResponsesStreamStateMachine;
use serde_json::Value;

/// Parse one SSE frame (`event: …\ndata: {…}\n\n` or data-only) into JSON.
/// Asserts that when `event:` is present it equals the JSON `type`.
fn parse_frame(sse: &str) -> Value {
    let mut event_name: Option<&str> = None;
    let mut data_body: Option<&str> = None;
    for line in sse.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_body = Some(rest.trim());
        }
    }
    let data = data_body.unwrap_or_else(|| {
        sse.strip_prefix("data: ")
            .unwrap_or(sse)
            .trim()
            .trim_end_matches('\n')
    });
    let value: Value = serde_json::from_str(data).expect("SSE frame must be valid JSON");
    let ty = value["type"].as_str().unwrap_or("");
    assert!(!ty.is_empty(), "frame JSON must carry type, got: {sse:?}");
    let event = event_name.unwrap_or_else(|| {
        panic!("SSE frame missing event: line (expected event: {ty}), got: {sse:?}")
    });
    assert_eq!(
        event, ty,
        "SSE event: name must equal JSON type, got event={event:?} type={ty:?}"
    );
    value
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
    assert_eq!(frames[2]["item_id"].as_str(), Some("msg_resp_test")); // content_part.added
    assert_eq!(frames[3]["item_id"].as_str(), Some("msg_resp_test")); // output_text.delta
    assert_eq!(frames[5]["item_id"].as_str(), Some("msg_resp_test")); // output_text.done

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
            "response.reasoning_summary_text.done",
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
    assert_eq!(frames[1]["item"]["id"].as_str(), Some("rs_resp_test"));
    assert_eq!(frames[6]["item"]["type"].as_str(), Some("reasoning"));

    // reasoning summary events carry item_id so codex can bind them
    assert_eq!(frames[2]["item_id"].as_str(), Some("rs_resp_test"));
    assert_eq!(frames[3]["delta"].as_str(), Some("analyzing..."));
    assert_eq!(frames[3]["item_id"].as_str(), Some("rs_resp_test"));
    assert_eq!(
        frames[4]["type"].as_str(),
        Some("response.reasoning_summary_text.done")
    );
    assert_eq!(frames[4]["text"].as_str(), Some("analyzing..."));
    assert_eq!(frames[4]["item_id"].as_str(), Some("rs_resp_test"));
    assert_eq!(frames[5]["part"]["text"].as_str(), Some("analyzing..."));
    assert_eq!(frames[5]["item_id"].as_str(), Some("rs_resp_test"));

    // reasoning item done carries full summary
    assert_eq!(
        frames[6]["item"]["summary"][0]["text"].as_str(),
        Some("analyzing...")
    );

    // text part still correct + carries unique message item_id
    assert_eq!(frames[7]["item"]["type"].as_str(), Some("message"));
    assert_eq!(frames[9]["delta"].as_str(), Some("answer"));
    assert_eq!(frames[9]["item_id"].as_str(), Some("msg_resp_test"));
    assert_eq!(frames[10]["text"].as_str(), Some("answer"));
    assert_eq!(frames[10]["item_id"].as_str(), Some("msg_resp_test"));

    // reasoning output_index disjoint from message output_index
    assert_eq!(frames[1]["output_index"].as_i64(), Some(0));
    assert_eq!(frames[7]["output_index"].as_i64(), Some(1));

    assert_strict_sequence(&frames);
}

#[test]
fn case_b2_same_chunk_thinking_and_content_both_emitted() {
    // Anthropic→IR can put thinking + text into one IrStreamChunk; the SM must
    // not early-return after delta_thinking and drop delta_content.
    let sm = ResponsesStreamStateMachine::new("resp_test".into(), "m".into());
    let frames = run(
        sm,
        &[
            IrStreamChunk {
                id: Some("resp_test".into()),
                model: None,
                delta_content: Some("answer".into()),
                delta_tool_calls: None,
                delta_thinking: Some("think".into()),
                finish_reason: None,
                usage: None,
                error: None,
            },
            finish_completed(),
        ],
    );
    let types: Vec<&str> = frames
        .iter()
        .map(|f| f["type"].as_str().unwrap_or(""))
        .collect();
    assert!(
        types.contains(&"response.reasoning_summary_text.delta"),
        "thinking must be emitted, got: {:?}",
        types
    );
    assert!(
        types.contains(&"response.reasoning_summary_text.done"),
        "summary_text.done must close reasoning before text, got: {:?}",
        types
    );
    assert!(
        types.contains(&"response.output_text.delta"),
        "text delta must not be swallowed by thinking early-return, got: {:?}",
        types
    );
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

#[test]
fn case_d_different_responses_get_distinct_item_ids() {
    // Regression: every response must derive a unique item_id from its
    // response_id. A fixed id like "msg_proxy" caused Codex to bind later
    // responses' text deltas to the same streaming item as earlier ones,
    // so later messages overwrote earlier ones in the UI.
    let sm1 = ResponsesStreamStateMachine::new("resp_alpha".into(), "m".into());
    let frames1 = run(sm1, &[delta_content("first"), finish_completed()]);

    let sm2 = ResponsesStreamStateMachine::new("resp_beta".into(), "m".into());
    let frames2 = run(sm2, &[delta_content("second"), finish_completed()]);

    // Extract the message item_id from the content_part.added event (frame[2])
    let id1 = frames1[2]["item_id"].as_str().unwrap();
    let id2 = frames2[2]["item_id"].as_str().unwrap();

    assert!(
        id1 != id2,
        "item_id must differ across responses, got {id1} == {id2}"
    );
    assert_eq!(id1, "msg_resp_alpha");
    assert_eq!(id2, "msg_resp_beta");
}
