use std::fs;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;
use uuid::Uuid;

use super::scanner::{self, ScannedSkill};
use super::types::*;

/// Ensure default global source exists in database
pub async fn ensure_default_source(pool: &SqlitePool) {
    let home = dirs::home_dir().unwrap_or_default();
    let global_path = home.join(".agents/skills");

    let exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM skill_sources WHERE is_global = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !exists {
        let id = Uuid::new_v4().to_string();
        let path_str = global_path.to_string_lossy().to_string();
        sqlx::query(
            "INSERT INTO skill_sources (id, name, path, is_global, is_default, discovery_order) VALUES (?, ?, ?, 1, 1, 0)",
        )
        .bind(&id)
        .bind("全局技能库")
        .bind(&path_str)
        .execute(pool)
        .await
        .ok();
    }
}

/// Scan all sources and update database cache
pub async fn scan_all(pool: &SqlitePool) -> Result<(), String> {
    let sources: Vec<SkillSource> =
        sqlx::query_as("SELECT * FROM skill_sources ORDER BY discovery_order, name")
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;

    for source in &sources {
        let path = Path::new(&source.path);
        let scanned = scanner::scan_source(path);

        let existing: Vec<Skill> =
            sqlx::query_as("SELECT * FROM skills WHERE source_id = ?")
                .bind(&source.id)
                .fetch_all(pool)
                .await
                .map_err(|e| e.to_string())?;

        let existing_map: std::collections::HashMap<String, Skill> = existing
            .into_iter()
            .map(|s| (s.skill_path.clone(), s))
            .collect();

        let mut scanned_map: std::collections::HashMap<String, &ScannedSkill> =
            std::collections::HashMap::new();
        for s in &scanned {
            scanned_map.insert(s.skill_path.to_string_lossy().to_string(), s);
        }

        for s in &scanned {
            let path_str = s.skill_path.to_string_lossy().to_string();
            if let Some(existing) = existing_map.get(&path_str) {
                if existing.name != s.name
                    || existing.description != s.description
                    || existing.is_symlink != s.is_symlink
                    || existing.link_target.as_deref() != s.link_target.as_deref()
                    || existing.has_skill_md != s.has_skill_md
                {
                    sqlx::query(
                        "UPDATE skills SET name=?, description=?, is_symlink=?, link_target=?, has_skill_md=?, updated_at=datetime('now') WHERE id=?",
                    )
                    .bind(&s.name)
                    .bind(&s.description)
                    .bind(s.is_symlink)
                    .bind(&s.link_target)
                    .bind(s.has_skill_md)
                    .bind(&existing.id)
                    .execute(pool)
                    .await
                    .map_err(|e| e.to_string())?;
                }
            } else {
                let id = Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO skills (id, name, description, source_id, skill_path, is_symlink, link_target, has_skill_md) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(&s.name)
                .bind(&s.description)
                .bind(&source.id)
                .bind(&path_str)
                .bind(s.is_symlink)
                .bind(&s.link_target)
                .bind(s.has_skill_md)
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;
            }
        }

        for (path_str, existing) in &existing_map {
            if !scanned_map.contains_key(path_str) {
                sqlx::query("DELETE FROM skills WHERE id = ?")
                    .bind(&existing.id)
                    .execute(pool)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(())
}

/// Auto-discover new skill source directories
pub async fn discover_sources(pool: &SqlitePool) -> Result<Vec<SkillSource>, String> {
    let discovered = scanner::discover_sources();
    let mut new_sources = Vec::new();

    for (name, path) in discovered {
        let path_str = path.to_string_lossy().to_string();

        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM skill_sources WHERE path = ?",
        )
        .bind(&path_str)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;

        if !exists {
            let id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO skill_sources (id, name, path, is_global, is_default, discovery_order) VALUES (?, ?, ?, 0, 0, 50)",
            )
            .bind(&id)
            .bind(&name)
            .bind(&path_str)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;

            new_sources.push(SkillSource {
                id,
                name,
                path: path_str,
                is_global: false,
                is_default: false,
                discovery_order: 50,
                created_at: String::new(),
                updated_at: String::new(),
            });
        }
    }

    Ok(new_sources)
}

/// Install a skill to target sources by creating symlinks
pub async fn install_skill(pool: &SqlitePool, body: &InstallSkillBody) -> Result<Vec<String>, String> {
    let skill: Skill = sqlx::query_as("SELECT * FROM skills WHERE id = ?")
        .bind(&body.skill_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let skill_name = std::path::Path::new(&skill.skill_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let mut results = Vec::new();

    for target_id in &body.target_source_ids {
        let source: SkillSource = sqlx::query_as("SELECT * FROM skill_sources WHERE id = ?")
            .bind(target_id)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Source not found: {}", e))?;

        let target_path = Path::new(&source.path).join(&skill_name);

        if target_path.exists() || target_path.is_symlink() {
            let (is_link, link_target) = scanner::check_symlink(&target_path);
            if is_link {
                if let Some(target) = &link_target {
                    let canonical_skill = fs::canonicalize(&skill.skill_path)
                        .unwrap_or_else(|_| PathBuf::from(&skill.skill_path));
                    let canonical_target = fs::canonicalize(target)
                        .unwrap_or_else(|_| PathBuf::from(target));
                    if canonical_skill == canonical_target {
                        results.push(format!("{}: 已安装，跳过", source.name));
                        continue;
                    }
                }
                fs::remove_file(&target_path).map_err(|e| format!("删除失效链接失败: {}", e))?;
            } else {
                results.push(format!("{}: 已存在本地技能，跳过", source.name));
                continue;
            }
        }

        #[cfg(unix)]
        {
            let source_path = PathBuf::from(&skill.skill_path);
            std::os::unix::fs::symlink(&source_path, &target_path)
                .map_err(|e| format!("创建符号链接失败: {}", e))?;
        }

        results.push(format!("{}: 安装成功", source.name));
    }

    Ok(results)
}

/// Uninstall a skill (remove symlink from application source)
pub async fn uninstall_skill(pool: &SqlitePool, skill_id: &str) -> Result<String, String> {
    let skill: Skill = sqlx::query_as("SELECT * FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let skill_path = Path::new(&skill.skill_path);

    if skill.is_symlink {
        fs::remove_file(skill_path).map_err(|e| format!("删除符号链接失败: {}", e))?;
        Ok("符号链接已删除".to_string())
    } else {
        Err("这是本地技能，不能通过卸载删除。请使用删除功能。".to_string())
    }
}

/// Find all symlinks in other sources that point to the same skill directory
pub async fn find_linked_skills(pool: &SqlitePool, skill_id: &str) -> Result<Vec<Skill>, String> {
    let skill: Skill = sqlx::query_as("SELECT * FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let skill_name = std::path::Path::new(&skill.skill_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let linked: Vec<Skill> = sqlx::query_as(
        "SELECT * FROM skills WHERE is_symlink = 1 AND source_id != ? AND skill_path LIKE ?"
    )
    .bind(&skill.source_id)
    .bind(format!("%/{}", skill_name))
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(linked)
}

/// Create a new skill in the global source directory
pub async fn create_skill(pool: &SqlitePool, body: &CreateSkillBody) -> Result<Skill, String> {
    let global_source: SkillSource = sqlx::query_as(
        "SELECT * FROM skill_sources WHERE is_global = 1 LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| format!("全局技能库未配置: {}", e))?;

    let skill_dir = Path::new(&global_source.path).join(&body.name);
    if skill_dir.exists() {
        return Err(format!("技能 '{}' 已存在", body.name));
    }

    fs::create_dir_all(&skill_dir).map_err(|e| format!("创建目录失败: {}", e))?;

    let skill_md_content = body.skill_md_content.clone().unwrap_or_else(|| {
        format!(
            "---\nname: {}\ndescription: \"{}\"\n---\n\n# {}\n\n",
            body.name,
            body.description.as_deref().unwrap_or(""),
            body.name
        )
    });

    let skill_md_path = skill_dir.join("SKILL.md");
    fs::write(&skill_md_path, &skill_md_content)
        .map_err(|e| format!("写入 SKILL.md 失败: {}", e))?;

    let id = Uuid::new_v4().to_string();
    let path_str = skill_dir.to_string_lossy().to_string();
    let description = body.description.clone().unwrap_or_default();

    sqlx::query(
        "INSERT INTO skills (id, name, description, source_id, skill_path, is_symlink, has_skill_md) VALUES (?, ?, ?, ?, ?, 0, 1)",
    )
    .bind(&id)
    .bind(&body.name)
    .bind(&description)
    .bind(&global_source.id)
    .bind(&path_str)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(Skill {
        id,
        name: body.name.clone(),
        description,
        source_id: global_source.id,
        skill_path: path_str,
        is_symlink: false,
        link_target: None,
        has_skill_md: true,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

/// Delete a skill entirely, including cleaning up symlinks in other sources
pub async fn delete_skill(pool: &SqlitePool, skill_id: &str) -> Result<Vec<String>, String> {
    let skill: Skill = sqlx::query_as("SELECT * FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let mut cleanup_log = Vec::new();

    // Find and remove all symlinks pointing to this skill in other sources
    let linked = find_linked_skills(pool, skill_id).await?;
    for link in &linked {
        let link_path = std::path::Path::new(&link.skill_path);
        if link_path.exists() || link_path.is_symlink() {
            fs::remove_file(link_path).map_err(|e| format!("删除关联链接失败: {}", e))?;
        }
        sqlx::query("DELETE FROM skills WHERE id = ?")
            .bind(&link.id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        cleanup_log.push(format!("已删除链接: {}", link.skill_path));
    }

    // Delete the skill itself
    let skill_path = std::path::Path::new(&skill.skill_path);
    if skill.is_symlink {
        fs::remove_file(skill_path).map_err(|e| format!("删除符号链接失败: {}", e))?;
    } else if skill_path.is_dir() {
        fs::remove_dir_all(skill_path).map_err(|e| format!("删除目录失败: {}", e))?;
    }

    sqlx::query("DELETE FROM skills WHERE id = ?")
        .bind(skill_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(cleanup_log)
}

/// Update SKILL.md content
pub async fn update_skill_md(pool: &SqlitePool, skill_id: &str, content: &str) -> Result<(), String> {
    let skill: Skill = sqlx::query_as("SELECT * FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let skill_path = Path::new(&skill.skill_path);

    let actual_path = if skill.is_symlink {
        match fs::read_link(skill_path) {
            Ok(target) => {
                if target.is_relative() {
                    skill_path.parent().unwrap_or(skill_path).join(&target)
                } else {
                    target
                }
            }
            Err(e) => return Err(format!("无法解析符号链接: {}", e)),
        }
    } else {
        skill_path.to_path_buf()
    };

    let skill_md_path = actual_path.join("SKILL.md");
    fs::write(&skill_md_path, content)
        .map_err(|e| format!("写入 SKILL.md 失败: {}", e))?;

    let (name, description) = scanner::parse_skill_md(&skill_md_path);
    sqlx::query("UPDATE skills SET name=?, description=?, updated_at=datetime('now') WHERE id=?")
        .bind(&name)
        .bind(&description)
        .bind(skill_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Read SKILL.md content
pub async fn read_skill_md(pool: &SqlitePool, skill_id: &str) -> Result<String, String> {
    let skill: Skill = sqlx::query_as("SELECT * FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let skill_path = Path::new(&skill.skill_path);

    let actual_path = if skill.is_symlink {
        match fs::read_link(skill_path) {
            Ok(target) => {
                if target.is_relative() {
                    skill_path.parent().unwrap_or(skill_path).join(&target)
                } else {
                    target
                }
            }
            Err(e) => return Err(format!("无法解析符号链接: {}", e)),
        }
    } else {
        skill_path.to_path_buf()
    };

    let skill_md_path = actual_path.join("SKILL.md");
    fs::read_to_string(&skill_md_path).map_err(|e| format!("读取 SKILL.md 失败: {}", e))
}

/// Install skill from URL (git clone)
pub async fn install_from_url(pool: &SqlitePool, url: &str) -> Result<Skill, String> {
    let global_source: SkillSource = sqlx::query_as(
        "SELECT * FROM skill_sources WHERE is_global = 1 LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| format!("全局技能库未配置: {}", e))?;

    let global_path = Path::new(&global_source.path);

    let repo_name = url
        .trim_end_matches('/')
        .split('/')
        .last()
        .unwrap_or("unknown-skill")
        .trim_end_matches(".git");

    let target_dir = global_path.join(repo_name);
    if target_dir.exists() {
        return Err(format!("技能 '{}' 已存在", repo_name));
    }

    let output = tokio::process::Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(url)
        .arg(&target_dir)
        .output()
        .await
        .map_err(|e| format!("执行 git clone 失败: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone 失败: {}", stderr));
    }

    let skill_md_path = target_dir.join("SKILL.md");
    if !skill_md_path.exists() {
        fs::remove_dir_all(&target_dir).ok();
        return Err("下载的技能不包含 SKILL.md，无效技能".to_string());
    }

    let (name, description) = scanner::parse_skill_md(&skill_md_path);
    let id = Uuid::new_v4().to_string();
    let path_str = target_dir.to_string_lossy().to_string();

    sqlx::query(
        "INSERT INTO skills (id, name, description, source_id, skill_path, is_symlink, has_skill_md) VALUES (?, ?, ?, ?, ?, 0, 1)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&description)
    .bind(&global_source.id)
    .bind(&path_str)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(Skill {
        id,
        name,
        description,
        source_id: global_source.id,
        skill_path: path_str,
        is_symlink: false,
        link_target: None,
        has_skill_md: true,
        created_at: String::new(),
        updated_at: String::new(),
    })
}
