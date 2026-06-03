use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppType {
    CodexCli,
    CodexDesktop,
    ClaudeCli,
    ClaudeDesktop,
}

impl fmt::Display for AppType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AppType::CodexCli => "codex_cli",
            AppType::CodexDesktop => "codex_desktop",
            AppType::ClaudeCli => "claude_cli",
            AppType::ClaudeDesktop => "claude_desktop",
        };
        write!(f, "{}", s)
    }
}

impl AppType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "codex" | "codex_cli" => Some(AppType::CodexCli),
            "codex_desktop" => Some(AppType::CodexDesktop),
            "claude_cli" => Some(AppType::ClaudeCli),
            "claude_desktop" => Some(AppType::ClaudeDesktop),
            _ => None,
        }
    }

    pub fn all() -> Vec<AppType> {
        vec![
            AppType::CodexCli,
            AppType::CodexDesktop,
            AppType::ClaudeCli,
            AppType::ClaudeDesktop,
        ]
    }

    pub fn is_cli(&self) -> bool {
        matches!(self, AppType::CodexCli | AppType::ClaudeCli)
    }

    pub fn is_codex(&self) -> bool {
        matches!(self, AppType::CodexCli | AppType::CodexDesktop)
    }

    pub fn is_claude(&self) -> bool {
        matches!(self, AppType::ClaudeCli | AppType::ClaudeDesktop)
    }

    pub fn proxy_url_suffix(&self) -> &str {
        if self.is_codex() {
            "/v1"
        } else {
            ""
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            AppType::CodexCli => "Codex CLI",
            AppType::CodexDesktop => "Codex Desktop",
            AppType::ClaudeCli => "Claude CLI",
            AppType::ClaudeDesktop => "Claude Desktop",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub app_type: AppType,
    pub installed: bool,
    pub install_path: Option<String>,
    pub config_path: Option<String>,
    pub model: Option<String>,
    pub model_haiku: Option<String>,
    pub model_sonnet: Option<String>,
    pub model_opus: Option<String>,
    pub work_dir: Option<String>,
    pub proxy_url: Option<String>,
    pub launched_at: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LaunchRequest {
    pub app_type: String,
    pub model: String,
    pub model_haiku: Option<String>,
    pub model_sonnet: Option<String>,
    pub model_opus: Option<String>,
    pub work_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetPathRequest {
    pub install_path: String,
}

#[derive(Debug, FromRow)]
pub struct DbAppConfig {
    pub app_type: String,
    pub model: String,
    pub proxy_url: String,
    pub launched_at: String,
    #[allow(dead_code)]
    pub config_path: Option<String>,
    pub install_path: Option<String>,
    pub status: String,
    pub work_dir: Option<String>,
    pub model_config: Option<String>,
}
