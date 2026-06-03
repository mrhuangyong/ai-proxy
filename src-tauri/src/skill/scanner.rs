use std::path::{Path, PathBuf};

pub struct ScannedSkill {
    pub name: String,
    pub description: String,
    pub skill_path: PathBuf,
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub has_skill_md: bool,
}

/// Scan a source directory for skills (subdirectories containing SKILL.md)
pub fn scan_source(source_path: &Path) -> Vec<ScannedSkill> {
    let mut skills = Vec::new();
    let entries = match std::fs::read_dir(source_path) {
        Ok(e) => e,
        Err(_) => return skills,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() && !path.is_symlink() {
            continue;
        }

        let (is_symlink, link_target) = check_symlink(&path);

        let file_name = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };
        if file_name.starts_with('.') {
            continue;
        }

        let actual_path = if is_symlink {
            match std::fs::read_link(&path) {
                Ok(target) => {
                    if target.is_relative() {
                        path.parent().unwrap_or(&path).join(&target)
                    } else {
                        target
                    }
                }
                Err(_) => path.clone(),
            }
        } else {
            path.clone()
        };

        let skill_md_path = actual_path.join("SKILL.md");
        let has_skill_md = skill_md_path.exists();

        let (name, description) = if has_skill_md {
            parse_skill_md(&skill_md_path)
        } else {
            (file_name.clone(), String::new())
        };

        skills.push(ScannedSkill {
            name,
            description,
            skill_path: path,
            is_symlink,
            link_target,
            has_skill_md,
        });
    }

    skills
}

/// Parse SKILL.md YAML frontmatter to extract name and description
pub fn parse_skill_md(path: &Path) -> (String, String) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (String::new(), String::new()),
    };

    let mut name = String::new();
    let mut description = String::new();

    if content.starts_with("---") {
        if let Some(end) = content[3..].find("---") {
            let frontmatter = &content[3..end + 3];
            for line in frontmatter.lines() {
                let line = line.trim();
                if let Some(value) = line.strip_prefix("name:") {
                    name = value.trim().trim_matches('"').trim_matches('\'').to_string();
                } else if let Some(value) = line.strip_prefix("description:") {
                    description = value.trim().trim_matches('"').trim_matches('\'').to_string();
                }
            }
        }
    }

    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if name.is_empty() {
        name = dir_name;
    }

    (name, description)
}

/// Check if path is a symlink and return its target
pub fn check_symlink(path: &Path) -> (bool, Option<String>) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(path)
                    .map(|p| p.to_string_lossy().to_string())
                    .ok();
                (true, target)
            } else {
                (false, None)
            }
        }
        Err(_) => (false, None),
    }
}

/// Discover skill source directories under home directory
/// Returns a list of (name, path) tuples for discovered sources
pub fn discover_sources() -> Vec<(String, PathBuf)> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    let mut found = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    // Always exclude the global path
    let global_path = home.join(".agents/skills");
    seen_paths.insert(global_path.to_string_lossy().to_string());

    if let Ok(entries) = std::fs::read_dir(&home) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();

            if !name.starts_with('.') {
                continue;
            }

            let skills_path = entry.path().join("skills");
            if !skills_path.is_dir() {
                continue;
            }

            let path_str = skills_path.to_string_lossy().to_string();
            if seen_paths.contains(&path_str) {
                continue;
            }

            if is_valid_skill_source(&skills_path) {
                let source_name = name.trim_start_matches('.').to_string();
                seen_paths.insert(path_str);
                found.push((source_name, skills_path));
            }
        }
    }

    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Check if a directory contains at least one subdirectory with SKILL.md
fn is_valid_skill_source(path: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with('.') {
                continue;
            }
            if p.is_dir() || p.is_symlink() {
                let actual = if p.is_symlink() {
                    match std::fs::read_link(&p) {
                        Ok(target) => {
                            if target.is_relative() {
                                p.parent().unwrap_or(&p).join(&target)
                            } else {
                                target
                            }
                        }
                        Err(_) => continue,
                    }
                } else {
                    p
                };
                if actual.join("SKILL.md").exists() {
                    return true;
                }
            }
        }
    }
    false
}
