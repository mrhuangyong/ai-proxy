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
    #[serde(rename = "opencode_cli")]
    OpenCodeCli,
}

impl fmt::Display for AppType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AppType::CodexCli => "codex_cli",
            AppType::CodexDesktop => "codex_desktop",
            AppType::ClaudeCli => "claude_cli",
            AppType::ClaudeDesktop => "claude_desktop",
            AppType::OpenCodeCli => "opencode_cli",
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
            "opencode" | "opencode_cli" => Some(AppType::OpenCodeCli),
            _ => None,
        }
    }

    pub fn all() -> Vec<AppType> {
        vec![
            AppType::CodexCli,
            AppType::CodexDesktop,
            AppType::ClaudeCli,
            AppType::ClaudeDesktop,
            AppType::OpenCodeCli,
        ]
    }

    pub fn is_cli(&self) -> bool {
        matches!(
            self,
            AppType::CodexCli | AppType::ClaudeCli | AppType::OpenCodeCli
        )
    }

    pub fn is_codex(&self) -> bool {
        matches!(self, AppType::CodexCli | AppType::CodexDesktop)
    }

    pub fn is_claude(&self) -> bool {
        matches!(self, AppType::ClaudeCli | AppType::ClaudeDesktop)
    }

    pub fn proxy_url_suffix(&self) -> &str {
        if self.is_codex() || matches!(self, AppType::OpenCodeCli) {
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
            AppType::OpenCodeCli => "OpenCode CLI",
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
    pub opencode_models: Option<Vec<String>>,
    /// codex only: models shown in codex's /model picker. Written as the
    /// `model_catalog_json` catalog at launch; empty/None → no catalog
    /// (auto-discovery via the proxy's /v1/models shows everything).
    pub visible_models: Option<Vec<String>>,
    pub status: Option<String>,
}

/// How Codex launch should treat `~/.codex/auth.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuthMode {
    /// Write proxy API key (`auth_mode = apikey`).
    Apikey,
    /// Leave existing auth.json untouched.
    Preserve,
    /// Write ChatGPT OAuth tokens from a `codex_accounts` row.
    Inject,
}

impl CodexAuthMode {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "apikey" => Some(Self::Apikey),
            "preserve" => Some(Self::Preserve),
            "inject" => Some(Self::Inject),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apikey => "apikey",
            Self::Preserve => "preserve",
            Self::Inject => "inject",
        }
    }

    /// Map legacy `codex_preserve_auth` ("true"/"false") to a mode.
    pub fn from_preserve_flag(preserve: bool) -> Self {
        if preserve {
            Self::Preserve
        } else {
            Self::Apikey
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LaunchRequest {
    pub app_type: String,
    pub model: String,
    pub model_haiku: Option<String>,
    pub model_sonnet: Option<String>,
    pub model_opus: Option<String>,
    pub models: Option<Vec<String>>,
    /// codex only: see `AppConfig::visible_models`.
    pub visible_models: Option<Vec<String>>,
    pub work_dir: Option<String>,
    /// codex only: `apikey` | `preserve` | `inject`.
    pub codex_auth_mode: Option<String>,
    /// codex only: provider_id of the Codex OAuth account to inject.
    pub codex_account_provider_id: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::CodexAuthMode;

    #[test]
    fn auth_mode_parse_and_legacy_flag() {
        assert_eq!(CodexAuthMode::from_str("apikey"), Some(CodexAuthMode::Apikey));
        assert_eq!(CodexAuthMode::from_str("preserve"), Some(CodexAuthMode::Preserve));
        assert_eq!(CodexAuthMode::from_str("inject"), Some(CodexAuthMode::Inject));
        assert_eq!(CodexAuthMode::from_str("nope"), None);
        assert_eq!(
            CodexAuthMode::from_preserve_flag(true),
            CodexAuthMode::Preserve
        );
        assert_eq!(
            CodexAuthMode::from_preserve_flag(false),
            CodexAuthMode::Apikey
        );
        assert_eq!(CodexAuthMode::Inject.as_str(), "inject");
    }
}
