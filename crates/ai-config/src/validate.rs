//! Cross-field validation for loaded configs.

use crate::loader::ClientConfig;

/// Verify that `model` exists in at least one provider's `models` list.
///
/// Used by `auto-ai-agent` to fail fast (with a clear message) when a
/// Profession references a model the user hasn't configured.
pub fn validate_model_exists(config: &ClientConfig, model: &str) -> Result<(), String> {
    for (_name, p) in &config.providers {
        if p.models.iter().any(|m| m == model) {
            return Ok(());
        }
    }
    let available: Vec<&str> = config
        .providers
        .values()
        .flat_map(|p| p.models.iter().map(|s| s.as_str()))
        .collect();
    Err(format!(
        "model '{}' not found in any configured provider; available: {:?}",
        model, available
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::parse_client_config;

    fn cfg() -> ClientConfig {
        parse_client_config(
            r#"
            client {
                default_provider : zhipu
                zhipu {
                    kind : openai
                    models : ["glm-4.5", "glm-flash"]
                }
            }
            "#,
        )
        .unwrap()
    }

    #[test]
    fn validates_existing_model() {
        assert!(validate_model_exists(&cfg(), "glm-4.5").is_ok());
        assert!(validate_model_exists(&cfg(), "glm-flash").is_ok());
    }

    #[test]
    fn rejects_unknown_model() {
        let err = validate_model_exists(&cfg(), "nonexistent").unwrap_err();
        assert!(err.contains("not found"));
        assert!(err.contains("glm-4.5")); // lists what's available
    }

    #[test]
    fn empty_config_rejects_everything() {
        let empty = ClientConfig::default();
        let err = validate_model_exists(&empty, "anything").unwrap_err();
        assert!(err.contains("not found"));
    }
}
