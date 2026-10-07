//! Bounded provider capability discovery. No inference or database tools.
use serde_json::Value;
use sift_protocol::AiProvider;
use sift_workspace_ui::AiModelOption;

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub(crate) fn codex_models(result: &Value) -> Result<Vec<AiModelOption>, String> {
    let entries = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Invalid Codex model catalog")?;
    entries
        .iter()
        .filter(|entry| entry.get("hidden").and_then(Value::as_bool) != Some(true))
        .map(|entry| {
            let id = entry
                .get("model")
                .and_then(Value::as_str)
                .filter(|id| identifier(id))
                .ok_or("Invalid provider model identifier")?;
            let reasoning = entry
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| option.get("reasoningEffort").and_then(Value::as_str))
                .filter(|effort| {
                    effort.len() <= 32
                        && effort
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                })
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let default_reasoning = entry
                .get("defaultReasoningEffort")
                .and_then(Value::as_str)
                .filter(|effort| reasoning.iter().any(|choice| choice == effort))
                .map(str::to_owned);
            Ok(AiModelOption {
                id: id.into(),
                name: entry
                    .get("displayName")
                    .and_then(Value::as_str)
                    .filter(|name| identifier(name))
                    .unwrap_or(id)
                    .into(),
                reasoning,
                default_reasoning,
                is_default: entry
                    .get("isDefault")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn opencode_models(output: &str) -> Result<Vec<AiModelOption>, String> {
    let mut remaining = output.trim();
    let mut models = Vec::new();
    while !remaining.is_empty() {
        let (id, json) = remaining
            .split_once('\n')
            .ok_or("Invalid OpenCode model catalog")?;
        if !identifier(id) {
            return Err("Invalid provider model identifier".into());
        }
        let mut stream = serde_json::Deserializer::from_str(json).into_iter::<Value>();
        let model = stream
            .next()
            .ok_or("Missing OpenCode model metadata")?
            .map_err(|_| "Invalid OpenCode model metadata")?;
        let reasoning = model
            .get("variants")
            .and_then(Value::as_object)
            .map(|variants| {
                variants
                    .keys()
                    .filter(|effort| {
                        effort.len() <= 32
                            && effort.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                            })
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        models.push(AiModelOption {
            id: id.into(),
            name: model
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| identifier(name))
                .unwrap_or(id)
                .into(),
            reasoning,
            default_reasoning: None,
            is_default: false,
        });
        if models.len() > 512 {
            return Err("Provider model catalog exceeds limit".into());
        }
        remaining = json[stream.byte_offset()..].trim();
    }
    Ok(models)
}

pub(crate) async fn load(provider: AiProvider) -> Result<Vec<AiModelOption>, String> {
    let mut models = discover(provider).await?;
    let configured = match provider {
        AiProvider::Codex => crate::ai_codex::configured_model(),
        _ => crate::ai_cli::configured_model(provider),
    };
    if let Some(configured) = configured {
        for model in &mut models {
            model.is_default = model.id == configured;
        }
        if !models.iter().any(|model| model.is_default) {
            models.insert(
                0,
                AiModelOption {
                    name: configured.clone(),
                    id: configured,
                    reasoning: Vec::new(),
                    default_reasoning: None,
                    is_default: true,
                },
            );
        }
    }
    if models.is_empty() {
        return Err("The provider reported no models; enter a custom model or refresh".into());
    }
    Ok(models)
}

async fn discover(provider: AiProvider) -> Result<Vec<AiModelOption>, String> {
    match provider {
        AiProvider::Codex => crate::ai_codex::models().await,
        AiProvider::ClaudeCode => {
            // Stable CLI aliases track the signed-in provider's current models,
            // as T3's Claude adapter uses a curated catalog rather than model/list.
            let configured = crate::ai_cli::configured_model(provider);
            let mut models = [
                ("sonnet", "Sonnet", vec!["low", "medium", "high"]),
                ("opus", "Opus", vec!["low", "medium", "high", "max"]),
                ("haiku", "Haiku", vec![]),
            ]
            .into_iter()
            .map(|(id, name, reasoning)| AiModelOption {
                id: id.into(),
                name: name.into(),
                reasoning: reasoning.into_iter().map(str::to_owned).collect(),
                default_reasoning: None,
                is_default: configured
                    .as_deref()
                    .map_or(id == "sonnet", |model| model == id),
            })
            .collect::<Vec<_>>();
            if let Some(id) = configured.filter(|id| !models.iter().any(|model| model.id == *id)) {
                models.insert(
                    0,
                    AiModelOption {
                        name: id.clone(),
                        id,
                        reasoning: Vec::new(),
                        default_reasoning: None,
                        is_default: true,
                    },
                );
            }
            Ok(models)
        }
        AiProvider::OpenCode => {
            use tokio::io::AsyncReadExt;
            let mut command = tokio::process::Command::new("opencode");
            command
                .args(["models", "--pure", "--verbose"])
                .current_dir(std::env::temp_dir())
                .env_clear();
            for key in [
                "HOME",
                "PATH",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_CACHE_HOME",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            let mut child = command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| "OpenCode CLI is unavailable")?;
            let mut bytes = Vec::new();
            child
                .stdout
                .take()
                .ok_or("OpenCode catalog unavailable")?
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| "Cannot read OpenCode models")?;
            if bytes.len() > 2 * 1024 * 1024 {
                return Err("Provider model catalog exceeds limit".into());
            }
            if !child
                .wait()
                .await
                .map_err(|_| "OpenCode catalog failed")?
                .success()
            {
                return Err("Cannot load OpenCode models".into());
            }
            let mut models = opencode_models(
                std::str::from_utf8(&bytes).map_err(|_| "Invalid provider catalog encoding")?,
            )?;
            if let Some(selected) = crate::ai_cli::configured_model(provider) {
                for model in &mut models {
                    model.is_default = model.id == selected;
                }
            }
            Ok(models)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_catalog_preserves_supported_efforts_and_provider_model_ids() {
        let models = codex_models(&serde_json::json!({"data":[{"id":"opaque", "model":"custom-model", "displayName":"Custom", "isDefault":true, "defaultReasoningEffort":"high", "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]},{"model":"hidden","hidden":true}]})).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "custom-model");
        assert_eq!(models[0].reasoning, ["low", "high"]);
        assert_eq!(models[0].default_reasoning.as_deref(), Some("high"));
        assert!(models[0].is_default);
        let models = opencode_models("provider/model\n{\"name\":\"Model\",\"variants\":{\"high\":{},\"low\":{}}}\nprovider/other\n{\"name\":\"Other\"}\n").unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "provider/model");
        assert_eq!(models[0].reasoning, ["high", "low"]);
        assert!(models[1].reasoning.is_empty());
    }
}
