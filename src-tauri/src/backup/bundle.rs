use serde::{Deserialize, Serialize};

/// Top-level backup container.
#[derive(Serialize, Deserialize)]
pub struct BackupBundle {
    pub version: u32,
    pub created_at: String,
    pub app_version: String,
    pub kdf: KdfParams,
    /// Metadata listing which field paths are encrypted (for debugging only).
    pub encrypted_fields: Vec<String>,
    pub data: BackupData,
}

#[derive(Serialize, Deserialize)]
pub struct KdfParams {
    pub algorithm: String,
    pub iterations: u32,
    pub salt: String, // base64
    pub key_len: u32,
}

/// Each table is a Vec of JSON objects (row as column→value). Sensitive
/// columns are replaced/added as {"enc": "<base64>", "nonce": "<base64>"}.
#[derive(Serialize, Deserialize, Default)]
pub struct BackupData {
    #[serde(default)]
    pub providers: Vec<serde_json::Value>,
    /// Upstream protocol rows (migration 028); absent in pre-028 backups.
    #[serde(default)]
    pub provider_protocols: Vec<serde_json::Value>,
    #[serde(default)]
    pub provider_models: Vec<serde_json::Value>,
    #[serde(default)]
    pub api_keys: Vec<serde_json::Value>,
    #[serde(default)]
    pub interceptor_rules: Vec<serde_json::Value>,
    #[serde(default)]
    pub virtual_models: Vec<serde_json::Value>,
    #[serde(default)]
    pub virtual_model_mappings: Vec<serde_json::Value>,
    #[serde(default)]
    pub mcp_servers: Vec<serde_json::Value>,
    #[serde(default)]
    pub mcp_app_bindings: Vec<serde_json::Value>,
    #[serde(default)]
    pub app_configs: Vec<serde_json::Value>,
    #[serde(default)]
    pub users: Vec<serde_json::Value>,
    #[serde(default)]
    pub settings: Vec<serde_json::Value>,
}
