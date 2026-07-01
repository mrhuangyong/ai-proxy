use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 将采样参数（temperature/top_p）四舍五入到 2 位小数。
///
/// 必须在字符串层取整：`0.17f32` 提升为 f64 后是
/// `0.1700000011920929…`，serde_json 会按"最短可往返串"输出该长串，
/// 导致上游报 1210（"限制小数点 2 位"）。返回 f64 直接进入
/// `serde_json::Value`（内部本就存 f64），从而绕开 f32→f64 提升。
pub fn round2(v: f32) -> f64 {
    format!("{:.2}", v).parse::<f64>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::round2;

    #[test]
    fn round2_serializes_to_at_most_two_decimals() {
        // 0.17f32 提升为 f64 ≈ 0.1700000011920929，曾经触发 1210。
        assert_eq!(serde_json::to_string(&round2(0.17)).unwrap(), "0.17");
        assert_eq!(serde_json::to_string(&round2(0.13)).unwrap(), "0.13");
        assert_eq!(serde_json::to_string(&round2(0.19)).unwrap(), "0.19");
        assert_eq!(serde_json::to_string(&round2(0.29)).unwrap(), "0.29");
        // 常见值仍正常
        assert_eq!(serde_json::to_string(&round2(0.7)).unwrap(), "0.7");
        assert_eq!(serde_json::to_string(&round2(1.0)).unwrap(), "1.0");
        assert_eq!(serde_json::to_string(&round2(0.0)).unwrap(), "0.0");
        // 四舍五入到位
        assert_eq!(serde_json::to_string(&round2(0.135)).unwrap(), "0.14");
        assert_eq!(serde_json::to_string(&round2(0.134)).unwrap(), "0.13");
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrRequest {
    pub model: String,
    pub messages: Vec<IrMessage>,
    pub tools: Option<Vec<IrTool>>,
    pub tool_choice: Option<Value>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    pub stop_sequences: Option<Vec<String>>,
    pub response_format: Option<Value>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub seed: Option<u64>,
    pub thinking: Option<IrThinkingConfig>,
    pub stream_options: Option<Value>,
    pub metadata: HashMap<String, Value>,
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrThinkingConfig {
    pub enabled: bool,
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrMessage {
    pub role: IrRole,
    pub content: Vec<IrContentPart>,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<IrToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IrRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IrContentPart {
    Text {
        text: String,
        citations: Option<Value>,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    Image {
        url: Option<String>,
        data: Option<String>,
        media_type: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        tool_name: Option<String>,
        id: Option<String>,
    },
    /// Opaque compaction item from the Responses API compact endpoint.
    /// Carries encrypted_content that must be passed through verbatim.
    Compaction {
        id: String,
        encrypted_content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrStreamError {
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrStreamChunk {
    pub id: Option<String>,
    pub model: Option<String>,
    pub delta_content: Option<String>,
    pub delta_tool_calls: Option<Vec<IrToolCallDelta>>,
    pub delta_thinking: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: Option<IrUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IrStreamError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IrUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub cached_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub thinking_tokens: u32,
    /// Raw upstream usage JSON (preserved verbatim for diagnostics).
    /// Skipped during serialization to keep IR representations stable.
    #[serde(skip)]
    #[serde(default)]
    pub raw: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrResponse {
    pub id: Option<String>,
    pub model: Option<String>,
    pub message: IrMessage,
    pub finish_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: IrUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ClientFormat {
    Completions,
    Responses,
    Anthropic,
    Gemini,
}

impl ClientFormat {
    #[allow(dead_code)]
    pub fn from_path(path: &str) -> Option<Self> {
        if path.contains("/v1/chat/completions") {
            Some(Self::Completions)
        } else if path.contains("/v1/responses") {
            Some(Self::Responses)
        } else if path.contains("/v1/messages") {
            Some(Self::Anthropic)
        } else if path.contains("/v1beta/models") {
            Some(Self::Gemini)
        } else {
            None
        }
    }
}
