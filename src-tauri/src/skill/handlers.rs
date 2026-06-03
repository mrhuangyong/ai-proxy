use axum::extract::{Path, Query};
use axum::Json;
use serde::Deserialize;

use crate::db::get_pool;
use crate::server::api::{err_json, ok, ApiError, ApiResponse};

use super::manager;
use super::types::*;

#[derive(Debug, Deserialize)]
pub struct SkillQuery {
    pub source_id: Option<String>,
}

pub async fn list_sources() -> Json<ApiResponse<Vec<SkillSourceWithCount>>> {
    let pool = get_pool().await;
    manager::ensure_default_source(pool).await;

    let sources: Vec<SkillSource> = match sqlx::query_as(
        "SELECT * FROM skill_sources ORDER BY discovery_order, name",
    )
    .fetch_all(pool)
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("list_sources error: {}", e);
            return ok(vec![]);
        }
    };

    let mut result = Vec::new();
    for source in sources {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skills WHERE source_id = ?")
            .bind(&source.id)
            .fetch_one(pool)
            .await
            .unwrap_or(0);

        result.push(SkillSourceWithCount {
            skill_count: count,
            source,
        });
    }

    ok(result)
}

pub async fn create_source(
    Json(body): Json<CreateSkillSourceBody>,
) -> Result<Json<ApiResponse<SkillSource>>, Json<ApiError>> {
    let pool = get_pool().await;

    let path = dirs::home_dir()
        .map(|h| {
            let p = body.path.trim();
            if p.starts_with('~') {
                h.join(&p[2..])
            } else if std::path::Path::new(p).is_absolute() {
                std::path::PathBuf::from(p)
            } else {
                h.join(p)
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from(&body.path));

    if !path.is_dir() {
        return Err(err_json(format!("目录不存在: {}", path.display())));
    }

    let path_str = path.to_string_lossy().to_string();
    let id = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO skill_sources (id, name, path, is_global, is_default, discovery_order) VALUES (?, ?, ?, 0, 0, 50)",
    )
    .bind(&id)
    .bind(&body.name)
    .bind(&path_str)
    .execute(pool)
    .await
    .map_err(|e| err_json(format!("创建源失败: {}", e)))?;

    let source: SkillSource = sqlx::query_as("SELECT * FROM skill_sources WHERE id = ?")
        .bind(&id)
        .fetch_one(pool)
        .await
        .map_err(|e| err_json(format!("查询源失败: {}", e)))?;

    Ok(ok(source))
}

pub async fn update_source(
    Path(id): Path<String>,
    Json(body): Json<UpdateSkillSourceBody>,
) -> Result<Json<ApiResponse<SkillSource>>, Json<ApiError>> {
    let pool = get_pool().await;

    if let Some(name) = &body.name {
        sqlx::query("UPDATE skill_sources SET name=?, updated_at=datetime('now') WHERE id=?")
            .bind(name)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| err_json(format!("更新失败: {}", e)))?;
    }

    if let Some(path) = &body.path {
        sqlx::query("UPDATE skill_sources SET path=?, updated_at=datetime('now') WHERE id=?")
            .bind(path)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| err_json(format!("更新失败: {}", e)))?;
    }

    let source: SkillSource = sqlx::query_as("SELECT * FROM skill_sources WHERE id = ?")
        .bind(&id)
        .fetch_one(pool)
        .await
        .map_err(|e| err_json(format!("查询失败: {}", e)))?;

    Ok(ok(source))
}

pub async fn delete_source(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;

    let is_default: bool =
        sqlx::query_scalar("SELECT is_default FROM skill_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(pool)
            .await
            .unwrap_or(false);

    if is_default {
        return Err(err_json("默认源不能删除"));
    }

    sqlx::query("DELETE FROM skill_sources WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| err_json(format!("删除失败: {}", e)))?;

    Ok(ok(()))
}

pub async fn discover() -> Result<Json<ApiResponse<Vec<SkillSource>>>, Json<ApiError>> {
    let pool = get_pool().await;
    let new_sources = manager::discover_sources(pool)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(new_sources))
}

pub async fn list_skills(
    Query(query): Query<SkillQuery>,
) -> Json<ApiResponse<Vec<Skill>>> {
    let pool = get_pool().await;

    let skills: Vec<Skill> = if let Some(source_id) = query.source_id {
        match sqlx::query_as("SELECT * FROM skills WHERE source_id = ? ORDER BY name")
            .bind(&source_id)
            .fetch_all(pool)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("list_skills error: {}", e);
                return ok(vec![]);
            }
        }
    } else {
        match sqlx::query_as("SELECT * FROM skills ORDER BY name")
            .fetch_all(pool)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("list_skills error: {}", e);
                return ok(vec![]);
            }
        }
    };

    ok(skills)
}

pub async fn get_skill(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<SkillDetail>>, Json<ApiError>> {
    let pool = get_pool().await;

    let skill: Skill = sqlx::query_as("SELECT * FROM skills WHERE id = ?")
        .bind(&id)
        .fetch_one(pool)
        .await
        .map_err(|e| err_json(format!("技能不存在: {}", e)))?;

    let skill_md_content = manager::read_skill_md(pool, &id).await.ok();

    Ok(ok(SkillDetail {
        skill,
        skill_md_content,
    }))
}

pub async fn update_skill_md(
    Path(id): Path<String>,
    Json(body): Json<UpdateSkillMdBody>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    manager::update_skill_md(pool, &id, &body.content)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(()))
}

pub async fn delete_skill_handler(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    manager::delete_skill(pool, &id)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(()))
}

pub async fn create_skill_handler(
    Json(body): Json<CreateSkillBody>,
) -> Result<Json<ApiResponse<Skill>>, Json<ApiError>> {
    let pool = get_pool().await;
    let skill = manager::create_skill(pool, &body)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(skill))
}

pub async fn install_skill(
    Json(body): Json<InstallSkillBody>,
) -> Result<Json<ApiResponse<Vec<String>>>, Json<ApiError>> {
    let pool = get_pool().await;
    let results = manager::install_skill(pool, &body)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(results))
}

pub async fn uninstall_skill(
    Json(body): Json<UninstallSkillBody>,
) -> Result<Json<ApiResponse<String>>, Json<ApiError>> {
    let pool = get_pool().await;
    let result = manager::uninstall_skill(pool, &body.skill_id)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(result))
}

pub async fn install_from_url(
    Json(body): Json<InstallFromUrlBody>,
) -> Result<Json<ApiResponse<Skill>>, Json<ApiError>> {
    let pool = get_pool().await;
    let skill = manager::install_from_url(pool, &body.url)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(skill))
}

pub async fn scan() -> Result<Json<ApiResponse<()>>, Json<ApiError>> {
    let pool = get_pool().await;
    manager::ensure_default_source(pool).await;
    manager::scan_all(pool)
        .await
        .map_err(|e| err_json(e))?;
    Ok(ok(()))
}
