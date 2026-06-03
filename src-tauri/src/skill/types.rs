use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SkillSource {
    pub id: String,
    pub name: String,
    pub path: String,
    pub is_global: bool,
    pub is_default: bool,
    pub discovery_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillSourceWithCount {
    #[serde(flatten)]
    pub source: SkillSource,
    pub skill_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source_id: String,
    pub skill_path: String,
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub has_skill_md: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillDetail {
    #[serde(flatten)]
    pub skill: Skill,
    pub skill_md_content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillSourceBody {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSkillSourceBody {
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillBody {
    pub name: String,
    pub description: Option<String>,
    pub skill_md_content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSkillMdBody {
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct InstallSkillBody {
    pub skill_id: String,
    pub target_source_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UninstallSkillBody {
    pub skill_id: String,
}

#[derive(Debug, Deserialize)]
pub struct InstallFromUrlBody {
    pub url: String,
}
