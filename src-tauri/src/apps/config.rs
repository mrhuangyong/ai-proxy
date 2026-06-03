use crate::apps::types::AppType;
use std::collections::HashMap;
use std::path::PathBuf;

const PROFILE_ID: &str = "a79ce5f5-a6ac-484e-91ff-c976b9ff98b3";

pub fn codex_config_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    home.join(".codex").join("config.toml")
}

pub fn codex_auth_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    home.join(".codex").join("auth.json")
}

pub fn claude_cli_config_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    home.join(".claude").join("settings.json")
}

pub fn claude_desktop_3p_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.join("Library")
            .join("Application Support")
            .join("Claude-3p")
    }
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("Claude-3p")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.join(".config").join("Claude-3p")
    }
}

pub fn claude_desktop_config_path() -> PathBuf {
    claude_desktop_3p_dir().join("claude_desktop_config.json")
}

fn claude_desktop_profile_path() -> PathBuf {
    claude_desktop_3p_dir()
        .join("configLibrary")
        .join(format!("{}.json", PROFILE_ID))
}

fn claude_desktop_meta_path() -> PathBuf {
    claude_desktop_3p_dir()
        .join("configLibrary")
        .join("_meta.json")
}

pub fn config_path_for(app_type: &AppType) -> PathBuf {
    match app_type {
        AppType::CodexCli | AppType::CodexDesktop => codex_config_path(),
        AppType::ClaudeCli => claude_cli_config_path(),
        AppType::ClaudeDesktop => claude_desktop_config_path(),
    }
}

pub async fn write_codex_config(model: &str, proxy_base: &str, api_key: &str) -> Result<PathBuf, String> {
    let path = codex_config_path();
    let mut config: HashMap<String, toml::Value> = if path.exists() {
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read codex config: {}", e))?;
        toml::from_str(&content).unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    };

    config.insert(
        "model".to_string(),
        toml::Value::String(model.to_string()),
    );

    config.insert(
        "model_provider".to_string(),
        toml::Value::String("ai-proxy".to_string()),
    );

    config.remove("openai_base_url");

    let provider_entry = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert("base_url".into(), toml::Value::String(proxy_base.to_string()));
        table.insert("name".into(), toml::Value::String(model.to_string()));
        table.insert("requires_openai_auth".into(), toml::Value::Boolean(true));
        table.insert("wire_api".into(), toml::Value::String("responses".to_string()));
        table
    });

    match config.get_mut("model_providers") {
        Some(toml::Value::Table(providers)) => {
            providers.insert("ai-proxy".to_string(), provider_entry);
        }
        _ => {
            let mut providers = toml::map::Map::new();
            providers.insert("ai-proxy".to_string(), provider_entry);
            config.insert("model_providers".to_string(), toml::Value::Table(providers));
        }
    }

    let content =
        toml::to_string_pretty(&config).map_err(|e| format!("Failed to serialize config: {}", e))?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    atomic_write(&path, &content).await?;
    tracing::info!("Wrote codex config to {:?}", path);

    // Update auth.json with API key
    write_codex_auth(api_key).await?;

    Ok(path)
}

async fn write_codex_auth(api_key: &str) -> Result<(), String> {
    let path = codex_auth_path();
    let mut auth: serde_json::Value = if path.exists() {
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read auth.json: {}", e))?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if api_key.is_empty() {
        return Ok(());
    }

    let obj = auth.as_object_mut().unwrap();
    obj.insert(
        "auth_mode".to_string(),
        serde_json::Value::String("apikey".to_string()),
    );
    obj.insert(
        "OPENAI_API_KEY".to_string(),
        serde_json::Value::String(api_key.to_string()),
    );
    obj.remove("tokens");
    obj.remove("last_refresh");
    obj.remove("account_id");

    let content = serde_json::to_string_pretty(&auth)
        .map_err(|e| format!("Failed to serialize auth.json: {}", e))?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create auth directory: {}", e))?;
    }

    atomic_write(&path, &content).await?;
    tracing::info!("Wrote codex auth to {:?}", path);
    Ok(())
}

pub async fn write_claude_cli_config(
    model: &str,
    model_haiku: Option<&str>,
    model_sonnet: Option<&str>,
    model_opus: Option<&str>,
    proxy_base: &str,
    api_key: &str,
) -> Result<PathBuf, String> {
    let path = claude_cli_config_path();
    let mut config: serde_json::Value = if path.exists() {
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read claude cli config: {}", e))?;
        serde_json::from_str(&content).unwrap_or(serde_json::Value::Object(Default::default()))
    } else {
        serde_json::Value::Object(Default::default())
    };

    let env = config
        .as_object_mut()
        .unwrap()
        .entry("env")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));

    if let Some(env_obj) = env.as_object_mut() {
        env_obj.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            serde_json::Value::String(proxy_base.to_string()),
        );
        if !api_key.is_empty() {
            env_obj.insert(
                "ANTHROPIC_API_KEY".to_string(),
                serde_json::Value::String(api_key.to_string()),
            );
        }
        env_obj.insert(
            "ANTHROPIC_MODEL".to_string(),
            serde_json::Value::String(model.to_string()),
        );
        if let Some(haiku) = model_haiku {
            env_obj.insert(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                serde_json::Value::String(haiku.to_string()),
            );
        }
        if let Some(sonnet) = model_sonnet {
            env_obj.insert(
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                serde_json::Value::String(sonnet.to_string()),
            );
        }
        if let Some(opus) = model_opus {
            env_obj.insert(
                "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                serde_json::Value::String(opus.to_string()),
            );
        }
    }

    let content = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    atomic_write(&path, &content).await?;
    tracing::info!("Wrote claude cli config to {:?}", path);
    Ok(path)
}

fn build_claude_desktop_inference_models(
    model: &str,
    model_haiku: Option<&str>,
    model_sonnet: Option<&str>,
    model_opus: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut models = Vec::new();

    // Haiku tier
    let mut haiku = serde_json::json!({"name": "claude-haiku-4-5"});
    if let Some(h) = model_haiku {
        haiku.as_object_mut().unwrap().insert(
            "labelOverride".into(),
            serde_json::Value::String(h.to_string()),
        );
    }
    models.push(haiku);

    // Sonnet tier — default model maps here
    let mut sonnet = serde_json::json!({"name": "claude-sonnet-4-6"});
    let sonnet_label = model_sonnet.or(Some(model));
    if let Some(s) = sonnet_label {
        sonnet.as_object_mut().unwrap().insert(
            "labelOverride".into(),
            serde_json::Value::String(s.to_string()),
        );
    }
    models.push(sonnet);

    // Opus tier
    let mut opus = serde_json::json!({"name": "claude-opus-4-7", "supports1m": true});
    if let Some(o) = model_opus {
        opus.as_object_mut().unwrap().insert(
            "labelOverride".into(),
            serde_json::Value::String(o.to_string()),
        );
    }
    models.push(opus);

    models
}

pub async fn write_claude_desktop_config(
    model: &str,
    model_haiku: Option<&str>,
    model_sonnet: Option<&str>,
    model_opus: Option<&str>,
    proxy_base: &str,
    api_key: &str,
) -> Result<PathBuf, String> {
    let base_dir = claude_desktop_3p_dir();
    let library_dir = base_dir.join("configLibrary");

    tokio::fs::create_dir_all(&library_dir)
        .await
        .map_err(|e| format!("Failed to create Claude-3p directories: {}", e))?;

    // 1. Write the gateway profile
    let inference_models = build_claude_desktop_inference_models(model, model_haiku, model_sonnet, model_opus);
    let profile = serde_json::json!({
        "disableDeploymentModeChooser": true,
        "inferenceGatewayApiKey": api_key,
        "inferenceGatewayAuthScheme": "bearer",
        "inferenceGatewayBaseUrl": proxy_base,
        "inferenceProvider": "gateway",
        "inferenceModels": inference_models
    });
    let profile_path = claude_desktop_profile_path();
    let profile_content = serde_json::to_string_pretty(&profile)
        .map_err(|e| format!("Failed to serialize profile: {}", e))?;
    atomic_write(&profile_path, &profile_content).await?;
    tracing::info!("Wrote Claude Desktop profile to {:?}", profile_path);

    // 2. Write _meta.json — merge with existing if present
    let meta_path = claude_desktop_meta_path();
    let mut meta = if meta_path.exists() {
        let content = tokio::fs::read_to_string(&meta_path)
            .await
            .map_err(|e| format!("Failed to read _meta.json: {}", e))?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let meta_obj = meta.as_object_mut().unwrap();
    meta_obj.insert(
        "appliedId".to_string(),
        serde_json::Value::String(PROFILE_ID.to_string()),
    );

    let entries = meta_obj
        .entry("entries")
        .or_insert_with(|| serde_json::Value::Array(vec![]));
    if let Some(entries_arr) = entries.as_array() {
        let has_profile = entries_arr.iter().any(|e| {
            e.as_object()
                .and_then(|o| o.get("id"))
                .and_then(|v| v.as_str())
                .map_or(false, |id| id == PROFILE_ID)
        });
        if !has_profile {
            entries
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "id": PROFILE_ID,
                    "name": "AiProxy"
                }));
        }
    }

    let meta_content = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("Failed to serialize _meta.json: {}", e))?;
    atomic_write(&meta_path, &meta_content).await?;
    tracing::info!("Wrote Claude Desktop meta to {:?}", meta_path);

    // 3. Write claude_desktop_config.json — set deployment mode
    let config_path = claude_desktop_config_path();
    let mut config = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|e| format!("Failed to read claude_desktop_config.json: {}", e))?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    config
        .as_object_mut()
        .unwrap()
        .insert("deploymentMode".to_string(), serde_json::Value::String("3p".to_string()));

    let config_content = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize claude_desktop_config.json: {}", e))?;
    atomic_write(&config_path, &config_content).await?;
    tracing::info!("Wrote Claude Desktop config to {:?}", config_path);

    Ok(config_path)
}

pub async fn write_config(
    app_type: &AppType,
    model: &str,
    model_haiku: Option<&str>,
    model_sonnet: Option<&str>,
    model_opus: Option<&str>,
    proxy_base: &str,
    api_key: &str,
) -> Result<PathBuf, String> {
    match app_type {
        AppType::CodexCli | AppType::CodexDesktop => {
            write_codex_config(model, proxy_base, api_key).await
        }
        AppType::ClaudeCli => {
            write_claude_cli_config(model, model_haiku, model_sonnet, model_opus, proxy_base, api_key).await
        }
        AppType::ClaudeDesktop => {
            write_claude_desktop_config(model, model_haiku, model_sonnet, model_opus, proxy_base, api_key).await
        }
    }
}

pub(crate) async fn atomic_write(path: &PathBuf, content: &str) -> Result<(), String> {
    let tmp_path = path.with_extension("tmp");
    tokio::fs::write(&tmp_path, content)
        .await
        .map_err(|e| format!("Failed to write temp file: {}", e))?;
    tokio::fs::rename(&tmp_path, path)
        .await
        .map_err(|e| format!("Failed to rename temp file: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_toml_config_serialization() {
        let mut config: HashMap<String, toml::Value> = HashMap::new();
        config.insert(
            "model".to_string(),
            toml::Value::String("gpt-4".to_string()),
        );
        config.insert(
            "openai_base_url".to_string(),
            toml::Value::String("http://127.0.0.1:7860/v1".to_string()),
        );

        let output = toml::to_string_pretty(&config).expect("Failed to serialize");

        assert!(output.contains("model = \"gpt-4\""));
        assert!(output.contains("openai_base_url = \"http://127.0.0.1:7860/v1\""));
    }

    #[test]
    fn test_toml_config_preserves_existing() {
        let initial = r#"approval_policy = "on-request""#;
        let mut config: HashMap<String, toml::Value> =
            toml::from_str(initial).expect("Failed to parse initial TOML");

        config.insert(
            "model".to_string(),
            toml::Value::String("gpt-4".to_string()),
        );
        config.insert(
            "openai_base_url".to_string(),
            toml::Value::String("http://127.0.0.1:7860/v1".to_string()),
        );

        let output = toml::to_string_pretty(&config).expect("Failed to serialize");

        assert!(
            output.contains("approval_policy = \"on-request\""),
            "original field should be preserved"
        );
        assert!(output.contains("model = \"gpt-4\""));
        assert!(output.contains("openai_base_url = \"http://127.0.0.1:7860/v1\""));
    }

    #[test]
    fn test_json_config_serialization() {
        let initial = r#"{"language":"Chinese","env":{"ANTHROPIC_API_KEY":"sk-xxx"}}"#;
        let mut config: serde_json::Value =
            serde_json::from_str(initial).expect("Failed to parse initial JSON");

        let env = config
            .as_object_mut()
            .unwrap()
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));

        if let Some(env_obj) = env.as_object_mut() {
            env_obj.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                serde_json::Value::String("http://127.0.0.1:7860".to_string()),
            );
            env_obj.insert(
                "ANTHROPIC_MODEL".to_string(),
                serde_json::Value::String("claude-sonnet-4-20250514".to_string()),
            );
        }

        let output = serde_json::to_string_pretty(&config).expect("Failed to serialize");

        assert!(output.contains("\"language\": \"Chinese\""));
        assert!(output.contains("\"ANTHROPIC_API_KEY\": \"sk-xxx\""));
        assert!(output.contains("\"ANTHROPIC_BASE_URL\": \"http://127.0.0.1:7860\""));
        assert!(output.contains("\"ANTHROPIC_MODEL\": \"claude-sonnet-4-20250514\""));
    }

    #[test]
    fn test_json_config_creates_env_if_missing() {
        let initial = r#"{"language":"Chinese"}"#;
        let mut config: serde_json::Value =
            serde_json::from_str(initial).expect("Failed to parse initial JSON");

        let env = config
            .as_object_mut()
            .unwrap()
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));

        if let Some(env_obj) = env.as_object_mut() {
            env_obj.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                serde_json::Value::String("http://127.0.0.1:7860".to_string()),
            );
            env_obj.insert(
                "ANTHROPIC_MODEL".to_string(),
                serde_json::Value::String("claude-sonnet-4-20250514".to_string()),
            );
        }

        let output = serde_json::to_string_pretty(&config).expect("Failed to serialize");

        assert!(output.contains("\"language\": \"Chinese\""));
        assert!(output.contains("\"ANTHROPIC_BASE_URL\": \"http://127.0.0.1:7860\""));
        assert!(output.contains("\"ANTHROPIC_MODEL\": \"claude-sonnet-4-20250514\""));
    }

    #[test]
    fn test_claude_cli_config_with_model_tiers() {
        let initial = r#"{"language":"Chinese"}"#;
        let mut config: serde_json::Value =
            serde_json::from_str(initial).expect("Failed to parse initial JSON");

        let env = config
            .as_object_mut()
            .unwrap()
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));

        if let Some(env_obj) = env.as_object_mut() {
            env_obj.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                serde_json::Value::String("http://127.0.0.1:7860".to_string()),
            );
            env_obj.insert(
                "ANTHROPIC_MODEL".to_string(),
                serde_json::Value::String("claude-sonnet-4-20250514".to_string()),
            );
            env_obj.insert(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                serde_json::Value::String("claude-haiku-4-20250514".to_string()),
            );
            env_obj.insert(
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                serde_json::Value::String("claude-sonnet-4-20250514".to_string()),
            );
            env_obj.insert(
                "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                serde_json::Value::String("claude-opus-4-20250514".to_string()),
            );
        }

        let output = serde_json::to_string_pretty(&config).expect("Failed to serialize");

        assert!(output.contains("\"ANTHROPIC_MODEL\": \"claude-sonnet-4-20250514\""));
        assert!(output.contains("\"ANTHROPIC_DEFAULT_HAIKU_MODEL\": \"claude-haiku-4-20250514\""));
        assert!(output.contains("\"ANTHROPIC_DEFAULT_SONNET_MODEL\": \"claude-sonnet-4-20250514\""));
        assert!(output.contains("\"ANTHROPIC_DEFAULT_OPUS_MODEL\": \"claude-opus-4-20250514\""));
    }

    #[test]
    fn test_claude_cli_config_optional_tiers_absent() {
        let initial = r#"{}"#;
        let mut config: serde_json::Value =
            serde_json::from_str(initial).expect("Failed to parse initial JSON");

        let env = config
            .as_object_mut()
            .unwrap()
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));

        if let Some(env_obj) = env.as_object_mut() {
            env_obj.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                serde_json::Value::String("http://127.0.0.1:7860".to_string()),
            );
            env_obj.insert(
                "ANTHROPIC_MODEL".to_string(),
                serde_json::Value::String("claude-sonnet-4-20250514".to_string()),
            );
            // No model tiers added
        }

        let output = serde_json::to_string_pretty(&config).expect("Failed to serialize");

        assert!(output.contains("\"ANTHROPIC_MODEL\": \"claude-sonnet-4-20250514\""));
        assert!(!output.contains("ANTHROPIC_DEFAULT_HAIKU_MODEL"));
        assert!(!output.contains("ANTHROPIC_DEFAULT_SONNET_MODEL"));
        assert!(!output.contains("ANTHROPIC_DEFAULT_OPUS_MODEL"));
    }

    #[test]
    fn test_claude_desktop_3p_profile_serialization() {
        let model = "my-custom-model";
        let proxy_base = "http://127.0.0.1:7860";
        let api_key = "sk-test-key-123";

        let inference_models = build_claude_desktop_inference_models(
            model, Some("haiku-model"), Some("sonnet-model"), Some("opus-model"),
        );
        let profile = serde_json::json!({
            "disableDeploymentModeChooser": true,
            "inferenceGatewayApiKey": api_key,
            "inferenceGatewayAuthScheme": "bearer",
            "inferenceGatewayBaseUrl": proxy_base,
            "inferenceProvider": "gateway",
            "inferenceModels": inference_models
        });

        let output = serde_json::to_string_pretty(&profile).expect("Failed to serialize");

        assert!(output.contains("\"disableDeploymentModeChooser\": true"));
        assert!(output.contains("\"inferenceGatewayApiKey\": \"sk-test-key-123\""));
        assert!(output.contains("\"inferenceGatewayAuthScheme\": \"bearer\""));
        assert!(output.contains("\"inferenceGatewayBaseUrl\": \"http://127.0.0.1:7860\""));
        assert!(output.contains("\"inferenceProvider\": \"gateway\""));
        assert!(output.contains("\"name\": \"claude-haiku-4-5\""));
        assert!(output.contains("\"labelOverride\": \"haiku-model\""));
        assert!(output.contains("\"name\": \"claude-sonnet-4-6\""));
        assert!(output.contains("\"labelOverride\": \"sonnet-model\""));
        assert!(output.contains("\"name\": \"claude-opus-4-7\""));
        assert!(output.contains("\"labelOverride\": \"opus-model\""));
        assert!(output.contains("\"supports1m\": true"));
    }

    #[test]
    fn test_claude_desktop_inference_models_default_fallback() {
        let models = build_claude_desktop_inference_models(
            "gpt-4o", None, None, None,
        );
        assert_eq!(models.len(), 3);

        // Haiku — no labelOverride
        let haiku = &models[0];
        assert_eq!(haiku["name"].as_str(), Some("claude-haiku-4-5"));
        assert!(haiku.get("labelOverride").is_none());

        // Sonnet — default model used as label
        let sonnet = &models[1];
        assert_eq!(sonnet["name"].as_str(), Some("claude-sonnet-4-6"));
        assert_eq!(sonnet["labelOverride"].as_str(), Some("gpt-4o"));

        // Opus — no labelOverride, has supports1m
        let opus = &models[2];
        assert_eq!(opus["name"].as_str(), Some("claude-opus-4-7"));
        assert!(opus.get("labelOverride").is_none());
        assert_eq!(opus["supports1m"].as_bool(), Some(true));
    }

    #[test]
    fn test_claude_desktop_meta_merges_profile_id() {
        let profile_id = "A79CE5F5-A6AC-484E-91FF-C976B9FF98B3";
        let mut meta = serde_json::json!({
            "entries": [{ "id": "other-profile", "name": "Other" }]
        });

        let meta_obj = meta.as_object_mut().unwrap();
        meta_obj.insert(
            "appliedId".to_string(),
            serde_json::Value::String(profile_id.to_string()),
        );

        let entries = meta_obj
            .entry("entries")
            .or_insert_with(|| serde_json::Value::Array(vec![]));
        if let Some(entries_arr) = entries.as_array() {
            let has_profile = entries_arr.iter().any(|e| {
                e.as_object()
                    .and_then(|o| o.get("id"))
                    .and_then(|v| v.as_str())
                    .map_or(false, |id| id == profile_id)
            });
            if !has_profile {
                entries
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "id": profile_id,
                        "name": "AiProxy"
                    }));
            }
        }

        let output = serde_json::to_string_pretty(&meta).expect("Failed to serialize");

        assert!(output.contains("\"appliedId\": \"A79CE5F5-A6AC-484E-91FF-C976B9FF98B3\""));
        assert!(output.contains("\"id\": \"other-profile\""));
        assert!(output.contains("\"name\": \"AiProxy\""));
    }

    #[test]
    fn test_claude_desktop_meta_no_duplicate_profile() {
        let profile_id = "A79CE5F5-A6AC-484E-91FF-C976B9FF98B3";
        let mut meta = serde_json::json!({
            "entries": [{ "id": profile_id, "name": "AiProxy" }]
        });

        let meta_obj = meta.as_object_mut().unwrap();
        meta_obj.insert(
            "appliedId".to_string(),
            serde_json::Value::String(profile_id.to_string()),
        );

        let entries = meta_obj
            .entry("entries")
            .or_insert_with(|| serde_json::Value::Array(vec![]));
        if let Some(entries_arr) = entries.as_array() {
            let has_profile = entries_arr.iter().any(|e| {
                e.as_object()
                    .and_then(|o| o.get("id"))
                    .and_then(|v| v.as_str())
                    .map_or(false, |id| id == profile_id)
            });
            if !has_profile {
                entries
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "id": profile_id,
                        "name": "AiProxy"
                    }));
            }
        }

        let entries_arr = meta.get("entries").unwrap().as_array().unwrap();
        let count = entries_arr
            .iter()
            .filter(|e| {
                e.as_object()
                    .and_then(|o| o.get("id"))
                    .and_then(|v| v.as_str())
                    .map_or(false, |id| id == profile_id)
            })
            .count();
        assert_eq!(count, 1, "Profile entry should not be duplicated");
    }

    #[test]
    fn test_claude_desktop_deployment_mode_merge() {
        let initial = r#"{"theme":"dark"}"#;
        let mut config: serde_json::Value =
            serde_json::from_str(initial).expect("Failed to parse initial JSON");

        config
            .as_object_mut()
            .unwrap()
            .insert("deploymentMode".to_string(), serde_json::Value::String("3p".to_string()));

        let output = serde_json::to_string_pretty(&config).expect("Failed to serialize");

        assert!(output.contains("\"theme\": \"dark\""));
        assert!(output.contains("\"deploymentMode\": \"3p\""));
    }

    #[tokio::test]
    async fn test_atomic_write() {
        let dir = tempfile::TempDir::new().expect("Failed to create temp dir");
        let file_path = PathBuf::from(dir.path()).join("test_config.toml");
        let content = "model = \"gpt-4\"\nopenai_base_url = \"http://127.0.0.1:7860/v1\"\n";

        atomic_write(&file_path, content)
            .await
            .expect("atomic_write failed");

        let read_back = tokio::fs::read_to_string(&file_path)
            .await
            .expect("Failed to read back file");

        assert_eq!(read_back, content);
    }
}
