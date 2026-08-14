use serde::Deserialize;
use sqlx::FromRow;
use std::collections::HashMap;

use crate::converter::ir::IrRequest;
use crate::db::get_pool;
use crate::error::ProxyError;
use crate::interceptor::rules::{InterceptorRule, RuleAction, RuleCondition, RulePhase};

#[derive(Debug, FromRow, Deserialize)]
struct DbRule {
    id: String,
    name: String,
    phase: String,
    #[allow(dead_code)]
    rule_type: String,
    condition_json: String,
    action_json: String,
    priority: i64,
    enabled: i64,
}

pub struct InterceptorEngine;

impl InterceptorEngine {
    pub async fn load_rules(phase: &RulePhase) -> Result<Vec<InterceptorRule>, ProxyError> {
        let pool = get_pool().await;

        let db_rules: Vec<DbRule> = sqlx::query_as(
            "SELECT id, name, phase, rule_type, condition_json, action_json, priority, enabled FROM interceptor_rules WHERE phase = ? AND enabled = 1 ORDER BY priority DESC",
        )
        .bind(phase.as_str())
        .fetch_all(pool)
        .await?;

        let mut rules = Vec::new();

        for db_rule in db_rules {
            let rule_phase = RulePhase::from_str(&db_rule.phase).unwrap_or(RulePhase::Pre);

            let condition: RuleCondition =
                serde_json::from_str(&db_rule.condition_json).unwrap_or(RuleCondition::Always);

            let action: RuleAction =
                serde_json::from_str(&db_rule.action_json).unwrap_or(RuleAction::SetHeader {
                    name: "x-no-op".into(),
                    value: "true".into(),
                });

            rules.push(InterceptorRule {
                id: db_rule.id,
                name: db_rule.name,
                phase: rule_phase,
                condition,
                action,
                priority: db_rule.priority,
                enabled: db_rule.enabled != 0,
            });
        }

        Ok(rules)
    }

    pub fn check_condition(
        condition: &RuleCondition,
        request: &IrRequest,
        path: &str,
        headers: &HashMap<String, String>,
    ) -> bool {
        match condition {
            RuleCondition::ModelMatches { pattern } => Self::glob_match(&request.model, pattern),
            RuleCondition::PathContains { substring } => path.contains(substring.as_str()),
            RuleCondition::HeaderExists { name } => headers.contains_key(&name.to_lowercase()),
            RuleCondition::Always => true,
        }
    }

    fn glob_match(model: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        if !pattern.contains('*') {
            return model == pattern;
        }
        let starts_star = pattern.starts_with('*');
        let ends_star = pattern.ends_with('*');
        let parts: Vec<&str> = pattern.split('*').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return true;
        }
        if !starts_star && !model.starts_with(parts[0]) {
            return false;
        }
        if !ends_star && !model.ends_with(parts.last().unwrap()) {
            return false;
        }
        let mut pos = 0;
        for part in &parts {
            match model[pos..].find(part) {
                Some(found) => pos += found + part.len(),
                None => return false,
            }
        }
        true
    }

    pub fn apply_action(
        action: &RuleAction,
        request: &mut IrRequest,
        headers: &mut HashMap<String, String>,
    ) {
        match action {
            RuleAction::ReplaceModel { model } => {
                tracing::info!(
                    "Interceptor: replacing model '{}' with '{}'",
                    request.model,
                    model
                );
                request.model = model.clone();
            }
            RuleAction::SetHeader { name, value } => {
                headers.insert(name.clone(), value.clone());
            }
            RuleAction::RemoveHeader { name } => {
                headers.remove(name);
            }
            RuleAction::InjectSystemPrompt { prompt } => {
                let system_msg = crate::converter::ir::IrMessage {
                    role: crate::converter::ir::IrRole::System,
                    content: vec![crate::converter::ir::IrContentPart::Text {
                        text: prompt.clone(),
                        citations: None,
                    }],
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                };

                let has_system = request
                    .messages
                    .iter()
                    .any(|m| m.role == crate::converter::ir::IrRole::System);

                if has_system {
                    if let Some(first_system) = request
                        .messages
                        .iter_mut()
                        .find(|m| m.role == crate::converter::ir::IrRole::System)
                    {
                        first_system
                            .content
                            .push(crate::converter::ir::IrContentPart::Text {
                                text: format!("\n\n{}", prompt),
                                citations: None,
                            });
                    }
                } else {
                    request.messages.insert(0, system_msg);
                }
            }
            RuleAction::OverrideParameter { parameter, value } => match parameter.as_str() {
                "temperature" => {
                    if let Some(v) = value.as_f64() {
                        request.temperature = Some(v as f32);
                    }
                }
                "top_p" => {
                    if let Some(v) = value.as_f64() {
                        request.top_p = Some(v as f32);
                    }
                }
                "max_tokens" => {
                    if let Some(v) = value.as_u64() {
                        request.max_tokens = Some(v as u32);
                    }
                }
                "stream" => {
                    if let Some(v) = value.as_bool() {
                        request.stream = v;
                    }
                }
                "thinking" => {
                    if value.is_null() || value.as_bool() == Some(false) {
                        request.thinking = None;
                    } else if value.as_bool() == Some(true) {
                        // Bare `true`: enable with the default budget.
                        request.thinking = Some(crate::converter::ir::IrThinkingConfig {
                            mode: crate::converter::ir::ThinkingMode::Enabled,
                            budget_tokens: None,
                            display: None,
                        });
                    } else if let Some(t) = value.as_object() {
                        // Structured form mirroring the Anthropic thinking
                        // parameter: {"type":"enabled","budget_tokens":10000}
                        let mode: Option<crate::converter::ir::ThinkingMode> = match t
                            .get("type")
                            .and_then(|v| v.as_str())
                        {
                            Some("enabled") => Some(crate::converter::ir::ThinkingMode::Enabled),
                            Some("adaptive") => Some(crate::converter::ir::ThinkingMode::Adaptive),
                            Some("disabled") => {
                                request.thinking = None;
                                None
                            }
                            other => {
                                tracing::warn!("Unknown thinking type in override: {:?}", other);
                                None
                            }
                        };
                        if let Some(mode) = mode {
                            request.thinking = Some(crate::converter::ir::IrThinkingConfig {
                                mode,
                                budget_tokens: t
                                    .get("budget_tokens")
                                    .and_then(|v| v.as_u64())
                                    .map(|v| v as u32),
                                display: t
                                    .get("display")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                            });
                        }
                    }
                }
                _ => {
                    tracing::warn!("Unknown override parameter: {}", parameter);
                }
            },
            RuleAction::FilterResponse { .. } => {
                // Applied during response phase, not here
            }
        }
    }

    pub async fn execute_pre_rules(
        request: &mut IrRequest,
        path: &str,
        headers: &mut HashMap<String, String>,
    ) -> Result<(), ProxyError> {
        let rules = Self::load_rules(&RulePhase::Pre).await?;

        for rule in &rules {
            if Self::check_condition(&rule.condition, request, path, headers) {
                tracing::info!("Applying pre-rule: {} ({})", rule.name, rule.id);
                Self::apply_action(&rule.action, request, headers);
            }
        }

        Ok(())
    }
}
