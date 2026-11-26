//! Pi's model defaults as Gentle Shell's `pi_settings` config store shows
//! them (`defaultProvider`, `defaultModel`, `defaultThinkingLevel`,
//! `enabledModels`, Pi's merged view of `settings.json`): read from a
//! `get_config` value and written back as a patch of the keys that changed.

use serde_json::{Map, Value};

/// The defaults one store read describes. A key of the wrong shape reads as
/// unset, as Pi treats it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelDefaults {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    /// The scoped models; empty means every model.
    pub enabled: Vec<String>,
}

impl ModelDefaults {
    /// The store's `value` (`None` when the file is missing).
    pub fn from_value(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        let text = |key: &str| {
            value[key]
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .map(str::to_string)
        };
        Self {
            provider: text("defaultProvider"),
            model: text("defaultModel"),
            thinking: text("defaultThinkingLevel"),
            enabled: value["enabledModels"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|model| model.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The write that turns `read` into these defaults: only the keys that
    /// differ, an unset one as `null` (removed). The server writes the
    /// agent home's `settings.json` alone, so a key left as it was read
    /// (perhaps from the project's file) is not copied there.
    pub fn patch_from(&self, read: &Self) -> Value {
        let mut out = Map::new();
        let mut put = |key: &str, now: &Option<String>, was: &Option<String>| {
            if now != was {
                out.insert(key.into(), now.as_deref().map_or(Value::Null, Value::from));
            }
        };
        put("defaultProvider", &self.provider, &read.provider);
        put("defaultModel", &self.model, &read.model);
        put("defaultThinkingLevel", &self.thinking, &read.thinking);
        if self.enabled != read.enabled {
            let enabled = if self.enabled.is_empty() {
                Value::Null
            } else {
                Value::from(self.enabled.clone())
            };
            out.insert("enabledModels".into(), enabled);
        }
        Value::Object(out)
    }

    /// The default model as shown: `provider/model`, or the model alone.
    pub fn model_label(&self) -> Option<String> {
        match (&self.provider, &self.model) {
            (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
            (None, Some(model)) => Some(model.clone()),
            _ => None,
        }
    }

    /// These defaults with `provider`'s `model` as the default model
    /// (`None`: no default, the first signed-in model).
    pub fn with_model(&self, choice: Option<(&str, &str)>) -> Self {
        let (provider, model) = match choice {
            Some((provider, model)) => (Some(provider.to_string()), Some(model.to_string())),
            None => (None, None),
        };
        Self {
            provider,
            model,
            ..self.clone()
        }
    }

    /// These defaults with `level` as the default effort (`None`: Pi's).
    pub fn with_thinking(&self, level: Option<&str>) -> Self {
        Self {
            thinking: level.map(str::to_string),
            ..self.clone()
        }
    }

    /// These defaults with every model in scope again.
    pub fn with_all_models(&self) -> Self {
        Self {
            enabled: Vec::new(),
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ModelDefaults;

    #[test]
    fn a_read_keeps_only_well_formed_keys() {
        let defaults = ModelDefaults::from_value(Some(&json!({
            "defaultProvider": "anthropic",
            "defaultModel": "claude-opus-4-5",
            "defaultThinkingLevel": 3,
            "enabledModels": ["anthropic/*", 7],
        })));
        assert_eq!(defaults.provider.as_deref(), Some("anthropic"));
        assert_eq!(
            defaults.model_label().as_deref(),
            Some("anthropic/claude-opus-4-5")
        );
        assert_eq!(defaults.thinking, None);
        assert_eq!(defaults.enabled, ["anthropic/*"]);
        assert_eq!(ModelDefaults::from_value(None), ModelDefaults::default());
    }

    #[test]
    fn a_write_patches_only_what_changed_and_an_unset_key_is_null() {
        let read = ModelDefaults::from_value(Some(&json!({
            "defaultProvider": "openai",
            "defaultModel": "gpt-5",
            "defaultThinkingLevel": "high",
            "enabledModels": ["openai/gpt-5", "anthropic/claude-opus-4-5"],
        })));
        assert_eq!(
            read.with_model(Some(("anthropic", "claude-opus-4-5")))
                .patch_from(&read),
            json!({
                "defaultProvider": "anthropic",
                "defaultModel": "claude-opus-4-5",
            })
        );
        assert_eq!(
            read.with_model(None)
                .with_thinking(None)
                .with_all_models()
                .patch_from(&read),
            json!({
                "defaultProvider": null,
                "defaultModel": null,
                "defaultThinkingLevel": null,
                "enabledModels": null,
            })
        );
        assert_eq!(
            read.with_thinking(Some("low")).patch_from(&read),
            json!({ "defaultThinkingLevel": "low" })
        );
        assert_eq!(read.patch_from(&read), json!({}));
    }
}
