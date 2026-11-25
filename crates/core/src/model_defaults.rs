//! Pi's model defaults as Gentle Shell's `model_defaults` config store holds
//! them (`defaultProvider`, `defaultModel`, `defaultThinkingLevel`,
//! `enabledModels` of the agent home's `settings.json`): read from a
//! `get_config` value and written back whole, an absent key unset.

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

    /// The complete write: every key set, nothing else.
    pub fn to_value(&self) -> Value {
        let mut out = Map::new();
        let mut put = |key: &str, value: &Option<String>| {
            if let Some(value) = value {
                out.insert(key.into(), Value::from(value.as_str()));
            }
        };
        put("defaultProvider", &self.provider);
        put("defaultModel", &self.model);
        put("defaultThinkingLevel", &self.thinking);
        if !self.enabled.is_empty() {
            out.insert("enabledModels".into(), Value::from(self.enabled.clone()));
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
    fn a_write_is_the_whole_store_and_an_unset_key_is_left_out() {
        let read = ModelDefaults::from_value(Some(&json!({
            "defaultProvider": "openai",
            "defaultModel": "gpt-5",
            "defaultThinkingLevel": "high",
            "enabledModels": ["openai/gpt-5", "anthropic/claude-opus-4-5"],
        })));
        assert_eq!(
            read.with_model(Some(("anthropic", "claude-opus-4-5")))
                .to_value(),
            json!({
                "defaultProvider": "anthropic",
                "defaultModel": "claude-opus-4-5",
                "defaultThinkingLevel": "high",
                "enabledModels": ["openai/gpt-5", "anthropic/claude-opus-4-5"],
            })
        );
        assert_eq!(
            read.with_model(None)
                .with_thinking(None)
                .with_all_models()
                .to_value(),
            json!({})
        );
        assert_eq!(
            read.with_thinking(Some("low")).to_value()["defaultThinkingLevel"],
            "low"
        );
    }
}
