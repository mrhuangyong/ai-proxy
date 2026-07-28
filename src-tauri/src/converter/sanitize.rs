//! Per-model parameter sanitization.
//!
//! When a virtual model fails over from one upstream mapping to another, the
//! same client IR is reused with only the `model` field swapped
//! (`handlers::handle_proxy_inner`). Each generator emits a parameter based
//! purely on whether the IR field is `Some`, with zero regard for the target
//! model's capabilities. A parameter the new upstream rejects surfaces as an
//! HTTP 400/422, which historically did NOT trigger failover rotation and was
//! returned to the client immediately — the "instant failure" symptom.
//!
//! [`ModelCapabilities`] is the per-model capability descriptor (one row in
//! `provider_models`, added by migration 026). [`sanitize_ir_for_capabilities`]
//! strips or clamps IR fields the target model does not support, so the
//! regenerated upstream body only carries parameters the model accepts.
//!
//! All capability flags default to permissive (`true`) so that models that were
//! never configured behave exactly as before.

use serde::{Deserialize, Serialize};

use super::ir::IrRequest;

/// Per-model capability descriptor. Mirrors the columns added by migration 026
/// (`provider_models`). All booleans default to `true` (permissive); `None`
/// means "do not clamp".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelCapabilities {
    pub supports_thinking: bool,
    pub supports_tools: bool,
    pub supports_temperature: bool,
    pub supports_top_p: bool,
    pub supports_top_k: bool,
    pub supports_presence_penalty: bool,
    pub supports_frequency_penalty: bool,
    pub supports_seed: bool,
    pub supports_response_format: bool,
    pub supports_stream_options: bool,
    pub supports_stop: bool,
    /// Upper bound for `ir.max_tokens`. `None` = leave untouched.
    pub max_output_tokens: Option<u32>,
    /// When `false`, the unguarded `ir.extra` passthrough (which injects
    /// arbitrary client keys like `chat_template_kwargs` / `logit_bias` into
    /// every generator) is cleared. This is the largest parameter-leak vector.
    pub extra_passthrough: bool,
}

impl ModelCapabilities {
    /// All capabilities enabled, no token clamp, extra passthrough on.
    /// Existing models that never configured capabilities get this, so their
    /// behaviour is byte-for-byte identical to before migration 026.
    pub fn permissive() -> Self {
        Self {
            supports_thinking: true,
            supports_tools: true,
            supports_temperature: true,
            supports_top_p: true,
            supports_top_k: true,
            supports_presence_penalty: true,
            supports_frequency_penalty: true,
            supports_seed: true,
            supports_response_format: true,
            supports_stream_options: true,
            supports_stop: true,
            max_output_tokens: None,
            extra_passthrough: true,
        }
    }
}

/// Strip / clamp IR fields that the target model does not support.
///
/// Call this after `ir.model` has been rewritten to the target model and
/// *before* `target_generator.generate_request(&ir)`.
pub fn sanitize_ir_for_capabilities(ir: &mut IrRequest, caps: &ModelCapabilities) {
    let mut changed: Vec<&str> = Vec::new();
    if !caps.supports_thinking && ir.thinking.take().is_some() {
        changed.push("thinking");
    }
    if !caps.supports_tools {
        let had_tools = ir.tools.is_some() || ir.tool_choice.is_some();
        ir.tools = None;
        ir.tool_choice = None;
        if had_tools {
            changed.push("tools/tool_choice");
        }
    }
    if !caps.supports_temperature && ir.temperature.take().is_some() {
        changed.push("temperature");
    }
    if !caps.supports_top_p && ir.top_p.take().is_some() {
        changed.push("top_p");
    }
    if !caps.supports_top_k && ir.top_k.take().is_some() {
        changed.push("top_k");
    }
    if !caps.supports_presence_penalty && ir.presence_penalty.take().is_some() {
        changed.push("presence_penalty");
    }
    if !caps.supports_frequency_penalty && ir.frequency_penalty.take().is_some() {
        changed.push("frequency_penalty");
    }
    if !caps.supports_seed && ir.seed.take().is_some() {
        changed.push("seed");
    }
    if !caps.supports_response_format && ir.response_format.take().is_some() {
        changed.push("response_format");
    }
    if !caps.supports_stream_options && ir.stream_options.take().is_some() {
        changed.push("stream_options");
    }
    if !caps.supports_stop && ir.stop_sequences.take().is_some() {
        changed.push("stop_sequences");
    }
    if let Some(cap) = caps.max_output_tokens {
        if let Some(mt) = ir.max_tokens {
            if mt > cap {
                ir.max_tokens = Some(cap);
                changed.push("max_tokens(clamped)");
            }
        }
    }
    let extra_cleared = !caps.extra_passthrough && !ir.extra.is_empty();
    if extra_cleared {
        ir.extra.clear();
    }
    if extra_cleared {
        changed.push("extra(cleared)");
    }
    if !changed.is_empty() {
        tracing::info!(
            "[sanitize] model={} stripped: {}",
            ir.model,
            changed.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::ir::{IrRequest, IrThinkingConfig, ThinkingMode};

    fn full_ir() -> IrRequest {
        IrRequest {
            model: "test-model".into(),
            messages: vec![],
            tools: Some(vec![]),
            tool_choice: Some(serde_json::json!("auto")),
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: Some(40),
            max_tokens: Some(8192),
            stream: false,
            stop_sequences: Some(vec!["END".into()]),
            response_format: Some(serde_json::json!({"type": "json_object"})),
            presence_penalty: Some(0.1),
            frequency_penalty: Some(0.2),
            seed: Some(42),
            thinking: Some(IrThinkingConfig {
                mode: ThinkingMode::Enabled,
                budget_tokens: Some(10000),
                display: None,
            }),
            stream_options: Some(serde_json::json!({"include_usage": true})),
            metadata: Default::default(),
            extra: {
                let mut m = std::collections::HashMap::new();
                m.insert("chat_template_kwargs".into(), serde_json::json!({}));
                m.insert("logit_bias".into(), serde_json::json!({}));
                m
            },
        }
    }

    #[test]
    fn permissive_caps_leave_everything_untouched() {
        let mut ir = full_ir();
        let before_extra_len = ir.extra.len();
        let before_max_tokens = ir.max_tokens;
        sanitize_ir_for_capabilities(&mut ir, &ModelCapabilities::permissive());
        // Nothing should be stripped; extra kept; max_tokens not clamped.
        assert!(ir.thinking.is_some());
        assert!(ir.tools.is_some());
        assert!(ir.tool_choice.is_some());
        assert!(ir.temperature.is_some());
        assert!(ir.top_p.is_some());
        assert!(ir.stop_sequences.is_some());
        assert!(ir.seed.is_some());
        assert_eq!(ir.extra.len(), before_extra_len);
        assert_eq!(ir.max_tokens, before_max_tokens);
    }

    #[test]
    fn strips_each_capability_when_disabled() {
        let mut ir = full_ir();
        let caps = ModelCapabilities {
            supports_thinking: false,
            supports_tools: false,
            supports_temperature: false,
            supports_top_p: false,
            supports_top_k: false,
            supports_presence_penalty: false,
            supports_frequency_penalty: false,
            supports_seed: false,
            supports_response_format: false,
            supports_stream_options: false,
            supports_stop: false,
            max_output_tokens: None,
            extra_passthrough: false,
        };
        sanitize_ir_for_capabilities(&mut ir, &caps);
        assert!(ir.thinking.is_none());
        assert!(ir.tools.is_none());
        assert!(ir.tool_choice.is_none());
        assert!(ir.temperature.is_none());
        assert!(ir.top_p.is_none());
        assert!(ir.top_k.is_none());
        assert!(ir.presence_penalty.is_none());
        assert!(ir.frequency_penalty.is_none());
        assert!(ir.seed.is_none());
        assert!(ir.response_format.is_none());
        assert!(ir.stream_options.is_none());
        assert!(ir.stop_sequences.is_none());
        assert!(ir.extra.is_empty(), "extra must be cleared");
    }

    #[test]
    fn clamps_max_tokens_down_to_cap() {
        let mut ir = full_ir();
        let caps = ModelCapabilities {
            max_output_tokens: Some(4096),
            ..ModelCapabilities::permissive()
        };
        sanitize_ir_for_capabilities(&mut ir, &caps);
        assert_eq!(ir.max_tokens, Some(4096));
    }

    #[test]
    fn does_not_clamp_max_tokens_below_request_value() {
        let mut ir = full_ir();
        let caps = ModelCapabilities {
            max_output_tokens: Some(16384),
            ..ModelCapabilities::permissive()
        };
        sanitize_ir_for_capabilities(&mut ir, &caps);
        assert_eq!(ir.max_tokens, Some(8192), "must not be raised");
    }

    #[test]
    fn disabling_tools_clears_both_tools_and_tool_choice() {
        let mut ir = full_ir();
        let caps = ModelCapabilities {
            supports_tools: false,
            ..ModelCapabilities::permissive()
        };
        sanitize_ir_for_capabilities(&mut ir, &caps);
        assert!(ir.tools.is_none());
        assert!(ir.tool_choice.is_none());
    }

    #[test]
    fn extra_passthrough_false_clears_extra_only() {
        let mut ir = full_ir();
        let caps = ModelCapabilities {
            extra_passthrough: false,
            ..ModelCapabilities::permissive()
        };
        sanitize_ir_for_capabilities(&mut ir, &caps);
        assert!(ir.extra.is_empty());
        // Everything else stays.
        assert!(ir.thinking.is_some());
        assert!(ir.tools.is_some());
    }
}
