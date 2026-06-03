use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub transport_type: String,
    pub command: Option<String>,
    pub args: Option<String>,
    pub url: Option<String>,
    pub headers: Option<String>,
    pub env: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct McpAppBinding {
    pub mcp_server_id: String,
    pub app_type: String,
    pub enabled: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpServerWithBindings {
    #[serde(flatten)]
    pub server: McpServer,
    pub bindings: Vec<McpAppBinding>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMcpServerBody {
    pub name: String,
    pub transport_type: String,
    pub command: Option<String>,
    pub args: Option<String>,
    pub url: Option<String>,
    pub headers: Option<String>,
    pub env: Option<String>,
    pub description: Option<String>,
    pub bindings: Option<Vec<McpAppBindingInput>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMcpServerBody {
    pub name: Option<String>,
    pub transport_type: Option<String>,
    pub command: Option<String>,
    pub args: Option<String>,
    pub url: Option<String>,
    pub headers: Option<String>,
    pub env: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct McpAppBindingInput {
    pub app_type: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBindingsBody {
    pub bindings: Vec<McpAppBindingInput>,
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub imported: u32,
    pub skipped: u32,
}

#[derive(Debug, Serialize)]
pub struct ApplyResult {
    pub applied: u32,
}
