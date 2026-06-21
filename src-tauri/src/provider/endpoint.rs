use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub format: String,
    pub endpoint_path: Option<String>,
    pub enabled: bool,
    pub upstream_user_agent: String,
    pub models: Vec<ProviderModel>,
    pub api_keys: Vec<ApiKeyInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModel {
    pub id: String,
    pub provider_id: String,
    pub model_name: String,
    pub target_model: Option<String>,
    pub context_window: u64,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub label: String,
    pub is_active: bool,
    pub usage_count: u32,
    pub last_used_at: Option<String>,
    pub created_at: String,
}
